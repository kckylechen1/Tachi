use super::*;
use rusqlite::Connection;
use std::{fs, path::Path};
use tempfile::tempdir;

fn make_healthy_db(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
            "CREATE TABLE memories (id TEXT PRIMARY KEY, text TEXT, archived INT DEFAULT 0, domain TEXT);
             INSERT INTO memories (id, text) VALUES ('a','hello');
             INSERT INTO memories (id, text, domain) VALUES ('b','world','trading');
             CREATE TABLE foundry_jobs (id TEXT, status TEXT);
             INSERT INTO foundry_jobs VALUES ('j1','completed'),('j2','skipped'),('j3','pending');",
        )
        .unwrap();
}

fn make_legacy_db(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE chunks (id TEXT PRIMARY KEY, text TEXT);
             INSERT INTO chunks VALUES ('c1','legacy');",
    )
    .unwrap();
}

#[test]
fn classify_healthy() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("memory.db");
    make_healthy_db(&p);
    let f = classify_one(&p);
    assert_eq!(f.classification, DbClassification::Healthy);
    assert_eq!(f.mem_count, Some(2));
    assert_eq!(f.jobs.total, 3);
    assert_eq!(f.jobs.completed, 1);
    assert_eq!(f.jobs.skipped, 1);
    assert_eq!(f.jobs.pending, 1);
    assert_eq!(f.none_domain_count, Some(1));
    assert_eq!(f.schema_kind, "tachi");
}

#[test]
fn classify_healthy_reports_actual_nonzero_file_size() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("memory.db");
    make_healthy_db(&p);
    let expected_size = fs::metadata(&p).unwrap().len();
    let f = classify_one(&p);
    assert_eq!(f.classification, DbClassification::Healthy);
    assert_eq!(f.file_size, expected_size);
    assert!(f.file_size > 0);
}

#[test]
fn classify_placeholder() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("memory.db");
    fs::File::create(&p).unwrap(); // 0 bytes
    let f = classify_one(&p);
    assert_eq!(f.classification, DbClassification::Placeholder);
    assert_eq!(f.file_size, 0);
}

#[test]
fn classify_backup_by_filename() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("memory.db.bak.20260101");
    make_healthy_db(&p); // contents are healthy but filename wins
    let f = classify_one(&p);
    assert_eq!(f.classification, DbClassification::Backup);
}

#[test]
fn classify_broken_filename() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("memory.db.broken.20260201");
    make_healthy_db(&p);
    let f = classify_one(&p);
    assert_eq!(f.classification, DbClassification::Backup);
}

#[test]
fn classify_backup_by_archival_path() {
    let dir = tempdir().unwrap();
    let p = dir
        .path()
        .join(".tachi/.agent/claude-code-runs/run-1/backups/global_memory.db");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    make_healthy_db(&p);
    let f = classify_one(&p);
    assert_eq!(f.classification, DbClassification::Backup);
}

#[test]
fn classify_openclaw_agent_named_backup_as_live_db() {
    let dir = tempdir().unwrap();
    let p = dir
        .path()
        .join(".openclaw/extensions/tachi/data/agents/weixin-backup/memory.db");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    make_healthy_db(&p);
    let f = classify_one(&p);
    assert_eq!(f.classification, DbClassification::Healthy);
    assert_eq!(f.schema_kind, "tachi");
    assert_eq!(f.scope_hint, "openclaw-agent:weixin-backup");
}

#[test]
fn classify_legacy_schema() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("memory.db");
    make_legacy_db(&p);
    let f = classify_one(&p);
    assert_eq!(f.classification, DbClassification::LegacySchema);
    assert_eq!(f.schema_kind, "openclaw_legacy");
}

#[test]
fn classify_corrupt() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("memory.db");
    fs::write(
        &p,
        b"this is not a sqlite database, just some junk bytes for testing 1234567890",
    )
    .unwrap();
    let f = classify_one(&p);
    assert_eq!(f.classification, DbClassification::Corrupt);
    assert!(f.error.is_some());
}

#[test]
fn auto_fix_quarantines_placeholder() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("memory.db");
    fs::File::create(&p).unwrap();
    let q = dir.path().join("quarantine");
    let report = scan(
        &[dir.path().to_path_buf()],
        &q,
        ScanOptions {
            auto_fix: true,
            max_depth: 5,
        },
    );
    assert_eq!(report.summary.placeholder, 1);
    assert_eq!(report.auto_fix_actions.len(), 1);
    assert_eq!(report.auto_fix_actions[0].outcome, "ok");
    assert!(!p.exists(), "original placeholder should have been moved");
}

#[test]
fn scan_ignores_own_quarantine_root() {
    let dir = tempdir().unwrap();
    let q = dir.path().join("quarantine");
    let nested = q.join("placeholders/old");
    fs::create_dir_all(&nested).unwrap();
    fs::File::create(nested.join("memory.db")).unwrap();

    let report = scan(
        &[dir.path().to_path_buf()],
        &q,
        ScanOptions {
            auto_fix: true,
            max_depth: 5,
        },
    );

    assert_eq!(report.summary.placeholder, 0);
    assert!(report.auto_fix_actions.is_empty());
}

#[test]
fn quarantine_destination_name_is_bounded_for_long_paths() {
    let long_src = format!("/{}", "very/".repeat(120));
    let name = quarantine_dest_filename(&long_src, "memory.db");

    assert!(
        name.len() < 255,
        "quarantine destination basename must fit common filesystem limits: {}",
        name.len()
    );
    assert!(name.contains("__memory.db"));
}

#[test]
fn scan_default_is_read_only() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("memory.db");
    fs::File::create(&p).unwrap();
    let q = dir.path().join("quarantine");

    let report = scan(&[dir.path().to_path_buf()], &q, ScanOptions::default());

    assert_eq!(report.summary.placeholder, 1);
    assert!(report.auto_fix_actions.is_empty());
    assert!(p.exists(), "default doctor scan must not move files");
}

#[test]
fn project_secret_file_warnings_detects_tracked_generated_env() {
    let dir = tempdir().unwrap();
    if std::process::Command::new("git")
        .arg("init")
        .arg(dir.path())
        .output()
        .map(|output| !output.status.success())
        .unwrap_or(true)
    {
        return;
    }
    let generated = dir.path().join(".tachi/env.generated");
    fs::create_dir_all(generated.parent().unwrap()).unwrap();
    fs::write(&generated, "OPENAI_API_KEY=sk-test\n").unwrap();

    let untracked = project_secret_file_warnings(Some(dir.path()));
    assert!(
        untracked.is_empty(),
        "untracked generated env must not warn"
    );

    let add = std::process::Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args(["add", "--", ".tachi/env.generated"])
        .output()
        .unwrap();
    assert!(add.status.success(), "git add failed: {add:?}");

    let warnings = project_secret_file_warnings(Some(dir.path()));
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].code, "tracked_generated_env");
    assert!(warnings[0].message.contains(".tachi/env.generated"));
    assert!(warnings[0].remediation.contains("git rm --cached"));
}

#[test]
fn project_secret_file_warnings_detects_plaintext_provider_env_without_leaking_value() {
    let dir = tempdir().unwrap();
    let secret_value = "test_plaintext_provider_key_123456";
    fs::write(
        dir.path().join(".env"),
        format!(
            "\
OPENAI_API_KEY={secret_value}
VOYAGE_API_KEY=vault:VOYAGE_API_KEY
IGNORED_API_KEY=not-a-provider-secret
"
        ),
    )
    .unwrap();

    let warnings = project_secret_file_warnings(Some(dir.path()));

    assert_eq!(warnings.len(), 1, "got: {warnings:?}");
    assert_eq!(warnings[0].code, "plaintext_provider_secret");
    assert!(warnings[0].message.contains(".env:1"));
    assert!(warnings[0].message.contains("OPENAI_API_KEY"));
    assert!(warnings[0].remediation.contains("vault:OPENAI_API_KEY"));
    assert!(!warnings[0].message.contains(secret_value));
    assert!(!warnings[0].remediation.contains(secret_value));
}

#[test]
fn project_secret_file_warnings_detects_generated_provider_env_values() {
    let dir = tempdir().unwrap();
    let generated = dir.path().join(".tachi/env.generated");
    fs::create_dir_all(generated.parent().unwrap()).unwrap();
    fs::write(
        &generated,
        "export VOYAGE_API_KEY_1='test_plaintext_provider_key_abcdef'\n",
    )
    .unwrap();

    let warnings = project_secret_file_warnings(Some(dir.path()));

    assert_eq!(warnings.len(), 1, "got: {warnings:?}");
    assert_eq!(warnings[0].code, "plaintext_provider_secret");
    assert!(warnings[0].message.contains(".tachi/env.generated:1"));
    assert!(warnings[0].message.contains("VOYAGE_API_KEY_1"));
    assert!(!warnings[0].message.contains("test_plaintext_provider_key"));
}

#[test]
fn scope_hints() {
    assert_eq!(
        scope_hint_for(Path::new("/Users/x/.tachi/global/memory.db")),
        "global"
    );
    assert_eq!(
        scope_hint_for(Path::new("/Users/x/.tachi/projects/hyperion/memory.db")),
        "project:hyperion"
    );
    assert_eq!(
        scope_hint_for(Path::new(
            "/Users/x/.openclaw/extensions/tachi/data/agents/main/memory.db"
        )),
        "openclaw-agent:main"
    );
    assert_eq!(
        scope_hint_for(Path::new("/Users/x/.gemini/antigravity/memory.db")),
        "antigravity"
    );
}

#[test]
fn backup_filename_patterns() {
    for name in [
        "memory.db.bak.20260101",
        "memory.db.broken.123",
        "memory.db.corrupted",
        "thing.old.sqlite",
        "memory.db.pre-split.20260330_211247",
        "memory.db.checkpointed.db",
    ] {
        assert!(is_backup_filename(name), "{name} should be backup");
    }
    assert!(!is_backup_filename("memory.db"));
    assert!(!is_backup_filename("memory-server.db"));
}
