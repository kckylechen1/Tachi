use super::*;

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
async fn memory_graph_caps_query_seed_top_k() {
    let server = make_server();

    server
        .with_global_store(|store| {
            for idx in 0..(crate::MAX_SEARCH_TOP_K + 25) {
                let mut entry = make_entry(&format!("graph_cap_{idx}"));
                entry.path = format!("/graph/cap/{idx}");
                entry.topic = "graph cap sentinel".to_string();
                entry.summary = format!("graph cap sentinel summary {idx}");
                entry.text = format!("graph cap sentinel searchable row {idx}");
                entry.keywords = vec!["graph".to_string(), "cap".to_string()];
                store.upsert(&entry).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed graph cap memories");

    let result = server
        .memory_graph(Parameters(MemoryGraphParams {
            memory_id: None,
            query: Some("graph cap sentinel".to_string()),
            path_prefix: Some("/graph/cap".to_string()),
            project: None,
            top_k: 10_000,
            depth: 1,
        }))
        .await
        .expect("memory_graph query should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    assert_eq!(json["status"], json!("completed"));
    assert_eq!(
        json["node_count"].as_u64(),
        Some(crate::MAX_SEARCH_TOP_K as u64)
    );
}
