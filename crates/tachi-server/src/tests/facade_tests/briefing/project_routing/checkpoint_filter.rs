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
