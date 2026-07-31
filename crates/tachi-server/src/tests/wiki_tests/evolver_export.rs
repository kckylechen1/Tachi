use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes HOME/TACHI_HOME across async mock LLM + REM run
async fn rem_wiki_evolver_writes_pending_drafts_to_wiki_project() {
    let _lock = home_test_lock().lock().unwrap_or_else(|e| e.into_inner());

    use axum::{routing::post, Json, Router};
    let synthesis_barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let app = Router::new().route(
        "/chat/completions",
        post({
            let synthesis_barrier = synthesis_barrier.clone();
            move |Json(_body): Json<serde_json::Value>| {
                let synthesis_barrier = synthesis_barrier.clone();
                async move {
                    synthesis_barrier.wait().await;
                    Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": "{\n  \"title\": \"Recall Gate Pattern\",\n  \"body\": \"## Pattern\\nUse recall diversity before promoting raw notes into durable knowledge.\\n\\n## Gotcha\\nDo not activate drafts without review.\",\n  \"summary\": \"Recall diversity gates promotion.\",\n  \"keywords\": [\"recall\", \"promotion\"],\n  \"entities\": [\"Tachi\"],\n  \"domain\": \"memory\"\n}"
                        },
                        "finish_reason": "stop"
                    }
                ],
                "usage": {"prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30}
                    }))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    let original_siliconflow_base = std::env::var_os("SILICONFLOW_BASE_URL");
    let original_reasoning_base = std::env::var_os("REASONING_BASE_URL");
    let original_siliconflow_key = std::env::var_os("SILICONFLOW_API_KEY");
    let original_voyage_key = std::env::var_os("VOYAGE_API_KEY");
    let temp_home =
        crate::utils::test_fixture_path(format!("tachi-rem-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(temp_home.join(".tachi/projects/wiki")).expect("create wiki project");
    std::env::set_var("HOME", &temp_home);
    std::env::set_var("TACHI_HOME", temp_home.join(".tachi"));
    let mock_url = format!("http://127.0.0.1:{port}/chat/completions");
    std::env::set_var("SILICONFLOW_BASE_URL", &mock_url);
    std::env::set_var("REASONING_BASE_URL", &mock_url);
    std::env::set_var("SILICONFLOW_API_KEY", "test-mock-key");
    std::env::set_var("VOYAGE_API_KEY", "test-mock-key");

    let wiki_db = temp_home.join(".tachi/projects/wiki/memory.db");
    MemoryStore::open(wiki_db.to_str().expect("wiki db utf8")).expect("init wiki db");
    let server = MemoryServer::new(
        temp_home.join("global.db"),
        Some(temp_home.join("project.db")),
    )
    .expect("server");
    server
        .with_global_store(|store| {
            let mut entry = make_entry("pattern-a");
            entry.path = "/global/tachi/pattern-a".to_string();
            entry.summary = "Global recall diversity gate".to_string();
            entry.text = "Global experience independently confirms that recall diversity should gate durable Wiki promotion and remain pending until review.".to_string();
            entry.importance = 0.9;
            entry.topic = "recall-gate".to_string();
            entry.keywords = vec!["recall".to_string(), "promotion".to_string()];
            entry.source = "manual".to_string();
            entry.tier = "pattern".to_string();
            store.upsert(&entry).map_err(|error| error.to_string())
        })
        .expect("seed same-id global pattern");
    server.with_project_store(|store| {
        for (id, summary, text) in [
            (
                "pattern-a",
                "Recall diversity gate",
                "Recall diversity should gate raw promotion before durable wiki synthesis. This fixture covers the first retrieval signal and promotion rule.",
            ),
            (
                "pattern-b",
                "Pending review draft gate",
                "Weekly REM synthesis should write drafts as pending review wiki notes. This fixture covers draft routing and review metadata safety.",
            ),
        ] {
            let mut entry = make_entry(id);
            entry.path = format!("/project/tachi/{id}");
            entry.summary = summary.to_string();
            entry.text = text.to_string();
            entry.importance = 0.9;
            entry.topic = "recall-gate".to_string();
            entry.keywords = vec!["recall".to_string(), "promotion".to_string()];
            entry.source = "manual".to_string();
            entry.tier = "pattern".to_string();
            store.upsert(&entry).map_err(|e| e.to_string())?;
        }
        let mut sft_entry = make_entry("pattern-sft-seed");
        sft_entry.path = "/sft/v4/strict/engineering/recall-gate".to_string();
        sft_entry.summary = "SFT recall gate exemplar".to_string();
        sft_entry.text =
            "SFT exemplar should not be promoted into REM wiki synthesis.".to_string();
        sft_entry.importance = 0.99;
        sft_entry.topic = "recall-gate".to_string();
        sft_entry.keywords = vec!["recall".to_string(), "promotion".to_string()];
        sft_entry.source = "sft_seed".to_string();
        sft_entry.tier = "pattern".to_string();
        sft_entry.metadata = json!({"training_sample": true});
        store.upsert(&sft_entry).map_err(|e| e.to_string())?;
        Ok(())
    }).expect("seed patterns");
    let seeded_count: i64 = server
        .with_project_store_read(|store| {
            store.connection().query_row(
            "SELECT COUNT(*) FROM memories WHERE tier = 'pattern' AND topic = 'recall-gate'",
            [],
            |row| row.get(0),
        ).map_err(|e| e.to_string())
        })
        .expect("count seeded patterns");
    assert_eq!(
        seeded_count, 3,
        "expected two live pattern memories plus one SFT seed before REM run"
    );

    let (first, second) = tokio::join!(
        crate::foundry_runtime_ops::wiki_evolver::run_weekly_wiki_evolution(&server),
        crate::foundry_runtime_ops::wiki_evolver::run_weekly_wiki_evolution(&server),
    );
    let reports = [
        first.expect("first concurrent wiki evolution"),
        second.expect("second concurrent wiki evolution"),
    ];
    assert_eq!(
        reports
            .iter()
            .map(|report| report.drafts_written)
            .sum::<usize>(),
        1,
        "concurrent replay must report exactly one newly written draft"
    );
    assert_eq!(reports.iter().map(|report| report.errors).sum::<usize>(), 0);
    let (draft_id, review_status, model_receipt, operation_status, source_count) = server.with_named_project_store_read("wiki", |store| {
        store.connection().query_row(
            "SELECT id, json_extract(metadata, '$.review_status'), json_extract(metadata, '$.provenance.model_invocation.schema'), json_extract(metadata, '$.rem.operation_status'), json_array_length(json_extract(metadata, '$.rem.sources')) FROM memories WHERE path LIKE '/wiki/drafts/%' LIMIT 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, i64>(4)?)),
        ).map_err(|e| e.to_string())
    }).expect("read wiki draft metadata");
    assert!(draft_id.starts_with("wiki-rem:"), "{draft_id}");
    assert_eq!(review_status.as_deref(), Some("pending"));
    assert_eq!(
        model_receipt.as_deref(),
        Some("model-invocation-v1"),
        "REM's first wiki draft write must carry the typed model receipt"
    );
    assert_eq!(operation_status.as_deref(), Some("complete"));
    assert_eq!(
        source_count, 3,
        "same memory ID in two stores stays distinct"
    );
    let operation_log = server
        .with_named_project_store_read("wiki", |store| {
            store
                .get("wiki-operation-log")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "Wiki operation log missing".to_string())
        })
        .expect("read REM Wiki operation log");
    assert!(
        operation_log.text.contains("weekly REM draft completed"),
        "successful REM draft persistence must remain visible in the Wiki operation log"
    );
    for read_marker in [
        server.with_global_store_read(|store| {
            store.get("pattern-a").map_err(|error| error.to_string())
        }),
        server.with_project_store_read(|store| {
            store.get("pattern-a").map_err(|error| error.to_string())
        }),
    ] {
        let source = read_marker
            .expect("read REM source marker")
            .expect("source exists");
        assert_eq!(source.metadata["rem"]["processed"], json!(1));
        assert_eq!(
            source.metadata["rem"]["processed_by"],
            json!(draft_id.clone())
        );
    }
    let sft_processed: Option<i64> = server
        .with_project_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT json_extract(metadata, '$.rem.processed') FROM memories WHERE id = 'pattern-sft-seed'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())
        })
        .expect("read SFT pattern metadata");
    assert_eq!(
        sft_processed, None,
        "SFT pattern seeds must not be consumed by REM wiki evolution"
    );
    let replay = crate::foundry_runtime_ops::wiki_evolver::run_weekly_wiki_evolution(&server)
        .await
        .expect("second REM run");
    assert_eq!(replay.drafts_written, 0, "replay must not mint a draft");
    let draft_count: i64 = server
        .with_named_project_store_read("wiki", |store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM memories WHERE path LIKE '/wiki/drafts/%'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("count REM drafts after replay");
    assert_eq!(draft_count, 1);

    server_task.abort();
    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
    if let Some(value) = original_siliconflow_base {
        std::env::set_var("SILICONFLOW_BASE_URL", value);
    } else {
        std::env::remove_var("SILICONFLOW_BASE_URL");
    }
    if let Some(value) = original_reasoning_base {
        std::env::set_var("REASONING_BASE_URL", value);
    } else {
        std::env::remove_var("REASONING_BASE_URL");
    }
    if let Some(value) = original_siliconflow_key {
        std::env::set_var("SILICONFLOW_API_KEY", value);
    } else {
        std::env::remove_var("SILICONFLOW_API_KEY");
    }
    if let Some(value) = original_voyage_key {
        std::env::set_var("VOYAGE_API_KEY", value);
    } else {
        std::env::remove_var("VOYAGE_API_KEY");
    }
    let _ = std::fs::remove_dir_all(temp_home);
}

#[tokio::test]
async fn wiki_export_obsidian_writes_markdown_index_and_wikilinks() {
    let mut entry = make_entry("wiki-export-entry");
    entry.path = "/wiki/engineering/debugging/export".to_string();
    entry.summary = "Export MCP lesson".to_string();
    entry.text =
        "MCP export lesson references MCP explicitly; [[MCP]] stays linked; MCPing stays plain."
            .to_string();
    entry.topic = "export-mcp".to_string();
    entry.keywords = vec!["debugging".to_string()];
    entry.entities = vec!["MCP".to_string()];

    let (server, _home) = seed_wiki_project_entries(vec![entry]);
    let out_dir = crate::utils::test_fixture_path(format!("wiki-export-{}", uuid::Uuid::new_v4()));

    let result = crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir)
        .expect("wiki export should succeed");
    assert_eq!(result["count"], json!(1));
    let md_path = out_dir.join("engineering/debugging/export/export-mcp.md");
    let markdown = std::fs::read_to_string(&md_path).expect("read exported markdown");
    assert!(markdown.contains("tags: [\"debugging\"]"));
    assert!(markdown.contains(
        "[[MCP]] export lesson references [[MCP]] explicitly; [[MCP]] stays linked; MCPing stays plain."
    ));
    let index = std::fs::read_to_string(out_dir.join("_index.md")).expect("read index");
    assert!(index.contains("[[export-mcp]]"));
    let _ = std::fs::remove_dir_all(out_dir);
}

#[test]
fn wiki_export_obsidian_explicit_project_never_falls_back_to_legacy_global() {
    let (server, _project_db) = crate::tests::make_server_with_project_fixture("export-target");
    let mut target = make_entry("wiki-export-store-identity");
    target.path = "/wiki/export/store-identity".to_string();
    target.topic = "named-store".to_string();
    target.text = "Named project export sentinel.".to_string();
    target.metadata = json!({"lifecycle": "active"});
    let mut legacy = target.clone();
    legacy.topic = "legacy-global".to_string();
    legacy.text = "Legacy global export sentinel.".to_string();

    server
        .with_named_project_store("export-target", |store| {
            store.upsert(&target).map_err(|error| error.to_string())
        })
        .expect("seed named export target");
    server
        .with_global_store(|store| store.upsert(&legacy).map_err(|error| error.to_string()))
        .expect("seed legacy global export decoy");

    let out_dir = crate::utils::test_fixture_path(format!(
        "wiki-export-store-identity-{}",
        uuid::Uuid::new_v4()
    ));
    let result = crate::wiki_ops::export_wiki_obsidian(&server, "export-target", &out_dir)
        .expect("strict named export");
    assert_eq!(result["count"], json!(1));
    let markdown = std::fs::read_to_string(out_dir.join("export/store-identity/named-store.md"))
        .expect("read named export");
    assert!(markdown.contains("Named project export sentinel."));
    assert!(!markdown.contains("Legacy global export sentinel."));
    assert!(!out_dir
        .join("export/store-identity/legacy-global.md")
        .exists());
    let _ = std::fs::remove_dir_all(out_dir);
}

#[tokio::test]
async fn wiki_export_obsidian_prefers_typed_refs_when_both_channels_exist() {
    let mut entry = make_entry("wiki-export-dual-refs");
    entry.path = "/wiki/engineering/debugging/dual-refs".to_string();
    entry.summary = "Dual refs export lesson".to_string();
    entry.text = "Dual refs export lesson body.".to_string();
    entry.topic = "dual-refs".to_string();
    entry.metadata = json!({
        "source_refs": ["kckylechen1/tachi#1072"],
        "evidence_refs_v1": [
            {"ref": "kckylechen1/tachi#1072", "target_kind": "issue", "captured_at": "2026-07-17T00:00:00Z"},
            {"ref": "docs/engineering/architecture/issue-refinery-memory-lanes.md", "target_kind": "canonical_doc", "captured_at": "2026-07-17T00:00:00Z"},
        ],
    });

    let (server, _home) = seed_wiki_project_entries(vec![entry]);
    let out_dir =
        crate::utils::test_fixture_path(format!("wiki-export-dual-refs-{}", uuid::Uuid::new_v4()));

    let result = crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir)
        .expect("wiki export should succeed");
    assert_eq!(result["count"], json!(1));
    let md_path = out_dir.join("engineering/debugging/dual-refs/dual-refs.md");
    let markdown = std::fs::read_to_string(&md_path).expect("read exported markdown");
    assert!(
        !markdown.contains("## References\n"),
        "legacy section must be suppressed: {markdown}"
    );
    assert!(
        markdown.contains("## Evidence Refs (typed)"),
        "typed refs section must be selected when both channels exist: {markdown}"
    );
    assert!(
        markdown.contains("docs/engineering/architecture/issue-refinery-memory-lanes.md"),
        "typed ref target must render: {markdown}"
    );
    assert_eq!(markdown.matches("kckylechen1/tachi#1072").count(), 1);
    let _ = std::fs::remove_dir_all(out_dir);
}

#[tokio::test]
async fn wiki_export_obsidian_exports_legacy_only_refs() {
    let mut entry = make_entry("wiki-export-legacy-refs");
    entry.path = "/wiki/export/legacy".to_string();
    entry.topic = "legacy-refs".to_string();
    entry.metadata = json!({"source_refs": ["kckylechen1/tachi#1072"]});
    let (server, _home) = seed_wiki_project_entries(vec![entry]);
    let out_dir =
        crate::utils::test_fixture_path(format!("wiki-export-legacy-{}", uuid::Uuid::new_v4()));
    crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir).unwrap();
    let markdown = std::fs::read_to_string(out_dir.join("export/legacy/legacy-refs.md")).unwrap();
    assert!(markdown.contains("## References\n"));
    assert!(!markdown.contains("## Evidence Refs (typed)"));
    assert_eq!(markdown.matches("kckylechen1/tachi#1072").count(), 1);
    let _ = std::fs::remove_dir_all(out_dir);
}

#[tokio::test]
async fn wiki_export_obsidian_exports_typed_only_refs() {
    let mut entry = make_entry("wiki-export-typed-refs");
    entry.path = "/wiki/export/typed".to_string();
    entry.topic = "typed-refs".to_string();
    entry.metadata = json!({"evidence_refs_v1": [{"ref": "#1296", "target_kind": "issue", "captured_at": "2026-07-19T00:00:00Z"}]});
    let (server, _home) = seed_wiki_project_entries(vec![entry]);
    let out_dir =
        crate::utils::test_fixture_path(format!("wiki-export-typed-{}", uuid::Uuid::new_v4()));
    crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir).unwrap();
    let markdown = std::fs::read_to_string(out_dir.join("export/typed/typed-refs.md")).unwrap();
    assert!(markdown.contains("## Evidence Refs (typed)"));
    assert!(!markdown.contains("## References\n"));
    assert_eq!(markdown.matches("#1296").count(), 1);
    let _ = std::fs::remove_dir_all(out_dir);
}

#[tokio::test]
async fn wiki_export_obsidian_ignores_invalid_typed_refs_and_falls_back_to_legacy() {
    let mut entry = make_entry("wiki-export-invalid-typed-refs");
    entry.path = "/wiki/export/invalid-typed".to_string();
    entry.topic = "invalid-typed-refs".to_string();
    entry.metadata = json!({
        "evidence_refs_v1": [{"ref": "  "}, {"ref": 42}],
        "source_refs": [null, "", "#legacy-valid"]
    });
    let (server, _home) = seed_wiki_project_entries(vec![entry]);
    let out_dir = crate::utils::test_fixture_path(format!(
        "wiki-export-invalid-typed-{}",
        uuid::Uuid::new_v4()
    ));
    crate::wiki_ops::export_wiki_obsidian(&server, "wiki", &out_dir).unwrap();
    let markdown =
        std::fs::read_to_string(out_dir.join("export/invalid-typed/invalid-typed-refs.md"))
            .unwrap();
    assert!(!markdown.contains("## Evidence Refs (typed)"));
    assert!(markdown.contains("## References\n"));
    assert_eq!(markdown.matches("#legacy-valid").count(), 1);
    let _ = std::fs::remove_dir_all(out_dir);
}
