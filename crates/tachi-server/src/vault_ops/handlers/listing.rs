use super::*;

pub(crate) async fn handle_vault_list(
    server: &MemoryServer,
    params: VaultListParams,
) -> Result<String, String> {
    if !is_vault_initialized(server)? {
        return Err("Vault not initialized. Call vault_init first.".into());
    }

    let entries = if let Some(ref secret_type) = params.secret_type {
        let secret_type = normalize_secret_type(secret_type);
        server.with_global_store_read(|store| {
            store
                .vault_list_entries_by_type(secret_type)
                .map_err(|e| e.to_string())
        })
    } else {
        server.with_global_store_read(|store| store.vault_list_entries().map_err(|e| e.to_string()))
    }
    .map_err(|e| format!("Failed to list secrets: {e}"))?;

    let payload: Vec<serde_json::Value> = entries
        .into_iter()
        .map(|e| {
            json!({
                "name": e.name,
                "secret_type": e.secret_type,
                "description": e.description,
                "allowed_agents": e.allowed_agents,
                "created_at": e.created_at,
                "updated_at": e.updated_at,
                "access_count": e.access_count,
            })
        })
        .collect();

    let resp = json!({
        "count": payload.len(),
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
        authorize_vault_mutation(server, &params.name, params.agent_id.as_deref())
            .map_err(|e| e.to_string())?;
        let removed = server
            .with_global_store(|store| {
                store
                    .vault_delete_entry(&params.name)
                    .map_err(|e| e.to_string())
            })
            .map_err(|e| format!("Failed to remove secret: {e}"))?;

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
