use super::*;

pub(crate) async fn handle_vault_set(
    server: &MemoryServer,
    mut params: VaultSetParams,
) -> Result<String, String> {
    let secret_name = params.name.clone();
    let value = crypto::ZeroizingString::new(std::mem::take(&mut params.value));
    let result = (|| {
        crypto::validate_secret_name(&params.name)?;
        if is_lane_slot_secret_name(&params.name) && value.trim().is_empty() {
            return Err("Secret value cannot be empty".to_string());
        }
        let effective_agent_id = resolve_vault_acl_agent_id(server, params.agent_id.as_deref())?;
        if !is_lane_slot_secret_name(&params.name) {
            authorize_vault_mutation(server, &params.name, params.agent_id.as_deref())
                .map_err(|e| e.to_string())?;
        }
        with_vault_key(server, |key| {
            let secret_type = if params.secret_type.trim().is_empty() {
                memcore::infer_vault_secret_type(&params.name)
            } else {
                normalize_secret_type(&params.secret_type)
            };
            validate_lane_slot_secret_type(&params.name, secret_type)?;
            memcore::reject_api_key_type_for_lane_config(&params.name, secret_type)?;
            let effective_type = memcore::effective_vault_secret_type(&params.name, secret_type);
            if params.enable_rotation && effective_type != SECRET_TYPE_API_KEY {
                return Err(format!(
                    "Vault name '{}' is {effective_type}, not an API-key credential; refusing to attach rotation",
                    params.name,
                ));
            }
            let allowed_agents = normalize_allowed_agents(params.allowed_agents.clone());
            if crate::vault_ops::is_lane_slot_secret_name(&params.name) {
                if secret_type != SECRET_TYPE_API_KEY {
                    return Err(format!(
                        "Vault name '{}' is a lane slot; it must bind as {SECRET_TYPE_API_KEY} (got {secret_type})",
                        params.name
                    ));
                }
                let (decided, created) = server.with_global_store(|store| {
                    crate::vault_ops::account_bind::write_lane_slot_binding(
                        store,
                        key,
                        &params.name,
                        &value,
                        params.rebind,
                        &params.description,
                        allowed_agents.clone(),
                        effective_agent_id.as_deref(),
                    )
                })?;
                return serde_json::to_string(&json!({
                    "stored": true,
                    "name": params.name,
                    "secret_type": secret_type,
                    "created": created,
                    "bound_account": decided.account,
                    "rebind": decided.rebound,
                    "noop": decided.noop,
                    "old_fingerprint": decided.old_fingerprint,
                    "new_fingerprint": decided.new_fingerprint,
                    "fingerprint": decided.fingerprint,
                }))
                .map_err(|e| format!("serialize: {e}"));
            }
            let (encrypted_value, nonce) = crypto::encrypt(key, value.as_bytes())?;

            server.with_global_store(|store| {
                // Keep the old-value decision and the resulting write in one
                // IMMEDIATE SQLite transaction. The in-process store lease is
                // not enough: the direct CLI opens an independent connection.
                let transaction = store
                    .begin_vault_transaction()
                    .map_err(|e| format!("Failed to begin vault transaction: {e}"))?;
                let existing_entry = transaction
                    .vault_get_entry(&params.name)
                    .map_err(|e| format!("Failed to read existing entry: {e}"))?;
                if let Some(existing) = existing_entry.as_ref() {
                    ensure_agent_allowed(existing, effective_agent_id.as_deref())
                        .map_err(|e| e.to_string())?;
                }
                let is_new = existing_entry.is_none();
                if is_new && memcore::is_lane_config_url_name(&params.name) {
                    if let Some(leak) =
                        memcore::catalog::endpoint::endpoint_credential_leak(&value)
                    {
                        return Err(format!(
                            "Vault name '{}' value embeds a credential in the endpoint ({leak}); refusing write",
                            params.name
                        ));
                    }
                }

                let now = Utc::now().to_rfc3339();
                let entry = VaultEntry {
                    name: params.name.clone(),
                    encrypted_value,
                    nonce,
                    secret_type: secret_type.to_string(),
                    description: params.description.clone(),
                    allowed_agents,
                    created_at: if is_new { now.clone() } else { String::new() },
                    updated_at: now,
                    accessed_at: String::new(),
                    access_count: 0,
                };

                transaction
                    .vault_upsert_entry(&entry)
                    .map_err(|e| format!("Failed to save secret: {e}"))?;

                if let Some((prefix, _)) =
                    crate::provider_config::parse_rotation_member_name(&params.name)
                {
                    let existing_rotation = transaction
                        .vault_get_rotation(prefix)
                        .map_err(|e| format!("Failed to read rotation config: {e}"))?;
                    if params.enable_rotation || existing_rotation.is_some() {
                        let all_entries = transaction
                            .vault_list_entries()
                            .map_err(|e| format!("Failed to list entries: {e}"))?;
                        let total_keys =
                            memcore::validate_api_key_rotation_members(&all_entries, prefix)
                                .map_err(|error| format!("{error}; refusing rotation"))?
                                as i64;
                        if params.enable_rotation {
                            let strategy = normalize_rotation_strategy(
                                &params
                                    .rotation_strategy
                                    .clone()
                                    .unwrap_or_else(|| "round_robin".to_string()),
                            );
                            let rotation = VaultKeyRotation {
                                prefix: prefix.to_string(),
                                current_index: 1,
                                total_keys,
                                rotation_strategy: strategy,
                                created_at: Utc::now().to_rfc3339(),
                                updated_at: Utc::now().to_rfc3339(),
                            };
                            transaction
                                .vault_set_rotation(&rotation)
                                .map_err(|e| format!("Failed to save rotation config: {e}"))?;
                        } else if let Some(rotation) = existing_rotation.as_ref() {
                            memcore::validate_api_key_rotation(&all_entries, rotation).map_err(
                                |error| format!("{error}; refusing rotation member update"),
                            )?;
                        }
                    }
                }

                transaction
                    .commit()
                    .map_err(|e| format!("Failed to commit vault transaction: {e}"))?;

                let body = json!({
                    "stored": true,
                    "name": params.name,
                    "secret_type": secret_type,
                    "created": is_new
                });
                serde_json::to_string(&body).map_err(|e| format!("serialize: {e}"))
            })
        })
    })();

    let result = result.and_then(|body| attach_provider_refresh_warning(server, body));

    let audit_result = record_vault_audit(
        server,
        "vault_set",
        Some(&secret_name),
        result.is_ok(),
        match &result {
            Ok(_) => Some("stored"),
            Err(err) => Some(err.as_str()),
        },
    );
    result_with_vault_audit_warning(result, audit_result)
}

pub(crate) async fn handle_vault_get(
    server: &MemoryServer,
    params: VaultGetParams,
) -> Result<String, String> {
    let requested_name = params.name.clone();
    let effective_agent_id = resolve_vault_acl_agent_id(server, params.agent_id.as_deref())?;
    let result = with_vault_key(server, |key| {
        let (selected, value, new_access_count) = server.with_global_store(|store| {
            select_authorized_vault_entry_and_record_access(
                store,
                &params,
                effective_agent_id.as_deref(),
                key,
            )
        })?;

        serde_json::to_string(&json!({
            "name": selected.entry.name,
            "value": value,
            "secret_type": selected.entry.secret_type,
            "description": selected.entry.description,
            "allowed_agents": selected.entry.allowed_agents,
            "access_count": new_access_count,
        }))
        .map_err(|e| format!("serialize: {e}"))
    });

    let audit_result = record_vault_audit(
        server,
        "vault_get",
        Some(&requested_name),
        result.is_ok(),
        result.as_ref().err().map(String::as_str),
    );
    result_with_vault_audit_warning(result, audit_result)
}
