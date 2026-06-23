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
        ensure_vault_unlocked(server)?;
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

    let resp = json!({
        "initialized": initialized,
        "locked": locked,
        "entry_count": entry_count,
        "auto_lock_after_secs": auto_lock_secs,
    });
    serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))
}
