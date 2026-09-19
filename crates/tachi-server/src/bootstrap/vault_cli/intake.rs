use crate::provider_config::parse_vault_alias;
use memcore::vault::{SECRET_TYPE_API_KEY, SECRET_TYPE_JSON_BLOB};
use memcore::{FingerprintKey, ProviderAccount};
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

#[derive(Debug)]
struct RawCandidate {
    source_path: PathBuf,
    logical_name: String,
    secret_type: &'static str,
    value: String,
}

#[derive(Debug)]
struct EndpointEvidence {
    source_path: PathBuf,
    prefix: Option<String>,
    provider_kind: String,
}

#[derive(Debug)]
struct ClassifiedCandidate {
    raw: RawCandidate,
    classification: &'static str,
    provider_kind: Option<String>,
    key_fingerprint: Option<String>,
    account_fingerprint: Option<String>,
    account_id: Option<String>,
    planned_action: Option<&'static str>,
    intended_slot_binds: BTreeSet<String>,
}

#[derive(Debug)]
struct AccountInventory {
    accounts: Vec<ProviderAccount>,
    aliases: HashMap<String, HashSet<String>>,
    custody_targets: HashMap<String, String>,
}

impl AccountInventory {
    fn empty() -> Self {
        Self {
            accounts: Vec::new(),
            aliases: HashMap::new(),
            custody_targets: HashMap::new(),
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
            json,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
        } => {
            let (filter, notes) = parse_host_filter(host.as_deref());
            let report = if filter == HostFilter::Unsupported {
                DiscoveryReport {
                    candidates: Vec::new(),
                    notes,
                }
            } else if filter == HostFilter::Codex {
                discover_report(
                    &env_home,
                    &cwd,
                    global_db_path,
                    filter,
                    notes,
                    None,
                )?
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
                discover_report(
                    &env_home,
                    &cwd,
                    global_db_path,
                    filter,
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
                plan_report(
                    &env_home,
                    &cwd,
                    global_db_path,
                    filter,
                    notes,
                    &fp_key,
                )?
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
    candidates: Vec<ClassifiedCandidate>,
    notes: Vec<DiscoveryNote>,
    inventory: &AccountInventory,
) -> PlanReport {
    let mut groups: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if candidate.classification != "known" {
            continue;
        }
        let (Some(kind), Some(account_fp)) = (
            candidate.provider_kind.as_ref(),
            candidate.account_fingerprint.as_ref(),
        ) else {
            continue;
        };
        groups
            .entry((kind.clone(), account_fp.clone()))
            .or_default()
            .push(index);
    }

    let mut actions = Vec::new();
    for ((provider_kind, account_fingerprint), indices) in groups {
        let key_fingerprint = candidates[indices[0]]
            .key_fingerprint
            .clone()
            .expect("known API-key candidates are fingerprinted");
        let account_id = candidates[indices[0]]
            .account_id
            .clone()
            .expect("known API-key candidates resolve an account id");
        let account_action = candidates[indices[0]].planned_action;
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

        let intended_slots: BTreeSet<String> = indices
            .iter()
            .flat_map(|index| candidates[*index].intended_slot_binds.iter().cloned())
            .collect();
        let existing_aliases = inventory.aliases.get(&account_id);
        for slot in intended_slots {
            if existing_aliases.is_some_and(|aliases| aliases.contains(&slot)) {
                continue;
            }
            actions.push(IntakePlanAction {
                action: ACTION_BIND_SLOT.to_string(),
                provider_kind: provider_kind.clone(),
                account_id: account_id.clone(),
                key_fingerprint: key_fingerprint.clone(),
                account_fingerprint: account_fingerprint.clone(),
                slot: Some(slot),
            });
        }
    }

    let digest = plan_digest(&actions);
    let mut candidates: Vec<Candidate> = candidates.into_iter().map(public_candidate).collect();
    candidates.sort_by(|a, b| {
        (&a.source_path, &a.logical_name).cmp(&(&b.source_path, &b.logical_name))
    });
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

    // An invented name can borrow provider identity only from identical bytes
    // already classified by admitted name/endpoint evidence. This comparison
    // stays in-memory and never enters a report.
    for index in 0..candidates.len() {
        if candidates[index].classification != "unknown"
            || candidates[index].raw.secret_type != SECRET_TYPE_API_KEY
        {
            continue;
        }
        let kinds: BTreeSet<String> = candidates
            .iter()
            .filter(|other| {
                other.classification == "known"
                    && other.raw.secret_type == SECRET_TYPE_API_KEY
                    && other.raw.value.trim() == candidates[index].raw.value.trim()
            })
            .filter_map(|other| other.provider_kind.clone())
            .collect();
        if kinds.len() == 1 {
            candidates[index].classification = "known";
            candidates[index].provider_kind = kinds.into_iter().next();
        } else if kinds.len() > 1 {
            candidates[index].classification = "ambiguous";
        }
    }

    for candidate in &mut candidates {
        if candidate.classification != "known" {
            continue;
        }
        let Some(provider_kind) = candidate.provider_kind.as_deref() else {
            continue;
        };
        if let Some(slot) = slot_for_key_name(&candidate.raw.logical_name) {
            candidate.intended_slot_binds.insert(slot.to_string());
        }
        candidate.intended_slot_binds.extend(
            endpoints
                .iter()
                .filter(|evidence| {
                    evidence.source_path == candidate.raw.source_path
                        && evidence.provider_kind == provider_kind
                })
                .filter_map(|evidence| lane_slot_for_prefix(evidence.prefix.as_deref()?))
                .map(str::to_string),
        );
        let Some(fp_key) = fp_key else {
            continue;
        };
        let key_fingerprint = fp_key.key_fingerprint(provider_kind, candidate.raw.value.trim());
        let account_fingerprint =
            fp_key.account_fingerprint_from_members([&key_fingerprint]);
        candidate.key_fingerprint = Some(key_fingerprint);
        candidate.account_fingerprint = Some(account_fingerprint.clone());
        let exact: Vec<&ProviderAccount> = inventory
            .accounts
            .iter()
            .filter(|account| {
                account.provider_kind == provider_kind
                    && account.account_fingerprint == account_fingerprint
            })
            .collect();
        if let [account] = exact.as_slice() {
            candidate.account_id = Some(account.account_id.clone());
            continue;
        }
        if exact.len() > 1 {
            candidate.classification = "ambiguous";
            continue;
        }

        let canonical =
            crate::status_ops::status_health::canonical_account_key_for_provider_kind(
                provider_kind,
            );
        let rotating: Vec<&ProviderAccount> = inventory
            .accounts
            .iter()
            .filter(|account| {
                account.provider_kind == provider_kind
                    && inventory
                        .custody_targets
                        .get(&account.account_id)
                        .map(String::as_str)
                        == canonical
            })
            .collect();
        match rotating.as_slice() {
            [account] => {
                candidate.account_id = Some(account.account_id.clone());
                candidate.planned_action = Some(ACTION_ROTATE_ACCOUNT);
            }
            [] => {
                candidate.account_id = Some(planned_account_id(
                    provider_kind,
                    &account_fingerprint,
                ));
                candidate.planned_action = Some(ACTION_CREATE_ACCOUNT);
            }
            _ => candidate.classification = "ambiguous",
        }
    }
    candidates
}

fn classify_candidate(raw: RawCandidate, endpoints: &[EndpointEvidence]) -> ClassifiedCandidate {
    let mut result = ClassifiedCandidate {
        raw,
        classification: "unknown",
        provider_kind: None,
        key_fingerprint: None,
        account_fingerprint: None,
        account_id: None,
        planned_action: None,
        intended_slot_binds: BTreeSet::new(),
    };
    if result.raw.secret_type != SECRET_TYPE_API_KEY
        || classify_non_key(&result.raw.logical_name, &result.raw.value).is_some()
    {
        return result;
    }

    let name_kind =
        crate::status_ops::status_health::provider_kind_for_env_name(&result.raw.logical_name);
    let prefix = key_prefix(&result.raw.logical_name);
    let mut endpoint_kinds: BTreeSet<String> = endpoints
        .iter()
        .filter(|evidence| {
            evidence.source_path == result.raw.source_path
                && evidence.prefix.as_deref() == prefix.as_deref()
        })
        .map(|evidence| evidence.provider_kind.clone())
        .collect();
    if endpoint_kinds.is_empty() {
        endpoint_kinds = endpoints
            .iter()
            .filter(|evidence| evidence.source_path == result.raw.source_path)
            .map(|evidence| evidence.provider_kind.clone())
            .collect();
    }

    match (name_kind, endpoint_kinds.len()) {
        (Some(kind), 0) => {
            result.classification = "known";
            result.provider_kind = Some(kind.to_string());
        }
        (Some(kind), 1) if endpoint_kinds.contains(kind) => {
            result.classification = "known";
            result.provider_kind = Some(kind.to_string());
        }
        (Some(_), _) => result.classification = "conflicted",
        (None, 1) => {
            result.classification = "known";
            result.provider_kind = endpoint_kinds.into_iter().next();
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
            let provider_kind = provider_kind_from_endpoint(&candidate.value)?;
            Some(EndpointEvidence {
                source_path: candidate.source_path.clone(),
                prefix: Some(prefix),
                provider_kind,
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
    let mut aliases = HashMap::new();
    let mut custody_targets = HashMap::new();
    for account in &accounts {
        aliases.insert(
            account.account_id.clone(),
            memcore::db::list_provider_account_aliases(store.connection(), &account.account_id)?
                .into_iter()
                .filter(|alias| !alias.retired)
                .map(|alias| alias.alias_name)
                .collect(),
        );
        if let Some(custody) =
            memcore::db::get_account_custody(store.connection(), &account.account_id)?
        {
            custody_targets.insert(account.account_id.clone(), custody.custody_target);
        }
    }
    Ok(AccountInventory {
        accounts,
        aliases,
        custody_targets,
    })
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
        cwd.join(".env"),
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
            if key.is_empty() || value.is_empty() {
                return None;
            }
            if ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
                && value.len() >= 2
            {
                value = &value[1..value.len() - 1];
            }
            (!value.is_empty()).then(|| (path.to_path_buf(), key.to_string(), value.to_string()))
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
    out.push_str("LOGICAL_NAME\tCLASSIFICATION\tPROVIDER_KIND\tACCOUNT_ID\tKEY_FP\tACCOUNT_FP\tSLOT_BINDS\n");
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
mod tests {
    use super::*;

    const MASTER: [u8; 32] = [0x42; 32];

    fn write_file(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        std::fs::write(path, contents).expect("write fixture");
    }

    fn plan(home: &Path, cwd: &Path, db_path: &Path) -> PlanReport {
        let fp_key = FingerprintKey::derive_from_master_key(&MASTER);
        plan_report(
            home,
            cwd,
            db_path,
            HostFilter::Env,
            Vec::new(),
            &fp_key,
        )
        .expect("plan")
    }

    #[test]
    fn deepseek_endpoint_creates_one_account_and_binds_lane_aliases() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        let secret = "sk-fixture-deepseek-official";
        write_file(
            &cwd.path().join(".env"),
            &format!(
                "DISTILL_API_KEY2={secret}\nEXTRACT_API_KEY2={secret}\nDISTILL_BASE_URL=https://api.deepseek.com/chat/completions\nEXTRACT_BASE_URL=https://api.deepseek.com/chat/completions\n"
            ),
        );

        let report = plan(
            home.path(),
            cwd.path(),
            &home.path().join(".tachi/global/tachi-global.db"),
        );

        assert_eq!(
            report
                .actions
                .iter()
                .filter(|action| action.action == ACTION_CREATE_ACCOUNT)
                .count(),
            1,
            "{report:#?}"
        );
        let slots: Vec<&str> = report
            .actions
            .iter()
            .filter(|action| action.action == ACTION_BIND_SLOT)
            .filter_map(|action| action.slot.as_deref())
            .collect();
        assert_eq!(slots, vec!["DISTILL_API_KEY", "EXTRACT_API_KEY"]);
        assert!(report
            .actions
            .iter()
            .all(|action| action.provider_kind == "deepseek"));
        let encoded = serde_json::to_string(&report).expect("encode");
        assert!(!encoded.contains(secret));
        let encoded_actions = serde_json::to_string(&report.actions).expect("encode actions");
        assert!(!encoded_actions.contains("DISTILL_API_KEY2"));
        assert!(!encoded_actions.contains("EXTRACT_API_KEY2"));
        assert!(encoded.contains("fp1:"));
        assert!(encoded.contains("fpa1:"));
    }

    #[test]
    fn same_bytes_under_two_lane_names_make_one_account_and_stable_digest() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        write_file(
            &cwd.path().join(".env"),
            "DISTILL_API_KEY2=same-fixture\nEXTRACT_API_KEY2=same-fixture\nDISTILL_BASE_URL=api.deepseek.com\nEXTRACT_BASE_URL=api.deepseek.com\n",
        );

        let first = plan(
            home.path(),
            cwd.path(),
            &home.path().join(".tachi/global/tachi-global.db"),
        );
        let second = plan(
            home.path(),
            cwd.path(),
            &home.path().join(".tachi/global/tachi-global.db"),
        );

        assert_eq!(first.plan_digest, second.plan_digest);
        assert_eq!(
            first
                .actions
                .iter()
                .filter(|action| action.action == ACTION_CREATE_ACCOUNT)
                .count(),
            1
        );
        assert_eq!(
            first
                .actions
                .iter()
                .filter(|action| action.action == ACTION_BIND_SLOT)
                .count(),
            2
        );
    }

    #[test]
    fn existing_fingerprint_binds_without_creating_another_account() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        let db_path = home.path().join(".tachi/global/tachi-global.db");
        std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
        let store = memcore::MemoryStore::open(db_path.to_str().expect("utf8")).expect("store");
        let fp_key = FingerprintKey::derive_from_master_key(&MASTER);
        let member = fp_key.key_fingerprint("deepseek", "same-fixture");
        let account_fp = fp_key.account_fingerprint_from_members([member]);
        memcore::db::insert_provider_account(
            store.connection(),
            &memcore::NewProviderAccount::api_key_pool(
                "account-existing",
                "deepseek",
                "va1:fixture",
                account_fp,
                memcore::AccountClass::ModelApi,
            ),
        )
        .expect("account");
        drop(store);
        write_file(
            &cwd.path().join(".env"),
            "DISTILL_API_KEY2=same-fixture\nDISTILL_BASE_URL=api.deepseek.com\n",
        );

        let report = plan(home.path(), cwd.path(), &db_path);

        assert!(report
            .actions
            .iter()
            .all(|action| action.action != ACTION_CREATE_ACCOUNT));
        assert_eq!(report.actions.len(), 1, "{report:#?}");
        assert_eq!(report.actions[0].action, ACTION_BIND_SLOT);
        assert_eq!(report.actions[0].account_id, "account-existing");
    }

    #[test]
    fn changed_fingerprint_for_canonical_custody_rotates_existing_account() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        let db_path = home.path().join(".tachi/global/tachi-global.db");
        std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
        let store = memcore::MemoryStore::open(db_path.to_str().expect("utf8")).expect("store");
        let fp_key = FingerprintKey::derive_from_master_key(&MASTER);
        let old_member = fp_key.key_fingerprint("deepseek", "old-fixture");
        let old_account_fp = fp_key.account_fingerprint_from_members([old_member]);
        memcore::db::insert_provider_account(
            store.connection(),
            &memcore::NewProviderAccount::api_key_pool(
                "account-rotating",
                "deepseek",
                "va1:rotating-fixture",
                old_account_fp,
                memcore::AccountClass::ModelApi,
            ),
        )
        .expect("account");
        memcore::db::insert_account_custody(
            store.connection(),
            "va1:rotating-fixture",
            "account-rotating",
            memcore::CustodyKind::VaultEntry,
            "DEEPSEEK_API_KEY",
        )
        .expect("custody");
        drop(store);
        write_file(
            &cwd.path().join(".env"),
            "DEEPSEEK_API_KEY=new-fixture\n",
        );

        let report = plan(home.path(), cwd.path(), &db_path);

        assert_eq!(report.actions.len(), 1, "{report:#?}");
        assert_eq!(report.actions[0].action, ACTION_ROTATE_ACCOUNT);
        assert_eq!(report.actions[0].account_id, "account-rotating");
    }

    #[test]
    fn official_provider_hosts_remain_distinct() {
        assert_eq!(
            provider_kind_from_endpoint("api.deepseek.com").as_deref(),
            Some("deepseek")
        );
        assert_eq!(
            provider_kind_from_endpoint("https://api.siliconflow.cn/v1").as_deref(),
            Some("siliconflow")
        );
        assert_eq!(
            provider_kind_from_endpoint("https://api.z.ai/api/paas/v4").as_deref(),
            Some("zai")
        );
        assert_eq!(
            crate::status_ops::status_health::provider_kind_for_env_name("ZHIPUAI_API_KEY"),
            Some("zhipuai")
        );
    }

    #[test]
    fn identical_bytes_for_official_deepseek_siliconflow_and_glm_stay_distinct() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        write_file(
            &cwd.path().join(".env"),
            "DEEPSEEK_API_KEY=same-fixture\nDEEPSEEK_BASE_URL=api.deepseek.com\n\
             SILICONFLOW_API_KEY=same-fixture\nSILICONFLOW_BASE_URL=api.siliconflow.cn\n\
             ZAI_API_KEY=same-fixture\nZAI_BASE_URL=api.z.ai\n",
        );

        let report = plan(
            home.path(),
            cwd.path(),
            &home.path().join(".tachi/global/tachi-global.db"),
        );

        let kinds: BTreeSet<&str> = report
            .actions
            .iter()
            .filter(|action| action.action == ACTION_CREATE_ACCOUNT)
            .map(|action| action.provider_kind.as_str())
            .collect();
        assert_eq!(kinds, BTreeSet::from(["deepseek", "siliconflow", "zai"]));
    }

    #[test]
    fn env_name_endpoint_mismatch_is_conflicted_and_plans_nothing() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        write_file(
            &cwd.path().join(".env"),
            "DEEPSEEK_API_KEY=fixture\nDEEPSEEK_BASE_URL=https://api.siliconflow.cn/v1\n",
        );

        let report = plan(
            home.path(),
            cwd.path(),
            &home.path().join(".tachi/global/tachi-global.db"),
        );

        assert!(report.actions.is_empty());
        let candidate = report
            .candidates
            .iter()
            .find(|candidate| candidate.logical_name == "DEEPSEEK_API_KEY")
            .expect("candidate");
        assert_eq!(candidate.classification, "conflicted");
    }

    #[test]
    fn unsupported_host_does_not_scan_env_or_codex_auth() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        write_file(&cwd.path().join(".env"), "DEEPSEEK_API_KEY=fixture\n");
        write_file(
            &home.path().join(".codex/auth.json"),
            r#"{"tokens":{"access":"must-not-be-read"}}"#,
        );

        let (filter, notes) = parse_host_filter(Some("openclaw"));
        assert_eq!(filter, HostFilter::Unsupported);
        let discovery = discover_report(
            home.path(),
            cwd.path(),
            &home.path().join("missing.db"),
            filter,
            notes,
            None,
        )
        .expect("unsupported discovery");
        let report = build_plan(discovery);

        assert!(report.candidates.is_empty());
        assert!(report.actions.is_empty());
        assert_eq!(report.notes[0].code, "unsupported_source");
    }

    #[test]
    fn env_parser_treats_command_substitution_as_data() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join(".env");
        let marker = dir.path().join("must-not-exist");
        let value = format!("$(touch {})", marker.display());
        let parsed = parse_env_content(&path, &format!("DEEPSEEK_API_KEY={value}\n"));
        assert_eq!(parsed[0].2, value);
        assert!(!marker.exists());
    }
}
