use super::*;

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

#[tokio::test]
async fn tachi_save_note_writes_markdown_file_and_normalizes_scope() {
    let (server, _temp_home) = make_server_with_temp_home();

    let saved = server
        .tachi_save(Parameters(TachiSaveParams {
            text: "中文 note body for UTF-8 slug safety".to_string(),
            id: None,
            kind: None,
            title: Some("中文 Note 标题".to_string()),
            summary: Some("note summary".to_string()),
            path: Some("brainstorm/demo.md".to_string()),
            importance: Some(0.7),
            category: None,
            keywords: vec!["brainstorm".to_string()],
            entities: Vec::new(),
            scope: Some("note".to_string()),
            project: None,
            domain: None,
            retention_policy: None,
            force: true,
            topic: Some("notes-test".to_string()),
            source: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            files: Vec::new(),
        }))
        .await
        .expect("tachi_save note should succeed");
    let json: serde_json::Value = serde_json::from_str(&saved).expect("save JSON");
    let note_file = json["note_file"].as_str().expect("note file returned");
    let note_path = json["note_path"].as_str().expect("note path returned");
    assert_eq!(note_path, "brainstorm/demo.md");
    let md = std::fs::read_to_string(note_file).expect("note markdown should exist");
    assert!(md.contains("title: \"中文 Note 标题\""));
    assert!(md.contains("中文 note body for UTF-8 slug safety"));

    let id = json["id"].as_str().expect("memory id returned").to_string();
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get note memory");
    let fetched_json: serde_json::Value = serde_json::from_str(&fetched).expect("memory JSON");
    assert_eq!(
        fetched_json["path"],
        serde_json::json!("/notes/brainstorm/demo.md")
    );
    assert_ne!(fetched_json["scope"], serde_json::json!("note"));
}

#[tokio::test]
async fn tachi_save_note_rejects_paths_outside_notes_root() {
    let (server, _temp_home) = make_server_with_temp_home();

    for bad_path in ["/tmp/escape.md", "../escape.md", "brainstorm/../escape.md"] {
        let err = server
            .tachi_save(Parameters(TachiSaveParams {
                text: "bad path should not be saved".to_string(),
                id: None,
                kind: Some("note".to_string()),
                title: Some("bad path".to_string()),
                summary: None,
                path: Some(bad_path.to_string()),
                importance: None,
                category: None,
                keywords: Vec::new(),
                entities: Vec::new(),
                scope: None,
                project: None,
                domain: None,
                retention_policy: None,
                force: true,
                topic: None,
                source: None,
                valid_from: None,
                valid_until: None,
                metadata: None,
                files: Vec::new(),
            }))
            .await
            .expect_err("invalid note path should be rejected");
        assert!(
            err.contains("relative") || err.contains("notes root"),
            "unexpected error for {bad_path}: {err}"
        );
    }
}

#[tokio::test]
async fn tachi_memory_save_with_title_stays_memory() {
    let server = make_server();

    let saved = server
        .tachi_memory(Parameters(TachiMemoryParams {
            action: "save".to_string(),
            format: None,
            query: None,
            scope: None,
            top_k: 6,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: Some("fact".to_string()),
            include_archived: false,
            enable_rerank: false,
            as_of: None,
            synthesize: false,
            model: None,
            text: Some("A one-line memory fact with a title should not become wiki.".to_string()),
            title: Some("Memory title only".to_string()),
            summary: Some("Memory title only".to_string()),
            topic: Some("memory-boundary".to_string()),
            keywords: vec!["memory-boundary".to_string()],
            entities: Vec::new(),
            importance: Some(0.7),
            retention_policy: None,
            kind: None,
            path: Some("/facts/memory-boundary".to_string()),
            id: None,
            force: true,
            source: None,
            valid_from: None,
            valid_until: None,
            flow_id: None,
            event: None,
            state: None,
            project: None,
            domain: None,
            metadata: None,
            files: Vec::new(),
        }))
        .await
        .expect("tachi_memory save should succeed");
    assert!(saved.contains("Saved ->"));
    assert!(saved.contains("/facts/memory-boundary"));
    let id = saved
        .split("id: `")
        .nth(1)
        .and_then(|rest| rest.split('`').next())
        .expect("save markdown id")
        .to_string();
    assert!(!id.is_empty());
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get memory");
    let fetched_json: serde_json::Value = serde_json::from_str(&fetched).expect("memory JSON");
    assert_eq!(fetched_json["path"], json!("/facts/memory-boundary"));
    assert_ne!(fetched_json["metadata"]["wiki"], json!(true));
}

#[tokio::test]
async fn tachi_search_memory_scope_excludes_wiki_rows() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut memory = make_entry("plain-memory-row");
            memory.path = "/facts/plain".to_string();
            memory.text = "UniqueBoundaryNeedle belongs in plain memory.".to_string();
            memory.summary = "Plain memory row".to_string();
            store.upsert(&memory).map_err(|e| e.to_string())?;

            let mut wiki = make_entry("wiki-row-should-not-appear");
            wiki.path = "/wiki/general/boundary".to_string();
            wiki.text = "UniqueBoundaryNeedle belongs in wiki.".to_string();
            wiki.summary = "Wiki row".to_string();
            wiki.domain = Some("wiki".to_string());
            wiki.metadata = json!({"wiki": true});
            store.upsert(&wiki).map_err(|e| e.to_string())
        })
        .expect("seed boundary entries");

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniqueBoundaryNeedle".to_string(),
            scope: "memory".to_string(),
            top_k: 5,
            path_prefix: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("memory scoped search");

    assert!(response.contains("plain-memory-row"));
    assert!(!response.contains("wiki-row-should-not-appear"));
}

#[tokio::test]
async fn tachi_search_surfaces_referenced_files() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut memory = make_entry("row-with-files");
            memory.path = "/facts/spec-pointer".to_string();
            memory.text = "UniqueFilesNeedle references a spec.".to_string();
            memory.summary = "Row with referenced files".to_string();
            memory.metadata = json!({
                "files": ["docs/SPEC.md", "crates/memory-server/src/lib.rs"]
            });
            store.upsert(&memory).map_err(|e| e.to_string())?;

            let mut bare = make_entry("row-without-files");
            bare.path = "/facts/no-pointer".to_string();
            bare.text = "UniqueFilesNeedle without any files.".to_string();
            bare.summary = "Row without files".to_string();
            store.upsert(&bare).map_err(|e| e.to_string())
        })
        .expect("seed referenced-files entries");

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniqueFilesNeedle".to_string(),
            scope: "memory".to_string(),
            top_k: 5,
            path_prefix: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("memory scoped search");

    assert!(
        response.contains("📎"),
        "expected referenced-files marker in: {response}"
    );
    assert!(
        response.contains("docs/SPEC.md"),
        "expected referenced file path in: {response}"
    );
}

#[tokio::test]
async fn search_memory_boosts_guide_rows_by_context() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut guide = make_entry("guide-context-boosted");
            guide.path = "/guide/fix_pattern/rust-build".to_string();
            guide.category = "guide".to_string();
            guide.text = "UniqueGuideContextNeedle operational fix pattern.".to_string();
            guide.summary = "Guide row".to_string();
            guide.metadata = json!({
                "guide": true,
                "guide_type": "fix_pattern",
                "file_patterns": ["crates/memory-server/src/*.rs"],
                "error_patterns": ["linker error"]
            });
            store.upsert(&guide).map_err(|e| e.to_string())?;

            let mut memory = make_entry("plain-context-result");
            memory.path = "/facts/plain-context".to_string();
            memory.text = "UniqueGuideContextNeedle plain memory row.".to_string();
            memory.summary = "Plain row".to_string();
            store.upsert(&memory).map_err(|e| e.to_string())
        })
        .expect("seed guide context entries");

    let response = server
        .search_memory(Parameters(SearchMemoryParams {
            query: "UniqueGuideContextNeedle".to_string(),
            query_vec: None,
            top_k: 2,
            path_prefix: None,
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            agent_role: None,
            project: None,
            domain: None,
            file_context: Some("crates/memory-server/src/tools.rs".to_string()),
            error_context: Some("linker error: could not find native static library".to_string()),
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("search memory with guide context");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&response).expect("search JSON");
    assert_eq!(rows[0]["id"], json!("guide-context-boosted"));
}

#[test]
fn write_note_file_falls_back_when_slug_has_no_ascii_tokens() {
    let _temp_home = TempHomeGuard::new();

    let (abs_path, rel_path) = crate::notes_ops::write_note_file(
        "body for non-ascii title",
        None,
        Some("中文 标题"),
        Some("notes-test"),
        Some("note"),
        &[],
    )
    .expect("write note file");

    assert!(abs_path.exists(), "note path should exist: {abs_path:?}");
    assert!(
        rel_path.starts_with("inbox/"),
        "unexpected note path: {rel_path}"
    );
    assert!(
        rel_path.ends_with("-note.md"),
        "non-ascii title should use note slug fallback: {rel_path}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn tachi_save_note_rejects_symlink_leaf() {
    let (server, temp_home) = make_server_with_temp_home();
    let notes_dir = temp_home.temp_home.join(".tachi/notes/brainstorm");
    std::fs::create_dir_all(&notes_dir).expect("create notes dir");
    let outside = temp_home.temp_home.join("outside.md");
    std::fs::write(&outside, "outside").expect("write outside target");
    std::os::unix::fs::symlink(&outside, notes_dir.join("escape.md")).expect("create symlink");

    let err = server
        .tachi_save(Parameters(TachiSaveParams {
            text: "must not follow symlink".to_string(),
            id: None,
            kind: Some("note".to_string()),
            title: Some("symlink leaf".to_string()),
            summary: None,
            path: Some("brainstorm/escape.md".to_string()),
            importance: None,
            category: None,
            keywords: Vec::new(),
            entities: Vec::new(),
            scope: None,
            project: None,
            domain: None,
            retention_policy: None,
            force: true,
            topic: None,
            source: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            files: Vec::new(),
        }))
        .await
        .expect_err("symlink note leaf should be rejected");
    assert!(
        err.contains("notes root"),
        "unexpected symlink rejection error: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&outside).expect("outside target should remain readable"),
        "outside"
    );
}

#[tokio::test]
async fn save_memory_includes_provenance_for_registered_agent() {
    let server = make_server();

    server
        .agent_register(Parameters(AgentRegisterParams {
            agent_id: "claude-code".to_string(),
            display_name: Some("Claude Code".to_string()),
            capabilities: vec!["code-gen".to_string()],
            tool_filter: None,
            rate_limit_rpm: None,
            rate_limit_burst: None,
        }))
        .await
        .expect("agent_register should succeed");

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "Investigated the failing OAuth callback edge case.".to_string(),
            summary: "OAuth callback investigation".to_string(),
            path: "/project/auth".to_string(),
            importance: 0.8,
            category: "fact".to_string(),
            topic: "auth".to_string(),
            keywords: vec!["oauth".to_string(), "callback".to_string()],
            persons: vec![],
            entities: vec!["oauth".to_string()],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
        }))
        .await
        .expect("save_memory should succeed");
    let saved_json: serde_json::Value = serde_json::from_str(&saved).expect("save JSON");
    let id = saved_json["id"]
        .as_str()
        .expect("save_memory should return id")
        .to_string();

    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: serde_json::Value = serde_json::from_str(&fetched).expect("get JSON");
    let provenance = &fetched_json["metadata"]["provenance"];

    assert_eq!(provenance["tool_name"], json!("save_memory"));
    assert_eq!(provenance["source_kind"], json!("memory_write"));
    assert_eq!(provenance["requested_scope"], json!("project"));
    assert_eq!(provenance["db_scope"], json!("global"));
    assert_eq!(provenance["agent"]["agent_id"], json!("claude-code"));
}

#[tokio::test]
async fn save_memory_allows_curated_tier_metadata() {
    let server = make_server();

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "Curated trading lessons should enter the lifecycle as consolidated knowledge."
                .to_string(),
            summary: "Curated lifecycle tier".to_string(),
            path: "/trading/equity/lessons/tier-test".to_string(),
            importance: 0.85,
            category: "experience".to_string(),
            topic: "memory-lifecycle".to_string(),
            keywords: vec!["tier".to_string()],
            persons: vec![],
            entities: vec!["Tachi".to_string()],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            retention_policy: Some("permanent".to_string()),
            domain: Some("equity_trading".to_string()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: Some(json!({"tier": "consolidated"})),
        }))
        .await
        .expect("save_memory should succeed");
    let saved_json: serde_json::Value = serde_json::from_str(&saved).expect("save JSON");
    let id = saved_json["id"].as_str().expect("id").to_string();

    let tier = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row("SELECT tier FROM memories WHERE id = ?1", [&id], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|e| e.to_string())
        })
        .expect("read tier");
    assert_eq!(tier, "consolidated");
}

#[tokio::test]
async fn memory_gc_prunes_expired_resolved_kanban_cards() {
    std::env::set_var("KANBAN_CLASSIFY_ENABLED", "false");
    let server = make_server();

    let post = server
        .post_card(Parameters(PostCardParams {
            from_agent: "hapi".to_string(),
            to_agent: "iris".to_string(),
            title: "Old resolved card".to_string(),
            body: "Can be pruned".to_string(),
            priority: "medium".to_string(),
            card_type: "request".to_string(),
            thread_id: None,
            workspace_id: None,
            project_id: None,
            conversation_id: None,
            agent_session_id: None,
        }))
        .await
        .expect("post_card should succeed");
    let post_json: serde_json::Value =
        serde_json::from_str(&post).expect("post_card response should be JSON");
    let card_id = post_json["card_id"]
        .as_str()
        .expect("post_card should return card_id")
        .to_string();

    server
        .update_card(Parameters(UpdateCardParams {
            card_id: card_id.clone(),
            new_status: "resolved".to_string(),
            response_text: None,
        }))
        .await
        .expect("update_card should succeed");

    let stale_timestamp = (Utc::now() - chrono::Duration::days(31)).to_rfc3339();
    server
        .with_global_store(|store| {
            store
                .connection()
                .execute(
                    "UPDATE memories SET timestamp = ?1 WHERE id = ?2",
                    (&stale_timestamp, &card_id),
                )
                .map_err(|e| format!("stale kanban timestamp update failed: {e}"))?;
            Ok(())
        })
        .expect("failed to age kanban card");

    let gc = server.memory_gc().await.expect("memory_gc should succeed");
    let gc_json: serde_json::Value =
        serde_json::from_str(&gc).expect("memory_gc response should be JSON");
    assert_eq!(gc_json["global"]["kanban_cards_pruned"], json!(1));

    let remaining = server
        .with_global_store_read(|store| {
            store
                .get_with_options(&card_id, true)
                .map_err(|e| format!("failed to fetch kanban card after GC: {e}"))
        })
        .expect("kanban fetch after GC should succeed");
    assert!(
        remaining.is_none(),
        "expired resolved kanban card should be deleted"
    );
}

#[tokio::test]
async fn get_memory_reports_pending_when_neither_summary_nor_vector_present() {
    let server = make_server();

    // Save a freshly-built entry: summary empty, vector None — the on-disk
    // shape immediately after a synchronous write but before the enrichment
    // batcher has flushed.
    let id = format!("pending-{}", uuid::Uuid::new_v4());
    let entry = make_entry(&id);
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("save: {e}")))
        .expect("save entry");

    let body = crate::memory_ops::handle_get_memory(
        &server,
        crate::tool_params::GetMemoryParams {
            id: id.clone(),
            include_archived: false,
            project: None,
        },
    )
    .await
    .expect("get_memory");

    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["embedding_pending"], json!(true), "body: {body}");
    assert_eq!(v["summary_pending"], json!(true), "body: {body}");
    // No foundry job has been queued for this id — the field must be absent.
    assert!(
        v.get("foundry_jobs").is_none(),
        "expected no foundry_jobs field, got: {body}"
    );
}

#[tokio::test]
async fn get_memory_reports_complete_when_summary_and_vector_present() {
    let server = make_server();

    let id = format!("complete-{}", uuid::Uuid::new_v4());
    let mut entry = make_entry(&id);
    entry.summary = "a brief precomputed summary".to_string();
    entry.vector = Some(vec![0.0_f32; 1024]); // schema requires 1024-dim vectors
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("save: {e}")))
        .expect("save entry");

    let body = crate::memory_ops::handle_get_memory(
        &server,
        crate::tool_params::GetMemoryParams {
            id: id.clone(),
            include_archived: false,
            project: None,
        },
    )
    .await
    .expect("get_memory");

    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["embedding_pending"], json!(false), "body: {body}");
    assert_eq!(v["summary_pending"], json!(false), "body: {body}");
}

#[tokio::test]
async fn stuck_detection_no_warning_below_threshold() {
    let server = make_server();

    // First two identical calls: no warning, hard block far away.
    for i in 0..2 {
        let warn = server
            .check_rate_limit("save_memory", "hash-pre", "session-soft")
            .unwrap_or_else(|e| panic!("call {} should succeed: {:?}", i + 1, e));
        assert!(
            warn.is_none(),
            "call {} should not carry a stuck warning, got: {:?}",
            i + 1,
            warn
        );
    }
}

#[tokio::test]
async fn stuck_detection_emits_warning_from_third_call_through_seventh() {
    let server = make_server();

    // Calls 1 and 2: no warning.
    for _ in 0..2 {
        let warn = server
            .check_rate_limit("save_memory", "hash-warn", "session-soft-2")
            .expect("call should succeed");
        assert!(warn.is_none());
    }

    // Calls 3 through 7 (5 calls): each succeeds and carries a warning.
    // Hard block triggers on call 9 (default burst = 8 means stamps.len() >= 8).
    for upcoming in 3u64..=7 {
        let warn = server
            .check_rate_limit("save_memory", "hash-warn", "session-soft-2")
            .unwrap_or_else(|e| panic!("call {upcoming} should succeed: {:?}", e));
        let msg =
            warn.unwrap_or_else(|| panic!("call {upcoming} should carry a soft stuck warning"));
        assert!(
            msg.contains("stuck-detection"),
            "warning text should include the 'stuck-detection' tag, got: {msg}"
        );
        assert!(
            msg.contains("save_memory"),
            "warning should mention the tool name, got: {msg}"
        );
        assert!(
            msg.contains(&format!("called {upcoming} times")),
            "warning should report current count {upcoming}, got: {msg}"
        );
        assert!(
            msg.contains("tachi_progress_check"),
            "warning should suggest tachi_progress_check, got: {msg}"
        );
    }
}

#[tokio::test]
async fn stuck_detection_warning_disappears_at_hard_block() {
    let server = make_server();

    // Burn through 8 successful calls (calls 3..=7 carry warnings, calls 1,2,8 do not).
    // Wait — at call 8 (upcoming_count=8), upcoming_count == effective_burst(8),
    // so the soft-warning condition `upcoming < effective_burst` is false. Verify.
    for upcoming in 1u64..=8 {
        let warn = server
            .check_rate_limit("save_memory", "hash-hard", "session-hard")
            .unwrap_or_else(|e| panic!("call {upcoming} should succeed: {:?}", e));
        if (3..=7).contains(&upcoming) {
            assert!(
                warn.is_some(),
                "call {upcoming} should carry a soft warning"
            );
        } else {
            assert!(
                warn.is_none(),
                "call {upcoming} should NOT carry a soft warning, got: {:?}",
                warn
            );
        }
    }

    // 9th identical call → hard block.
    let err = server
        .check_rate_limit("save_memory", "hash-hard", "session-hard")
        .expect_err("9th identical call should hit the hard loop block");
    assert!(err.message.contains("Loop detected"));
}

#[tokio::test]
async fn memory_graph_returns_seed_nodes_and_edges() {
    let server = make_server();
    let mut a = make_entry("m_a");
    a.topic = "alpha".to_string();
    a.text = "Alpha memory".to_string();
    let mut b = make_entry("m_b");
    b.topic = "beta".to_string();
    b.text = "Beta memory".to_string();
    let mut c = make_entry("m_c");
    c.topic = "gamma".to_string();
    c.text = "Gamma memory".to_string();

    server
        .with_global_store(|store| {
            store.upsert(&a).map_err(|e| e.to_string())?;
            store.upsert(&b).map_err(|e| e.to_string())?;
            store.upsert(&c).map_err(|e| e.to_string())?;
            store
                .add_edge(&memory_core::MemoryEdge {
                    source_id: "m_a".to_string(),
                    target_id: "m_b".to_string(),
                    relation: "related_to".to_string(),
                    weight: 1.0,
                    metadata: json!({}),
                    created_at: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_to: None,
                })
                .map_err(|e| e.to_string())?;
            store
                .add_edge(&memory_core::MemoryEdge {
                    source_id: "m_b".to_string(),
                    target_id: "m_c".to_string(),
                    relation: "supports".to_string(),
                    weight: 0.8,
                    metadata: json!({}),
                    created_at: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_to: None,
                })
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed graph");

    let result = server
        .memory_graph(Parameters(MemoryGraphParams {
            memory_id: Some("m_a".to_string()),
            query: None,
            path_prefix: None,
            project: None,
            top_k: 3,
            depth: 2,
        }))
        .await
        .expect("memory_graph should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    assert_eq!(json["status"], json!("completed"));
    assert!(json["node_count"].as_u64().unwrap_or(0) >= 2);
    assert!(json["edge_count"].as_u64().unwrap_or(0) >= 1);
}

#[tokio::test]
async fn save_memory_clamps_importance_into_valid_range() {
    let server = make_server();

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "importance clamp regression".to_string(),
            summary: "importance clamp".to_string(),
            path: "/project/tests".to_string(),
            importance: 9.9,
            category: "fact".to_string(),
            topic: "testing".to_string(),
            keywords: vec!["importance".to_string()],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
        }))
        .await
        .expect("save_memory should succeed");

    let saved_json: serde_json::Value =
        serde_json::from_str(&saved).expect("save should be valid JSON");
    let id = saved_json["id"].as_str().expect("save should return id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: serde_json::Value =
        serde_json::from_str(&fetched).expect("get should be valid JSON");
    assert_eq!(fetched_json["importance"], json!(1.0));
}

#[tokio::test]
async fn save_memory_redacts_obvious_secrets_before_persisting() {
    let server = make_server();

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "The bearer is Authorization: Bearer test-bearer-token-abcdefghijklmnopqrstuvwxyz and token=secret_token_value_abcdefghijklmnopqrstuvwxyz".to_string(),
            summary: "redaction".to_string(),
            path: "/scratch/redaction".to_string(),
            importance: 0.9,
            category: "fact".to_string(),
            topic: "testing".to_string(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
        }))
        .await
        .expect("save_memory should succeed");

    let saved_json: Value = serde_json::from_str(&saved).expect("save JSON");
    assert!(saved_json["secret_redactions"].as_u64().unwrap_or(0) >= 2);
    let id = saved_json["id"].as_str().expect("saved id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: Value = serde_json::from_str(&fetched).expect("get JSON");
    let text = fetched_json["text"].as_str().unwrap_or_default();
    assert!(text.contains("[REDACTED]"));
    assert!(!text.contains("test-bearer-token-abcdefghijklmnopqrstuvwxyz"));
    assert!(!text.contains("secret_token_value_abcdefghijklmnopqrstuvwxyz"));
}

#[tokio::test]
async fn save_memory_scrubs_think_tags_from_text_and_summary() {
    let server = make_server();

    let saved = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "Keep this.\n<think>private reasoning\nwith details</think>\nAnd this."
                .to_string(),
            summary: "Summary <think>hidden</think> visible".to_string(),
            path: "/scratch/tests/think-scrub".to_string(),
            importance: 0.7,
            category: "fact".to_string(),
            topic: "testing".to_string(),
            keywords: vec!["think-scrub".to_string()],
            persons: vec![],
            entities: vec!["memory-server".to_string()],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
        }))
        .await
        .expect("save_memory should succeed");

    let saved_json: Value = serde_json::from_str(&saved).expect("save JSON");
    let id = saved_json["id"].as_str().expect("saved id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched_json: Value = serde_json::from_str(&fetched).expect("get JSON");
    let text = fetched_json["text"].as_str().unwrap_or_default();
    let summary = fetched_json["summary"].as_str().unwrap_or_default();
    assert!(text.contains("Keep this."));
    assert!(text.contains("And this."));
    assert!(!text.contains("<think>"));
    assert!(!text.contains("private reasoning"));
    assert_eq!(summary, "Summary  visible");
}

#[tokio::test]
async fn save_memory_noise_rejection_returns_structured_json() {
    let server = make_server();

    let response = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "hello".to_string(),
            summary: String::new(),
            path: "/scratch/tests/noise".to_string(),
            importance: 0.7,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: false,
            auto_link: false,
            project: None,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
        }))
        .await
        .expect("noise rejection should be a normal JSON response");

    let json: Value = serde_json::from_str(&response).expect("noise JSON");
    assert_eq!(json["saved"], json!(false));
    assert_eq!(json["noise"], json!(true));
}

#[test]
fn strip_code_fence_uses_last_closing_fence() {
    let raw = "```json\n{\"outer\":\"ok\",\"inner\":\"```json\\n{}\\n```\"}\n```";
    let stripped = crate::llm::LlmClient::strip_code_fence(raw);
    assert_eq!(
        stripped,
        "{\"outer\":\"ok\",\"inner\":\"```json\\n{}\\n```\"}"
    );
}

#[test]
fn fact_to_entry_merges_legacy_persons_into_entities() {
    let fact = json!({
        "text": "Kyle migrated Sigil search completely and successfully.",
        "topic": "migration",
        "keywords": ["sigil", "search"],
        "persons": ["Kyle", ""],
        "entities": ["Sigil", "memory-server"],
        "scope": "project",
        "importance": 0.9
    });

    let entry = crate::tool_params::fact_to_entry(&fact, "extraction", json!({}))
        .expect("fact_to_entry should build an entry");
    assert!(entry.persons.is_empty());
    assert_eq!(
        entry.entities,
        vec![
            "Sigil".to_string(),
            "memory-server".to_string(),
            "Kyle".to_string(),
            "user".to_string()
        ]
    );
}

/// Regression for "auto-link bumps access stats" bug.
///
/// `save_memory` with `auto_link: true` spawns a background search keyed by
/// each entity to discover related memories. That search is a write-side side
/// effect, NOT a user read — it must use `SearchOptions { record_access: false,
/// .. }` so the matched-but-not-actually-read entries do not get their
/// `access_count` / `last_access` bumped (which would inflate ACT-R frequency,
/// suppress the `access_count = 0` GC prune path, and bias promotion ranking).
#[tokio::test]
async fn save_memory_auto_link_does_not_bump_target_access_count() {
    let server = make_server();

    // Seed an entry tagged with the entity we will later search via auto-link.
    let seeded_id = format!("auto-link-target-{}", uuid::Uuid::new_v4());
    let mut seeded = make_entry(&seeded_id);
    seeded.entities = vec!["sigil".to_string()];
    seeded.text = "Original notes about sigil internals".to_string();
    server
        .with_global_store(|store| store.upsert(&seeded).map_err(|e| format!("seed: {e}")))
        .expect("seed entry");

    // Sanity: freshly-written entry has access_count = 0.
    let pre = server
        .with_global_store_read(|store| {
            store
                .get_with_options(&seeded_id, false)
                .map_err(|e| format!("get: {e}"))
        })
        .expect("pre-read")
        .expect("seeded entry exists");
    assert_eq!(
        pre.access_count, 0,
        "fresh seed should have access_count=0, got {}",
        pre.access_count
    );
    assert!(
        pre.last_access.is_none(),
        "fresh seed should have no last_access"
    );

    // Trigger save_memory with auto_link=true and an entity that matches the
    // seeded entry. Auto-link will search global store for "sigil", which will
    // return the seeded entry as a candidate.
    let _ = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "New observation about sigil rotation".to_string(),
            summary: String::new(),
            path: "/".to_string(),
            importance: 0.7,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: vec![],
            persons: vec![],
            entities: vec!["sigil".to_string()],
            location: String::new(),
            scope: "general".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: true,
            project: None,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
        }))
        .await
        .expect("save_memory should succeed");

    // Auto-link runs in tokio::spawn; give it time to execute the search.
    // 500ms is generous for a sync sqlite read against an in-memory test DB.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Re-read the seeded entry. Its access_count MUST still be 0 — auto-link
    // is a write-side side effect, not a user read.
    let post = server
        .with_global_store_read(|store| {
            store
                .get_with_options(&seeded_id, false)
                .map_err(|e| format!("get: {e}"))
        })
        .expect("post-read")
        .expect("seeded entry still exists");

    assert_eq!(
        post.access_count, 0,
        "auto-link search must NOT bump access_count of matched entries; got {}",
        post.access_count
    );
    assert!(
        post.last_access.is_none(),
        "auto-link search must NOT set last_access on matched entries; got {:?}",
        post.last_access
    );
}
