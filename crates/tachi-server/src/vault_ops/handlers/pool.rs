use super::*;

pub(crate) async fn handle_vault_setup_rotation(
    server: &impl VaultCtx,
    params: VaultSetupRotationParams,
) -> Result<String, String> {
    let rotation_prefix = params.prefix.clone();
    let result = (|| {
        memcore::reject_api_key_type_for_lane_config(&params.prefix, memcore::SECRET_TYPE_API_KEY)?;
        crate::vault_ops::account_bind::refuse_lane_slot_pool_prefix(&params.prefix)?;
        authorize_vault_pool_mutation(server, &params.prefix, params.agent_id.as_deref())
            .map_err(|e| e.to_string())?;
        let effective_agent_id = resolve_vault_acl_agent_id(server, params.agent_id.as_deref())?;
        if params.total_keys < 2 {
            return Err("Rotation requires at least 2 keys".into());
        }

        let strategy = normalize_rotation_strategy(&params.strategy);
        let now = Utc::now().to_rfc3339();
        let rotation = VaultKeyRotation {
            prefix: params.prefix.clone(),
            current_index: 1,
            total_keys: params.total_keys,
            rotation_strategy: strategy.clone(),
            created_at: now.clone(),
            updated_at: now,
        };

        server.with_global_store(|store| {
            let transaction = store
                .begin_vault_transaction()
                .map_err(|e| format!("Failed to begin rotation transaction: {e}"))?;
            let all_entries = transaction
                .vault_list_entries()
                .map_err(|e| format!("Failed to list entries: {e}"))?;
            for entry in &all_entries {
                if memcore::api_key_pool_member_index(&entry.name, &params.prefix).is_some() {
                    ensure_agent_allowed(entry, effective_agent_id.as_deref())
                        .map_err(|e| e.to_string())?;
                }
            }
            let member_count =
                memcore::validate_api_key_rotation_members(&all_entries, &params.prefix)
                    .map_err(|error| format!("{error}; refusing rotation setup"))?;
            if member_count != params.total_keys as usize {
                return Err(format!(
                    "Expected exactly {} contiguous API keys for prefix '{}', found {}. Please reconcile all numeric members first.",
                    params.total_keys, params.prefix, member_count
                ));
            }
            transaction
                .vault_set_rotation(&rotation)
                .map_err(|e| format!("Failed to save rotation config: {e}"))?;
            transaction
                .commit()
                .map_err(|e| format!("Failed to commit rotation config: {e}"))
        })?;

        let resp = json!({
            "setup": true,
            "prefix": params.prefix,
            "total_keys": params.total_keys,
            "strategy": strategy,
        });
        let body = serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))?;
        attach_provider_refresh_warning(server, body)
    })();

    let audit_result = record_vault_audit(
        server,
        "vault_setup_rotation",
        Some(&rotation_prefix),
        result.is_ok(),
        match &result {
            Ok(_) => Some("setup"),
            Err(err) => Some(err.as_str()),
        },
    );
    result_with_vault_audit_warning(result, audit_result)
}

pub(crate) async fn handle_vault_lease_api_key(
    server: &impl VaultCtx,
    params: VaultLeaseApiKeyParams,
) -> Result<String, String> {
    let requested_name = params.name.clone();
    let mut success_audit_detail = None;
    let result = (|| {
        let env_name = params
            .env_name
            .clone()
            .unwrap_or_else(|| params.name.clone());
        if !crate::utils::is_shell_env_name(&env_name) {
            return Err(format!(
                "env_name '{}' must be a valid shell env name",
                env_name
            ));
        }
        if memcore::is_lane_config_secret_name(&params.name) {
            return Err(format!(
                "Vault name '{}' is lane config, not a credential; refusing to lease it as an API key",
                params.name
            ));
        }
        let stored = server
            .with_global_store_read(|store| {
                store
                    .vault_get_entry(&params.name)
                    .map_err(|e| e.to_string())
            })
            .map_err(|e| format!("Failed to read vault entry: {e}"))?;
        if let Some(entry) = stored {
            let effective = memcore::effective_vault_secret_type(&entry.name, &entry.secret_type);
            if effective != memcore::SECRET_TYPE_API_KEY {
                return Err(format!(
                    "Vault name '{}' is {effective}, not a credential; refusing to lease it as an API key",
                    params.name
                ));
            }
        }

        let effective_agent_id = resolve_vault_acl_agent_id(server, params.agent_id.as_deref())?;
        let selected =
            lease_authorized_api_key(server, &params.name, effective_agent_id.as_deref())?;

        success_audit_detail = Some(
            json!({
                "logical_name": selected.logical_name.clone(),
                "key_id": selected.key_id.clone(),
                "env_name": env_name.clone(),
                "agent_id": params.agent_id.clone(),
            })
            .to_string(),
        );

        serde_json::to_string(&json!({
            "leased": true,
            "logical_name": selected.logical_name,
            "key_id": selected.key_id,
            "env_name": env_name,
            "agent_id": params.agent_id,
            "env": {
                env_name: selected.value
            },
            "access_count": selected.access_count,
        }))
        .map_err(|e| format!("serialize: {e}"))
    })();

    let audit_detail = match result.as_ref() {
        Ok(_) => success_audit_detail.as_deref(),
        Err(err) => Some(err.as_str()),
    };
    let audit_result = record_vault_audit(
        server,
        "vault_lease_api_key",
        Some(&requested_name),
        result.is_ok(),
        audit_detail,
    );
    result_with_vault_audit_warning(result, audit_result)
}
