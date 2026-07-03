use crate::cli::VaultAction;
use crate::provider_config::parse_vault_alias;
use memory_core::vault::{SECRET_TYPE_API_KEY, SECRET_TYPE_JSON_BLOB};
use serde::Serialize;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

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
    let (host, json_output) = action.into_discover();
    let cwd = std::env::current_dir()?;
    let env_home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let report = discover_report(&env_home, &cwd, global_db_path, host.as_deref());

    if json_output {
        println!("{}", render_json(&report)?);
    } else {
        print!("{}", render_human(&report));
    }
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn discover_candidates(env_home: &Path, cwd: &Path) -> Vec<Candidate> {
    let global_db_path = env_home.join(".tachi").join("global").join("memory.db");
    discover_report(env_home, cwd, &global_db_path, None).candidates
}

fn discover_report(
    env_home: &Path,
    cwd: &Path,
    global_db_path: &Path,
    host: Option<&str>,
) -> DiscoveryReport {
    let (filter, notes) = parse_host_filter(host);
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
            let value = value.trim();
            if key.is_empty() || value.is_empty() {
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
    let Ok(store) = memory_core::MemoryStore::open_read_only(path) else {
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
    // control. Avoid adding a crypto dependency for this read-only discovery slice.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    let full = format!("{:016x}", hasher.finish());
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

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::vault::VaultEntry;

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
            memory_core::MemoryStore::open(db_path.to_str().expect("utf8 db")).expect("open db");
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
        assert_eq!(left.len(), 8);
        assert!(left.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_eq!(left, left.to_ascii_lowercase());
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
        let entries_before = memory_core::MemoryStore::open_read_only(db_path.to_str().unwrap())
            .expect("read db before")
            .vault_list_entries()
            .expect("entries before")
            .len();

        let rows = discover_candidates(home.path(), cwd.path());

        let entries_after = memory_core::MemoryStore::open_read_only(db_path.to_str().unwrap())
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
