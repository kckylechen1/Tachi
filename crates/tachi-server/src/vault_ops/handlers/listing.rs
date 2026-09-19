use super::*;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use chrono::Utc;
use memcore::vault::accounts::{AccountCustody, CustodyKind, ProviderAccount};
use memcore::vault::health::{
    EvidenceKind, EVIDENCE_OUTCOME_FIELD, HEALTH_STATUS_AUTH_FAILED, HEALTH_STATUS_EXHAUSTED,
    HEALTH_STATUS_OK,
};
use memcore::vault::{VaultEntry, VaultKeyHealth};

struct AccountBinding {
    account: ProviderAccount,
    custody: AccountCustody,
    aliases: Vec<memcore::vault::accounts::ProviderAccountAlias>,
}

fn effective_vault_alias_bindings(resolved_home: &Path) -> HashMap<String, BTreeSet<String>> {
    let mut configured = crate::provider_config::collect_config_env_values(Some(resolved_home));
    configured.extend(std::env::vars());

    let mut by_target: HashMap<String, BTreeSet<String>> = HashMap::new();
    for (slot, value) in configured {
        if !crate::vault_ops::is_lane_slot_secret_name(&slot) {
            continue;
        }
        if let Some(target) = tachi_llm::parse_vault_alias(&value) {
            by_target
                .entry(target.to_string())
                .or_default()
                .insert(slot);
        }
    }
    by_target
}

fn account_bindings(store: &memcore::MemoryStore) -> Result<Vec<AccountBinding>, String> {
    let mut bindings = Vec::new();
    let accounts = memcore::db::list_provider_accounts(store.connection())
        .map_err(|error| format!("Failed to list provider accounts: {error}"))?;
    for account in accounts {
        if account.status != memcore::vault::accounts::ACCOUNT_STATUS_ACTIVE {
            continue;
        }
        let Some(custody) = memcore::db::get_account_custody(
            store.connection(),
            &account.account_id,
        )
        .map_err(|error| format!("Failed to read provider account custody: {error}"))?
        else {
            continue;
        };
        let aliases = memcore::db::list_provider_account_aliases(
            store.connection(),
            &account.account_id,
        )
        .map_err(|error| format!("Failed to list provider account aliases: {error}"))?;
        bindings.push(AccountBinding {
            account,
            custody,
            aliases,
        });
    }
    Ok(bindings)
}

fn custody_contains_entry(custody: &AccountCustody, entry_name: &str) -> bool {
    match custody.custody_kind {
        CustodyKind::VaultEntry => custody.custody_target == entry_name,
        CustodyKind::VaultRotationPool => {
            memcore::vault::api_key_pool_member_index(entry_name, &custody.custody_target).is_some()
        }
    }
}

fn latest_health<'a>(
    entry_name: &str,
    health_rows: &'a [VaultKeyHealth],
) -> Option<&'a VaultKeyHealth> {
    health_rows
        .iter()
        .filter(|health| health.key_id == entry_name)
        .max_by(|left, right| {
            left.last_attempt
                .as_deref()
                .unwrap_or(left.updated_at.as_str())
                .cmp(
                    right
                        .last_attempt
                        .as_deref()
                        .unwrap_or(right.updated_at.as_str()),
                )
        })
}

fn health_evidence_outcome(health: &VaultKeyHealth) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(&health.metadata)
        .ok()?
        .get(EVIDENCE_OUTCOME_FIELD)?
        .as_str()
        .map(str::to_string)
}

fn last_probe_class(health: Option<&VaultKeyHealth>) -> &'static str {
    let Some(health) = health else {
        return "unknown";
    };
    if health.disabled {
        return "disabled";
    }
    if health
        .last_error
        .as_deref()
        .is_some_and(|error| error.to_ascii_lowercase().contains("empty assistant content"))
    {
        return "empty_content";
    }

    let evidence = EvidenceKind::from_metadata(&health.metadata);
    let outcome = health_evidence_outcome(health);
    match (evidence, outcome.as_deref()) {
        (Some(EvidenceKind::Probed), Some("success")) => "ok",
        (Some(EvidenceKind::Probed), Some("auth_failed")) => "401",
        (Some(EvidenceKind::Probed), Some("exhausted")) => "402",
        (Some(EvidenceKind::SelfReported), Some("success")) => "ok",
        (Some(EvidenceKind::SelfReported), Some("auth_failed")) => "auth_failed",
        _ if health.auth_failed || health.status == HEALTH_STATUS_AUTH_FAILED => "auth_failed",
        _ if health.status == HEALTH_STATUS_EXHAUSTED => "402",
        _ if health.status == HEALTH_STATUS_OK
            && (health.last_attempt.is_some() || health.last_success.is_some()) =>
        {
            "ok"
        }
        _ => "unknown",
    }
}

fn account_secret_type(secret_type: &str) -> bool {
    matches!(
        secret_type,
        memcore::vault::SECRET_TYPE_API_KEY
            | memcore::vault::SECRET_TYPE_OAUTH_TOKEN
            | memcore::vault::SECRET_TYPE_JSON_BLOB
            | memcore::vault::SECRET_TYPE_COOKIE
    )
}

fn is_model_provider_account(name: &str) -> bool {
    let account_name = tachi_llm::parse_rotation_member_name(name)
        .map(|(prefix, _)| prefix)
        .unwrap_or(name);
    crate::status_ops::status_health::account_class_for_env_name(account_name)
        == Some(memcore::AccountClass::ModelApi)
}

fn alias_integrity(
    entry: &VaultEntry,
    secret_type: &str,
    health: Option<&VaultKeyHealth>,
    bound_slots: &BTreeSet<String>,
    runtime_available: bool,
    runtime_binding: &impl Fn(&str, &str) -> bool,
) -> &'static str {
    if bound_slots.is_empty() {
        return "unknown";
    }
    if secret_type != memcore::vault::SECRET_TYPE_API_KEY {
        return "wrong_type";
    }
    if entry
        .allowed_agents
        .as_ref()
        .is_some_and(|agents| !agents.is_empty())
    {
        return "fenced";
    }
    if !is_model_provider_account(&entry.name) {
        return "unusable";
    }
    if health.is_some_and(|health| {
        crate::vault_ops::unusable_skip_class(health, Utc::now()).is_some()
    }) {
        return "unusable";
    }
    if bound_slots
        .iter()
        .any(|slot| runtime_binding(slot, &entry.name))
    {
        return "resolved";
    }
    if runtime_available {
        "empty"
    } else {
        "unknown"
    }
}

pub(crate) fn build_vault_list_payload(
    store: &memcore::MemoryStore,
    resolved_home: &Path,
    requested_secret_type: Option<&str>,
    runtime_available: bool,
    runtime_binding: impl Fn(&str, &str) -> bool,
) -> Result<serde_json::Value, String> {
    let mut entries = store
        .vault_list_entries()
        .map_err(|error| format!("Failed to list secrets: {error}"))?;
    if let Some(secret_type) = requested_secret_type {
        let want = normalize_secret_type(secret_type);
        entries.retain(|entry| {
            memcore::effective_vault_secret_type(&entry.name, &entry.secret_type) == want
        });
    }

    let health_rows = store
        .vault_list_key_health(None)
        .map_err(|error| format!("Failed to list Vault key health: {error}"))?;
    let account_bindings = account_bindings(store)?;
    let env_bindings = effective_vault_alias_bindings(resolved_home);
    let mut payload = Vec::with_capacity(entries.len());

    for entry in entries {
        let secret_type = memcore::effective_vault_secret_type(&entry.name, &entry.secret_type);
        let group = if secret_type == memcore::SECRET_TYPE_CONFIG {
            "config"
        } else {
            "credential"
        };
        let mut row = json!({
            "name": entry.name.clone(),
            "secret_type": secret_type,
            "group": group,
            "description": entry.description.clone(),
            "allowed_agents": entry.allowed_agents.clone(),
            "created_at": entry.created_at.clone(),
            "updated_at": entry.updated_at.clone(),
            "access_count": entry.access_count,
        });

        if account_secret_type(secret_type) {
            let health = latest_health(&entry.name, &health_rows);
            let mut bound_slots = env_bindings.get(&entry.name).cloned().unwrap_or_default();
            let matching_accounts = account_bindings
                .iter()
                .filter(|binding| custody_contains_entry(&binding.custody, &entry.name))
                .collect::<Vec<_>>();
            for binding in &matching_accounts {
                for alias in &binding.aliases {
                    if !alias.retired
                        && crate::vault_ops::is_lane_slot_secret_name(&alias.alias_name)
                    {
                        bound_slots.insert(alias.alias_name.clone());
                    }
                }
            }

            let object = row.as_object_mut().expect("vault list rows are objects");
            object.insert(
                "bound_slots".to_string(),
                json!(bound_slots.iter().collect::<Vec<_>>()),
            );
            object.insert(
                "last_probe_class".to_string(),
                json!(last_probe_class(health)),
            );
            object.insert(
                "last_probe_at".to_string(),
                health
                    .and_then(|row| {
                        row.last_attempt
                            .as_deref()
                            .or(row.last_success.as_deref())
                    })
                    .map_or(serde_json::Value::Null, |value| json!(value)),
            );
            object.insert(
                "alias_integrity".to_string(),
                json!(alias_integrity(
                    &entry,
                    secret_type,
                    health,
                    &bound_slots,
                    runtime_available,
                    &runtime_binding,
                )),
            );
            if let [binding] = matching_accounts.as_slice() {
                object.insert(
                    "provider_kind".to_string(),
                    json!(&binding.account.provider_kind),
                );
                object.insert(
                    "account_id".to_string(),
                    json!(&binding.account.account_id),
                );
            }
        }
        payload.push(row);
    }

    let config = payload
        .iter()
        .filter(|row| row.get("group").and_then(|value| value.as_str()) == Some("config"))
        .cloned()
        .collect::<Vec<_>>();
    let credentials = payload
        .iter()
        .filter(|row| row.get("group").and_then(|value| value.as_str()) != Some("config"))
        .cloned()
        .collect::<Vec<_>>();

    Ok(json!({
        "count": payload.len(),
        "credentials": credentials,
        "config": config,
        "secrets": payload,
    }))
}

pub(crate) async fn handle_vault_list(
    server: &MemoryServer,
    params: VaultListParams,
) -> Result<String, String> {
    if !is_vault_initialized(server)? {
        return Err("Vault not initialized. Call vault_init first.".into());
    }

    let runtime_available = server.vault_read().key.is_some();
    let resp = server.with_global_store_read(|store| {
        build_vault_list_payload(
            store,
            &server.tachi_home_dir(),
            params.secret_type.as_deref(),
            runtime_available,
            |logical_name, key_id| {
                server
                    .llm
                    .has_provider_secret_binding(logical_name, key_id)
            },
        )
    })?;
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
