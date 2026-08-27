use super::*;

pub(crate) async fn handle_vault_set(
    server: &MemoryServer,
    params: VaultSetParams,
) -> Result<String, String> {
    let secret_name = params.name.clone();
    let result = (|| {
        crypto::validate_secret_name(&params.name)?;
        authorize_vault_mutation(server, &params.name, params.agent_id.as_deref())
            .map_err(|e| e.to_string())?;
        with_vault_key(server, |key| {
            let secret_type = normalize_secret_type(&params.secret_type);
            let allowed_agents = normalize_allowed_agents(params.allowed_agents.clone());
            let (encrypted_value, nonce) = crypto::encrypt(key, params.value.as_bytes())?;

            let is_new = !server
                .with_global_store(|store| {
                    store
                        .vault_entry_exists(&params.name)
                        .map_err(|e| e.to_string())
                })
                .map_err(|e| format!("Failed to check existing entry: {e}"))?;

            let mut rebind_meta: Option<(bool, String, String)> = None;
            if is_lane_slot_secret_name(&params.name) && secret_type == SECRET_TYPE_API_KEY {
                if is_new {
                    let entries = server
                        .with_global_store_read(|store| {
                            store.vault_list_entries().map_err(|e| e.to_string())
                        })
                        .map_err(|e| format!("Failed to list entries: {e}"))?;
                    for other in entries {
                        if other.name == params.name
                            || other.secret_type != SECRET_TYPE_API_KEY
                            || is_lane_slot_secret_name(&other.name)
                        {
                            continue;
                        }
                        let Some(kind) = provider_kind_for_env_name(&other.name) else {
                            continue;
                        };
                        let Ok(plain) = crypto::decrypt(key, &other.encrypted_value, &other.nonce)
                        else {
                            continue;
                        };
                        let Ok(other_value) = String::from_utf8(plain) else {
                            continue;
                        };
                        if fingerprint_secret(key, kind, &other_value)
                            == fingerprint_secret(key, kind, &params.value)
                        {
                            return Err(copy_existing_account_message(&params.name, &other.name));
                        }
                    }
                } else {
                    let existing = server
                        .with_global_store_read(|store| {
                            store
                                .vault_get_entry(&params.name)
                                .map_err(|e| e.to_string())
                        })
                        .map_err(|e| format!("Failed to read existing slot: {e}"))?;
                    if let Some(existing) = existing {
                        if existing.secret_type == SECRET_TYPE_API_KEY {
                            let old_bytes =
                                crypto::decrypt(key, &existing.encrypted_value, &existing.nonce)?;
                            let old_value = String::from_utf8(old_bytes).map_err(|e| {
                                format!("Existing slot '{}' is not valid UTF-8: {e}", params.name)
                            })?;
                            let provider_kind =
                                provider_kind_for_env_name(&params.name).unwrap_or("unknown");
                            match evaluate_lane_slot_overwrite(
                                &old_value,
                                &params.value,
                                provider_kind,
                                key,
                                params.rebind,
                            ) {
                                Ok(LaneSlotOverwrite::Identical { fingerprint }) => {
                                    rebind_meta = Some((false, fingerprint.clone(), fingerprint));
                                }
                                Ok(LaneSlotOverwrite::Rebound { old_fp, new_fp }) => {
                                    rebind_meta = Some((true, old_fp, new_fp));
                                }
                                Err(err) => return Err(err.operator_message(&params.name)),
                            }
                        }
                    }
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

            server
                .with_global_store(|store| {
                    store.vault_upsert_entry(&entry).map_err(|e| e.to_string())
                })
                .map_err(|e| format!("Failed to save secret: {e}"))?;

            if params.enable_rotation {
                if let Some(pos) = params.name.rfind('_') {
                    let suffix = &params.name[pos + 1..];
                    if suffix.parse::<u32>().is_ok() {
                        let prefix = &params.name[..pos];
                        let strategy = normalize_rotation_strategy(
                            &params
                                .rotation_strategy
                                .clone()
                                .unwrap_or_else(|| "round_robin".to_string()),
                        );

                        let all_entries = server
                            .with_global_store_read(|store| {
                                store.vault_list_entries().map_err(|e| e.to_string())
                            })
                            .map_err(|e| format!("Failed to list entries: {e}"))?;

                        let total_keys = collect_rotation_entries(all_entries, prefix).len() as i64;
                        let rotation = VaultKeyRotation {
                            prefix: prefix.to_string(),
                            current_index: 1,
                            total_keys,
                            rotation_strategy: strategy,
                            created_at: Utc::now().to_rfc3339(),
                            updated_at: Utc::now().to_rfc3339(),
                        };

                        server
                            .with_global_store(|store| {
                                store
                                    .vault_set_rotation(&rotation)
                                    .map_err(|e| e.to_string())
                            })
                            .map_err(|e| format!("Failed to save rotation config: {e}"))?;
                    }
                }
            }

            let mut body = json!({
                "stored": true,
                "name": params.name,
                "secret_type": secret_type,
                "created": is_new
            });
            if let Some((rebound, old_fp, new_fp)) = rebind_meta {
                body["rebind"] = json!(rebound);
                body["old_fingerprint"] = json!(old_fp);
                body["new_fingerprint"] = json!(new_fp);
            }
            serde_json::to_string(&body).map_err(|e| format!("serialize: {e}"))
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
        let selected = server.with_global_store(|store| select_vault_entry(store, &params))?;

        ensure_agent_allowed(&selected.entry, effective_agent_id.as_deref())
            .map_err(|e| e.to_string())?;

        let decrypted =
            crypto::decrypt(key, &selected.entry.encrypted_value, &selected.entry.nonce)?;
        let value = String::from_utf8(decrypted)
            .map_err(|e| format!("Decrypted value is not valid UTF-8: {e}"))?;

        let new_access_count = server
            .with_global_store(|store| {
                record_successful_vault_access(
                    store,
                    &selected.target_name,
                    selected.pending_rotation.as_ref(),
                )
            })
            .map_err(|e| format!("Failed to update access stats: {e}"))?;

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
