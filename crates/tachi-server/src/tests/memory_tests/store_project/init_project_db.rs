use super::*;

#[tokio::test]
async fn tachi_init_project_db_creates_expected_path() {
    let server = make_server();
    let root = std::env::temp_dir().join(format!("tachi-project-db-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");

    let response = server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect("tachi_init_project_db should succeed");
    let json: serde_json::Value =
        serde_json::from_str(&response).expect("tachi_init_project_db response should be JSON");

    let db_path =
        crate::path_utils::resolve_project_db_path(&root, std::path::Path::new(".tachi/memory.db"))
            .expect("resolve project db path");
    assert_eq!(json["created"], json!(true));
    assert_eq!(json["db_path"], json!(db_path.display().to_string()));
    assert!(db_path.exists(), "project db should be created on disk");

    let response_second = server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect("second tachi_init_project_db should succeed");
    let json_second: serde_json::Value = serde_json::from_str(&response_second)
        .expect("second tachi_init_project_db response should be JSON");
    assert_eq!(json_second["created"], json!(false));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn tachi_init_project_db_activates_project_store_and_read_pool() {
    let server = make_server();
    let root = std::env::temp_dir().join(format!("tachi-project-runtime-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");

    assert_eq!(server.project_db_path_buf(), None);

    let response = server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect("tachi_init_project_db should activate project state");
    let json: serde_json::Value =
        serde_json::from_str(&response).expect("tachi_init_project_db response should be JSON");
    assert_eq!(json["active"], json!(true));

    let db_path =
        crate::path_utils::resolve_project_db_path(&root, std::path::Path::new(".tachi/memory.db"))
            .expect("resolve project db path");
    assert_eq!(server.project_db_path_buf(), Some(db_path));

    server
        .with_project_store(|store| {
            store
                .upsert(&make_entry("activated-project-read-visible"))
                .map_err(|e| format!("project upsert failed: {e}"))
        })
        .expect("project writer should be active");

    let found = server
        .with_project_store_read(|store| {
            store
                .get("activated-project-read-visible")
                .map_err(|e| format!("project read get failed: {e}"))
        })
        .expect("project read pool should be active");
    assert_eq!(
        found.expect("project entry exists").id,
        "activated-project-read-visible"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn tachi_init_project_db_reports_plan_c_split_brain() {
    let (server, temp_home) = make_server_with_temp_home();
    let root = temp_home
        .temp_home
        .join("Split Brain Repo")
        .canonicalize()
        .unwrap_or_else(|_| temp_home.temp_home.join("Split Brain Repo"));
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");

    let alias_db = crate::path_utils::plan_c_global_db_path("Split_Brain_Repo");
    std::fs::create_dir_all(alias_db.parent().expect("alias parent")).expect("create alias parent");
    memcore::MemoryStore::open(alias_db.to_str().expect("alias db")).expect("open alias db");

    let response = server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect("tachi_init_project_db should report split-brain without failing");
    let json: serde_json::Value =
        serde_json::from_str(&response).expect("tachi_init_project_db response should be JSON");
    assert_eq!(
        json["plan_c_split_brain"]["project_name"],
        json!("Split_Brain_Repo")
    );
    assert!(
        json["note"]
            .as_str()
            .is_some_and(|note| note.contains("Plan C split-brain detected")),
        "note should surface split-brain guidance: {json}"
    );
    assert!(
        !alias_db.is_symlink(),
        "init must not silently replace a regular alias DB"
    );
}

#[tokio::test]
async fn tachi_init_project_db_rejects_path_traversal() {
    let server = make_server();
    let root = std::env::temp_dir().join(format!("tachi-project-escape-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");

    let err = server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: "../outside.db".to_string(),
        }))
        .await
        .expect_err("path traversal db_relpath should be rejected");

    assert!(err.contains("db_relpath"), "unexpected error: {err}");

    let _ = std::fs::remove_dir_all(root);
}
