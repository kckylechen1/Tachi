use super::*;

pub(crate) async fn handle_vault_setup_rotation(
    server: &MemoryServer,
    params: VaultSetupRotationParams,
) -> Result<String, String> {
    let rotation_prefix = params.prefix.clone();
    let result = (|| {
        memcore::reject_api_key_type_for_lane_config(&params.prefix, memcore::SECRET_TYPE_API_KEY)?;
        authorize_vault_pool_mutation(server, &params.prefix, params.agent_id.as_deref())
            .map_err(|e| e.to_string())?;
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

pub(crate) async fn handle_vault_set_api_key_pool(
    server: &MemoryServer,
    mut params: VaultSetApiKeyPoolParams,
) -> Result<String, String> {
    let values = std::mem::take(&mut params.values)
        .into_iter()
        .map(|mut value| {
            let trimmed = value.trim().to_string();
            crypto::zero_string(&mut value);
            crypto::ZeroizingString::new(trimmed)
        })
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let logical_name = params.prefix.clone();
    let result = (|| {
        crypto::validate_secret_name(&params.prefix)?;
        memcore::reject_api_key_type_for_lane_config(&params.prefix, memcore::SECRET_TYPE_API_KEY)?;
        if !crate::utils::is_shell_env_name(&params.prefix) {
            return Err(format!(
                "API key pool prefix '{}' must be a shell env name such as OPENAI_API_KEY",
                params.prefix
            ));
        }
        authorize_vault_pool_mutation(server, &params.prefix, params.agent_id.as_deref())
            .map_err(|e| e.to_string())?;
        let effective_agent_id = resolve_vault_acl_agent_id(server, params.agent_id.as_deref())?;
        if values.is_empty() {
            return Err("API key pool requires at least one non-empty value".to_string());
        }

        let allowed_agents = normalize_allowed_agents(params.allowed_agents.clone());
        let now = Utc::now().to_rfc3339();
        let strategy = normalize_rotation_strategy(&params.strategy);
        let removed_members = with_vault_key(server, |key| {
            let mut entries = Vec::with_capacity(values.len());
            for (idx, value) in values.iter().enumerate() {
                let name = format!("{}_{}", params.prefix, idx + 1);
                let (encrypted_value, nonce) = crypto::encrypt(key, value.as_bytes())?;
                entries.push(VaultEntry {
                    name,
                    encrypted_value,
                    nonce,
                    secret_type: SECRET_TYPE_API_KEY.to_string(),
                    description: params.description.clone(),
                    allowed_agents: allowed_agents.clone(),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                    accessed_at: String::new(),
                    access_count: 0,
                });
            }
            let rotation = VaultKeyRotation {
                prefix: params.prefix.clone(),
                current_index: 1,
                total_keys: values.len() as i64,
                rotation_strategy: strategy.clone(),
                created_at: now.clone(),
                updated_at: now.clone(),
            };
            server
                .with_global_store(|store| {
                    let transaction = store
                        .begin_vault_transaction()
                        .map_err(|e| format!("begin pool replacement transaction: {e}"))?;
                    let current_entries = transaction
                        .vault_list_entries()
                        .map_err(|e| format!("vault_list_entries: {e}"))?;
                    for entry in &current_entries {
                        if memcore::api_key_pool_member_index(&entry.name, &params.prefix).is_some()
                        {
                            ensure_agent_allowed(entry, effective_agent_id.as_deref())
                                .map_err(|e| e.to_string())?;
                        }
                    }
                    let removed = transaction
                        .vault_replace_api_key_pool(&params.prefix, &entries, &rotation)
                        .map_err(|e| format!("vault_replace_api_key_pool: {e}"))?;
                    transaction
                        .commit()
                        .map_err(|e| format!("commit pool replacement transaction: {e}"))?;
                    Ok::<_, String>(removed)
                })
                .map_err(|e| format!("save API key pool: {e}"))
        })?;

        serde_json::to_string(&json!({
            "stored": true,
            "logical_name": params.prefix,
            "total_keys": values.len(),
            "strategy": strategy,
            "members": (1..=values.len()).map(|idx| format!("{}_{}", logical_name, idx)).collect::<Vec<_>>(),
            "removed_members": removed_members,
        }))
        .map_err(|e| format!("serialize: {e}"))
    })();

    let result = result.and_then(|body| attach_provider_refresh_warning(server, body));
    let audit_result = record_vault_audit(
        server,
        "vault_set_api_key_pool",
        Some(&logical_name),
        result.is_ok(),
        match &result {
            Ok(_) => Some("stored"),
            Err(err) => Some(err.as_str()),
        },
    );
    result_with_vault_audit_warning(result, audit_result)
}

fn advance_rotation_after_key(
    server: &MemoryServer,
    logical_name: &str,
    key_id: &str,
) -> Result<(), String> {
    let Some((prefix, idx)) = crate::provider_config::parse_rotation_member_name(key_id) else {
        return Ok(());
    };
    if prefix != logical_name {
        return Ok(());
    }

    server
        .with_global_store(|store| {
            let transaction = store
                .begin_vault_transaction()
                .map_err(|e| format!("begin rotation advance transaction: {e}"))?;
            let Some(rotation) = transaction
                .vault_get_rotation(logical_name)
                .map_err(|e| e.to_string())?
            else {
                return Ok(());
            };
            if rotation.total_keys <= 0 {
                return Ok(());
            }
            let entries = transaction
                .vault_list_entries()
                .map_err(|e| e.to_string())?;
            memcore::validate_api_key_rotation(&entries, &rotation)
                .map_err(|error| format!("{error}; refusing rotation advance"))?;
            let next = (idx as i64 % rotation.total_keys) + 1;
            let updated = VaultKeyRotation {
                current_index: next,
                updated_at: Utc::now().to_rfc3339(),
                ..rotation
            };
            transaction
                .vault_set_rotation(&updated)
                .map_err(|e| e.to_string())?;
            transaction.commit().map_err(|e| e.to_string())?;
            Ok(())
        })
        .map_err(|e| format!("advance rotation: {e}"))
}

pub(crate) async fn handle_vault_lease_api_key(
    server: &MemoryServer,
    params: VaultLeaseApiKeyParams,
) -> Result<String, String> {
    let requested_name = params.name.clone();
    let mut success_audit_detail = None;
    let result = (|| {
        let logical_name = server.with_global_store_read(|store| {
            crate::vault_ops::canonical_api_key_health_logical_name(store, &params.name)
        })?;
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

        let pool = load_unlocked_api_key_secret_pool(server, &params.name)?;
        let selected = pool.first().ok_or_else(|| {
            format!(
                "No usable API key available for '{}'. Vault may be locked, missing, or all keys are disabled/auth-failed/rate-limited.",
                params.name
            )
        })?;

        advance_rotation_after_key(server, &logical_name, &selected.key_id)?;
        let selected_key_id = selected.key_id.clone();
        let access_count = server
            .with_global_store_read(|store| {
                store
                    .vault_get_entry(&selected_key_id)
                    .map_err(|e| e.to_string())?
                    .map(|entry| entry.access_count)
                    .ok_or_else(|| {
                        format!("leased key '{selected_key_id}' disappeared after materialization")
                    })
            })
            .map_err(|e| format!("read materialized key access count: {e}"))?;

        success_audit_detail = Some(
            json!({
                "logical_name": logical_name.clone(),
                "key_id": selected.key_id.clone(),
                "env_name": env_name.clone(),
                "agent_id": params.agent_id.clone(),
            })
            .to_string(),
        );

        serde_json::to_string(&json!({
            "leased": true,
            "logical_name": logical_name,
            "key_id": selected.key_id,
            "env_name": env_name,
            "agent_id": params.agent_id,
            "env": {
                env_name: selected.value
            },
            "access_count": access_count,
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
