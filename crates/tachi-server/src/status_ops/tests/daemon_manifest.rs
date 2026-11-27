use super::*;

#[test]
fn orphan_classification_matches_scheduler_routing() {
    let tachi_home = Path::new("/tmp/status-tachi-home");
    let global = PathBuf::from("/tmp/status/global/memory.db");
    let project = PathBuf::from("/tmp/status/project/memory.db");
    let named = PathBuf::from("/tmp/status-tachi-home/projects/sigil/memory.db");
    let agent = PathBuf::from("/tmp/status-tachi-home/agents/main/memory.db");
    assert!(!is_orphan_entry_in_home(
        &entry(DbRole::Global, "global"),
        &global,
        &global,
        Some(&project),
        tachi_home,
    ));
    assert!(!is_orphan_entry_in_home(
        &entry(DbRole::Project, "project"),
        &project,
        &global,
        Some(&project),
        tachi_home,
    ));
    assert!(!is_orphan_entry_in_home(
        &entry(DbRole::Project, "project:sigil"),
        &named,
        &global,
        Some(&project),
        tachi_home,
    ));
    assert!(!is_orphan_entry_in_home(
        &entry(DbRole::Agent, "openclaw-agent:main"),
        &agent,
        &global,
        Some(&project),
        tachi_home,
    ));
}

#[test]
fn daemon_mismatch_detects_foreign_version() {
    let global = PathBuf::from("/tmp/status/global/memory.db");
    let info = DaemonPidInfo {
        pid: Some(42),
        port: Some(6888),
        version: Some("1.3.0".to_string()),
        global_db: Some(global.display().to_string()),
        project_db: None,
    };
    let reason = daemon_mismatch_reason(42, Some(&info), &global)
        .expect("foreign version should be reported");
    assert!(reason.contains("1.3.0"));
    assert!(reason.contains(env!("CARGO_PKG_VERSION")));
}

/// The installer stops a foreign daemon only on a version mismatch. One that
/// belongs to another database scope and also runs another version must be
/// reported by its scope, or the installer would stop it.
#[test]
fn daemon_mismatch_reports_another_database_scope_before_its_version() {
    let global = PathBuf::from("/tmp/status/global/memory.db");
    let info = DaemonPidInfo {
        pid: Some(42),
        port: Some(6919),
        version: Some("1.3.0".to_string()),
        global_db: Some("/elsewhere/tachi-memory.db".to_string()),
        project_db: None,
    };
    let reason = daemon_mismatch_reason(42, Some(&info), &global)
        .expect("another scope's daemon is foreign");
    assert!(
        reason.starts_with("daemon global_db /elsewhere/tachi-memory.db"),
        "{reason}"
    );
}

#[test]
fn daemon_mismatch_detects_pid_file_lock_pid_disagreement() {
    let global = PathBuf::from("/tmp/status/global/memory.db");
    let info = DaemonPidInfo {
        pid: Some(7),
        port: Some(6919),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        global_db: Some(global.display().to_string()),
        project_db: None,
    };
    let reason = daemon_mismatch_reason(42, Some(&info), &global)
        .expect("pid disagreement should be reported");
    assert!(reason.contains("pid=7"));
    assert!(reason.contains("lock pid=42"));
}

#[test]
fn daemon_mismatch_accepts_matching_daemon_pid_file() {
    let global = PathBuf::from("/tmp/status/global/memory.db");
    let info = DaemonPidInfo {
        pid: Some(42),
        port: Some(6919),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        global_db: Some(global.display().to_string()),
        project_db: None,
    };
    assert!(daemon_mismatch_reason(42, Some(&info), &global).is_none());
}

/// Automatic handling rests on what the pid file records. A file that names
/// no database, or no pid, cannot be told from another scope's daemon,
/// whatever version it reports.
#[test]
fn daemon_mismatch_will_not_name_a_version_for_a_pid_file_without_a_scope() {
    let global = PathBuf::from("/tmp/status/global/memory.db");
    for global_db in [None, Some(String::new())] {
        let info = DaemonPidInfo {
            pid: Some(42),
            port: Some(6919),
            version: Some("1.3.0".to_string()),
            global_db,
            project_db: None,
        };
        let reason = daemon_mismatch_reason(42, Some(&info), &global)
            .expect("a pid file without a database is foreign");
        assert!(reason.starts_with("daemon scope unknown"), "{reason}");
    }
    let info = DaemonPidInfo {
        pid: None,
        port: None,
        version: Some("1.3.0".to_string()),
        global_db: Some(global.display().to_string()),
        project_db: None,
    };
    let reason = daemon_mismatch_reason(42, Some(&info), &global)
        .expect("a pid file without a pid is foreign");
    assert!(reason.starts_with("daemon scope unknown"), "{reason}");
}

fn record(kind: &str, pid: i32, lock: &str) -> crate::status_ops::DaemonStatus {
    use crate::status_ops::DaemonStatus;
    let lock_path = PathBuf::from(lock);
    match kind {
        "running" => DaemonStatus::Running { pid, lock_path },
        "stale" => DaemonStatus::StalePid { pid, lock_path },
        "unknown" => DaemonStatus::Unavailable {
            pid,
            lock_path,
            reason: "receipt unreadable".to_string(),
        },
        other => panic!("no such record kind: {other}"),
    }
}

fn foreign_record(pid: i32, lock: &str) -> crate::status_ops::DaemonStatus {
    crate::status_ops::DaemonStatus::Foreign {
        pid,
        lock_path: PathBuf::from(lock),
        reason: "daemon version 1.3.0 does not match binary x".to_string(),
        version: None,
        port: None,
        global_db: None,
    }
}

/// Which of the two records answers for the scope. A live record beats a
/// stale one whichever file holds it, an unreadable one stays unknown, and a
/// stale one is still preferred over nothing.
#[test]
fn a_live_record_wins_over_a_stale_one_whichever_file_holds_it() {
    use crate::status_ops::daemon::prefer_live_record;
    let scoped = foreign_record(7, "/scoped.lock");
    let old_way = Some(record("stale", 9, "/legacy.lock"));

    // This scope's old daemon used to be hidden by a stale legacy pid file.
    assert!(matches!(
        prefer_live_record(Some(scoped.clone()), old_way.clone()),
        Some(crate::status_ops::DaemonStatus::Foreign { pid: 7, .. })
    ));
    // The other way round the legacy record answers, as before.
    assert!(matches!(
        prefer_live_record(
            Some(record("stale", 9, "/scoped.lock")),
            Some(scoped.clone())
        ),
        Some(crate::status_ops::DaemonStatus::Foreign { pid: 7, .. })
    ));
    // Nothing live on either side keeps the old fallback.
    assert!(matches!(
        prefer_live_record(None, old_way.clone()),
        Some(crate::status_ops::DaemonStatus::StalePid { pid: 9, .. })
    ));
    assert!(matches!(
        prefer_live_record(Some(record("stale", 9, "/scoped.lock")), None),
        Some(crate::status_ops::DaemonStatus::StalePid { pid: 9, .. })
    ));
    assert!(prefer_live_record(None, None).is_none());
    // Unknown stays unknown, on either side.
    assert!(matches!(
        prefer_live_record(
            Some(record("unknown", 7, "/scoped.lock")),
            Some(record("running", 9, "/legacy.lock"))
        ),
        Some(crate::status_ops::DaemonStatus::Unavailable { pid: 7, .. })
    ));
}

/// Two live records that name different processes are a conflict. Nothing
/// resolves it on its own, and the installer must not stop either.
#[test]
fn two_live_records_that_disagree_are_a_conflict() {
    use crate::status_ops::daemon::prefer_live_record;
    let status = prefer_live_record(
        Some(foreign_record(7, "/scoped.lock")),
        Some(record("running", 9, "/legacy.lock")),
    )
    .expect("a conflict is reported");
    match status {
        crate::status_ops::DaemonStatus::Foreign {
            pid,
            lock_path,
            reason,
            ..
        } => {
            assert_eq!(pid, 7);
            assert_eq!(lock_path, PathBuf::from("/scoped.lock"));
            assert!(
                reason.contains("pid 7") && reason.contains("pid 9"),
                "{reason}"
            );
            assert!(
                !reason.starts_with("daemon version "),
                "the installer must not stop either: {reason}"
            );
        }
        other => panic!("expected a foreign conflict, got {other:?}"),
    }
}

/// Both files name the same live process. The legacy record answers for it as
/// before, and a running one either way is this scope's daemon. Two records
/// that claim the process for different databases are a conflict: nothing may
/// stop it on the word of either.
#[test]
fn the_same_process_claimed_for_two_databases_is_a_conflict() {
    use crate::status_ops::daemon::prefer_live_record;
    use crate::status_ops::DaemonStatus;
    let ours = "/tachi/tachi-memory.db";
    let theirs = "/openclaw/memory.db";
    let claimed = |lock: &str, reason: &str, db: &str| DaemonStatus::Foreign {
        pid: 7,
        lock_path: PathBuf::from(lock),
        reason: reason.to_string(),
        version: None,
        port: None,
        global_db: Some(db.to_string()),
    };
    let old_version = "daemon version 1.3.0 does not match binary x";
    let other_scope = "daemon global_db /openclaw/memory.db does not match /tachi/tachi-memory.db";

    for (scoped, legacy) in [
        (
            claimed("/scoped.lock", old_version, ours),
            claimed("/legacy.lock", other_scope, theirs),
        ),
        (
            claimed("/scoped.lock", other_scope, theirs),
            claimed("/legacy.lock", old_version, ours),
        ),
    ] {
        let status =
            prefer_live_record(Some(scoped), Some(legacy)).expect("a conflict is reported");
        match status {
            DaemonStatus::Foreign {
                pid,
                lock_path,
                reason,
                ..
            } => {
                assert_eq!(pid, 7);
                assert_eq!(lock_path, PathBuf::from("/legacy.lock"));
                assert!(
                    !reason.starts_with("daemon version "),
                    "the installer must not stop it: {reason}"
                );
                assert!(
                    reason.contains(old_version) && reason.contains(other_scope),
                    "both claims are named: {reason}"
                );
            }
            other => panic!("expected a foreign conflict, got {other:?}"),
        }
    }

    // Records that agree keep the old answer, the legacy one.
    let legacy_lock = PathBuf::from("/legacy.lock");
    let agrees = prefer_live_record(
        Some(claimed("/scoped.lock", old_version, ours)),
        Some(claimed("/legacy.lock", old_version, ours)),
    );
    assert!(matches!(
        agrees,
        Some(DaemonStatus::Foreign {
            ref lock_path,
            ..
        }) if *lock_path == legacy_lock
    ));
    // So does a record that says nothing about the database.
    let silent = DaemonStatus::Foreign {
        pid: 7,
        lock_path: PathBuf::from("/scoped.lock"),
        reason: "daemon scope unknown: daemon.pid records no global_db".to_string(),
        version: None,
        port: None,
        global_db: None,
    };
    let answered = prefer_live_record(
        Some(silent),
        Some(claimed("/legacy.lock", old_version, ours)),
    );
    assert!(matches!(
        answered,
        Some(DaemonStatus::Foreign {
            ref lock_path,
            ..
        }) if *lock_path == legacy_lock
    ));
    // An empty database is no more a claim than a missing one: the legacy
    // record still answers, whichever side the empty one is on.
    let empty = |lock: &str| DaemonStatus::Foreign {
        pid: 7,
        lock_path: PathBuf::from(lock),
        reason: "daemon scope unknown: daemon.pid records no global_db".to_string(),
        version: None,
        port: None,
        global_db: Some(String::new()),
    };
    for (scoped, legacy, expected) in [
        (
            empty("/scoped.lock"),
            claimed("/legacy.lock", old_version, ours),
            old_version,
        ),
        (
            claimed("/scoped.lock", old_version, ours),
            empty("/legacy.lock"),
            "daemon scope unknown: daemon.pid records no global_db",
        ),
    ] {
        match prefer_live_record(Some(scoped), Some(legacy)) {
            Some(DaemonStatus::Foreign { reason, .. }) => assert_eq!(
                reason, expected,
                "an empty database contradicts nothing, so this is no conflict"
            ),
            other => panic!("expected the legacy record to answer, got {other:?}"),
        }
    }
    // A running legacy record says the process is this scope's daemon: the
    // installer stops it through the CLI, it does not signal the pid.
    let running = prefer_live_record(
        Some(foreign_record(7, "/scoped.lock")),
        Some(record("running", 7, "/legacy.lock")),
    );
    assert!(matches!(
        running,
        Some(DaemonStatus::Running {
            pid: 7,
            ref lock_path
        }) if *lock_path == legacy_lock
    ));
}

#[test]
fn collect_daemon_status_prefers_scoped_match_over_legacy_foreign() {
    let app_home = tempfile::tempdir().expect("temp app home");
    let global = app_home.path().join("global").join("memory.db");
    let foreign_global = app_home.path().join("openclaw").join("memory.db");
    let pid = std::process::id();

    let scoped_lock = crate::daemon_lock::scoped_daemon_lock_path(app_home.path(), &global);
    let scoped_pid = crate::daemon_lock::scoped_daemon_pid_path(app_home.path(), &global);
    std::fs::write(&scoped_lock, pid.to_string()).expect("scoped lock");
    std::fs::write(
        &scoped_pid,
        serde_json::json!({
            "pid": pid,
            "port": 7001,
            "version": env!("CARGO_PKG_VERSION"),
            "global_db": global.display().to_string(),
        })
        .to_string(),
    )
    .expect("scoped pid");
    std::fs::write(
        crate::daemon_lock::legacy_daemon_lock_path(app_home.path()),
        pid.to_string(),
    )
    .expect("legacy lock");
    std::fs::write(
        crate::daemon_lock::legacy_daemon_pid_path(app_home.path()),
        serde_json::json!({
            "pid": pid,
            "port": 6919,
            "version": env!("CARGO_PKG_VERSION"),
            "global_db": foreign_global.display().to_string(),
        })
        .to_string(),
    )
    .expect("legacy pid");

    match collect_daemon_status(app_home.path(), &global) {
        DaemonStatus::Running { lock_path, .. } => assert_eq!(lock_path, scoped_lock),
        other => panic!("expected scoped daemon to win, got {other:?}"),
    }
}

#[test]
fn collect_daemon_status_reports_legacy_foreign_without_scoped_match() {
    let app_home = tempfile::tempdir().expect("temp app home");
    let global = app_home.path().join("global").join("memory.db");
    let foreign_global = app_home.path().join("openclaw").join("memory.db");
    let pid = std::process::id();

    std::fs::write(
        crate::daemon_lock::legacy_daemon_lock_path(app_home.path()),
        pid.to_string(),
    )
    .expect("legacy lock");
    std::fs::write(
        crate::daemon_lock::legacy_daemon_pid_path(app_home.path()),
        serde_json::json!({
            "pid": pid,
            "port": 6919,
            "version": env!("CARGO_PKG_VERSION"),
            "global_db": foreign_global.display().to_string(),
        })
        .to_string(),
    )
    .expect("legacy pid");

    match collect_daemon_status(app_home.path(), &global) {
        DaemonStatus::Foreign { reason, .. } => assert!(reason.contains("does not match")),
        other => panic!("expected legacy foreign daemon, got {other:?}"),
    }
}
