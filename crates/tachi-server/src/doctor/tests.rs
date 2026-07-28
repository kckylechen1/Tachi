use super::*;
use std::{fs, path::Path};
use tempfile::tempdir;

fn with_env_lock<F: FnOnce()>(f: F) {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    f();
}

use crate::test_support::EnvRestore;

fn make_healthy_db(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
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
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE chunks (id TEXT PRIMARY KEY, text TEXT);
             INSERT INTO chunks VALUES ('c1','legacy');",
    )
    .unwrap();
}

#[cfg(unix)]
fn make_corrupt_db(path: &Path) {
    fs::write(
        path,
        b"this is not a sqlite database, just some junk bytes for testing 1234567890",
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
    // #1041 F7: a bare tempdir path has no `project:<name>` scope_hint, so it
    // can never have a registered domain in `RoutingConfig::domain_routes` —
    // the tripwire is skipped (None = "not evaluated"), not run with the
    // trading-keyword default (that blanket default was the F7 bug: it flagged
    // healthy trading content as "suspect" in every store, registered or not).
    assert_eq!(f.cross_domain_suspect_count, None);
    assert!(f.cross_domain_suspect_sample.is_empty());
}

// ── #1041 S4: doctor cross-domain suspect tripwire ──────────────────────────

fn make_engineering_store_with_trading_leak(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE memories (id TEXT PRIMARY KEY, text TEXT, archived INT DEFAULT 0, domain TEXT);
         INSERT INTO memories (id, text, domain) VALUES ('eng-1', 'refactored the save handler', 'engineering');
         INSERT INTO memories (id, text, domain) VALUES ('leak-1', '持仓 300502.SZ 止损 set', 'engineering');
         CREATE TABLE foundry_jobs (id TEXT, status TEXT);",
    )
    .unwrap();
}

#[test]
fn classify_flags_cross_domain_suspect_in_engineering_store() {
    // #1041 F7: `classify_one` derives its cross-domain scan from the
    // store's REGISTERED domain now, and a registered domain only exists
    // via a `project:<name>` scope_hint matched against
    // `RoutingConfig::domain_routes` — a bare tempdir path can never
    // produce that, so this fixture (no registration reachable through
    // `classify_one`) now correctly reports "not evaluated", not a false
    // hit and not a false clean. The keyword-selection logic itself
    // (trading-registered store -> engineering vocabulary, and vice versa)
    // is covered directly and deterministically in `cross_domain`'s own
    // `#[cfg(test)]` module, which injects a `RoutingConfig` directly.
    let dir = tempdir().unwrap();
    let p = dir.path().join("memory.db");
    make_engineering_store_with_trading_leak(&p);
    let f = classify_one(&p);
    assert_eq!(f.classification, DbClassification::Healthy);
    assert_eq!(f.cross_domain_suspect_count, None);
    assert!(f.cross_domain_suspect_sample.is_empty());
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
    make_corrupt_db(&p);
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

#[cfg(unix)]
#[test]
fn scan_counts_path_symlink_and_hardlink_as_one_physical_database() {
    let dir = tempdir().unwrap();
    let real = dir.path().join("real/memory.db");
    let symlink = dir.path().join("symlink/memory.db");
    let hardlink = dir.path().join("hardlink/memory.db");
    for path in [&real, &symlink, &hardlink] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
    }
    make_healthy_db(&real);
    std::os::unix::fs::symlink(&real, &symlink).unwrap();
    fs::hard_link(&real, &hardlink).unwrap();

    let report = scan(
        &[dir.path().to_path_buf()],
        &dir.path().join("quarantine"),
        ScanOptions::default(),
    );

    assert_eq!(report.summary.total_databases, 1);
    assert_eq!(report.summary.total_aliases, 3);
    assert_eq!(report.summary.total_memories, 2);
    assert_eq!(report.findings.len(), 3, "all path aliases remain evidence");
    assert_eq!(report.physical_stores.len(), 1);
    assert_eq!(report.physical_stores[0].aliases.len(), 3);
    let rendered = render_report(&report);
    assert!(rendered.contains("1 physical dbs, 3 path aliases, 2 memories"));
}

#[cfg(unix)]
#[test]
fn scan_keeps_broken_memory_db_alias_as_explicit_finding() {
    let dir = tempdir().unwrap();
    let alias = dir.path().join("broken/memory.db");
    fs::create_dir_all(alias.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(dir.path().join("missing/memory.db"), &alias).unwrap();

    let report = scan(
        &[dir.path().to_path_buf()],
        &dir.path().join("quarantine"),
        ScanOptions::default(),
    );

    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].path, alias.display().to_string());
    assert!(report.findings[0]
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("broken"));
    assert_eq!(report.summary.total_databases, 0);
    assert_eq!(report.summary.total_aliases, 1);
}

#[cfg(unix)]
#[test]
fn scan_reads_committed_wal_while_writer_owns_database() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("memory.db");
    let writer = rusqlite::Connection::open(&db).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=0;
             CREATE TABLE memories (id TEXT PRIMARY KEY, text TEXT, archived INT DEFAULT 0, domain TEXT);
             CREATE TABLE foundry_jobs (id TEXT, status TEXT);
             INSERT INTO memories (id, text) VALUES ('checkpointed', 'main file');
             PRAGMA wal_checkpoint(TRUNCATE);
             INSERT INTO memories (id, text) VALUES ('wal-row', 'committed WAL');
             BEGIN IMMEDIATE;",
        )
        .unwrap();

    let report = scan(
        &[dir.path().to_path_buf()],
        &dir.path().join("quarantine"),
        ScanOptions::default(),
    );

    assert_eq!(report.summary.total_databases, 1);
    assert_eq!(report.summary.total_memories, 2);
    assert_eq!(report.findings[0].mem_count, Some(2));

    writer.execute_batch("ROLLBACK").unwrap();
}

#[cfg(unix)]
#[test]
fn scan_prefers_hardlink_alias_with_active_wal_sidecars() {
    let dir = tempdir().unwrap();
    let hardlink = dir.path().join("a-hardlink/memory.db");
    let live = dir.path().join("z-live/memory.db");
    fs::create_dir_all(hardlink.parent().unwrap()).unwrap();
    fs::create_dir_all(live.parent().unwrap()).unwrap();

    let writer = rusqlite::Connection::open(&live).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=0;
             CREATE TABLE memories (id TEXT PRIMARY KEY, text TEXT, archived INT DEFAULT 0, domain TEXT);
             CREATE TABLE foundry_jobs (id TEXT, status TEXT);
             INSERT INTO memories (id, text) VALUES ('checkpointed', 'main file');
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();
    fs::hard_link(&live, &hardlink).unwrap();
    writer
        .execute_batch(
            "INSERT INTO memories (id, text) VALUES ('wal-row', 'committed WAL');
             BEGIN IMMEDIATE;",
        )
        .unwrap();
    assert!(live.with_file_name("memory.db-wal").exists());

    let report = scan(
        &[dir.path().to_path_buf()],
        &dir.path().join("quarantine"),
        ScanOptions::default(),
    );

    assert_eq!(report.summary.total_databases, 1);
    assert_eq!(
        report.summary.total_memories, 2,
        "must read the alias owning live WAL"
    );
    assert_eq!(
        report.physical_stores[0].primary_path,
        live.display().to_string()
    );
    assert_eq!(
        report.physical_stores[0].open_path_basis,
        crate::physical_db_identity::OpenPathBasis::WalAndShmVisible
    );
    assert!(report.physical_stores[0]
        .sidecar_paths
        .contains(&live.display().to_string()));
    assert!(!report.physical_stores[0]
        .sidecar_paths
        .contains(&hardlink.display().to_string()));

    writer.execute_batch("ROLLBACK").unwrap();
}

#[cfg(unix)]
#[test]
fn doctor_fix_retires_old_hash_alias_without_touching_legacy() {
    with_env_lock(|| {
        let dir = tempdir().unwrap();

        let tachi_home = dir.path().join(".tachi");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");

        let repo = dir.path().join("Sigil");
        let local_db = repo.join(".tachi/memory.db");
        fs::create_dir_all(local_db.parent().unwrap()).unwrap();
        make_healthy_db(&local_db);
        let local_db = fs::canonicalize(&local_db).unwrap();
        let original_bytes = fs::read(&local_db).unwrap();

        let legacy_name =
            crate::path_utils::plan_c_legacy_dir_name_from_root(&repo).expect("legacy alias name");
        let new_name =
            crate::path_utils::plan_c_dir_name_from_root(&repo).expect("derived alias name");
        let old_hash_name = "Sigil-94c144a9";
        assert_ne!(new_name, legacy_name);
        assert_ne!(new_name, old_hash_name);

        let legacy_dir = tachi_home.join("projects").join(&legacy_name);
        fs::create_dir_all(&legacy_dir).unwrap();
        std::os::unix::fs::symlink(&local_db, legacy_dir.join("memory.db")).unwrap();

        let old_hash_dir = tachi_home.join("projects").join(old_hash_name);
        fs::create_dir_all(&old_hash_dir).unwrap();
        std::os::unix::fs::symlink(&local_db, old_hash_dir.join("memory.db")).unwrap();
        let old_hash_backup = old_hash_dir.join("memory.db.migration-bak.20260707");
        fs::write(&old_hash_backup, b"backup bytes").unwrap();

        let new_dir = tachi_home.join("projects").join(&new_name);
        assert!(
            !new_dir.exists(),
            "precondition: the current hashed alias is absent"
        );

        let report = scan(
            &[tachi_home.clone(), repo.join(".tachi")],
            &tachi_home.join("quarantine"),
            ScanOptions {
                auto_fix: true,
                max_depth: 5,
            },
        );

        assert!(
            new_dir.join(memcore::MEMORY_DB_FILENAME).is_symlink(),
            "doctor --fix must explicitly create the current hashed alias"
        );
        assert_eq!(
            fs::canonicalize(new_dir.join(memcore::MEMORY_DB_FILENAME)).unwrap(),
            local_db
        );
        assert!(
            legacy_dir.join("memory.db").is_symlink(),
            "legacy un-hashed alias must remain addressable"
        );
        assert!(
            !old_hash_dir.join("memory.db").exists(),
            "old-hash memory.db symlink should be retired"
        );
        assert_eq!(
            fs::read(&old_hash_backup).unwrap(),
            b"backup bytes",
            "backup files under the old-hash dir are never deleted"
        );
        assert_eq!(
            fs::read(&local_db).unwrap(),
            original_bytes,
            "repo-local DB bytes must be untouched"
        );
        assert!(
            report.auto_fix_actions.iter().any(|action| {
                action.action == "plan_c_alias_retire_old_hash" && action.outcome == "ok"
            }),
            "doctor --fix should report the alias retirement action: {:?}",
            report.auto_fix_actions
        );
    });
}

#[cfg(unix)]
#[test]
fn doctor_fix_does_not_rebrand_corrupt_plan_c_db() {
    with_env_lock(|| {
        let dir = tempdir().unwrap();

        let tachi_home = dir.path().join(".tachi");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");

        let repo = dir.path().join("Sigil");
        let local_db = repo.join(".tachi/memory.db");
        fs::create_dir_all(local_db.parent().unwrap()).unwrap();
        make_corrupt_db(&local_db);
        let local_db = fs::canonicalize(&local_db).unwrap();

        let new_name =
            crate::path_utils::plan_c_dir_name_from_root(&repo).expect("derived alias name");
        let old_hash_name = "Sigil-94c144a9";
        assert_ne!(new_name, old_hash_name);

        let old_hash_dir = tachi_home.join("projects").join(old_hash_name);
        fs::create_dir_all(&old_hash_dir).unwrap();
        let old_hash_db = old_hash_dir.join("memory.db");
        std::os::unix::fs::symlink(&local_db, &old_hash_db).unwrap();

        let new_db = tachi_home
            .join("projects")
            .join(&new_name)
            .join("memory.db");
        assert!(
            !new_db.exists(),
            "precondition: the current hashed alias is absent"
        );

        let report = scan(
            &[repo.join(".tachi")],
            &tachi_home.join("quarantine"),
            ScanOptions {
                auto_fix: true,
                max_depth: 5,
            },
        );

        assert_eq!(report.summary.corrupt, 1);
        assert!(
            !new_db.exists(),
            "doctor --fix must not create a current alias for a corrupt DB"
        );
        assert!(
            old_hash_db.is_symlink(),
            "old-hash alias for a corrupt DB must remain untouched"
        );
        assert_eq!(fs::canonicalize(&old_hash_db).unwrap(), local_db);
        assert!(
            report
                .auto_fix_actions
                .iter()
                .all(|action| !action.action.starts_with("plan_c_alias_")),
            "corrupt DB must emit no Plan C alias retirement actions: {:?}",
            report.auto_fix_actions
        );
    });
}

#[cfg(unix)]
#[test]
fn doctor_fix_plan_c_alias_retirement_is_idempotent() {
    with_env_lock(|| {
        let dir = tempdir().unwrap();

        let tachi_home = dir.path().join(".tachi");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");

        let repo = dir.path().join("Sigil");
        let local_db = repo.join(".tachi/memory.db");
        fs::create_dir_all(local_db.parent().unwrap()).unwrap();
        make_healthy_db(&local_db);
        let local_db = fs::canonicalize(&local_db).unwrap();

        let new_name =
            crate::path_utils::plan_c_dir_name_from_root(&repo).expect("derived alias name");
        let old_hash_name = "Sigil-94c144a9";
        let old_hash_db = tachi_home
            .join("projects")
            .join(old_hash_name)
            .join("memory.db");
        fs::create_dir_all(old_hash_db.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&local_db, &old_hash_db).unwrap();

        let new_db = tachi_home
            .join("projects")
            .join(&new_name)
            .join(memcore::MEMORY_DB_FILENAME);

        let first = scan(
            &[repo.join(".tachi")],
            &tachi_home.join("quarantine"),
            ScanOptions {
                auto_fix: true,
                max_depth: 5,
            },
        );
        assert!(
            first.auto_fix_actions.iter().any(|action| {
                action.action == "plan_c_alias_retire_old_hash" && action.outcome == "ok"
            }),
            "first doctor --fix should retire the drifted old-hash alias: {:?}",
            first.auto_fix_actions
        );

        let second = scan(
            &[repo.join(".tachi")],
            &tachi_home.join("quarantine"),
            ScanOptions {
                auto_fix: true,
                max_depth: 5,
            },
        );

        assert!(
            second
                .auto_fix_actions
                .iter()
                .all(|action| !action.action.starts_with("plan_c_alias_")),
            "second doctor --fix should be a clean Plan C no-op: {:?}",
            second.auto_fix_actions
        );
        assert!(new_db.is_symlink());
        assert_eq!(fs::canonicalize(&new_db).unwrap(), local_db);
        assert!(
            !old_hash_db.exists(),
            "old-hash alias should remain retired after the second run"
        );
    });
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
    assert!(!is_backup_filename("tachi-server.db"));
}
