use super::*;

pub(crate) async fn handle_vault_list(
    server: &MemoryServer,
    params: VaultListParams,
) -> Result<String, String> {
    if !is_vault_initialized(server)? {
        return Err("Vault not initialized. Call vault_init first.".into());
    }

    let mut entries = server
        .with_global_store_read(|store| store.vault_list_entries().map_err(|e| e.to_string()))
        .map_err(|e| format!("Failed to list secrets: {e}"))?;
    if let Some(ref secret_type) = params.secret_type {
        let want = normalize_secret_type(secret_type);
        entries.retain(|entry| {
            memcore::effective_vault_secret_type(&entry.name, &entry.secret_type) == want
        });
    }

    let payload: Vec<serde_json::Value> = entries
        .into_iter()
        .map(|e| {
            let secret_type = memcore::effective_vault_secret_type(&e.name, &e.secret_type);
            let group = if secret_type == memcore::SECRET_TYPE_CONFIG {
                "config"
            } else {
                "credential"
            };
            json!({
                "name": e.name,
                "secret_type": secret_type,
                "group": group,
                "description": e.description,
                "allowed_agents": e.allowed_agents,
                "created_at": e.created_at,
                "updated_at": e.updated_at,
                "access_count": e.access_count,
            })
        })
        .collect();
    let config: Vec<_> = payload
        .iter()
        .filter(|row| row.get("group").and_then(|v| v.as_str()) == Some("config"))
        .cloned()
        .collect();
    let credentials: Vec<_> = payload
        .iter()
        .filter(|row| row.get("group").and_then(|v| v.as_str()) != Some("config"))
        .cloned()
        .collect();

    let resp = json!({
        "count": payload.len(),
        "credentials": credentials,
        "config": config,
        "secrets": payload,
    });
    serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_vault_remove(
    server: &MemoryServer,
    params: VaultRemoveParams,
) -> Result<String, String> {
    let secret_name = params.name.clone();
    let result = (|| {
        let effective_agent_id = resolve_vault_acl_agent_id(server, params.agent_id.as_deref())?;
        authorize_vault_mutation(server, &params.name, params.agent_id.as_deref())
            .map_err(|e| e.to_string())?;
        let removed = with_vault_key(server, |key| {
            server.with_global_store(|store| {
            let transaction = store
                .begin_vault_transaction()
                .map_err(|e| format!("Failed to begin remove transaction: {e}"))?;
            if let Some(existing) = transaction
                .vault_get_entry(&params.name)
                .map_err(|e| format!("Failed to read removal target: {e}"))?
            {
                ensure_agent_allowed(&existing, effective_agent_id.as_deref())
                    .map_err(|e| e.to_string())?;
            }
            if let Some((prefix, _)) =
                crate::provider_config::parse_rotation_member_name(&params.name)
            {
                if transaction
                    .vault_get_rotation(prefix)
                    .map_err(|e| format!("Failed to read rotation config: {e}"))?
                    .is_some()
                {
                    return Err(format!(
                        "Vault name '{}' is a configured rotation member; refusing deletion while rotation '{}' exists",
                        params.name, prefix
                    ));
                }
            }
            crate::vault_ops::account_events::prepare_entry_removal(&transaction, key, &params.name)?;
            let removed = transaction
                .vault_delete_entry(&params.name)
                .map_err(|e| format!("Failed to remove secret: {e}"))?;
            transaction
                .commit()
                .map_err(|e| format!("Failed to commit remove transaction: {e}"))?;
            Ok::<_, String>(removed)
        })
        })?;

        if removed {
            serde_json::to_string(&json!({
                "removed": true,
                "name": params.name
            }))
            .map_err(|e| format!("serialize: {e}"))
        } else {
            Err(format!("Secret not found: {}", params.name))
        }
    })();

    let audit_result = record_vault_audit(
        server,
        "vault_remove",
        Some(&secret_name),
        result.is_ok(),
        result.as_ref().err().map(String::as_str),
    );

    let result = match result {
        Ok(value) => Ok(value),
        Err(err) if err.starts_with("Secret not found: ") => serde_json::to_string(&json!({
            "removed": false,
            "error": err,
        }))
        .map_err(|e| format!("serialize: {e}")),
        Err(err) => Err(err),
    };
    result_with_vault_audit_warning(result, audit_result)
}

pub(crate) async fn handle_vault_status(server: &MemoryServer) -> Result<String, String> {
    let initialized = is_vault_initialized(server)?;
    maybe_auto_lock_vault(server);
    let (locked, auto_lock_secs) = {
        let v = server.vault_read();
        (v.key.is_none(), v.auto_lock_after_secs)
    };
    let entry_count = if initialized {
        server
            .with_global_store_read(|store| store.vault_count_entries().map_err(|e| e.to_string()))
            .unwrap_or(0)
    } else {
        0
    };
    let keychain_available_result =
        crate::provider_config::keychain_vault_password_entry_available();
    let (keychain_available, keychain_error) = match keychain_available_result {
        Ok(available) => (available, None),
        Err(error) => (false, Some(error)),
    };
    let resolver_state = if !initialized {
        "not_initialized"
    } else if !locked {
        "unlocked"
    } else if keychain_available {
        "locked_keychain_available"
    } else if keychain_error.is_some() {
        "auto_unlock_failed"
    } else {
        "locked"
    };
    let provider_secret_count = server.llm.provider_secret_count();

    let resp = json!({
        "initialized": initialized,
        "locked": locked,
        "entry_count": entry_count,
        "auto_lock_after_secs": auto_lock_secs,
        "session": {
            "locked": locked,
            "unlocked": !locked,
            "auto_lock_after_secs": auto_lock_secs,
        },
        "secure_store": {
            "backend": if cfg!(target_os = "macos") { "macos_keychain" } else { "unavailable" },
            "service": "tachi-vault",
            "account": "default",
            "auto_unlock_available": keychain_available,
            "last_error": keychain_error,
        },
        "provider_cache": {
            "secret_pool_count": provider_secret_count,
            "loaded": provider_secret_count > 0,
        },
        "resolver": {
            "state": resolver_state,
            "last_failure": null,
        },
    });
    serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))
}
