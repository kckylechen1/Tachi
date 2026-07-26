use super::*;

#[tokio::test]
async fn tachi_memory_briefing_filters_recent_checkpoints_to_bound_project() {
    let (server, temp_home) = make_server_with_temp_home();
    let root = temp_home
        .temp_home
        .join("Checkpoint Project Repo")
        .canonicalize()
        .unwrap_or_else(|_| temp_home.temp_home.join("Checkpoint Project Repo"));
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");

    server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect("project DB init should succeed");

    server
        .with_global_store(|store| {
            let mut entry = make_entry("global-router-checkpoint-noise");
            entry.path = "/agent/checkpoints/2026-06-26".to_string();
            entry.summary = "Unrelated iStoreOS router audit checkpoint".to_string();
            entry.text = "This global checkpoint belongs to another workspace.".to_string();
            entry.timestamp = "2026-06-26T11:00:00Z".to_string();
            store.upsert(&entry).map_err(|e| e.to_string())
        })
        .expect("seed global checkpoint noise");
    server
        .with_project_store(|store| {
            let mut entry = make_entry("project-checkpoint-briefing-hit");
            entry.path = "/agent/checkpoints/2026-06-26".to_string();
            entry.summary = "Current project checkpoint should remain visible".to_string();
            entry.text = "This checkpoint belongs to the bound project.".to_string();
            entry.timestamp = "2026-06-26T10:00:00Z".to_string();
            store.upsert(&entry).map_err(|e| e.to_string())
        })
        .expect("seed project checkpoint");

    let mut params = tachi_memory_params("briefing");
    params.format = Some("json".to_string());
    params.query = Some("checkpoint".to_string());
    params.compact = true;
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("briefing should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("briefing JSON");

    let checkpoint_ids = parsed["recent_checkpoints"]
        .as_array()
        .expect("checkpoint rows")
        .iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(
        checkpoint_ids.contains(&"project-checkpoint-briefing-hit"),
        "expected project checkpoint in briefing: {parsed}"
    );
    assert!(
        !checkpoint_ids.contains(&"global-router-checkpoint-noise"),
        "briefing should not mix global checkpoints into a project briefing: {parsed}"
    );
}

#[tokio::test]
// The process-global TACHI_HOME override must remain serialized through
// the awaited briefing request.
#[allow(clippy::await_holding_lock)]
async fn briefing_checkpoints_use_server_home_after_environment_drift() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let server = make_server();
    let project_root = crate::utils::find_project_git_root().expect("test project root");
    let project_name =
        crate::path_utils::plan_c_dir_name_from_root(&project_root).expect("test project identity");
    server
        .with_project_store(|store| {
            let mut entry = make_entry("fixture-briefing-checkpoint");
            entry.path = "/agent/checkpoints/2026-07-26".to_string();
            entry.summary = "fixture checkpoint".to_string();
            entry.timestamp = "2026-07-26T08:00:00Z".to_string();
            store.upsert(&entry).map_err(|error| error.to_string())
        })
        .expect("seed fixture checkpoint");

    let ambient_home = tempfile::tempdir().expect("ambient home");
    let ambient_db = ambient_home.path().join("ambient-project.db");
    let mut ambient_store =
        memcore::MemoryStore::open(ambient_db.to_str().expect("utf8 ambient DB"))
            .expect("ambient DB");
    let mut ambient_entry = make_entry("ambient-briefing-checkpoint");
    ambient_entry.path = "/agent/checkpoints/2026-07-26".to_string();
    ambient_entry.summary = "ambient checkpoint".to_string();
    ambient_entry.timestamp = "2026-07-26T09:00:00Z".to_string();
    ambient_store
        .upsert(&ambient_entry)
        .expect("seed ambient checkpoint");
    drop(ambient_store);
    crate::manifest::Manifest {
        schema_version: 1,
        generated_at: chrono::Utc::now().to_rfc3339(),
        comment: String::new(),
        dbs: vec![crate::manifest::DbEntry {
            path: ambient_db.display().to_string(),
            role: crate::manifest::DbRole::Project,
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

    let mut params = tachi_memory_params("briefing");
    params.format = Some("json".to_string());
    params.project = Some(project_name);
    params.compact = true;
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("briefing after environment drift");
    let parsed: Value = serde_json::from_str(&body).expect("briefing JSON");
    let checkpoint_ids = parsed["recent_checkpoints"]
        .as_array()
        .expect("checkpoint rows")
        .iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(checkpoint_ids.contains(&"fixture-briefing-checkpoint"));
    assert!(!checkpoint_ids.contains(&"ambient-briefing-checkpoint"));
}
