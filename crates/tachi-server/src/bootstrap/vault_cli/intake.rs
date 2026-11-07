use crate::provider_config::parse_vault_alias;
use memcore::vault::{SECRET_TYPE_API_KEY, SECRET_TYPE_JSON_BLOB};
use memcore::{AccountClass, AuthMode, CustodyKind, FingerprintKey, ProviderAccount};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use tachi_bootstrap::cli::{VaultAction, VaultIntakeAction};

use super::keys::{read_vault_config_for_key, read_verified_vault_key};
use super::open_cli_store_read_only;

const PLAN_SCHEMA: &str = "tachi.vault-intake-plan.v2";
const ACTION_CREATE_ACCOUNT: &str = "create-account";
const ACTION_BIND_SLOT: &str = "bind-slot";
const ACTION_ROTATE_ACCOUNT: &str = "rotate-account";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Candidate {
    pub source_path: String,
    pub logical_name: String,
    pub secret_type: String,
    pub classification: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub intended_slot_binds: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DiscoveryNote {
    code: String,
    source: String,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DiscoveryReport {
    candidates: Vec<Candidate>,
    notes: Vec<DiscoveryNote>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct IntakePlanAction {
    action: String,
    provider_kind: String,
    account_id: String,
    key_fingerprint: String,
    account_fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PlanReport {
    schema: String,
    plan_digest: String,
    actions: Vec<IntakePlanAction>,
    candidates: Vec<Candidate>,
    notes: Vec<DiscoveryNote>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostFilter {
    All,
    Env,
    Codex,
    Unsupported,
}

struct RawCandidate {
    source_path: PathBuf,
    logical_name: String,
    secret_type: &'static str,
    value: String,
}

#[derive(Debug)]
struct EndpointEvidence {
    source_path: PathBuf,
    prefix: String,
    admitted_provider_kind: Option<String>,
}

struct ClassifiedCandidate {
    raw: RawCandidate,
    classification: &'static str,
    provider_kind: Option<String>,
    canonical_key: Option<&'static str>,
    account_class: Option<AccountClass>,
    key_fingerprint: Option<String>,
    account_fingerprint: Option<String>,
    account_id: Option<String>,
    planned_action: Option<&'static str>,
    intended_slot_binds: BTreeSet<String>,
}

struct AccountInventory {
    accounts: Vec<ProviderAccount>,
    lane_slot_bindings: HashMap<String, HashSet<String>>,
    custody: HashMap<String, AccountCustodyEvidence>,
    member_fingerprints: HashMap<String, BTreeSet<String>>,
}

struct AccountCustodyEvidence {
    kind: CustodyKind,
    target: String,
}

impl AccountInventory {
    fn empty() -> Self {
        Self {
            accounts: Vec::new(),
            lane_slot_bindings: HashMap::new(),
            custody: HashMap::new(),
            member_fingerprints: HashMap::new(),
        }
    }
}

pub(super) fn run_intake_action(
    global_db_path: &PathBuf,
    _app_home: &Path,
    action: VaultAction,
) -> Result<(), Box<dyn std::error::Error>> {
    let VaultAction::Intake { action } = action else {
        unreachable!("intake router received non-intake action");
    };
    let cwd = std::env::current_dir()?;
    let env_home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

    match action {
        VaultIntakeAction::Discover {
            host,
            files,
            json,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
        } => {
            let (filter, notes) = parse_host_filter(host.as_deref());
            if !files.is_empty() && filter == HostFilter::Codex {
                return Err("Cannot combine --file with --host codex".into());
            }
            let report = if filter == HostFilter::Unsupported {
                DiscoveryReport {
                    candidates: Vec::new(),
                    notes,
                }
            } else if filter == HostFilter::Codex {
                discover_report(&env_home, &cwd, global_db_path, filter, notes, None)?
            } else {
                let config = read_vault_config_for_key(global_db_path)?;
                let key = read_verified_vault_key(
                    &config,
                    stdin_password,
                    keychain,
                    password_file.as_deref(),
                    insecure_password_file,
                )?;
                let fp_key = FingerprintKey::derive_from_master_key(key.bytes());
                discover_report_with_files(
                    &env_home,
                    &cwd,
                    global_db_path,
                    filter,
                    &files,
                    notes,
                    Some(&fp_key),
                )?
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", render_discovery_human(&report));
            }
        }
        VaultIntakeAction::Plan {
            host,
            json,
            write,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
        } => {
            let (filter, notes) = parse_host_filter(host.as_deref());
            let report = if filter == HostFilter::Unsupported {
                build_plan(DiscoveryReport {
                    candidates: Vec::new(),
                    notes,
                })
            } else if filter == HostFilter::Codex {
                build_plan(discover_report(
                    &env_home,
                    &cwd,
                    global_db_path,
                    filter,
                    notes,
                    None,
                )?)
            } else {
                let config = read_vault_config_for_key(global_db_path)?;
                let key = read_verified_vault_key(
                    &config,
                    stdin_password,
                    keychain,
                    password_file.as_deref(),
                    insecure_password_file,
                )?;
                let fp_key = FingerprintKey::derive_from_master_key(key.bytes());
                plan_report(&env_home, &cwd, global_db_path, filter, notes, &fp_key)?
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", render_plan_human(&report));
            }
            if write {
                let path = write_plan_artifact(&cwd, &report)?;
                println!("wrote redacted plan artifact to {}", path.display());
            }
        }
    }
    Ok(())
}

fn parse_host_filter(host: Option<&str>) -> (HostFilter, Vec<DiscoveryNote>) {
    let Some(host) = host.map(str::trim).filter(|host| !host.is_empty()) else {
        return (HostFilter::All, Vec::new());
    };
    match host.to_ascii_lowercase().as_str() {
        "env" => (HostFilter::Env, Vec::new()),
        "codex" => (HostFilter::Codex, Vec::new()),
        other => (
            HostFilter::Unsupported,
            vec![DiscoveryNote {
                code: "unsupported_source".to_string(),
                source: other.to_string(),
                message: "source is not supported by this read-only intake slice".to_string(),
            }],
        ),
    }
}

fn discover_report(
    env_home: &Path,
    cwd: &Path,
    global_db_path: &Path,
    filter: HostFilter,
    notes: Vec<DiscoveryNote>,
    fp_key: Option<&FingerprintKey>,
) -> Result<DiscoveryReport, Box<dyn std::error::Error>> {
    discover_report_with_files(env_home, cwd, global_db_path, filter, &[], notes, fp_key)
}

fn discover_report_with_files(
    env_home: &Path,
    cwd: &Path,
    global_db_path: &Path,
    filter: HostFilter,
    explicit_files: &[PathBuf],
    notes: Vec<DiscoveryNote>,
    fp_key: Option<&FingerprintKey>,
) -> Result<DiscoveryReport, Box<dyn std::error::Error>> {
    let mut raw = Vec::new();
    if !explicit_files.is_empty() {
        let mut seen = HashSet::new();
        for file in explicit_files {
            let path = if file.is_absolute() {
                file.clone()
            } else {
                cwd.join(file)
            };
            if !seen.insert(path.clone()) {
                continue;
            }
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read --file {}: {e}", path.display()))?;
            for (src_path, logical_name, value) in parse_env_content(&path, &content) {
                raw.push(RawCandidate {
                    source_path: src_path,
                    logical_name,
                    secret_type: SECRET_TYPE_API_KEY,
                    value,
                });
            }
        }
    } else {
        if matches!(filter, HostFilter::All | HostFilter::Env) {
            for (path, logical_name, value) in discover_env_values(env_home, cwd) {
                raw.push(RawCandidate {
                    source_path: path,
                    logical_name,
                    secret_type: SECRET_TYPE_API_KEY,
                    value,
                });
            }
        }
        if matches!(filter, HostFilter::All | HostFilter::Codex) {
            let path = env_home.join(".codex").join("auth.json");
            if let Ok(value) = std::fs::read_to_string(&path) {
                if !value.trim().is_empty() {
                    raw.push(RawCandidate {
                        source_path: path,
                        logical_name: "codex.auth".to_string(),
                        secret_type: SECRET_TYPE_JSON_BLOB,
                        value,
                    });
                }
            }
        }
    }

    let inventory = account_inventory(global_db_path)?;
    let endpoints = endpoint_evidence(&raw);
    let classified = classify_candidates(raw, &endpoints, fp_key, &inventory);
    Ok(DiscoveryReport {
        candidates: classified.into_iter().map(public_candidate).collect(),
        notes,
    })
}

fn plan_report(
    env_home: &Path,
    cwd: &Path,
    global_db_path: &Path,
    filter: HostFilter,
    notes: Vec<DiscoveryNote>,
    fp_key: &FingerprintKey,
) -> Result<PlanReport, Box<dyn std::error::Error>> {
    let mut raw = Vec::new();
    if matches!(filter, HostFilter::All | HostFilter::Env) {
        for (path, logical_name, value) in discover_env_values(env_home, cwd) {
            raw.push(RawCandidate {
                source_path: path,
                logical_name,
                secret_type: SECRET_TYPE_API_KEY,
                value,
            });
        }
    }
    if matches!(filter, HostFilter::All | HostFilter::Codex) {
        let path = env_home.join(".codex").join("auth.json");
        if let Ok(value) = std::fs::read_to_string(&path) {
            if !value.trim().is_empty() {
                raw.push(RawCandidate {
                    source_path: path,
                    logical_name: "codex.auth".to_string(),
                    secret_type: SECRET_TYPE_JSON_BLOB,
                    value,
                });
            }
        }
    }
    let inventory = account_inventory(global_db_path)?;
    let endpoints = endpoint_evidence(&raw);
    let classified = classify_candidates(raw, &endpoints, Some(fp_key), &inventory);
    Ok(build_plan_from_classified(classified, notes, &inventory))
}

fn build_plan(discovery: DiscoveryReport) -> PlanReport {
    let actions = Vec::new();
    PlanReport {
        schema: PLAN_SCHEMA.to_string(),
        plan_digest: plan_digest(&actions),
        actions,
        candidates: discovery.candidates,
        notes: discovery.notes,
    }
}

fn build_plan_from_classified(
    mut candidates: Vec<ClassifiedCandidate>,
    notes: Vec<DiscoveryNote>,
    inventory: &AccountInventory,
) -> PlanReport {
    // A canonical lane slot can name only one account target. Resolve conflicts
    // before grouping by account, so distinct fresh accounts cannot each emit a
    // binding for the same slot. Repeated sightings of the same target are safe.
    let mut targets_by_slot: HashMap<String, BTreeSet<(String, String)>> = HashMap::new();
    for candidate in &candidates {
        if candidate.classification != "known" {
            continue;
        }
        if let (Some(account_id), Some(account_fingerprint)) = (
            candidate.account_id.as_ref(),
            candidate.account_fingerprint.as_ref(),
        ) {
            for slot in &candidate.intended_slot_binds {
                targets_by_slot
                    .entry(slot.clone())
                    .or_default()
                    .insert((account_id.clone(), account_fingerprint.clone()));
            }
        }
    }
    let conflicted_targets: HashSet<_> = targets_by_slot
        .into_values()
        .filter(|targets| targets.len() > 1)
        .flatten()
        .collect();
    for candidate in &mut candidates {
        // Include canonical-key siblings without their own slot intent: they
        // must not recreate an account rejected by the slot conflict above.
        if candidate
            .account_id
            .as_ref()
            .zip(candidate.account_fingerprint.as_ref())
            .is_some_and(|(account_id, fingerprint)| {
                conflicted_targets.contains(&(account_id.clone(), fingerprint.clone()))
            })
        {
            candidate.classification = "ambiguous";
            candidate.account_id = None;
            candidate.planned_action = None;
            candidate.intended_slot_binds.clear();
        }
    }

    let mut groups: BTreeMap<(String, String, String), Vec<usize>> = BTreeMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if candidate.classification != "known" {
            continue;
        }
        let (Some(kind), Some(account_id), Some(account_fp)) = (
            candidate.provider_kind.as_ref(),
            candidate.account_id.as_ref(),
            candidate.account_fingerprint.as_ref(),
        ) else {
            continue;
        };
        groups
            .entry((kind.clone(), account_id.clone(), account_fp.clone()))
            .or_default()
            .push(index);
    }

    let mut actions = Vec::new();
    for ((provider_kind, account_id, account_fingerprint), indices) in groups {
        let key_fingerprint = indices
            .iter()
            .filter_map(|index| candidates[*index].key_fingerprint.as_ref())
            .min()
            .cloned()
            .expect("known API-key candidates are fingerprinted");
        let account_actions: BTreeSet<Option<&'static str>> = indices
            .iter()
            .map(|index| candidates[*index].planned_action)
            .collect();
        if account_actions.len() != 1 {
            continue;
        }
        let account_action = account_actions.iter().next().copied().flatten();
        if let Some(action) = account_action {
            actions.push(IntakePlanAction {
                action: action.to_string(),
                provider_kind: provider_kind.clone(),
                account_id: account_id.clone(),
                key_fingerprint: key_fingerprint.clone(),
                account_fingerprint: account_fingerprint.clone(),
                slot: None,
            });
        }

        let mut intended_slots: BTreeMap<String, String> = BTreeMap::new();
        for index in &indices {
            let candidate = &candidates[*index];
            let candidate_fingerprint = candidate
                .key_fingerprint
                .as_ref()
                .expect("known API-key candidates are fingerprinted");
            for slot in &candidate.intended_slot_binds {
                intended_slots
                    .entry(slot.clone())
                    .and_modify(|fingerprint| {
                        if candidate_fingerprint.as_str() < fingerprint.as_str() {
                            fingerprint.clone_from(candidate_fingerprint);
                        }
                    })
                    .or_insert_with(|| candidate_fingerprint.clone());
            }
        }
        let existing_bindings = inventory.lane_slot_bindings.get(&account_id);
        for (slot, slot_fingerprint) in intended_slots {
            if existing_bindings.is_some_and(|bindings| bindings.contains(&slot)) {
                continue;
            }
            actions.push(IntakePlanAction {
                action: ACTION_BIND_SLOT.to_string(),
                provider_kind: provider_kind.clone(),
                account_id: account_id.clone(),
                key_fingerprint: slot_fingerprint,
                account_fingerprint: account_fingerprint.clone(),
                slot: Some(slot),
            });
        }
    }

    let digest = plan_digest(&actions);
    let mut candidates: Vec<Candidate> = candidates.into_iter().map(public_candidate).collect();
    candidates
        .sort_by(|a, b| (&a.source_path, &a.logical_name).cmp(&(&b.source_path, &b.logical_name)));
    PlanReport {
        schema: PLAN_SCHEMA.to_string(),
        plan_digest: digest,
        actions,
        candidates,
        notes,
    }
}

fn classify_candidates(
    raw: Vec<RawCandidate>,
    endpoints: &[EndpointEvidence],
    fp_key: Option<&FingerprintKey>,
    inventory: &AccountInventory,
) -> Vec<ClassifiedCandidate> {
    let mut candidates: Vec<ClassifiedCandidate> = raw
        .into_iter()
        .map(|raw| classify_candidate(raw, endpoints))
        .collect();

    for candidate in &mut candidates {
        if candidate.classification != "known" {
            continue;
        }
        let (Some(provider_kind), Some(account_class)) =
            (candidate.provider_kind.as_deref(), candidate.account_class)
        else {
            continue;
        };
        if let Some(slot) = slot_for_key_name(&candidate.raw.logical_name) {
            candidate.intended_slot_binds.insert(slot.to_string());
        }
        let Some(fp_key) = fp_key else {
            continue;
        };
        let key_fingerprint = fp_key.key_fingerprint(provider_kind, candidate.raw.value.trim());
        let account_fingerprint = fp_key.account_fingerprint_from_members([&key_fingerprint]);
        candidate.key_fingerprint = Some(key_fingerprint.clone());
        candidate.account_fingerprint = Some(account_fingerprint.clone());
        let matching_accounts: Vec<&ProviderAccount> = inventory
            .accounts
            .iter()
            .filter(|account| {
                account.provider_kind == provider_kind
                    && account_is_eligible(account, account_class)
                    && (account.account_fingerprint == account_fingerprint
                        || current_member_fingerprints(account, inventory, fp_key)
                            .is_some_and(|members| members.contains(&key_fingerprint)))
            })
            .collect();
        if let [account] = matching_accounts.as_slice() {
            candidate.account_id = Some(account.account_id.clone());
            candidate.account_fingerprint = Some(account.account_fingerprint.clone());
            continue;
        }
        if matching_accounts.len() > 1 {
            candidate.classification = "ambiguous";
            continue;
        }

        let Some(canonical) = candidate.canonical_key else {
            candidate.account_id = Some(planned_account_id(provider_kind, &account_fingerprint));
            candidate.planned_action = Some(ACTION_CREATE_ACCOUNT);
            continue;
        };
        let rotating: Vec<&ProviderAccount> = inventory
            .accounts
            .iter()
            .filter(|account| {
                account.provider_kind == provider_kind
                    && account_is_eligible(account, account_class)
                    && inventory
                        .custody
                        .get(&account.account_id)
                        .is_some_and(|custody| custody.target == canonical)
            })
            .collect();
        match rotating.as_slice() {
            [account] => {
                candidate.account_id = Some(account.account_id.clone());
                candidate.planned_action = Some(ACTION_ROTATE_ACCOUNT);
            }
            [] => {
                candidate.account_id =
                    Some(planned_account_id(provider_kind, &account_fingerprint));
                candidate.planned_action = Some(ACTION_CREATE_ACCOUNT);
            }
            _ => candidate.classification = "ambiguous",
        }
    }

    let mut fingerprints_by_account: HashMap<String, BTreeSet<String>> = HashMap::new();
    for candidate in &candidates {
        if candidate.classification != "known" {
            continue;
        }
        if let (Some(account_id), Some(account_fingerprint)) = (
            candidate.account_id.as_ref(),
            candidate.account_fingerprint.as_ref(),
        ) {
            fingerprints_by_account
                .entry(account_id.clone())
                .or_default()
                .insert(account_fingerprint.clone());
        }
    }
    let ambiguous_accounts: HashSet<String> = fingerprints_by_account
        .into_iter()
        .filter_map(|(account_id, fingerprints)| (fingerprints.len() > 1).then_some(account_id))
        .collect();
    for candidate in &mut candidates {
        if candidate
            .account_id
            .as_ref()
            .is_some_and(|account_id| ambiguous_accounts.contains(account_id))
        {
            candidate.classification = "ambiguous";
            candidate.account_id = None;
            candidate.planned_action = None;
            candidate.intended_slot_binds.clear();
        }
    }
    candidates
}

fn current_member_fingerprints<'a>(
    account: &ProviderAccount,
    inventory: &'a AccountInventory,
    fp_key: &FingerprintKey,
) -> Option<&'a BTreeSet<String>> {
    let custody = inventory.custody.get(&account.account_id)?;
    if custody.kind != CustodyKind::VaultRotationPool {
        return None;
    }
    let members = inventory.member_fingerprints.get(&account.account_id)?;
    (fp_key.account_fingerprint_from_members(members) == account.account_fingerprint)
        .then_some(members)
}

fn account_is_eligible(account: &ProviderAccount, expected_class: AccountClass) -> bool {
    account.auth_mode == AuthMode::ApiKeyPool
        && account.status == memcore::vault::accounts::ACCOUNT_STATUS_ACTIVE
        && account.account_class == expected_class
}

fn classify_candidate(raw: RawCandidate, endpoints: &[EndpointEvidence]) -> ClassifiedCandidate {
    let mut result = ClassifiedCandidate {
        raw,
        classification: "unknown",
        provider_kind: None,
        canonical_key: None,
        account_class: None,
        key_fingerprint: None,
        account_fingerprint: None,
        account_id: None,
        planned_action: None,
        intended_slot_binds: BTreeSet::new(),
    };
    if result.raw.secret_type != SECRET_TYPE_API_KEY
        || result.raw.value.trim().is_empty()
        || classify_non_key(&result.raw.logical_name, &result.raw.value).is_some()
    {
        return result;
    }

    let name_kind =
        crate::provider_config::key_registry::provider_kind_for_env_name(&result.raw.logical_name);
    let prefix = key_prefix(&result.raw.logical_name);
    let matching: Vec<&EndpointEvidence> = endpoints
        .iter()
        .filter(|evidence| {
            evidence.source_path == result.raw.source_path
                && Some(evidence.prefix.as_str()) == prefix.as_deref()
        })
        .collect();
    if matching
        .iter()
        .any(|evidence| evidence.admitted_provider_kind.is_none())
    {
        result.classification = "conflicted";
        return result;
    }
    let endpoint_kinds: BTreeSet<String> = matching
        .iter()
        .filter_map(|evidence| evidence.admitted_provider_kind.clone())
        .collect();

    match (name_kind, endpoint_kinds.len()) {
        (Some(kind), 0) => {
            result.classification = "known";
            result.provider_kind = Some(kind.to_string());
            result.canonical_key = crate::provider_config::key_registry::canonical_key_for_env_name(
                &result.raw.logical_name,
            );
            result.account_class = crate::provider_config::key_registry::account_class_for_env_name(
                &result.raw.logical_name,
            );
        }
        (Some(kind), 1) if endpoint_kinds.contains(kind) => {
            result.classification = "known";
            result.provider_kind = Some(kind.to_string());
            result.canonical_key = crate::provider_config::key_registry::canonical_key_for_env_name(
                &result.raw.logical_name,
            );
            result.account_class = crate::provider_config::key_registry::account_class_for_env_name(
                &result.raw.logical_name,
            );
        }
        (Some(_), _) => result.classification = "conflicted",
        (None, 1) => {
            let provider_kind = endpoint_kinds
                .into_iter()
                .next()
                .expect("one endpoint kind");
            let inferred_key_name = prefix.as_ref().map(|prefix| format!("{prefix}_API_KEY"));
            let registry_matches_endpoint = inferred_key_name.as_deref().is_some_and(|name| {
                crate::provider_config::key_registry::provider_kind_for_env_name(name)
                    == Some(provider_kind.as_str())
            });
            if registry_matches_endpoint
                || prefix.as_deref().and_then(lane_slot_for_prefix).is_some()
            {
                result.classification = "known";
                result.provider_kind = Some(provider_kind);
                if registry_matches_endpoint {
                    result.canonical_key = inferred_key_name
                        .as_deref()
                        .and_then(crate::provider_config::key_registry::canonical_key_for_env_name);
                }
                result.account_class = Some(
                    inferred_key_name
                        .as_deref()
                        .and_then(crate::provider_config::key_registry::account_class_for_env_name)
                        .unwrap_or(AccountClass::ModelApi),
                );
            }
        }
        (None, 0) => {}
        (None, _) => result.classification = "ambiguous",
    }
    result
}

fn classify_non_key(logical_name: &str, value: &str) -> Option<&'static str> {
    if parse_vault_alias(value).is_some() {
        return Some("vault_reference");
    }
    if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("false") {
        return Some("boolean");
    }
    if value.starts_with("https://") || value.starts_with("http://") {
        return Some("url");
    }
    if is_lane_config(logical_name) {
        return Some("lane_config");
    }
    if is_config_name(logical_name) {
        return Some("config");
    }
    None
}

fn endpoint_evidence(raw: &[RawCandidate]) -> Vec<EndpointEvidence> {
    raw.iter()
        .filter_map(|candidate| {
            let prefix = config_prefix(&candidate.logical_name)?;
            Some(EndpointEvidence {
                source_path: candidate.source_path.clone(),
                prefix,
                admitted_provider_kind: provider_kind_from_endpoint(&candidate.value),
            })
        })
        .collect()
}

fn provider_kind_from_endpoint(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let parsed = url::Url::parse(trimmed)
        .or_else(|_| url::Url::parse(&format!("https://{trimmed}")))
        .ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    tachi_llm::auth_probe_descriptor_for_host(parsed.host_str()?)
        .map(|descriptor| descriptor.provider_kind.to_string())
}

fn config_prefix(name: &str) -> Option<String> {
    ["_BASE_URL", "_URL"]
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .filter(|prefix| !prefix.is_empty())
        .map(str::to_string)
}

fn key_prefix(name: &str) -> Option<String> {
    name.split_once("_API_KEY")
        .map(|(prefix, _)| prefix)
        .filter(|prefix| !prefix.is_empty())
        .map(str::to_string)
}

fn slot_for_key_name(name: &str) -> Option<&'static str> {
    lane_slot_for_prefix(key_prefix(name)?.as_str())
}

fn lane_slot_for_prefix(prefix: &str) -> Option<&'static str> {
    match prefix {
        "EXTRACT" => Some("EXTRACT_API_KEY"),
        "SUMMARY" => Some("SUMMARY_API_KEY"),
        "DISTILL" => Some("DISTILL_API_KEY"),
        "REASONING" => Some("REASONING_API_KEY"),
        _ => None,
    }
}

fn planned_account_id(provider_kind: &str, account_fingerprint: &str) -> String {
    let digest = account_fingerprint.rsplit(':').next().unwrap_or("unknown");
    format!("planned:{provider_kind}:{digest}")
}

fn plan_digest(actions: &[IntakePlanAction]) -> String {
    let encoded = serde_json::to_vec(actions).expect("intake plan actions are JSON encodable");
    format!("pd1:{}", tachi_params::sha256_hex(&encoded))
}

fn public_candidate(candidate: ClassifiedCandidate) -> Candidate {
    Candidate {
        source_path: candidate.raw.source_path.to_string_lossy().to_string(),
        logical_name: candidate.raw.logical_name,
        secret_type: candidate.raw.secret_type.to_string(),
        classification: candidate.classification.to_string(),
        provider_kind: candidate.provider_kind,
        account_id: candidate.account_id,
        key_fingerprint: candidate.key_fingerprint,
        account_fingerprint: candidate.account_fingerprint,
        intended_slot_binds: candidate.intended_slot_binds.into_iter().collect(),
    }
}

fn account_inventory(
    global_db_path: &Path,
) -> Result<AccountInventory, Box<dyn std::error::Error>> {
    if !global_db_path.exists() {
        return Ok(AccountInventory::empty());
    }
    let store = open_cli_store_read_only(&global_db_path.to_path_buf())?;
    let accounts = memcore::db::list_provider_accounts(store.connection())?;
    let mut lane_slot_bindings = HashMap::new();
    let mut custody = HashMap::new();
    let mut member_fingerprints = HashMap::new();
    for account in &accounts {
        lane_slot_bindings.insert(
            account.account_id.clone(),
            memcore::db::list_provider_account_aliases(store.connection(), &account.account_id)?
                .into_iter()
                .filter(|alias| !alias.retired && alias.source_kind == "lane_slot")
                .map(|alias| alias.alias_name)
                .collect(),
        );
        if let Some(account_custody) =
            memcore::db::get_account_custody(store.connection(), &account.account_id)?
        {
            custody.insert(
                account.account_id.clone(),
                AccountCustodyEvidence {
                    kind: account_custody.custody_kind,
                    target: account_custody.custody_target,
                },
            );
        }
        let events =
            memcore::db::list_provider_account_events(store.connection(), &account.account_id)?;
        if let Some(members) = events
            .iter()
            .rev()
            .filter(|event| event.revision == account.revision)
            .find_map(|event| member_fingerprints_from_evidence(&event.evidence))
        {
            member_fingerprints.insert(account.account_id.clone(), members);
        }
    }
    Ok(AccountInventory {
        accounts,
        lane_slot_bindings,
        custody,
        member_fingerprints,
    })
}

fn member_fingerprints_from_evidence(evidence: &str) -> Option<BTreeSet<String>> {
    let parsed: serde_json::Value = serde_json::from_str(evidence).ok()?;
    let values = parsed.get("member_fingerprints")?.as_array()?;
    let members: BTreeSet<String> = values
        .iter()
        .map(|value| value.as_str())
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .filter(|fingerprint| fingerprint.starts_with("fp1:"))
        .map(str::to_string)
        .collect();
    (!members.is_empty() && members.len() == values.len()).then_some(members)
}

fn discover_env_values(env_home: &Path, cwd: &Path) -> Vec<(PathBuf, String, String)> {
    env_source_paths(env_home, cwd)
        .into_iter()
        .flat_map(|path| parse_env_file(&path))
        .collect()
}

pub(super) fn env_source_paths(env_home: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut paths = vec![
        env_home.join(".secrets").join("master.env"),
        env_home.join(".tachi").join("config.env"),
        env_home.join(".sigil").join("config.env"),
    ];
    if let Some(home) = std::env::var_os("TACHI_HOME") {
        paths.push(PathBuf::from(home).join("config.env"));
    }
    paths.extend([
        cwd.join(".tachi").join("config.env"),
        cwd.join(".sigil").join("config.env"),
        cwd.join(".env.credentials"),
        cwd.join(".env"),
        cwd.join("docker").join(".env"),
        cwd.join(".tachi").join("vault.env"),
    ]);
    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    paths
}

pub(super) fn parse_env_file(path: &Path) -> Vec<(PathBuf, String, String)> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_env_content(path, &raw)
}

/// Parse bytes the caller already holds. Values are data and are never executed.
pub(super) fn parse_env_content(path: &Path, raw: &str) -> Vec<(PathBuf, String, String)> {
    raw.lines()
        .filter_map(|raw_line| {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line).trim();
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            let mut value = value.trim();
            if key.is_empty() {
                return None;
            }
            if ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
                && value.len() >= 2
            {
                value = &value[1..value.len() - 1];
            }
            if value.is_empty() && config_prefix(key).is_none() {
                return None;
            }
            Some((path.to_path_buf(), key.to_string(), value.to_string()))
        })
        .collect()
}

fn is_lane_config(name: &str) -> bool {
    ["SUMMARY_", "EXTRACT_", "REASONING_", "DISTILL_"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        && !name.contains("_API_KEY")
}

fn is_config_name(name: &str) -> bool {
    [
        "_BASE_URL",
        "_URL",
        "_MODEL",
        "_BACKEND",
        "_TIMEOUT",
        "_ENABLED",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix))
}

fn render_discovery_human(report: &DiscoveryReport) -> String {
    let mut out = render_notes(&report.notes);
    if report.candidates.is_empty() {
        out.push_str("(no credential candidates discovered)\n");
        return out;
    }
    out.push_str(
        "LOGICAL_NAME\tCLASSIFICATION\tPROVIDER_KIND\tACCOUNT_ID\tKEY_FP\tACCOUNT_FP\tSLOT_BINDS\n",
    );
    for row in &report.candidates {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            row.logical_name,
            row.classification,
            row.provider_kind.as_deref().unwrap_or(""),
            row.account_id.as_deref().unwrap_or(""),
            row.key_fingerprint.as_deref().unwrap_or(""),
            row.account_fingerprint.as_deref().unwrap_or(""),
            row.intended_slot_binds.join(",")
        ));
    }
    out
}

fn render_plan_human(report: &PlanReport) -> String {
    let mut out = render_notes(&report.notes);
    out.push_str(&format!("PLAN\t{}\n", report.plan_digest));
    if report.actions.is_empty() {
        out.push_str("(no provider-account actions)\n");
    }
    for action in &report.actions {
        out.push_str(&format!(
            "ACTION\t{}\t{}\t{}\t{}\t{}\t{}\n",
            action.action,
            action.provider_kind,
            action.account_id,
            action.key_fingerprint,
            action.account_fingerprint,
            action.slot.as_deref().unwrap_or("")
        ));
    }
    out
}

fn render_notes(notes: &[DiscoveryNote]) -> String {
    let mut out = String::new();
    for note in notes {
        out.push_str(&format!(
            "NOTE\t{}\t{}\t{}\n",
            note.code, note.source, note.message
        ));
    }
    out
}

fn write_plan_artifact(
    cwd: &Path,
    report: &PlanReport,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dir = cwd.join(".tachi");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("intake-plan.json");
    std::fs::write(&path, serde_json::to_string_pretty(report)?)?;
    Ok(path)
}

#[cfg(test)]
mod tests;
