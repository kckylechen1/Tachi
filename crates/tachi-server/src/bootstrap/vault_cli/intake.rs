use crate::provider_config::parse_vault_alias;
use memcore::vault::{SECRET_TYPE_API_KEY, SECRET_TYPE_JSON_BLOB};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tachi_bootstrap::cli::{VaultAction, VaultIntakeAction};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Candidate {
    pub source_path: String,
    pub logical_name: String,
    pub secret_type: String,
    pub fingerprint: String,
    pub in_vault: bool,
    pub classification: String,
    pub suggested_action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias_family: Option<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostFilter {
    All,
    Env,
    Codex,
    Unsupported,
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
        VaultIntakeAction::Discover { host, json } => {
            let report = discover_report(&env_home, &cwd, global_db_path, host.as_deref());
            if json {
                println!("{}", render_json(&report)?);
            } else {
                print!("{}", render_human(&report));
            }
        }
        VaultIntakeAction::Plan { host, json, write } => {
            let report = plan_report(&env_home, &cwd, global_db_path, host.as_deref());
            if json {
                println!("{}", render_plan_json(&report)?);
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

// Only unit tests in this module call the thin wrapper; production uses
// `discover_report` / CLI entrypoints directly.
//
// Path resolution here mirrors `path_utils::plan_c_global_db_path_existing`:
// prefer the canonical (post-#1132) filename, but fall back to the legacy
// name so fixtures seeded via `seed_vault_entry` (which write the vault DB
// directly with `MemoryStore::open`, and so never pass through the
// canonical-only rename-on-open seam) still resolve to the DB they actually
// created on disk.
#[cfg(test)]
pub(crate) fn discover_candidates(env_home: &Path, cwd: &Path) -> Vec<Candidate> {
    let global_dir = env_home.join(".tachi").join("global");
    let canonical = global_dir.join(memcore::MEMORY_DB_FILENAME);
    let global_db_path = if canonical.exists() {
        canonical
    } else {
        global_dir.join(memcore::LEGACY_MEMORY_DB_FILENAME)
    };
    discover_report(env_home, cwd, &global_db_path, None).candidates
}

fn discover_report(
    env_home: &Path,
    cwd: &Path,
    global_db_path: &Path,
    host: Option<&str>,
) -> DiscoveryReport {
    let (filter, notes) = parse_host_filter(host);
    if filter == HostFilter::Unsupported {
        return DiscoveryReport {
            candidates: Vec::new(),
            notes,
        };
    }
    let vault_names = vault_entry_names(global_db_path);
    let mut candidates = Vec::new();

    if matches!(filter, HostFilter::All | HostFilter::Env) {
        for (path, key, value) in discover_env_values(env_home, cwd) {
            candidates.push(candidate_from_value(
                &path,
                &key,
                SECRET_TYPE_API_KEY,
                &value,
                &vault_names,
            ));
        }
    }

    if matches!(filter, HostFilter::All | HostFilter::Codex) {
        let path = env_home.join(".codex").join("auth.json");
        if let Ok(value) = std::fs::read_to_string(&path) {
            if !value.trim().is_empty() {
                candidates.push(candidate_from_value(
                    &path,
                    "codex.auth",
                    SECRET_TYPE_JSON_BLOB,
                    &value,
                    &vault_names,
                ));
            }
        }
    }

    DiscoveryReport { candidates, notes }
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

fn discover_env_values(env_home: &Path, cwd: &Path) -> Vec<(PathBuf, String, String)> {
    env_source_paths(env_home, cwd)
        .into_iter()
        .flat_map(|path| parse_env_file(&path))
        .collect()
}

fn env_source_paths(env_home: &Path, cwd: &Path) -> Vec<PathBuf> {
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

fn parse_env_file(path: &Path) -> Vec<(PathBuf, String, String)> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
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
            if value.is_empty() {
                return None;
            }
            Some((path.to_path_buf(), key.to_string(), value.to_string()))
        })
        .collect()
}

fn vault_entry_names(global_db_path: &Path) -> HashSet<String> {
    let Some(path) = global_db_path.to_str() else {
        return HashSet::new();
    };
    if !global_db_path.exists() {
        return HashSet::new();
    }
    let Ok(store) = memcore::MemoryStore::open_read_only(path) else {
        return HashSet::new();
    };
    store
        .vault_list_entries()
        .unwrap_or_default()
        .into_iter()
        .map(|entry| entry.name)
        .collect()
}

fn candidate_from_value(
    source_path: &Path,
    logical_name: &str,
    secret_type: &str,
    value: &str,
    vault_names: &HashSet<String>,
) -> Candidate {
    let in_vault = vault_names.contains(logical_name);
    let classification = classify(logical_name, value);
    let suggested_action = suggested_action(&classification, in_vault);
    Candidate {
        source_path: source_path.to_string_lossy().to_string(),
        logical_name: logical_name.to_string(),
        secret_type: secret_type.to_string(),
        fingerprint: fingerprint(value),
        in_vault,
        classification,
        suggested_action,
        alias_family: alias_family(logical_name).map(str::to_string),
    }
}

fn classify(logical_name: &str, value: &str) -> String {
    if parse_vault_alias(value).is_some() {
        return "vault_reference".to_string();
    }
    if is_lane_config(logical_name) {
        return "lane_config".to_string();
    }
    if logical_name.contains("LONGPORT") || logical_name.contains("LONGBRIDGE") {
        return "review".to_string();
    }
    "api_key".to_string()
}

fn suggested_action(classification: &str, in_vault: bool) -> String {
    match classification {
        "vault_reference" => "vault_reference",
        "lane_config" => "lane_config",
        "review" => "review",
        _ if in_vault => "already_present",
        _ => "import_new",
    }
    .to_string()
}

fn is_lane_config(name: &str) -> bool {
    let has_lane_prefix = ["SUMMARY_", "EXTRACT_", "REASONING_", "DISTILL_"]
        .iter()
        .any(|prefix| name.starts_with(prefix));
    has_lane_prefix && !name.ends_with("_API_KEY")
}

fn alias_family(name: &str) -> Option<&'static str> {
    match name {
        "KIMI_API_KEY" | "MOONSHOT_API_KEY" => Some("moonshot/kimi"),
        "GOOGLE_API_KEY" | "GEMINI_API_KEY" | "GOOGLE_SEARCH_API_KEY" => Some("google/gemini"),
        _ => None,
    }
}

fn fingerprint(value: &str) -> String {
    // This is a stable dedupe hint for one raw candidate value, not a security
    // control. Use dependency-free FNV-1a so output is stable across processes
    // and platforms without adding a crypto crate for this read-only slice.
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in value.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let full = format!("{hash:016x}");
    full[..8].to_string()
}

fn render_json(report: &DiscoveryReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

fn render_human(report: &DiscoveryReport) -> String {
    let mut out = String::new();
    for note in &report.notes {
        out.push_str(&format!(
            "NOTE\t{}\t{}\t{}\n",
            note.code, note.source, note.message
        ));
    }
    if report.candidates.is_empty() {
        out.push_str("(no credential candidates discovered)\n");
        return out;
    }
    out.push_str(&format!(
        "{:<36} {:<24} {:<10} {:<11} {:<8} {:<16} {:<16} {}\n",
        "SOURCE",
        "LOGICAL_NAME",
        "TYPE",
        "FINGERPRINT",
        "IN_VAULT",
        "CLASSIFICATION",
        "ACTION",
        "ALIAS_FAMILY"
    ));
    for row in &report.candidates {
        out.push_str(&format!(
            "{:<36} {:<24} {:<10} {:<11} {:<8} {:<16} {:<16} {}\n",
            row.source_path,
            row.logical_name,
            row.secret_type,
            row.fingerprint,
            row.in_vault,
            row.classification,
            row.suggested_action,
            row.alias_family.as_deref().unwrap_or("")
        ));
    }
    out
}

// ─── Intake planner (read-only) ────────────────────────────────────────────────
// Assigns the design-doc action enum to discovered candidates without unlocking
// the Vault or reading any secret value. Fingerprint dedup is among discovered
// plaintext candidates only; Vault matching stays metadata/name-based so the
// planner is safe while the Vault is locked. `replace_stale` and `promote_to_pool`
// from the doc enum require reading/comparing Vault secret VALUES (only possible
// once unlocked) and belong to the later `apply` slice — they are intentionally
// not emitted by this locked-safe planner.

const ACTION_SKIP: &str = "skip_existing_same_fingerprint";
const ACTION_IMPORT_NEW: &str = "import_new";
const ACTION_MERGE_ALIAS: &str = "merge_alias";
const ACTION_MARK_AUTH_FAILED: &str = "mark_auth_failed";
const ACTION_REMOVE_ORPHAN: &str = "remove_or_archive_orphan";
const ACTION_LANE_CONFIG: &str = "lane_config";
const ACTION_VAULT_REFERENCE: &str = "vault_reference";
const ACTION_REVIEW: &str = "review";

#[derive(Debug, Clone, Serialize)]
struct PlannedCandidate {
    #[serde(flatten)]
    candidate: Candidate,
    action: String,
    rationale: String,
}

#[derive(Debug, Clone, Serialize)]
struct PlanOrphan {
    name: String,
    action: String,
    rationale: String,
}

#[derive(Debug, Clone, Serialize)]
struct PlanReport {
    candidates: Vec<PlannedCandidate>,
    notes: Vec<DiscoveryNote>,
    orphans: Vec<PlanOrphan>,
}

fn plan_report(
    env_home: &Path,
    cwd: &Path,
    global_db_path: &Path,
    host: Option<&str>,
) -> PlanReport {
    let discovery = discover_report(env_home, cwd, global_db_path, host);
    let vault_names = vault_entry_names(global_db_path);
    let auth_failed = vault_auth_failed_names(global_db_path);

    // Alias families among discovered candidates, keyed to the distinct
    // fingerprints seen for each family. A family holding two or more distinct
    // fingerprints is an advisory merge candidate — never an automatic merge.
    let mut family_fingerprints: HashMap<String, HashSet<String>> = HashMap::new();
    for candidate in &discovery.candidates {
        if let Some(family) = &candidate.alias_family {
            family_fingerprints
                .entry(family.clone())
                .or_default()
                .insert(candidate.fingerprint.clone());
        }
    }
    // Alias families represented by existing Vault entry names (metadata only).
    let vault_families: HashSet<String> = vault_names
        .iter()
        .filter_map(|name| alias_family(name))
        .map(str::to_string)
        .collect();

    let mut seen_fingerprints: HashSet<String> = HashSet::new();
    let mut planned = Vec::new();
    for candidate in &discovery.candidates {
        let (action, rationale) = plan_action(
            candidate,
            &mut seen_fingerprints,
            &auth_failed,
            &family_fingerprints,
            &vault_families,
        );
        planned.push(PlannedCandidate {
            candidate: candidate.clone(),
            action: action.to_string(),
            rationale,
        });
    }

    let discovered_names: HashSet<&str> = discovery
        .candidates
        .iter()
        .map(|candidate| candidate.logical_name.as_str())
        .collect();
    let mut orphan_names: Vec<&String> = vault_names
        .iter()
        .filter(|name| !discovered_names.contains(name.as_str()))
        .filter(|name| !is_lane_config(name))
        .collect();
    orphan_names.sort();
    let orphans = orphan_names
        .into_iter()
        .map(|name| {
            if auth_failed.contains(name) {
                PlanOrphan {
                    name: name.clone(),
                    action: ACTION_MARK_AUTH_FAILED.to_string(),
                    rationale: "vault key-health marks this orphan entry auth_failed".to_string(),
                }
            } else {
                PlanOrphan {
                    name: name.clone(),
                    action: ACTION_REMOVE_ORPHAN.to_string(),
                    rationale: "vault entry with no discovered source".to_string(),
                }
            }
        })
        .collect();

    PlanReport {
        candidates: planned,
        notes: discovery.notes,
        orphans,
    }
}

fn plan_action(
    candidate: &Candidate,
    seen_fingerprints: &mut HashSet<String>,
    auth_failed: &HashSet<String>,
    family_fingerprints: &HashMap<String, HashSet<String>>,
    vault_families: &HashSet<String>,
) -> (&'static str, String) {
    if candidate.classification == "lane_config" {
        return (
            ACTION_LANE_CONFIG,
            "lane configuration, not an independent API key".to_string(),
        );
    }
    if candidate.classification == "vault_reference" {
        return (
            ACTION_VAULT_REFERENCE,
            "vault reference indirection, not a raw secret to import".to_string(),
        );
    }
    if candidate.classification == "review" {
        // e.g. LongPort/LongBridge broker credentials — flagged for human review
        // by discover; preserve that signal instead of silently importing them
        // as if they were an LLM provider key.
        return (
            ACTION_REVIEW,
            "flagged for human review (e.g. broker/data credential, not an LLM key)".to_string(),
        );
    }
    // Fingerprint dedup among discovered plaintext candidates only.
    if !seen_fingerprints.insert(candidate.fingerprint.clone()) {
        return (
            ACTION_SKIP,
            "duplicate fingerprint of an already-planned candidate".to_string(),
        );
    }
    if candidate.in_vault && auth_failed.contains(&candidate.logical_name) {
        return (
            ACTION_MARK_AUTH_FAILED,
            "vault key-health marks this entry auth_failed".to_string(),
        );
    }
    if alias_conflict(candidate, family_fingerprints, vault_families) {
        return (
            ACTION_MERGE_ALIAS,
            "shares an alias family with a different-fingerprint entry (advisory; never auto-merged)"
                .to_string(),
        );
    }
    if candidate.in_vault {
        // Name matches a Vault entry. The Vault stays locked, so the value is
        // never read; the locked-safe verdict is to treat it as already present.
        return (
            ACTION_SKIP,
            "logical name already present in vault (metadata match; value not read while locked)"
                .to_string(),
        );
    }
    (
        ACTION_IMPORT_NEW,
        "new credential not present in vault".to_string(),
    )
}

fn alias_conflict(
    candidate: &Candidate,
    family_fingerprints: &HashMap<String, HashSet<String>>,
    vault_families: &HashSet<String>,
) -> bool {
    let Some(family) = &candidate.alias_family else {
        return false;
    };
    let multiple_discovered = family_fingerprints
        .get(family)
        .is_some_and(|fingerprints| fingerprints.len() >= 2);
    let vault_family_conflict = vault_families.contains(family) && !candidate.in_vault;
    multiple_discovered || vault_family_conflict
}

fn vault_auth_failed_names(global_db_path: &Path) -> HashSet<String> {
    let Some(path) = global_db_path.to_str() else {
        return HashSet::new();
    };
    if !global_db_path.exists() {
        return HashSet::new();
    }
    let Ok(store) = memcore::MemoryStore::open_read_only(path) else {
        return HashSet::new();
    };
    store
        .vault_list_key_health(None)
        .unwrap_or_default()
        .into_iter()
        .filter(|health| health.auth_failed)
        .map(|health| health.logical_name)
        .collect()
}

fn render_plan_json(report: &PlanReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

fn render_plan_human(report: &PlanReport) -> String {
    let mut out = String::new();
    for note in &report.notes {
        out.push_str(&format!(
            "NOTE\t{}\t{}\t{}\n",
            note.code, note.source, note.message
        ));
    }
    if report.candidates.is_empty() && report.orphans.is_empty() {
        out.push_str("(no intake candidates to plan)\n");
        return out;
    }
    if !report.candidates.is_empty() {
        out.push_str(&format!(
            "{:<24} {:<11} {:<8} {:<32} {}\n",
            "LOGICAL_NAME", "FINGERPRINT", "IN_VAULT", "ACTION", "RATIONALE"
        ));
        for row in &report.candidates {
            out.push_str(&format!(
                "{:<24} {:<11} {:<8} {:<32} {}\n",
                row.candidate.logical_name,
                row.candidate.fingerprint,
                row.candidate.in_vault,
                row.action,
                row.rationale
            ));
        }
    }
    for orphan in &report.orphans {
        out.push_str(&format!(
            "ORPHAN\t{}\t{}\t{}\n",
            orphan.name, orphan.action, orphan.rationale
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
    std::fs::write(&path, render_plan_json(report)?)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use memcore::vault::{VaultEntry, VaultKeyHealth};

    fn write_file(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        std::fs::write(path, contents).expect("write fixture");
    }

    fn candidate<'a>(rows: &'a [Candidate], name: &str) -> &'a Candidate {
        rows.iter()
            .find(|row| row.logical_name == name)
            .unwrap_or_else(|| panic!("missing candidate {name}: {rows:#?}"))
    }

    fn seed_vault_entry(env_home: &Path, name: &str) -> PathBuf {
        let db_path = env_home.join(".tachi").join("global").join("memory.db");
        std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("create db parent");
        let store =
            memcore::MemoryStore::open(db_path.to_str().expect("utf8 db")).expect("open db");
        store
            .vault_upsert_entry(&VaultEntry {
                name: name.to_string(),
                encrypted_value: "encrypted-fixture".to_string(),
                nonce: "nonce-fixture".to_string(),
                secret_type: SECRET_TYPE_API_KEY.to_string(),
                description: String::new(),
                allowed_agents: None,
                created_at: "2026-07-03T00:00:00Z".to_string(),
                updated_at: "2026-07-03T00:00:00Z".to_string(),
                accessed_at: "2026-07-03T00:00:00Z".to_string(),
                access_count: 0,
            })
            .expect("seed entry");
        db_path
    }

    fn seed_key_health(env_home: &Path, logical_name: &str, auth_failed: bool) {
        let db_path = env_home.join(".tachi").join("global").join("memory.db");
        std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("create db parent");
        let store =
            memcore::MemoryStore::open(db_path.to_str().expect("utf8 db")).expect("open db");
        store
            .vault_upsert_key_health(&VaultKeyHealth {
                logical_name: logical_name.to_string(),
                key_id: format!("{logical_name}_1"),
                status: if auth_failed { "auth_failed" } else { "ok" }.to_string(),
                auth_failed,
                ..VaultKeyHealth::default()
            })
            .expect("seed key health");
    }

    fn planned<'a>(report: &'a PlanReport, name: &str) -> &'a PlannedCandidate {
        report
            .candidates
            .iter()
            .find(|row| row.candidate.logical_name == name)
            .unwrap_or_else(|| panic!("missing planned candidate {name}: {report:#?}"))
    }

    #[test]
    fn vault_intake_g_a1_plan_is_read_only() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        let db_path = seed_vault_entry(home.path(), "OPENAI_API_KEY");
        let fixture_path = cwd.path().join(".env");
        write_file(&fixture_path, "OPENAI_API_KEY=fixture\n");
        let fixture_before = std::fs::read(&fixture_path).expect("read fixture before");
        let entries_before = memcore::MemoryStore::open_read_only(db_path.to_str().unwrap())
            .expect("read db before")
            .vault_list_entries()
            .expect("entries before")
            .len();

        let report = plan_report(home.path(), cwd.path(), &db_path, None);

        let entries_after = memcore::MemoryStore::open_read_only(db_path.to_str().unwrap())
            .expect("read db after")
            .vault_list_entries()
            .expect("entries after")
            .len();
        assert_eq!(entries_before, entries_after);
        assert_eq!(
            fixture_before,
            std::fs::read(&fixture_path).expect("read fixture after")
        );
        // Default plan performs no writes: no artifact is produced.
        assert!(!cwd.path().join(".tachi").join("intake-plan.json").exists());
        assert!(!report.candidates.is_empty());
    }

    #[test]
    fn vault_intake_g_a2_fingerprint_dedup_keeps_one_actionable() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        write_file(
            &home.path().join(".secrets").join("master.env"),
            "OPENAI_API_KEY=samevalue\n",
        );
        write_file(&cwd.path().join(".env"), "OPENAI_API_KEY=samevalue\n");

        let report = plan_report(
            home.path(),
            cwd.path(),
            &home.path().join(".tachi/global/memory.db"),
            None,
        );
        let openai: Vec<_> = report
            .candidates
            .iter()
            .filter(|row| row.candidate.logical_name == "OPENAI_API_KEY")
            .collect();
        assert_eq!(
            openai.len(),
            2,
            "expected two discovered sources: {report:#?}"
        );
        let skips = openai
            .iter()
            .filter(|row| row.action == "skip_existing_same_fingerprint")
            .count();
        let actionable = openai
            .iter()
            .filter(|row| row.action != "skip_existing_same_fingerprint")
            .count();
        assert_eq!(skips, 1);
        assert_eq!(actionable, 1);
    }

    #[test]
    fn vault_intake_g_a3_alias_family_flagged_merge_not_collapsed() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        write_file(
            &cwd.path().join(".env"),
            "KIMI_API_KEY=kimi-value\nMOONSHOT_API_KEY=moonshot-value\n",
        );

        let report = plan_report(
            home.path(),
            cwd.path(),
            &home.path().join(".tachi/global/memory.db"),
            None,
        );
        let kimi = planned(&report, "KIMI_API_KEY");
        let moonshot = planned(&report, "MOONSHOT_API_KEY");
        assert_eq!(kimi.action, "merge_alias");
        assert_eq!(moonshot.action, "merge_alias");
        assert_ne!(kimi.candidate.fingerprint, moonshot.candidate.fingerprint);
        // Never collapsed: both remain distinct rows in the plan.
        let family_rows = report
            .candidates
            .iter()
            .filter(|row| row.candidate.alias_family.as_deref() == Some("moonshot/kimi"))
            .count();
        assert_eq!(family_rows, 2);
    }

    #[test]
    fn vault_intake_g_a4_lane_config_not_import_new() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        write_file(&cwd.path().join(".env"), "DISTILL_MODEL=qwen\n");

        let report = plan_report(
            home.path(),
            cwd.path(),
            &home.path().join(".tachi/global/memory.db"),
            None,
        );
        let distill = planned(&report, "DISTILL_MODEL");
        assert_ne!(distill.action, "import_new");
        assert_eq!(distill.action, "lane_config");
    }

    #[test]
    fn vault_intake_g_a5_auth_failed_entry_marked() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        seed_vault_entry(home.path(), "DEEPSEEK_API_KEY");
        seed_key_health(home.path(), "DEEPSEEK_API_KEY", true);
        write_file(&cwd.path().join(".env"), "DEEPSEEK_API_KEY=fixture\n");

        let report = plan_report(
            home.path(),
            cwd.path(),
            &home.path().join(".tachi/global/memory.db"),
            None,
        );
        let deepseek = planned(&report, "DEEPSEEK_API_KEY");
        assert!(deepseek.candidate.in_vault);
        assert_eq!(deepseek.action, "mark_auth_failed");
    }

    #[test]
    fn vault_intake_g_a6_orphan_marked_remove_or_archive() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        seed_vault_entry(home.path(), "LEGACY_API_KEY");

        let report = plan_report(
            home.path(),
            cwd.path(),
            &home.path().join(".tachi/global/memory.db"),
            None,
        );
        let orphan = report
            .orphans
            .iter()
            .find(|orphan| orphan.name == "LEGACY_API_KEY")
            .expect("orphan present");
        assert_eq!(orphan.action, "remove_or_archive_orphan");
        assert!(report
            .candidates
            .iter()
            .all(|row| row.candidate.logical_name != "LEGACY_API_KEY"));
    }

    #[test]
    fn vault_intake_g_a7_no_secret_leak_in_plan_output() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        let secret = "sk-plan-SECRET-789"; // gitleaks:allow — fixture value, not a real key
        write_file(
            &home.path().join(".secrets").join("master.env"),
            &format!("OPENAI_API_KEY={secret}\n"),
        );

        let report = plan_report(
            home.path(),
            cwd.path(),
            &home.path().join(".tachi/global/memory.db"),
            None,
        );
        let json = render_plan_json(&report).expect("json");
        let human = render_plan_human(&report);
        assert!(json.contains("OPENAI_API_KEY"));
        assert!(human.contains("OPENAI_API_KEY"));
        assert!(!json.contains(secret), "plan JSON leaked secret: {json}");
        assert!(
            !human.contains(secret),
            "plan human output leaked secret: {human}"
        );
    }

    #[test]
    fn vault_intake_g_a8_name_alone_never_merged() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        write_file(
            &cwd.path().join(".env"),
            "KIMI_API_KEY=one-value\nMOONSHOT_API_KEY=another-value\n",
        );

        let report = plan_report(
            home.path(),
            cwd.path(),
            &home.path().join(".tachi/global/memory.db"),
            None,
        );
        let family_rows: Vec<_> = report
            .candidates
            .iter()
            .filter(|row| row.candidate.alias_family.as_deref() == Some("moonshot/kimi"))
            .collect();
        // Differ by fingerprint -> two independent rows, only advisory-flagged.
        assert_eq!(family_rows.len(), 2);
        assert!(family_rows.iter().all(|row| row.action == "merge_alias"));
        assert_ne!(
            family_rows[0].candidate.fingerprint,
            family_rows[1].candidate.fingerprint
        );
    }

    #[test]
    fn vault_intake_g_a9_new_key_is_import_new() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        write_file(&cwd.path().join(".env"), "ANTHROPIC_API_KEY=fresh-value\n");

        let report = plan_report(
            home.path(),
            cwd.path(),
            &home.path().join(".tachi/global/memory.db"),
            None,
        );
        let anthropic = planned(&report, "ANTHROPIC_API_KEY");
        assert!(!anthropic.candidate.in_vault);
        assert_eq!(anthropic.action, "import_new");
    }

    #[test]
    fn vault_intake_g_a10_review_credential_not_silently_import_new() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        write_file(&cwd.path().join(".env"), "LONGPORT_API_KEY=broker-value\n");

        let report = plan_report(
            home.path(),
            cwd.path(),
            &home.path().join(".tachi/global/memory.db"),
            None,
        );
        let longport = planned(&report, "LONGPORT_API_KEY");
        // Broker/data credential flagged for review by discover must NOT be
        // downgraded to import_new — the human-review signal is preserved.
        assert_eq!(longport.candidate.classification, "review");
        assert_eq!(longport.action, "review");
    }

    #[test]
    fn vault_intake_gv1_no_secret_leakage_in_json_or_human() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        let secret = "sk-fixture-SECRET-123"; // gitleaks:allow — fixture value, not a real key
        write_file(
            &home.path().join(".secrets").join("master.env"),
            &format!("OPENAI_API_KEY={secret}\n"),
        );

        let report = discover_report(
            home.path(),
            cwd.path(),
            &home.path().join(".tachi/global/memory.db"),
            None,
        );
        let row = candidate(&report.candidates, "OPENAI_API_KEY");
        assert_eq!(row.fingerprint.len(), 8);
        assert!(row.fingerprint.chars().all(|ch| ch.is_ascii_hexdigit()));
        let json = render_json(&report).expect("json");
        let human = render_human(&report);

        assert!(json.contains("OPENAI_API_KEY"));
        assert!(human.contains("OPENAI_API_KEY"));
        assert!(!json.contains(secret), "JSON leaked secret: {json}");
        assert!(
            !human.contains(secret),
            "human output leaked secret: {human}"
        );
    }

    #[test]
    fn vault_intake_gv2_fingerprint_stable_and_8_hex() {
        let left = fingerprint("same-secret");
        let right = fingerprint("same-secret");

        assert_eq!(left, right);
        assert_eq!(left, "df908812");
        assert_eq!(left.len(), 8);
        assert!(left.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_eq!(left, left.to_ascii_lowercase());
    }

    #[test]
    fn vault_intake_strips_env_value_quotes_before_fingerprinting() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        write_file(
            &cwd.path().join(".env"),
            "DOUBLE_QUOTED=\"same-secret\"\nSINGLE_QUOTED='same-secret'\nUNQUOTED=same-secret\n",
        );

        let rows = discover_candidates(home.path(), cwd.path());
        let unquoted = candidate(&rows, "UNQUOTED").fingerprint.clone();

        assert_eq!(candidate(&rows, "DOUBLE_QUOTED").fingerprint, unquoted);
        assert_eq!(candidate(&rows, "SINGLE_QUOTED").fingerprint, unquoted);
    }

    #[test]
    fn vault_intake_deduplicates_overlapping_source_paths() {
        let home = tempfile::tempdir().expect("home");
        let paths = env_source_paths(home.path(), home.path());
        let unique = paths.iter().collect::<HashSet<_>>();

        assert_eq!(paths.len(), unique.len(), "duplicate paths: {paths:#?}");
    }

    #[test]
    fn vault_intake_gv3_lane_config_vs_key() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        write_file(
            &cwd.path().join(".env"),
            "SUMMARY_MODEL=qwen\nEXTRACT_API_KEY=fake\n",
        );

        let rows = discover_candidates(home.path(), cwd.path());
        let summary = candidate(&rows, "SUMMARY_MODEL");
        assert_eq!(summary.classification, "lane_config");
        assert_eq!(summary.suggested_action, "lane_config");
        assert_eq!(
            candidate(&rows, "EXTRACT_API_KEY").classification,
            "api_key"
        );
    }

    #[test]
    fn vault_intake_gv4_alias_family_is_advisory() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        write_file(&cwd.path().join(".env"), "KIMI_API_KEY=fixture\n");

        let rows = discover_candidates(home.path(), cwd.path());
        let kimi = candidate(&rows, "KIMI_API_KEY");

        assert_eq!(kimi.classification, "api_key");
        assert_eq!(kimi.alias_family.as_deref(), Some("moonshot/kimi"));
    }

    #[test]
    fn vault_intake_gv5_codex_auth_is_redacted_json_blob() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        let raw = r#"{"tokens":{"access":"fixture"}}"#;
        write_file(&home.path().join(".codex").join("auth.json"), raw);

        let report = discover_report(
            home.path(),
            cwd.path(),
            &home.path().join(".tachi/global/memory.db"),
            Some("codex"),
        );
        let row = candidate(&report.candidates, "codex.auth");

        assert_eq!(row.secret_type, "json_blob");
        assert_eq!(row.classification, "api_key");
        let json = render_json(&report).expect("json");
        assert!(!json.contains(raw), "JSON echoed auth blob: {json}");
        assert!(!render_human(&report).contains(raw));
    }

    #[test]
    fn vault_intake_gv6_vault_name_match_uses_metadata_only() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        seed_vault_entry(home.path(), "OPENAI_API_KEY");
        write_file(
            &cwd.path().join(".env"),
            "OPENAI_API_KEY=fixture\nANTHROPIC_API_KEY=fixture\n",
        );

        let rows = discover_candidates(home.path(), cwd.path());
        let openai = candidate(&rows, "OPENAI_API_KEY");
        assert!(openai.in_vault);
        assert_eq!(openai.suggested_action, "already_present");
        let anthropic = candidate(&rows, "ANTHROPIC_API_KEY");
        assert!(!anthropic.in_vault);
        assert_eq!(anthropic.suggested_action, "import_new");
    }

    #[test]
    fn vault_intake_gv7_discover_is_read_only() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        let db_path = seed_vault_entry(home.path(), "OPENAI_API_KEY");
        let fixture_path = cwd.path().join(".env");
        write_file(&fixture_path, "OPENAI_API_KEY=fixture\n");
        let fixture_before = std::fs::read(&fixture_path).expect("read fixture before");
        let entries_before = memcore::MemoryStore::open_read_only(db_path.to_str().unwrap())
            .expect("read db before")
            .vault_list_entries()
            .expect("entries before")
            .len();

        let rows = discover_candidates(home.path(), cwd.path());

        let entries_after = memcore::MemoryStore::open_read_only(db_path.to_str().unwrap())
            .expect("read db after")
            .vault_list_entries()
            .expect("entries after")
            .len();
        assert_eq!(entries_before, entries_after);
        assert_eq!(
            fixture_before,
            std::fs::read(&fixture_path).expect("read fixture after")
        );
        assert!(candidate(&rows, "OPENAI_API_KEY").in_vault);
    }

    #[test]
    fn vault_intake_gv8_vault_reference_is_not_import_new() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        write_file(
            &cwd.path().join(".tachi").join("vault.env"),
            "MY_KEY=vault:SOME_SECRET\n",
        );

        let rows = discover_candidates(home.path(), cwd.path());
        let row = candidate(&rows, "MY_KEY");

        assert_eq!(row.classification, "vault_reference");
        assert_eq!(row.suggested_action, "vault_reference");
        assert_ne!(row.suggested_action, "import_new");
    }

    #[test]
    fn vault_intake_unsupported_host_reports_note_without_scanning_all_sources() {
        let home = tempfile::tempdir().expect("home");
        let cwd = tempfile::tempdir().expect("cwd");
        write_file(&cwd.path().join(".env"), "OPENAI_API_KEY=fixture\n");

        let report = discover_report(
            home.path(),
            cwd.path(),
            &home.path().join(".tachi/global/memory.db"),
            Some("openclaw"),
        );

        assert!(report.candidates.is_empty());
        assert_eq!(report.notes.len(), 1);
        assert_eq!(report.notes[0].code, "unsupported_source");
        assert_eq!(report.notes[0].source, "openclaw");
    }
}
