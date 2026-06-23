use super::*;

#[tokio::test]
async fn cached_global_read_store_sees_writes_and_stays_read_only() {
    let server = make_server();

    server
        .with_global_store(|store| {
            store
                .upsert(&make_entry("cached-read-visible"))
                .map_err(|e| format!("upsert failed: {e}"))
        })
        .expect("seed after cached read store startup");

    let found = server
        .with_global_store_read(|store| {
            store
                .get("cached-read-visible")
                .map_err(|e| format!("cached read get failed: {e}"))
        })
        .expect("cached read should see committed write");
    assert_eq!(found.expect("entry exists").id, "cached-read-visible");

    let write_err = server
        .with_global_store_read(|store| {
            store
                .upsert(&make_entry("cached-read-write-blocked"))
                .map_err(|e| format!("cached read write failed: {e}"))
        })
        .expect_err("cached read store must reject writes");
    assert!(
        write_err.contains("cached read write failed"),
        "{write_err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn global_read_pool_allows_concurrent_read_closures() {
    let server = make_server();
    assert!(
        server.global_read_pool_size_for_tests() >= 2,
        "test requires the default read pool to have multiple slots"
    );

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let entered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let first = {
        let server = server.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        let entered = std::sync::Arc::clone(&entered);
        tokio::task::spawn_blocking(move || {
            server.with_global_store_read(|_store| {
                entered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                barrier.wait();
                Ok(())
            })
        })
    };
    let second = {
        let server = server.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        let entered = std::sync::Arc::clone(&entered);
        tokio::task::spawn_blocking(move || {
            server.with_global_store_read(|_store| {
                entered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                barrier.wait();
                Ok(())
            })
        })
    };

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        first
            .await
            .expect("first reader task should join")
            .expect("first reader should succeed");
        second
            .await
            .expect("second reader task should join")
            .expect("second reader should succeed");
    })
    .await
    .expect("two readers should enter read closures concurrently");

    assert_eq!(
        entered.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "both reader closures should have entered before either returned"
    );
}

#[tokio::test]
async fn sync_memories_errors_if_agent_state_persist_fails() {
    let server = make_server();

    server
        .with_global_store(|store| {
            store
                .upsert(&make_entry("sync-1"))
                .map_err(|e| format!("upsert failed: {e}"))
        })
        .expect("failed to seed memory");

    server
        .with_global_store(|store| {
            store
                .connection()
                .execute_batch(
                    r#"
                    DROP TRIGGER IF EXISTS block_agent_known_state_insert;
                    CREATE TRIGGER block_agent_known_state_insert
                    BEFORE INSERT ON agent_known_state
                    BEGIN
                        SELECT RAISE(FAIL, 'blocked by test');
                    END;
                    "#,
                )
                .map_err(|e| format!("trigger setup failed: {e}"))
        })
        .expect("failed to install blocking trigger");

    let params = SyncMemoriesParams {
        agent_id: "agent-sync-test".to_string(),
        path_prefix: Some("/".to_string()),
        limit: 10,
    };

    let err = server
        .sync_memories(Parameters(params))
        .await
        .expect_err("sync_memories should fail when state persistence fails");

    assert!(
        err.contains("failed to persist agent state"),
        "unexpected error: {err}"
    );
}

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
    memory_core::MemoryStore::open(alias_db.to_str().expect("alias db")).expect("open alias db");

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
