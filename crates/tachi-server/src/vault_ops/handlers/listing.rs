use super::*;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use chrono::Utc;
use memcore::vault::accounts::{AccountCustody, CustodyKind, ProviderAccount};
use memcore::vault::health::{
    EvidenceKind, EVIDENCE_AT_FIELD, EVIDENCE_OUTCOME_FIELD, HEALTH_STATUS_AUTH_FAILED,
    HEALTH_STATUS_EXHAUSTED, HEALTH_STATUS_OK,
};
use memcore::vault::{VaultEntry, VaultKeyHealth};

struct AccountBinding {
    account: ProviderAccount,
    custody: AccountCustody,
    aliases: Vec<memcore::vault::accounts::ProviderAccountAlias>,
}

#[derive(Clone, Default)]
struct EnvAliasClaims {
    source_names: BTreeSet<String>,
    targets: BTreeSet<String>,
    has_non_alias: bool,
}

fn normalized_alias_source(
    values: impl IntoIterator<Item = (String, String)>,
) -> HashMap<String, EnvAliasClaims> {
    let mut by_slot: HashMap<String, EnvAliasClaims> = HashMap::new();
    for (raw_slot, value) in values {
        let slot = raw_slot.trim();
        if !crate::vault_ops::is_lane_slot_secret_name(slot) {
            continue;
        }
        let claims = by_slot.entry(slot.to_string()).or_default();
        claims.source_names.insert(raw_slot);
        if let Some(target) = tachi_llm::parse_vault_alias(&value) {
            claims.targets.insert(target.to_string());
        } else {
            claims.has_non_alias = true;
        }
    }
    by_slot
}

fn reconcile_alias_sources(
    mut configured: HashMap<String, EnvAliasClaims>,
    process: HashMap<String, EnvAliasClaims>,
) -> HashMap<String, EnvAliasClaims> {
    // Runtime process precedence chooses the effective value, but listing must
    // retain contradictory config/process evidence and fail closed rather than
    // hide it behind precedence or HashMap iteration order.
    for (slot, claims) in process {
        let combined = configured.entry(slot).or_default();
        combined.source_names.extend(claims.source_names);
        combined.targets.extend(claims.targets);
        combined.has_non_alias |= claims.has_non_alias;
    }
    configured
}

fn effective_vault_alias_bindings(resolved_home: &Path) -> HashMap<String, EnvAliasClaims> {
    reconcile_alias_sources(
        normalized_alias_source(crate::provider_config::collect_config_env_claims(Some(
            resolved_home,
        ))),
        normalized_alias_source(std::env::vars()),
    )
}

fn account_bindings(
    transaction: &memcore::store::vault::VaultTransaction<'_>,
) -> Result<Vec<AccountBinding>, String> {
    let mut bindings = Vec::new();
    let accounts = transaction
        .list_provider_accounts()
        .map_err(|error| format!("Failed to list provider accounts: {error}"))?;
    for account in accounts {
        if account.status != memcore::vault::accounts::ACCOUNT_STATUS_ACTIVE {
            continue;
        }
        let Some(custody) = transaction
            .get_account_custody(&account.account_id)
            .map_err(|error| format!("Failed to read provider account custody: {error}"))?
        else {
            continue;
        };
        let aliases = transaction
            .list_provider_account_aliases(&account.account_id)
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

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct AccountAliasClaim {
    account_id: String,
    custody_target: String,
}

fn account_alias_targets(
    bindings: &[AccountBinding],
) -> HashMap<String, BTreeSet<AccountAliasClaim>> {
    let mut by_slot: HashMap<String, BTreeSet<AccountAliasClaim>> = HashMap::new();
    for binding in bindings {
        for alias in &binding.aliases {
            let slot = alias.alias_name.trim();
            if !alias.retired && crate::vault_ops::is_lane_slot_secret_name(slot) {
                by_slot
                    .entry(slot.to_string())
                    .or_default()
                    .insert(AccountAliasClaim {
                        account_id: binding.account.account_id.clone(),
                        custody_target: binding.custody.custody_target.clone(),
                    });
            }
        }
    }
    by_slot
}

fn account_custody_owners(
    bindings: &[AccountBinding],
) -> HashMap<String, BTreeSet<String>> {
    let mut owners: HashMap<String, BTreeSet<String>> = HashMap::new();
    for binding in bindings {
        owners
            .entry(binding.custody.custody_target.clone())
            .or_default()
            .insert(binding.account.account_id.clone());
    }
    owners
}

#[derive(Default)]
struct BindingClaims {
    slots: BTreeSet<String>,
    conflicts: BTreeSet<String>,
}

fn binding_claims(
    target: &str,
    env_targets: &HashMap<String, EnvAliasClaims>,
    account_targets: &HashMap<String, BTreeSet<AccountAliasClaim>>,
) -> BindingClaims {
    let mut claims = BindingClaims::default();
    let slots = env_targets
        .keys()
        .chain(account_targets.keys())
        .collect::<BTreeSet<_>>();
    for slot in slots {
        let env = env_targets.get(slot);
        let accounts = account_targets.get(slot);
        let mut targets = env
            .into_iter()
            .flat_map(|claims| claims.targets.iter().cloned())
            .collect::<BTreeSet<_>>();
        if let Some(accounts) = accounts {
            targets.extend(accounts.iter().map(|claim| claim.custody_target.clone()));
        }
        if targets.contains(target) {
            claims.slots.insert(slot.clone());
            let normalized_env_collision =
                env.is_some_and(|claims| claims.source_names.len() > 1);
            let direct_value_conflict = env.is_some_and(|claims| claims.has_non_alias);
            let ambiguous_account_owner = accounts.is_some_and(|claims| claims.len() > 1);
            if targets.len() > 1
                || normalized_env_collision
                || direct_value_conflict
                || ambiguous_account_owner
            {
                claims.conflicts.insert(slot.clone());
            }
        }
    }
    claims
}

fn configured_logical_name(
    entry_name: &str,
    matching_accounts: &[&AccountBinding],
    rotation_prefixes: &BTreeSet<String>,
) -> String {
    let mut custody_targets = matching_accounts
        .iter()
        .map(|binding| binding.custody.custody_target.as_str());
    if let Some(target) = custody_targets.next() {
        if custody_targets.all(|candidate| candidate == target) {
            return target.to_string();
        }
    }
    if let Some((prefix, _)) = tachi_llm::parse_rotation_member_name(entry_name) {
        if rotation_prefixes.contains(prefix) {
            return prefix.to_string();
        }
    }
    entry_name.to_string()
}

fn relevant_health<'a>(
    health_by_logical: &'a HashMap<String, HashMap<String, VaultKeyHealth>>,
    logical_name: &str,
    key_id: &str,
    bound_slots: &BTreeSet<String>,
) -> Vec<&'a VaultKeyHealth> {
    std::iter::once(logical_name)
        .chain(bound_slots.iter().map(String::as_str))
        .filter_map(|logical| health_by_logical.get(logical)?.get(key_id))
        .collect()
}

fn health_updated_at(health: &VaultKeyHealth) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(&health.updated_at).ok()
}

fn health_is_attributed(
    health: &VaultKeyHealth,
    runtime_generation_current: bool,
    source_health_generation: &HashMap<(String, String), u64>,
) -> bool {
    runtime_generation_current
        && source_health_generation
            .get(&(health.logical_name.clone(), health.key_id.clone()))
            .is_some_and(|expected| {
                *expected
                    == crate::vault_ops::access::vault_key_health_revision(health)
            })
}

fn selected_health<'a>(health: &[&'a VaultKeyHealth]) -> Option<&'a VaultKeyHealth> {
    let now = Utc::now();
    let unusable = health
        .iter()
        .copied()
        .filter(|row| crate::vault_ops::unusable_skip_class(row, now).is_some())
        .max_by(|left, right| {
            health_updated_at(left)
                .cmp(&health_updated_at(right))
                .then_with(|| left.updated_at.cmp(&right.updated_at))
        });
    unusable.or_else(|| {
        health.iter().copied().max_by(|left, right| {
            health_updated_at(left)
                .cmp(&health_updated_at(right))
                .then_with(|| left.updated_at.cmp(&right.updated_at))
        })
    })
}

fn probe_observation(health: Option<&VaultKeyHealth>) -> (&'static str, Option<String>) {
    let Some(health) = health else {
        return ("unknown", None);
    };
    let metadata = serde_json::from_str::<serde_json::Value>(&health.metadata).ok();
    let evidence = EvidenceKind::from_metadata(&health.metadata);
    let outcome = metadata
        .as_ref()
        .and_then(|value| value.get(EVIDENCE_OUTCOME_FIELD))
        .and_then(serde_json::Value::as_str);
    let evidence_at = metadata
        .as_ref()
        .and_then(|value| value.get(EVIDENCE_AT_FIELD))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let empty_content = health.last_error.as_deref().is_some_and(|error| {
        error
            .to_ascii_lowercase()
            .contains("empty assistant content")
    });
    if let Some(class) = match (evidence, outcome) {
        (Some(EvidenceKind::Probed), Some("success")) => Some("ok"),
        (Some(EvidenceKind::Probed), Some("auth_failed")) => Some("auth_failed"),
        (Some(_), Some("exhausted")) => Some("402"),
        (Some(_), Some("rate_limited")) => Some("unknown"),
        (Some(_), Some("error")) if empty_content => Some("empty_content"),
        (Some(_), Some("error")) => Some("unknown"),
        (Some(_), Some("unknown")) => Some("unknown"),
        (Some(EvidenceKind::SelfReported), Some("success")) => Some("ok"),
        (Some(EvidenceKind::SelfReported), Some("auth_failed")) => Some("auth_failed"),
        _ => None,
    } {
        return (class, evidence_at);
    }

    let class = if health.disabled {
        "disabled"
    } else if empty_content {
        "empty_content"
    } else if health.auth_failed || health.status == HEALTH_STATUS_AUTH_FAILED {
        "auth_failed"
    } else if health.status == HEALTH_STATUS_EXHAUSTED {
        "402"
    } else if health.status == HEALTH_STATUS_OK
        && (health.last_attempt.is_some() || health.last_success.is_some())
    {
        "ok"
    } else {
        "unknown"
    };
    (
        class,
        health
            .last_attempt
            .as_deref()
            .or(health.last_success.as_deref())
            .map(str::to_string),
    )
}

fn health_is_unusable(health: &[&VaultKeyHealth]) -> bool {
    let now = Utc::now();
    health
        .iter()
        .any(|row| crate::vault_ops::unusable_skip_class(row, now).is_some())
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
    crate::provider_config::is_model_provider_pool_name(name)
}

fn alias_integrity(
    entry: &VaultEntry,
    logical_name: &str,
    secret_type: &str,
    health: &[&VaultKeyHealth],
    claims: &BindingClaims,
    ambiguous_custody_owners: bool,
    runtime_generation_current: bool,
    runtime_bindings: Option<&HashMap<String, BTreeSet<String>>>,
) -> &'static str {
    if claims.slots.is_empty() {
        return "unknown";
    }
    if !runtime_generation_current {
        return "unknown";
    }
    let Some(runtime_bindings) = runtime_bindings else {
        return "unknown";
    };
    if ambiguous_custody_owners || !claims.conflicts.is_empty() {
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
    if !is_model_provider_account(logical_name) || health_is_unusable(health) {
        return "unusable";
    }

    let exact = claims
        .slots
        .iter()
        .filter(|slot| {
            runtime_bindings
                .get(*slot)
                .is_some_and(|members| members.contains(&entry.name))
        })
        .count();
    if exact == claims.slots.len() {
        "resolved"
    } else if exact > 0
        || claims
            .slots
            .iter()
            .any(|slot| runtime_bindings.contains_key(slot))
    {
        "absent"
    } else {
        "unknown"
    }
}

pub(crate) fn build_vault_list_payload(
    store: &memcore::MemoryStore,
    resolved_home: &Path,
    requested_secret_type: Option<&str>,
    runtime_health: HashMap<String, HashMap<String, VaultKeyHealth>>,
    runtime_bindings: Option<&HashMap<String, BTreeSet<String>>>,
    runtime_source_generation: Option<u64>,
    source_health_generation: &HashMap<(String, String), u64>,
) -> Result<serde_json::Value, String> {
    let transaction = store
        .begin_vault_read_transaction_shared()
        .map_err(|error| format!("Failed to begin Vault list snapshot: {error}"))?;
    let entries = transaction
        .vault_list_entries()
        .map_err(|error| format!("Failed to list secrets: {error}"))?;
    let health_rows = transaction
        .vault_list_key_health(None)
        .map_err(|error| format!("Failed to list Vault key health: {error}"))?;
    let rotations = transaction
        .vault_list_rotations()
        .map_err(|error| format!("Failed to list Vault rotations: {error}"))?;
    let account_bindings = account_bindings(&transaction)?;
    transaction
        .commit()
        .map_err(|error| format!("Failed to finish Vault list snapshot: {error}"))?;
    let current_generation = crate::vault_ops::vault_materialization_acl_revision_from_rows(
        &entries,
        &rotations,
        &health_rows,
    )
    .contents;
    let runtime_generation_current = runtime_source_generation == Some(current_generation);

    let mut entries = entries;
    if let Some(secret_type) = requested_secret_type {
        let want = normalize_secret_type(secret_type);
        entries.retain(|entry| {
            memcore::effective_vault_secret_type(&entry.name, &entry.secret_type) == want
        });
    }

    let health_by_logical = crate::vault_ops::access::health_snapshot::merge_provider_key_health(
        health_rows,
        runtime_health,
    );
    let rotation_prefixes = rotations
        .into_iter()
        .map(|rotation| rotation.prefix)
        .collect::<BTreeSet<_>>();
    let env_targets = effective_vault_alias_bindings(resolved_home);
    let account_targets = account_alias_targets(&account_bindings);
    let custody_owners = account_custody_owners(&account_bindings);
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
            let matching_accounts = account_bindings
                .iter()
                .filter(|binding| custody_contains_entry(&binding.custody, &entry.name))
                .collect::<Vec<_>>();
            let logical_name =
                configured_logical_name(&entry.name, &matching_accounts, &rotation_prefixes);
            let claims = binding_claims(&logical_name, &env_targets, &account_targets);
            let ambiguous_custody_owners = custody_owners
                .get(&logical_name)
                .is_some_and(|owners| owners.len() > 1);
            let health = relevant_health(
                &health_by_logical,
                &logical_name,
                &entry.name,
                &claims.slots,
            )
            .into_iter()
            .filter(|row| {
                health_is_attributed(
                    row,
                    runtime_generation_current,
                    source_health_generation,
                )
            })
            .collect::<Vec<_>>();
            let (probe_class, probe_at) = probe_observation(selected_health(&health));

            let object = row.as_object_mut().expect("vault list rows are objects");
            object.insert(
                "bound_slots".to_string(),
                json!(claims.slots.iter().collect::<Vec<_>>()),
            );
            object.insert("last_probe_class".to_string(), json!(probe_class));
            object.insert(
                "last_probe_at".to_string(),
                probe_at.map_or(serde_json::Value::Null, |value| json!(value)),
            );
            object.insert(
                "alias_integrity".to_string(),
                json!(alias_integrity(
                    &entry,
                    &logical_name,
                    secret_type,
                    &health,
                    &claims,
                    ambiguous_custody_owners,
                    runtime_generation_current,
                    runtime_bindings,
                )),
            );
            if let [binding] = matching_accounts.as_slice() {
                object.insert(
                    "provider_kind".to_string(),
                    json!(&binding.account.provider_kind),
                );
                object.insert("account_id".to_string(), json!(&binding.account.account_id));
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
    let (runtime_health, runtime_bindings, runtime_source_generation, source_health_generation) =
        if runtime_available {
            let (health, bindings, generation, health_generation) =
                server.llm.provider_health_board_snapshot();
            (health, Some(bindings), generation, health_generation)
        } else {
            (Default::default(), None, None, Default::default())
        };
    let resolved_home = server.tachi_home_dir();
    let resp = server.with_global_store_read(move |store| {
        build_vault_list_payload(
            store,
            &resolved_home,
            params.secret_type.as_deref(),
            runtime_health,
            runtime_bindings.as_ref(),
            runtime_source_generation,
            &source_health_generation,
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use memcore::vault::health::{new_key_health, record_key_outcome, TypedOutcome};

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + seconds, 0).expect("fixed timestamp")
    }

    fn entry(name: &str) -> VaultEntry {
        VaultEntry {
            name: name.to_string(),
            encrypted_value: String::new(),
            nonce: String::new(),
            secret_type: memcore::vault::SECRET_TYPE_API_KEY.to_string(),
            description: String::new(),
            allowed_agents: None,
            created_at: at(0).to_rfc3339(),
            updated_at: at(0).to_rfc3339(),
            accessed_at: String::new(),
            access_count: 0,
        }
    }

    fn env_claim(raw_name: &str, target: &str) -> EnvAliasClaims {
        EnvAliasClaims {
            source_names: BTreeSet::from([raw_name.to_string()]),
            targets: BTreeSet::from([target.to_string()]),
            has_non_alias: false,
        }
    }

    fn account_claim(account_id: &str, target: &str) -> AccountAliasClaim {
        AccountAliasClaim {
            account_id: account_id.to_string(),
            custody_target: target.to_string(),
        }
    }

    fn account_binding(account_id: &str, target: &str, aliases: &[&str]) -> AccountBinding {
        AccountBinding {
            account: ProviderAccount {
                account_id: account_id.to_string(),
                provider_kind: "deepseek".to_string(),
                auth_mode: memcore::vault::accounts::AuthMode::ApiKeyPool,
                auth_ref: Some(format!("va1:{account_id}")),
                account_fingerprint: format!("fp-{account_id}"),
                account_class: memcore::vault::accounts::AccountClass::ModelApi,
                capabilities: Vec::new(),
                credential_policy_ref: None,
                refresh_authority: memcore::vault::accounts::REFRESH_AUTHORITY_NONE.to_string(),
                status: memcore::vault::accounts::ACCOUNT_STATUS_ACTIVE.to_string(),
                revision: 1,
                source_refs: Vec::new(),
                created_at: at(0).to_rfc3339(),
                updated_at: at(0).to_rfc3339(),
            },
            custody: AccountCustody {
                auth_ref: format!("va1:{account_id}"),
                account_id: account_id.to_string(),
                custody_kind: CustodyKind::VaultEntry,
                custody_target: target.to_string(),
                revision: 1,
                updated_at: at(0).to_rfc3339(),
            },
            aliases: aliases
                .iter()
                .map(|alias| memcore::vault::accounts::ProviderAccountAlias {
                    account_id: account_id.to_string(),
                    alias_name: (*alias).to_string(),
                    source_kind: "config_env".to_string(),
                    first_seen: at(0).to_rfc3339(),
                    last_seen: at(0).to_rfc3339(),
                    retired: false,
                })
                .collect(),
        }
    }

    #[test]
    fn typed_unknown_stays_unknown_with_its_evidence_timestamp_and_no_raw_error() {
        let prior = record_key_outcome(
            None,
            "OPENAI_API_KEY",
            "OPENAI_API_KEY",
            TypedOutcome::Success,
            EvidenceKind::Probed,
            None,
            at(1),
        )
        .health;
        let unknown = record_key_outcome(
            Some(&prior),
            "OPENAI_API_KEY",
            "OPENAI_API_KEY",
            TypedOutcome::Unknown,
            EvidenceKind::Probed,
            None,
            at(2),
        )
        .health;

        let observation = probe_observation(Some(&unknown));
        assert_eq!(observation.0, "unknown");
        assert_eq!(observation.1.as_deref(), Some(at(2).to_rfc3339().as_str()));
        assert_eq!(unknown.last_success.as_deref(), Some(at(1).to_rfc3339().as_str()));

        let first_unknown = record_key_outcome(
            None,
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_API_KEY",
            TypedOutcome::Unknown,
            EvidenceKind::Probed,
            None,
            at(3),
        )
        .health;
        let observation = probe_observation(Some(&first_unknown));
        assert_eq!(observation.0, "unknown");
        assert_eq!(observation.1.as_deref(), Some(at(3).to_rfc3339().as_str()));

        let raw_error = record_key_outcome(
            None,
            "RAW_ERROR_API_KEY",
            "RAW_ERROR_API_KEY",
            TypedOutcome::Error,
            EvidenceKind::Probed,
            Some("RAW_PROVIDER_BODY_SENTINEL"),
            at(4),
        )
        .health;
        assert_eq!(probe_observation(Some(&raw_error)).0, "unknown");
        assert!(!format!("{:?}", probe_observation(Some(&raw_error)))
            .contains("RAW_PROVIDER_BODY_SENTINEL"));
    }

    #[test]
    fn exact_health_identity_keeps_account_disable_ahead_of_newer_slot_success() {
        let mut account = new_key_health("DEEPSEEK_API_KEY", "DEEPSEEK_API_KEY", at(1));
        account.disabled = true;
        let slot = record_key_outcome(
            None,
            "EXTRACT_API_KEY",
            "DEEPSEEK_API_KEY",
            TypedOutcome::Success,
            EvidenceKind::SelfReported,
            None,
            at(3),
        )
        .health;
        let collision = record_key_outcome(
            None,
            "UNRELATED_POOL",
            "DEEPSEEK_API_KEY",
            TypedOutcome::Success,
            EvidenceKind::Probed,
            None,
            at(4),
        )
        .health;
        let health = HashMap::from([
            (
                "DEEPSEEK_API_KEY".to_string(),
                HashMap::from([("DEEPSEEK_API_KEY".to_string(), account)]),
            ),
            (
                "EXTRACT_API_KEY".to_string(),
                HashMap::from([("DEEPSEEK_API_KEY".to_string(), slot)]),
            ),
            (
                "UNRELATED_POOL".to_string(),
                HashMap::from([("DEEPSEEK_API_KEY".to_string(), collision)]),
            ),
        ]);
        let rows = relevant_health(
            &health,
            "DEEPSEEK_API_KEY",
            "DEEPSEEK_API_KEY",
            &BTreeSet::from(["EXTRACT_API_KEY".to_string()]),
        );

        let selected = selected_health(&rows).expect("account disable");
        assert_eq!(selected.logical_name, "DEEPSEEK_API_KEY");
        assert_eq!(probe_observation(Some(selected)).0, "disabled");
        assert!(health_is_unusable(&rows));
    }

    #[test]
    fn newer_memory_health_replaces_only_the_same_complete_identity() {
        let persisted = record_key_outcome(
            None,
            "OPENAI_API_KEY",
            "OPENAI_API_KEY",
            TypedOutcome::AuthFailed,
            EvidenceKind::Probed,
            None,
            at(1),
        )
        .health;
        let current = record_key_outcome(
            Some(&persisted),
            "OPENAI_API_KEY",
            "OPENAI_API_KEY",
            TypedOutcome::Success,
            EvidenceKind::SelfReported,
            None,
            at(2),
        )
        .health;
        let second_persisted = record_key_outcome(
            None,
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_API_KEY",
            TypedOutcome::Success,
            EvidenceKind::Probed,
            None,
            at(4),
        )
        .health;
        let older_memory = record_key_outcome(
            None,
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_API_KEY",
            TypedOutcome::AuthFailed,
            EvidenceKind::Probed,
            None,
            at(3),
        )
        .health;
        let merged = crate::vault_ops::access::health_snapshot::merge_provider_key_health(
            vec![persisted, second_persisted],
            HashMap::from([
                (
                    "OPENAI_API_KEY".to_string(),
                    HashMap::from([("OPENAI_API_KEY".to_string(), current)]),
                ),
                (
                    "ANTHROPIC_API_KEY".to_string(),
                    HashMap::from([("ANTHROPIC_API_KEY".to_string(), older_memory)]),
                ),
            ]),
        );
        let rows = relevant_health(
            &merged,
            "OPENAI_API_KEY",
            "OPENAI_API_KEY",
            &BTreeSet::new(),
        );

        let observation = probe_observation(selected_health(&rows));
        assert_eq!(observation.0, "ok");
        assert_eq!(observation.1.as_deref(), Some(at(2).to_rfc3339().as_str()));
        let second = relevant_health(
            &merged,
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_API_KEY",
            &BTreeSet::new(),
        );
        let observation = probe_observation(selected_health(&second));
        assert_eq!(observation.0, "ok");
        assert_eq!(observation.1.as_deref(), Some(at(4).to_rfc3339().as_str()));
    }

    #[test]
    fn probe_projection_preserves_typed_classes_without_inventing_http_status() {
        for reason in ["provider returned HTTP 401", "provider returned HTTP 403"] {
            let health = record_key_outcome(
                None,
                "DEEPSEEK_API_KEY",
                "DEEPSEEK_API_KEY",
                TypedOutcome::AuthFailed,
                EvidenceKind::Probed,
                Some(reason),
                at(1),
            )
            .health;
            let observation = probe_observation(Some(&health));
            assert_eq!(observation.0, "auth_failed");
            assert!(!format!("{observation:?}").contains(reason));
        }

        let empty = record_key_outcome(
            None,
            "EXTRACT_API_KEY",
            "DEEPSEEK_API_KEY",
            TypedOutcome::Error,
            EvidenceKind::SelfReported,
            Some("Empty assistant content: RAW_BODY_SENTINEL"),
            at(2),
        )
        .health;
        assert_eq!(probe_observation(Some(&empty)).0, "empty_content");

        let rate_limited = record_key_outcome(
            None,
            "EXTRACT_API_KEY",
            "DEEPSEEK_API_KEY",
            TypedOutcome::RateLimited {
                retry_after_secs: None,
            },
            EvidenceKind::SelfReported,
            None,
            at(3),
        )
        .health;
        assert_eq!(probe_observation(Some(&rate_limited)).0, "unknown");

        let no_evidence = new_key_health("EXTRACT_API_KEY", "DEEPSEEK_API_KEY", at(4));
        assert_eq!(probe_observation(Some(&no_evidence)), ("unknown", None));
    }

    #[test]
    fn identityless_doctor_cache_never_becomes_account_or_member_health() {
        let identityless_cache = crate::status_ops::status_health::ProviderProbeCache {
            last_probe_at: at(2).to_rfc3339(),
            ttl_seconds: 60,
            probes: vec![crate::status_ops::status_health::ProviderProbeResult {
                name: "chat_extract".to_string(),
                status: "failed".to_string(),
                message: Some("Empty assistant content: RAW_BODY_SENTINEL".to_string()),
            }],
            rotation_groups: Vec::new(),
        };
        assert_eq!(identityless_cache.probes[0].name, "chat_extract");
        for context in [
            "account-rebind-a-to-b",
            "replacement-during-probe",
            "rotation-member-1",
            "rotation-member-2",
        ] {
            let observation = probe_observation(selected_health(&[]));
            assert_eq!(observation, ("unknown", None), "{context}");
        }
    }

    #[test]
    fn entry_replacement_invalidates_older_health_and_runtime_generation() {
        let mut replacement = entry("DEEPSEEK_API_KEY");
        replacement.updated_at = at(3).to_rfc3339();
        let old_success = record_key_outcome(
            None,
            "EXTRACT_API_KEY",
            "DEEPSEEK_API_KEY",
            TypedOutcome::Success,
            EvidenceKind::Probed,
            None,
            at(2),
        )
        .health;
        let late_old_success = record_key_outcome(
            Some(&old_success),
            "EXTRACT_API_KEY",
            "DEEPSEEK_API_KEY",
            TypedOutcome::Success,
            EvidenceKind::Probed,
            None,
            at(4),
        )
        .health;
        let source_health_generation = HashMap::from([(
            (
                old_success.logical_name.clone(),
                old_success.key_id.clone(),
            ),
            crate::vault_ops::access::vault_key_health_revision(&old_success),
        )]);
        assert!(health_is_attributed(
            &old_success,
            true,
            &source_health_generation,
        ));
        assert!(health_updated_at(&late_old_success) > Some(at(3).fixed_offset()));
        assert!(!health_is_attributed(
            &late_old_success,
            true,
            &source_health_generation,
        ));
        assert!(!health_is_attributed(
            &late_old_success,
            false,
            &source_health_generation,
        ));

        let claims = BindingClaims {
            slots: BTreeSet::from(["EXTRACT_API_KEY".to_string()]),
            conflicts: BTreeSet::new(),
        };
        let stale_cache = HashMap::from([(
            "EXTRACT_API_KEY".to_string(),
            BTreeSet::from(["DEEPSEEK_API_KEY".to_string()]),
        )]);
        assert_eq!(
            alias_integrity(
                &replacement,
                "DEEPSEEK_API_KEY",
                memcore::vault::SECRET_TYPE_API_KEY,
                &[],
                &claims,
                false,
                false,
                Some(&stale_cache),
            ),
            "unknown"
        );
        assert_eq!(
            probe_observation(selected_health(&[])),
            ("unknown", None)
        );
    }

    #[test]
    fn normalized_slot_collisions_and_same_target_account_owners_fail_closed() {
        let normalized = normalized_alias_source([
            (
                "EXTRACT_API_KEY".to_string(),
                "vault:DEEPSEEK_API_KEY".to_string(),
            ),
            (
                " EXTRACT_API_KEY ".to_string(),
                " vault:OPENAI_API_KEY ".to_string(),
            ),
        ]);
        let normalized_claims = binding_claims(
            "DEEPSEEK_API_KEY",
            &normalized,
            &HashMap::new(),
        );
        assert_eq!(
            normalized_claims.conflicts,
            BTreeSet::from(["EXTRACT_API_KEY".to_string()])
        );

        let cross_source = reconcile_alias_sources(
            normalized_alias_source([(
                "EXTRACT_API_KEY".to_string(),
                "vault:DEEPSEEK_API_KEY".to_string(),
            )]),
            normalized_alias_source([(
                " EXTRACT_API_KEY ".to_string(),
                "vault:DEEPSEEK_API_KEY".to_string(),
            )]),
        );
        let cross_source_claims =
            binding_claims("DEEPSEEK_API_KEY", &cross_source, &HashMap::new());
        assert_eq!(
            cross_source_claims.conflicts,
            BTreeSet::from(["EXTRACT_API_KEY".to_string()])
        );

        let accounts = HashMap::from([(
            "EXTRACT_API_KEY".to_string(),
            BTreeSet::from([
                account_claim("account-a", "DEEPSEEK_API_KEY"),
                account_claim("account-b", "DEEPSEEK_API_KEY"),
            ]),
        )]);
        let account_claims = binding_claims("DEEPSEEK_API_KEY", &HashMap::new(), &accounts);
        assert_eq!(
            account_claims.conflicts,
            BTreeSet::from(["EXTRACT_API_KEY".to_string()])
        );
    }

    #[test]
    fn config_parser_preserves_duplicate_and_cross_source_normalized_conflicts() {
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            home.path().join("config.env"),
            concat!(
                "EXTRACT_API_KEY=vault:DEEPSEEK_API_KEY\n",
                "EXTRACT_API_KEY = vault:OPENAI_API_KEY\n",
            ),
        )
        .expect("write config.env");

        let raw_claims = crate::provider_config::collect_config_env_claims(Some(home.path()))
            .into_iter()
            .filter(|(name, _)| name.trim() == "EXTRACT_API_KEY")
            .collect::<Vec<_>>();
        assert!(
            raw_claims
                .iter()
                .any(|(_, target)| target.trim() == "vault:DEEPSEEK_API_KEY")
        );
        assert!(
            raw_claims
                .iter()
                .any(|(_, target)| target.trim() == "vault:OPENAI_API_KEY")
        );

        let configured = normalized_alias_source(raw_claims);
        let config_claims =
            binding_claims("DEEPSEEK_API_KEY", &configured, &HashMap::new());
        assert_eq!(
            config_claims.conflicts,
            BTreeSet::from(["EXTRACT_API_KEY".to_string()]),
            "duplicate config claims must survive parser collection"
        );

        let reconciled = reconcile_alias_sources(
            configured,
            normalized_alias_source([(
                " EXTRACT_API_KEY ".to_string(),
                "vault:ANTHROPIC_API_KEY".to_string(),
            )]),
        );
        let claims = binding_claims("DEEPSEEK_API_KEY", &reconciled, &HashMap::new());
        assert_eq!(
            claims.conflicts,
            BTreeSet::from(["EXTRACT_API_KEY".to_string()]),
            "process precedence must not erase padded contradictory evidence"
        );
    }

    #[test]
    fn shared_custody_target_is_ambiguous_across_disjoint_or_one_sided_aliases() {
        let bindings = vec![
            account_binding(
                "account-a",
                "DEEPSEEK_API_KEY",
                &["EXTRACT_API_KEY"],
            ),
            account_binding(
                "account-b",
                "DEEPSEEK_API_KEY",
                &["SUMMARY_API_KEY"],
            ),
        ];
        let owners = account_custody_owners(&bindings);
        assert_eq!(
            owners.get("DEEPSEEK_API_KEY"),
            Some(&BTreeSet::from([
                "account-a".to_string(),
                "account-b".to_string(),
            ]))
        );

        let account_targets = account_alias_targets(&bindings);
        let claims = binding_claims("DEEPSEEK_API_KEY", &HashMap::new(), &account_targets);
        assert!(claims.conflicts.is_empty(), "aliases are deliberately disjoint");
        let runtime = HashMap::from([
            (
                "EXTRACT_API_KEY".to_string(),
                BTreeSet::from(["DEEPSEEK_API_KEY".to_string()]),
            ),
            (
                "SUMMARY_API_KEY".to_string(),
                BTreeSet::from(["DEEPSEEK_API_KEY".to_string()]),
            ),
        ]);
        assert_eq!(
            alias_integrity(
                &entry("DEEPSEEK_API_KEY"),
                "DEEPSEEK_API_KEY",
                memcore::vault::SECRET_TYPE_API_KEY,
                &[],
                &claims,
                true,
                true,
                Some(&runtime),
            ),
            "unknown"
        );

        let one_sided = vec![
            account_binding(
                "account-a",
                "DEEPSEEK_API_KEY",
                &["EXTRACT_API_KEY"],
            ),
            account_binding("account-b", "DEEPSEEK_API_KEY", &[]),
        ];
        assert_eq!(
            account_custody_owners(&one_sided)
                .get("DEEPSEEK_API_KEY")
                .map(|owners| owners.len()),
            Some(2)
        );
    }

    #[test]
    fn alias_integrity_requires_every_binding_and_fails_closed_on_conflict_or_absence() {
        let account = entry("DEEPSEEK_API_KEY");
        let detected_conflict = binding_claims(
            "DEEPSEEK_API_KEY",
            &HashMap::from([(
                "EXTRACT_API_KEY".to_string(),
                env_claim("EXTRACT_API_KEY", "OPENAI_API_KEY"),
            )]),
            &HashMap::from([(
                "EXTRACT_API_KEY".to_string(),
                BTreeSet::from([account_claim("deepseek-main", "DEEPSEEK_API_KEY")]),
            )]),
        );
        assert_eq!(
            detected_conflict.conflicts,
            BTreeSet::from(["EXTRACT_API_KEY".to_string()])
        );
        let claims = BindingClaims {
            slots: BTreeSet::from(["EXTRACT_API_KEY".to_string(), "SUMMARY_API_KEY".to_string()]),
            conflicts: BTreeSet::new(),
        };
        let one_binding = HashMap::from([(
            "EXTRACT_API_KEY".to_string(),
            BTreeSet::from(["DEEPSEEK_API_KEY".to_string()]),
        )]);
        assert_eq!(
            alias_integrity(
                &account,
                "DEEPSEEK_API_KEY",
                memcore::vault::SECRET_TYPE_API_KEY,
                &[],
                &claims,
                false,
                true,
                Some(&one_binding),
            ),
            "absent"
        );

        let all_bindings = HashMap::from([
            (
                "EXTRACT_API_KEY".to_string(),
                BTreeSet::from(["DEEPSEEK_API_KEY".to_string()]),
            ),
            (
                "SUMMARY_API_KEY".to_string(),
                BTreeSet::from(["DEEPSEEK_API_KEY".to_string()]),
            ),
        ]);
        assert_eq!(
            alias_integrity(
                &account,
                "DEEPSEEK_API_KEY",
                memcore::vault::SECRET_TYPE_API_KEY,
                &[],
                &claims,
                false,
                true,
                Some(&all_bindings),
            ),
            "resolved"
        );

        let conflicting = BindingClaims {
            slots: claims.slots.clone(),
            conflicts: BTreeSet::from(["EXTRACT_API_KEY".to_string()]),
        };
        assert_eq!(
            alias_integrity(
                &account,
                "DEEPSEEK_API_KEY",
                memcore::vault::SECRET_TYPE_API_KEY,
                &[],
                &conflicting,
                false,
                true,
                Some(&all_bindings),
            ),
            "unknown"
        );
        assert_eq!(
            alias_integrity(
                &account,
                "DEEPSEEK_API_KEY",
                memcore::vault::SECRET_TYPE_OAUTH_TOKEN,
                &[],
                &claims,
                false,
                false,
                None,
            ),
            "unknown",
            "a direct or locked list cannot infer integrity from persisted metadata"
        );
        assert_eq!(
            alias_integrity(
                &account,
                "DEEPSEEK_API_KEY",
                memcore::vault::SECRET_TYPE_API_KEY,
                &[],
                &claims,
                false,
                true,
                Some(&HashMap::new()),
            ),
            "unknown",
            "cache absence is not proof of an empty credential"
        );
    }

    #[test]
    fn rotation_prefix_binding_applies_to_each_member() {
        let rotations = BTreeSet::from(["DEEPSEEK_API_KEY".to_string()]);
        let env = HashMap::from([(
            "EXTRACT_API_KEY".to_string(),
            env_claim("EXTRACT_API_KEY", "DEEPSEEK_API_KEY"),
        )]);
        let claims = binding_claims("DEEPSEEK_API_KEY", &env, &HashMap::new());
        let runtime = HashMap::from([(
            "EXTRACT_API_KEY".to_string(),
            BTreeSet::from([
                "DEEPSEEK_API_KEY_1".to_string(),
                "DEEPSEEK_API_KEY_2".to_string(),
            ]),
        )]);

        for member in ["DEEPSEEK_API_KEY_1", "DEEPSEEK_API_KEY_2"] {
            assert_eq!(
                configured_logical_name(member, &[], &rotations),
                "DEEPSEEK_API_KEY"
            );
            assert_eq!(
                claims.slots,
                BTreeSet::from(["EXTRACT_API_KEY".to_string()])
            );
            assert_eq!(
                alias_integrity(
                    &entry(member),
                    "DEEPSEEK_API_KEY",
                    memcore::vault::SECRET_TYPE_API_KEY,
                    &[],
                    &claims,
                    false,
                    true,
                    Some(&runtime),
                ),
                "resolved"
            );
        }
    }

    #[test]
    fn standalone_registered_rotation_member_is_admitted_without_a_configured_pool() {
        let member = "VOYAGE_API_KEY_2";
        assert!(crate::provider_config::is_model_provider_pool_name(member));
        let env = HashMap::from([(
            "EXTRACT_API_KEY".to_string(),
            env_claim("EXTRACT_API_KEY", member),
        )]);
        let claims = binding_claims(member, &env, &HashMap::new());
        let runtime = HashMap::from([(
            "EXTRACT_API_KEY".to_string(),
            BTreeSet::from([member.to_string()]),
        )]);
        assert_eq!(
            alias_integrity(
                &entry(member),
                member,
                memcore::vault::SECRET_TYPE_API_KEY,
                &[],
                &claims,
                false,
                true,
                Some(&runtime),
            ),
            "resolved"
        );

        assert!(!crate::provider_config::is_model_provider_pool_name(
            "VOYAGE_API_KEY_X"
        ));
        assert!(!crate::provider_config::is_model_provider_pool_name(
            "TAVILY_API_KEY_2"
        ));
        assert!(!crate::provider_config::is_model_provider_pool_name(
            "UNREGISTERED_API_KEY_2"
        ));
    }
}
