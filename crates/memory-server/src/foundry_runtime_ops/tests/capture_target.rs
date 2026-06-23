use super::*;
#[tokio::test]
async fn resolve_capture_target_prefers_explicit_project() {
    let tmp = tempdir().expect("tempdir");
    let server = crate::MemoryServer::new(tmp.path().join("global.db"), None).expect("server");

    let (target_db, named_project, db_path, warning) =
        resolve_capture_target(&server, "global", Some("wiki"), "main");

    assert_eq!(target_db, DbScope::Project);
    assert_eq!(named_project.as_deref(), Some("wiki"));
    assert!(db_path.is_none());
    assert!(warning.is_none());
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
        resolve_capture_target(&server, "global", None, "main");

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
        resolve_capture_target(&server, "project", None, "missing-agent");

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
