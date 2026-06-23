use super::*;

#[test]
fn orphan_classification_matches_scheduler_routing() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var_os("TACHI_HOME");
    std::env::set_var("TACHI_HOME", "/tmp/status-tachi-home");
    let global = PathBuf::from("/tmp/status/global/memory.db");
    let project = PathBuf::from("/tmp/status/project/memory.db");
    let named = PathBuf::from("/tmp/status-tachi-home/projects/sigil/memory.db");
    let agent = PathBuf::from("/tmp/status-tachi-home/agents/main/memory.db");
    assert!(!is_orphan_entry(
        &entry(DbRole::Global, "global"),
        &global,
        &global,
        Some(&project)
    ));
    assert!(!is_orphan_entry(
        &entry(DbRole::Project, "project"),
        &project,
        &global,
        Some(&project)
    ));
    assert!(!is_orphan_entry(
        &entry(DbRole::Project, "project:sigil"),
        &named,
        &global,
        Some(&project)
    ));
    assert!(!is_orphan_entry(
        &entry(DbRole::Agent, "openclaw-agent:main"),
        &agent,
        &global,
        Some(&project)
    ));
    if let Some(v) = saved {
        std::env::set_var("TACHI_HOME", v);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[test]
fn daemon_mismatch_detects_foreign_version() {
    let global = PathBuf::from("/tmp/status/global/memory.db");
    let info = DaemonPidInfo {
        pid: Some(42),
        port: Some(6888),
        version: Some("1.3.0".to_string()),
        global_db: Some(global.display().to_string()),
    };
    let reason = daemon_mismatch_reason(42, Some(&info), &global)
        .expect("foreign version should be reported");
    assert!(reason.contains("1.3.0"));
    assert!(reason.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn daemon_mismatch_detects_pid_file_lock_pid_disagreement() {
    let global = PathBuf::from("/tmp/status/global/memory.db");
    let info = DaemonPidInfo {
        pid: Some(7),
        port: Some(6919),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        global_db: Some(global.display().to_string()),
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
    };
    assert!(daemon_mismatch_reason(42, Some(&info), &global).is_none());
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
