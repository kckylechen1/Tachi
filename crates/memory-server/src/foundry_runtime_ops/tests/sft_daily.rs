use super::*;
struct SftEnvGuard {
    original_tachi_home: Option<std::ffi::OsString>,
    original_tachi_app_home: Option<std::ffi::OsString>,
    original_siliconflow_api_key: Option<std::ffi::OsString>,
    original_voyage_api_key: Option<std::ffi::OsString>,
    original_siliconflow_base: Option<std::ffi::OsString>,
    original_reasoning_base: Option<std::ffi::OsString>,
    original_claude_bin: Option<std::ffi::OsString>,
}

impl SftEnvGuard {
    fn new(temp_path: &std::path::Path, mock_url: &str) -> Self {
        let original_tachi_home = std::env::var_os("TACHI_HOME");
        let original_tachi_app_home = std::env::var_os("TACHI_APP_HOME");
        let original_siliconflow_api_key = std::env::var_os("SILICONFLOW_API_KEY");
        let original_voyage_api_key = std::env::var_os("VOYAGE_API_KEY");
        let original_siliconflow_base = std::env::var_os("SILICONFLOW_BASE_URL");
        let original_reasoning_base = std::env::var_os("REASONING_BASE_URL");
        let original_claude_bin = std::env::var_os("CLAUDE_BIN");

        std::env::set_var("TACHI_HOME", temp_path);
        std::env::set_var("TACHI_APP_HOME", temp_path);
        std::env::set_var("SILICONFLOW_BASE_URL", mock_url);
        std::env::set_var("REASONING_BASE_URL", mock_url);
        std::env::set_var("SILICONFLOW_API_KEY", "test-mock-key");
        std::env::set_var("VOYAGE_API_KEY", "test-mock-key");
        std::env::set_var("CLAUDE_BIN", "/nonexistent/fake/bin");

        Self {
            original_tachi_home,
            original_tachi_app_home,
            original_siliconflow_api_key,
            original_voyage_api_key,
            original_siliconflow_base,
            original_reasoning_base,
            original_claude_bin,
        }
    }
}

impl Drop for SftEnvGuard {
    fn drop(&mut self) {
        fn restore(name: &str, val: Option<&std::ffi::OsStr>) {
            if let Some(v) = val {
                std::env::set_var(name, v);
            } else {
                std::env::remove_var(name);
            }
        }
        restore("TACHI_HOME", self.original_tachi_home.as_deref());
        restore("TACHI_APP_HOME", self.original_tachi_app_home.as_deref());
        restore(
            "SILICONFLOW_API_KEY",
            self.original_siliconflow_api_key.as_deref(),
        );
        restore("VOYAGE_API_KEY", self.original_voyage_api_key.as_deref());
        restore(
            "SILICONFLOW_BASE_URL",
            self.original_siliconflow_base.as_deref(),
        );
        restore(
            "REASONING_BASE_URL",
            self.original_reasoning_base.as_deref(),
        );
        restore("CLAUDE_BIN", self.original_claude_bin.as_deref());
    }
}
#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes TACHI_HOME across async mock LLM + distillation
async fn test_run_daily_sft_distillation() {
    let _guard = tachi_home_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    // 1. Mock HTTP LLM Server
    use axum::{routing::post, Json, Router};
    let app = Router::new().route(
        "/chat/completions",
        post(|Json(_body): Json<serde_json::Value>| async {
            Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": "{\n  \"user\": \"How to design Promotion Gates?\",\n  \"assistant\": \"Promotion Gates protect long-term memory by promoting raw tier to consolidated.\",\n  \"type\": \"architecture\"\n}"
                        },
                        "finish_reason": "stop"
                    }
                ],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 20,
                    "total_tokens": 30
                }
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let port = addr.port();

    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // 2. Set environment overrides inside temp home using RAII Guard
    let temp_home = tempdir().expect("temp home");
    let mock_url = format!("http://127.0.0.1:{port}/chat/completions");
    let _env_guard = SftEnvGuard::new(temp_home.path(), &mock_url);

    // 3. Create server and seed consolidated memory entry in Project DB
    let global_db = temp_home.path().join("global.db");
    let project_db = temp_home.path().join("project.db");
    let server = crate::MemoryServer::new(global_db, Some(project_db.clone())).expect("server");

    server.with_project_store(|store| {
        store.upsert(&MemoryEntry {
            id: "sft-test-1".to_string(),
            path: "/project/tachi/sft-1".to_string(),
            summary: "Implement Promotion Gates".to_string(),
            text: "This memory describes the Promotion Gates mechanism including REM and Deep sleep gates with access thresholds.".to_string(),
            importance: 0.85,
            timestamp: "2026-05-30T00:00:00Z".to_string(),
            category: "experience".to_string(),
            topic: "memory-lifecycle".to_string(),
            keywords: vec!["sft".to_string()],
            persons: vec![],
            entities: vec![],
            location: "".to_string(),
            source: "manual".to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 5,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            valid_from: String::new(),
            valid_until: None,
            recall_count: 5,
            query_diversity: 3,
            tier: "consolidated".to_string(),
        }).map_err(|e| e.to_string())
    }).expect("seed test memory");

    // 4. Invoke run_daily_sft_distillation
    let result = sft_factory::run_daily_sft_distillation(&server).await;
    assert!(result.is_ok(), "sft distillation failed: {:?}", result);

    // 5. Verify the files are produced and populated
    let sft_dir = temp_home.path().join("foundry-runs").join("sft");
    let v3_file = sft_dir.join("sft_v3.jsonl");
    let chat_file = sft_dir.join("sft_data_chat.jsonl");
    let hf_file = sft_dir.join("sft_data_hf.jsonl");

    assert!(v3_file.exists());
    assert!(chat_file.exists());
    assert!(hf_file.exists());

    let v3_content = std::fs::read_to_string(v3_file).unwrap();
    assert!(v3_content.contains("How to design Promotion Gates?"));
    assert!(v3_content.contains("Promotion Gates protect long-term memory"));
    let first_v3: serde_json::Value = serde_json::from_str(
        v3_content
            .lines()
            .next()
            .expect("sft_v3 should include one JSONL row"),
    )
    .expect("sft_v3 first row should be JSON");
    assert_eq!(
        first_v3["metadata"]["artifact_class"],
        json!("sft_candidate"),
        "SFT exports must remain candidate artifacts until the model-training gate promotes them"
    );
    assert_eq!(
        first_v3["metadata"]["promotion_status"],
        json!("candidate_only"),
        "SFT exports must not look like production memory/wiki/eval artifacts"
    );
    let pending_dir = sft_dir.join("pending");
    let pending_batches = std::fs::read_dir(&pending_dir)
        .expect("pending SFT dir")
        .filter_map(Result::ok)
        .count();
    assert_eq!(
        pending_batches, 3,
        "expected one durable pending file per export format"
    );

    // 6. Verify entry in DB has been updated to processed
    let (is_processed, batch_id) = server.with_project_store_read(|store| {
        let conn = store.connection();
        let row: (Option<i64>, Option<String>) = conn.query_row(
            "SELECT json_extract(metadata, '$.sft.processed'), json_extract(metadata, '$.sft.batch_id') FROM memories WHERE id = 'sft-test-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?))
        ).map_err(|e| e.to_string())?;
        Ok((row.0.unwrap_or(0) == 1, row.1))
    }).unwrap();
    assert!(is_processed, "entry was not marked as sft.processed");
    assert!(
        batch_id.is_some(),
        "SFT marker should include durable batch id"
    );

    // 7. Cleanup server task
    server_task.abort();
}
