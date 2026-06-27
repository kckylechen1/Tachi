use super::*;

#[tokio::test]
async fn tachi_memory_briefing_uses_bound_project_db_when_cwd_project_is_unknown() {
    let (server, temp_home) = make_server_with_temp_home();
    let root = temp_home
        .temp_home
        .join("Bound Project Repo")
        .canonicalize()
        .unwrap_or_else(|_| temp_home.temp_home.join("Bound Project Repo"));
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");

    server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect("project DB init should succeed");
    server
        .with_project_store(|store| {
            let mut entry = make_entry("bound-project-briefing-hit");
            entry.path = "/scratch/tachi/bound-project-briefing".to_string();
            entry.summary = "Bound project briefing hit".to_string();
            entry.text = "BoundProjectBriefingNeedle should surface from the hot-bound project DB."
                .to_string();
            entry.entities = vec!["BoundProjectBriefingNeedle".to_string()];
            store.upsert(&entry).map_err(|e| e.to_string())
        })
        .expect("seed bound project memory");

    let mut params = tachi_memory_params("briefing");
    params.format = Some("json".to_string());
    params.query = Some("BoundProjectBriefingNeedle".to_string());
    params.compact = true;
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("briefing should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("briefing JSON");

    // The project alias name now carries a stable-hash suffix; it still starts
    // with the sanitized repo basename.
    let expected_project = crate::path_utils::plan_c_dir_name_from_root(&root).expect("alias name");
    assert!(
        expected_project.starts_with("Bound_Project_Repo-"),
        "{expected_project}"
    );
    assert_eq!(parsed["project"], json!(expected_project));
    assert!(
        parsed["memories"]
            .as_array()
            .is_some_and(|rows| rows
                .iter()
                .any(|row| row["id"] == json!("bound-project-briefing-hit"))),
        "briefing should search the bound project DB even when cwd has no matching named DB: {parsed}"
    );
}
