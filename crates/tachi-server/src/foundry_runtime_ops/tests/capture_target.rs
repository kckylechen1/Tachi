use super::*;

#[test]
fn capture_provenance_uses_server_home_after_environment_drift() {
    let _guard = tachi_home_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let project_root = crate::utils::find_project_git_root().expect("test project root");
    let project_name =
        crate::path_utils::plan_c_dir_name_from_root(&project_root).expect("test project identity");
    let (server, fixture_db) = crate::tests::make_server_with_project_fixture(&project_name);

    let ambient_home = tempdir().expect("ambient home");
    let ambient_db = ambient_home.path().join("ambient-project.db");
    memcore::MemoryStore::open(ambient_db.to_str().expect("utf8 ambient DB")).expect("ambient DB");
    Manifest {
        schema_version: 1,
        generated_at: chrono::Utc::now().to_rfc3339(),
        comment: String::new(),
        dbs: vec![DbEntry {
            path: ambient_db.display().to_string(),
            role: DbRole::Project,
            owner: "test".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: chrono::Utc::now().to_rfc3339(),
            last_classification: "healthy".to_string(),
            scope_hint: format!("project:{project_name}"),
            notes: String::new(),
        }],
    }
    .save(&ambient_home.path().join("manifest.json"))
    .expect("ambient manifest");
    let _ambient = crate::test_support::EnvRestore::set_path("TACHI_HOME", ambient_home.path());

    let entry = crate::tests::make_entry("capture-env-drift-provenance");
    persist_capture_entry(&server, DbScope::Project, Some(&project_name), None, &entry)
        .expect("persist capture through server-bound project");

    let stored = server
        .with_named_project_store_read(&project_name, |store| {
            store.get(&entry.id).map_err(|error| error.to_string())
        })
        .expect("read fixture project")
        .expect("capture stored in fixture project");
    assert_eq!(
        stored.metadata["provenance"]["db_path"],
        std::fs::canonicalize(&fixture_db)
            .expect("canonical fixture DB")
            .display()
            .to_string()
    );
    assert!(
        memcore::MemoryStore::open(ambient_db.to_str().expect("utf8 ambient DB"))
            .expect("reopen ambient DB")
            .get(&entry.id)
            .expect("read ambient DB")
            .is_none(),
        "capture must not follow post-construction TACHI_HOME"
    );
}
#[tokio::test]
async fn resolve_capture_target_prefers_explicit_project() {
    let tmp = tempdir().expect("tempdir");
    let server = crate::MemoryServer::new(tmp.path().join("global.db"), None).expect("server");

    let (target_db, named_project, db_path, warning) =
        resolve_capture_target(&server, "global", Some("wiki"), true, "main");

    assert_eq!(target_db, DbScope::Project);
    assert_eq!(named_project.as_deref(), Some("wiki"));
    assert!(db_path.is_none());
    assert!(warning.is_none());
}

/// #1114 codex round-2 item 2 discriminating test (RED before this fix): a
/// `project=` value that is present but NOT a caller decision (a
/// transport-injected session default, `project_explicit: false`) must NOT
/// bypass an agent's manifest DB pin — before the fix, `Some(project)` alone
/// short-circuited straight past the manifest-pin check regardless of the
/// marker, silently routing a pinned agent's capture into the session's
/// bound project instead of its pinned manifest DB.
#[tokio::test]
async fn resolve_capture_target_transport_default_does_not_bypass_manifest_pin() {
    let _guard = tachi_home_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp_home = tempdir().expect("temp home");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("TACHI_HOME", temp_home.path());

    let manifest_path = temp_home.path().join("manifest.json");
    let agent_db = temp_home
        .path()
        .join(".openclaw/extensions/tachi/data/agents/main/memory.db");
    let manifest = Manifest {
        schema_version: 1,
        generated_at: chrono::Utc::now().to_rfc3339(),
        comment: String::new(),
        dbs: vec![DbEntry {
            path: agent_db.to_string_lossy().into_owned(),
            role: DbRole::Agent,
            owner: "openclaw-agent:main".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: chrono::Utc::now().to_rfc3339(),
            last_classification: "healthy".to_string(),
            scope_hint: "agent:main".to_string(),
            notes: String::new(),
        }],
    };
    manifest.save(&manifest_path).expect("save manifest");

    let server =
        crate::MemoryServer::new(temp_home.path().join("global.db"), None).expect("server");
    // `Some("session-bound-project")` here represents `enforce_session_
    // project`'s transport-injected default — NOT a caller `project=`.
    let (target_db, named_project, db_path, warning) = resolve_capture_target(
        &server,
        "global",
        Some("session-bound-project"),
        false,
        "main",
    );

    assert_eq!(target_db, DbScope::Project);
    assert!(
        named_project.is_none(),
        "the manifest pin must win over the transport default, got named_project={named_project:?}"
    );
    assert_eq!(
        db_path.as_deref(),
        Some(agent_db.as_path()),
        "must resolve to the agent's manifest-pinned DB"
    );
    assert_eq!(
        warning.as_deref(),
        Some("agent capture pinned to manifest DB for main")
    );

    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

/// A transport default with NO manifest pin still gets honored — it's the
/// best signal available for which store, just not strong enough to
/// override a pin.
#[tokio::test]
async fn resolve_capture_target_transport_default_used_without_manifest_pin() {
    let _guard = tachi_home_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp_home = tempdir().expect("temp home");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("TACHI_HOME", temp_home.path());

    let server =
        crate::MemoryServer::new(temp_home.path().join("global.db"), None).expect("server");
    let (target_db, named_project, db_path, warning) = resolve_capture_target(
        &server,
        "global",
        Some("session-bound-project"),
        false,
        "no-pin-agent",
    );

    assert_eq!(target_db, DbScope::Project);
    assert_eq!(named_project.as_deref(), Some("session-bound-project"));
    assert!(db_path.is_none());
    assert!(warning.is_none());

    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}
#[tokio::test]
async fn resolve_capture_target_prefers_manifest_agent_db() {
    let _guard = tachi_home_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp_home = tempdir().expect("temp home");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("TACHI_HOME", temp_home.path());

    let manifest_path = temp_home.path().join("manifest.json");
    let agent_db = temp_home
        .path()
        .join(".openclaw/extensions/tachi/data/agents/main/memory.db");
    let manifest = Manifest {
        schema_version: 1,
        generated_at: chrono::Utc::now().to_rfc3339(),
        comment: String::new(),
        dbs: vec![DbEntry {
            path: agent_db.to_string_lossy().into_owned(),
            role: DbRole::Agent,
            owner: "openclaw-agent:main".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: chrono::Utc::now().to_rfc3339(),
            last_classification: "healthy".to_string(),
            scope_hint: "agent:main".to_string(),
            notes: String::new(),
        }],
    };
    manifest.save(&manifest_path).expect("save manifest");

    let server =
        crate::MemoryServer::new(temp_home.path().join("global.db"), None).expect("server");
    let (target_db, named_project, db_path, warning) =
        resolve_capture_target(&server, "global", None, false, "main");

    assert_eq!(target_db, DbScope::Project);
    assert!(named_project.is_none());
    assert_eq!(db_path.as_deref(), Some(agent_db.as_path()));
    assert_eq!(
        warning.as_deref(),
        Some("agent capture pinned to manifest DB for main")
    );

    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}
#[tokio::test]
async fn resolve_capture_target_falls_back_to_server_scope_without_manifest_match() {
    let _guard = tachi_home_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp_home = tempdir().expect("temp home");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("TACHI_HOME", temp_home.path());

    let project_db = temp_home.path().join("project.db");
    let server = crate::MemoryServer::new(temp_home.path().join("global.db"), Some(project_db))
        .expect("server");
    let (target_db, named_project, db_path, warning) =
        resolve_capture_target(&server, "project", None, false, "missing-agent");

    assert_eq!(target_db, DbScope::Project);
    assert!(named_project.is_none());
    assert!(db_path.is_none());
    assert!(warning.is_none());

    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}
#[test]
fn matches_agent_tag_handles_hyphenated_agent_ids() {
    assert!(matches_agent_tag("jayne-main", "jayne"));
    assert!(matches_agent_tag("openclaw:jayne:main", "jayne"));
    assert!(!matches_agent_tag("jayneville-bot", "jayne"));
}
#[test]
fn matches_agent_tag_handles_user_memory_slugs_without_substring_false_positives() {
    assert!(matches_agent_tag("user-memory", "user-memory"));
    assert!(matches_agent_tag("user-memory-v3", "user-memory"));
    assert!(matches_agent_tag("agent/user-memory", "user-memory"));
    assert!(!matches_agent_tag("my-user-memory-analyzer", "user-memory"));
}
