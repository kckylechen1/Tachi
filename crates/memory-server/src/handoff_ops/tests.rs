use super::*;
use crate::tool_params::AgentRegisterParams;
use rmcp::handler::server::wrapper::Parameters;

fn ensure_test_env() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        std::env::set_var("VOYAGE_API_KEY", "test-voyage-key");
        std::env::set_var("SILICONFLOW_API_KEY", "test-siliconflow-key");
        std::env::set_var("SILICONFLOW_MODEL", "test-model");
        std::env::set_var("SUMMARY_MODEL", "test-summary-model");
    });
}

fn env_lock() -> &'static std::sync::Mutex<()> {
    crate::shell_ops::tachi_run_root_env_lock()
}

fn test_server(db_path: std::path::PathBuf) -> MemoryServer {
    ensure_test_env();
    MemoryServer::new(db_path, None).expect("test memory server")
}

fn test_store() -> MemoryStore {
    MemoryStore::open_in_memory().expect("test memory store")
}

fn test_entry(memo: HandoffMemo) -> MemoryEntry {
    MemoryEntry {
        id: format!("handoff:{}", memo.id),
        path: HANDOFF_PATH.to_string(),
        summary: memo.summary.clone(),
        text: memo.summary.clone(),
        importance: 0.9,
        timestamp: memo.created_at.clone(),
        valid_from: String::new(),
        valid_until: None,
        category: "handoff".to_string(),
        topic: "agent-handoff".to_string(),
        keywords: vec!["handoff".to_string()],
        persons: vec![],
        entities: vec![memo.from_agent.clone()],
        location: String::new(),
        source: "test".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({
            "handoff_memo_id": memo.id,
            "handoff": memo,
            "status": "pending",
        }),
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

#[test]
fn pending_handoff_entries_reads_persisted_memory() {
    let mut store = test_store();
    let memo = HandoffMemo {
        id: "memo-1".to_string(),
        from_agent: "agent-a".to_string(),
        target_agent: Some("agent-b".to_string()),
        summary: "persisted memo".to_string(),
        next_steps: vec!["continue".to_string()],
        context: None,
        created_at: Utc::now().to_rfc3339(),
        acknowledged: false,
    };
    store.upsert(&test_entry(memo)).expect("upsert memo");

    let entries = pending_handoff_entries(&mut store).expect("pending entries");
    assert_eq!(entries.len(), 1);
    let memo = memo_from_entry(&entries[0]);
    assert_eq!(memo.id, "memo-1");
    assert_eq!(memo.from_agent, "agent-a");
    assert_eq!(memo.target_agent.as_deref(), Some("agent-b"));
}

#[test]
fn acknowledge_updates_persisted_handoff_metadata() {
    let mut store = test_store();
    let memo = HandoffMemo {
        id: "memo-ack".to_string(),
        from_agent: "agent-a".to_string(),
        target_agent: Some("agent-b".to_string()),
        summary: "needs ack".to_string(),
        next_steps: vec!["ack".to_string()],
        context: None,
        created_at: Utc::now().to_rfc3339(),
        acknowledged: false,
    };
    let entry = test_entry(memo);
    store.upsert(&entry).expect("upsert memo");

    upsert_acknowledged_entry(&mut store, entry, Some("agent-b")).expect("ack memo");

    let pending = pending_handoff_entries(&mut store).expect("pending entries");
    assert!(pending.is_empty());
    let stored = store
        .get("handoff:memo-ack")
        .expect("get memo")
        .expect("memo exists");
    assert_eq!(stored.metadata["status"], json!("acknowledged"));
    assert_eq!(stored.metadata["acknowledged_by"], json!("agent-b"));
    assert_eq!(stored.metadata["handoff"]["acknowledged"], json!(true));
}

#[tokio::test]
async fn handoff_check_reads_and_acks_persisted_memos_after_restart() {
    let db_path = std::env::temp_dir().join(format!(
        "handoff-persistence-{}.sqlite",
        uuid::Uuid::new_v4()
    ));

    {
        let server = test_server(db_path.clone());
        server
            .agent_register(Parameters(AgentRegisterParams {
                agent_id: "agent-a".to_string(),
                display_name: None,
                capabilities: vec![],
                tool_filter: None,
                rate_limit_rpm: None,
                rate_limit_burst: None,
            }))
            .await
            .expect("register source agent");
        server
            .handoff_leave(Parameters(HandoffLeaveParams {
                summary: "persist across restart".to_string(),
                next_steps: vec!["resume from db".to_string()],
                target_agent: Some("agent-b".to_string()),
                context: Some(json!({"file": "src/lib.rs"})),
            }))
            .await
            .expect("leave handoff");
    }

    let server = test_server(db_path.clone());
    let check = server
        .handoff_check(Parameters(HandoffCheckParams {
            agent_id: Some("agent-b".to_string()),
            acknowledge: true,
        }))
        .await
        .expect("check persisted handoff");
    let check_json: serde_json::Value = serde_json::from_str(&check).expect("check json");
    assert_eq!(check_json["pending_memos"], json!(1));
    assert_eq!(check_json["memos"][0]["from_agent"], json!("agent-a"));
    assert_eq!(
        check_json["memos"][0]["next_steps"],
        json!(["resume from db"])
    );

    let after = server
        .handoff_check(Parameters(HandoffCheckParams {
            agent_id: Some("agent-b".to_string()),
            acknowledge: false,
        }))
        .await
        .expect("check after ack");
    let after_json: serde_json::Value = serde_json::from_str(&after).expect("after json");
    assert_eq!(after_json["pending_memos"], json!(0));

    let stored = server
        .with_global_store_read(|store| {
            let entries = store
                .list_by_path(HANDOFF_PATH, 10, false)
                .map_err(|e| e.to_string())?;
            entries
                .into_iter()
                .next()
                .ok_or_else(|| "missing handoff memory".to_string())
        })
        .expect("read stored handoff");
    assert_eq!(stored.metadata["status"], json!("acknowledged"));
    assert_eq!(stored.metadata["acknowledged_by"], json!("agent-b"));

    let _ = std::fs::remove_file(db_path);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn promote_handoff_issue_updates_memory_and_flow_artifacts() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
    let db_path =
        std::env::temp_dir().join(format!("handoff-promote-{}.sqlite", uuid::Uuid::new_v4()));
    let run_root =
        std::env::temp_dir().join(format!("handoff-promote-runs-{}", uuid::Uuid::new_v4()));
    let flow_id = "flow_handoff-promote";
    std::fs::create_dir_all(run_root.join(flow_id)).expect("create run dir");
    std::env::set_var("TACHI_RUN_ROOT", &run_root);

    let server = test_server(db_path.clone());
    let left = server
        .handoff_leave(Parameters(HandoffLeaveParams {
            summary: "Promote this memo".to_string(),
            next_steps: vec!["Create a tracked GitHub issue".to_string()],
            target_agent: Some("next-agent".to_string()),
            context: Some(json!({"risk": "medium"})),
        }))
        .await
        .expect("leave handoff");
    let left_json: serde_json::Value = serde_json::from_str(&left).expect("leave json");
    let memo_id = left_json["memo_id"].as_str().expect("memo id").to_string();
    let client = crate::gh_safe_merge::MockGhClient::new();

    let promoted = promote_handoff_issue_with_client(
        &server,
        &client,
        HandoffPromoteIssueParams {
            memo_id: memo_id.clone(),
            repo: "owner/repo".to_string(),
            title: None,
            labels: vec!["task".to_string()],
            flow_id: Some(flow_id.to_string()),
            force: false,
        },
    )
    .await
    .expect("promote handoff");
    let promoted_json: serde_json::Value = serde_json::from_str(&promoted).expect("promote json");
    assert_eq!(promoted_json["issue_number"], json!(1000));
    assert_eq!(promoted_json["event_persisted"], json!(true));

    let stored = server
        .with_global_store_read(|store| {
            store
                .get(&format!("handoff:{memo_id}"))
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "missing promoted handoff".to_string())
        })
        .expect("read promoted handoff");
    assert_eq!(stored.metadata["status"], json!("promoted"));
    assert_eq!(stored.metadata["github"]["issue_number"], json!(1000));
    assert_eq!(stored.metadata["acknowledged"], json!(true));
    assert_eq!(stored.metadata["handoff"]["acknowledged"], json!(true));
    assert_eq!(
        stored.retention_policy.as_deref(),
        Some(memory_core::RetentionPolicy::Pinned.as_str())
    );

    let status =
        std::fs::read_to_string(run_root.join(flow_id).join("status.json")).expect("read status");
    assert!(status.contains("https://github.com/owner/repo/issues/1000"));
    let events =
        std::fs::read_to_string(run_root.join(flow_id).join("events.jsonl")).expect("read events");
    assert!(events.contains("github_issue_created"));

    if let Some(root) = original_run_root {
        std::env::set_var("TACHI_RUN_ROOT", root);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
    let _ = std::fs::remove_file(db_path);
    let _ = std::fs::remove_dir_all(run_root);
}

#[tokio::test]
async fn promote_rejects_invalid_flow_id_before_issue_creation() {
    let db_path = std::env::temp_dir().join(format!(
        "handoff-invalid-flow-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = test_server(db_path.clone());
    let left = server
        .handoff_leave(Parameters(HandoffLeaveParams {
            summary: "Invalid flow id test".to_string(),
            next_steps: vec![],
            target_agent: None,
            context: None,
        }))
        .await
        .expect("leave");
    let left_json: serde_json::Value = serde_json::from_str(&left).expect("json");
    let memo_id = left_json["memo_id"].as_str().expect("memo id").to_string();
    let client = crate::gh_safe_merge::MockGhClient::new();

    let err = promote_handoff_issue_with_client(
        &server,
        &client,
        HandoffPromoteIssueParams {
            memo_id,
            repo: "owner/repo".to_string(),
            title: None,
            labels: vec![],
            flow_id: Some("../escape".to_string()),
            force: false,
        },
    )
    .await
    .expect_err("invalid flow id should fail");
    assert!(err.contains("Invalid flow_id"));

    let _ = std::fs::remove_file(db_path);
}

#[test]
fn resolve_from_agent_falls_back_to_profile_env_then_unknown() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let original_profile = std::env::var_os("TACHI_PROFILE");
    std::env::remove_var("TACHI_PROFILE");
    assert_eq!(fallback_agent_id(None), "unknown-agent");

    std::env::set_var("TACHI_PROFILE", "antigravity");
    assert_eq!(fallback_agent_id(None), "antigravity");
    assert_eq!(
        fallback_agent_id(Some("registered".to_string())),
        "registered"
    );

    if let Some(profile) = original_profile {
        std::env::set_var("TACHI_PROFILE", profile);
    } else {
        std::env::remove_var("TACHI_PROFILE");
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn promote_dedup_returns_already_promoted_without_force() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
    let db_path =
        std::env::temp_dir().join(format!("handoff-dedup-{}.sqlite", uuid::Uuid::new_v4()));
    let run_root =
        std::env::temp_dir().join(format!("handoff-dedup-runs-{}", uuid::Uuid::new_v4()));
    std::env::set_var("TACHI_RUN_ROOT", &run_root);

    let server = test_server(db_path.clone());
    let left = server
        .handoff_leave(Parameters(HandoffLeaveParams {
            summary: "Dedup test memo".to_string(),
            next_steps: vec!["step".to_string()],
            target_agent: None,
            context: None,
        }))
        .await
        .expect("leave");
    let left_json: serde_json::Value = serde_json::from_str(&left).expect("json");
    let memo_id = left_json["memo_id"].as_str().expect("memo id").to_string();
    let client = crate::gh_safe_merge::MockGhClient::new();

    // First promote succeeds
    let first = promote_handoff_issue_with_client(
        &server,
        &client,
        HandoffPromoteIssueParams {
            memo_id: memo_id.clone(),
            repo: "owner/repo".to_string(),
            title: None,
            labels: vec![],
            flow_id: None,
            force: false,
        },
    )
    .await
    .expect("first promote");
    let first_json: serde_json::Value = serde_json::from_str(&first).expect("json");
    assert_eq!(first_json["status"], json!("promoted"));

    // Second promote (no force) returns already_promoted
    let second = promote_handoff_issue_with_client(
        &server,
        &client,
        HandoffPromoteIssueParams {
            memo_id: memo_id.clone(),
            repo: "owner/repo".to_string(),
            title: None,
            labels: vec![],
            flow_id: None,
            force: false,
        },
    )
    .await
    .expect("second promote");
    let second_json: serde_json::Value = serde_json::from_str(&second).expect("json");
    assert_eq!(second_json["status"], json!("already_promoted"));
    assert!(second_json["issue_url"].as_str().is_some());
    assert!(second_json["hint"].as_str().unwrap().contains("force=true"));

    if let Some(root) = original_run_root {
        std::env::set_var("TACHI_RUN_ROOT", root);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
    let _ = std::fs::remove_file(db_path);
    let _ = std::fs::remove_dir_all(run_root);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn promote_force_creates_new_issue_even_if_already_promoted() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
    let db_path =
        std::env::temp_dir().join(format!("handoff-force-{}.sqlite", uuid::Uuid::new_v4()));
    let run_root =
        std::env::temp_dir().join(format!("handoff-force-runs-{}", uuid::Uuid::new_v4()));
    std::env::set_var("TACHI_RUN_ROOT", &run_root);

    let server = test_server(db_path.clone());
    let left = server
        .handoff_leave(Parameters(HandoffLeaveParams {
            summary: "Force re-promote test".to_string(),
            next_steps: vec![],
            target_agent: None,
            context: None,
        }))
        .await
        .expect("leave");
    let left_json: serde_json::Value = serde_json::from_str(&left).expect("json");
    let memo_id = left_json["memo_id"].as_str().expect("memo id").to_string();
    let client = crate::gh_safe_merge::MockGhClient::new();

    // First promote
    let first = promote_handoff_issue_with_client(
        &server,
        &client,
        HandoffPromoteIssueParams {
            memo_id: memo_id.clone(),
            repo: "owner/repo".to_string(),
            title: None,
            labels: vec![],
            flow_id: None,
            force: false,
        },
    )
    .await
    .expect("first promote");
    let first_json: serde_json::Value = serde_json::from_str(&first).expect("json");
    let first_issue_number = first_json["issue_number"].as_u64().expect("issue number");

    // Force re-promote creates a new issue with a different number
    let second = promote_handoff_issue_with_client(
        &server,
        &client,
        HandoffPromoteIssueParams {
            memo_id: memo_id.clone(),
            repo: "owner/repo".to_string(),
            title: None,
            labels: vec![],
            flow_id: None,
            force: true,
        },
    )
    .await
    .expect("force promote");
    let second_json: serde_json::Value = serde_json::from_str(&second).expect("json");
    assert_eq!(second_json["status"], json!("promoted"));
    let second_issue_number = second_json["issue_number"].as_u64().expect("issue number");
    assert_ne!(first_issue_number, second_issue_number);

    if let Some(root) = original_run_root {
        std::env::set_var("TACHI_RUN_ROOT", root);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
    let _ = std::fs::remove_file(db_path);
    let _ = std::fs::remove_dir_all(run_root);
}

#[test]
fn promoting_intermediate_state_is_set_before_issue_creation() {
    let mut store = test_store();
    let memo = HandoffMemo {
        id: "memo-promoting".to_string(),
        from_agent: "agent-a".to_string(),
        target_agent: None,
        summary: "test promoting state".to_string(),
        next_steps: vec![],
        context: None,
        created_at: Utc::now().to_rfc3339(),
        acknowledged: false,
    };
    let entry = test_entry(memo);
    store.upsert(&entry).expect("upsert");

    upsert_promoting_entry(&mut store, entry.clone()).expect("mark promoting");
    let stored = store
        .get("handoff:memo-promoting")
        .expect("get")
        .expect("exists");
    assert_eq!(stored.metadata["status"], json!("promoting"));
    assert!(stored.metadata["promoting_at"].as_str().is_some());

    revert_promoting_entry(&mut store, stored, "pending").expect("revert");
    let reverted = store
        .get("handoff:memo-promoting")
        .expect("get")
        .expect("exists");
    assert_eq!(reverted.metadata["status"], json!("pending"));
    assert!(reverted.metadata.get("promoting_at").is_none());
}

#[test]
fn promoted_status_is_not_pending() {
    let mut store = test_store();
    let memo = HandoffMemo {
        id: "memo-promoted".to_string(),
        from_agent: "agent-a".to_string(),
        target_agent: None,
        summary: "promoted memo".to_string(),
        next_steps: vec![],
        context: None,
        created_at: Utc::now().to_rfc3339(),
        acknowledged: false,
    };
    let mut entry = test_entry(memo);
    entry.metadata["status"] = json!("promoted");
    store.upsert(&entry).expect("upsert");

    let entries = pending_handoff_entries(&mut store).expect("pending entries");
    assert!(entries.is_empty());
}

#[test]
fn existing_issue_url_extracts_from_metadata() {
    let mut entry = test_entry(HandoffMemo {
        id: "memo-url".to_string(),
        from_agent: "a".to_string(),
        target_agent: None,
        summary: "s".to_string(),
        next_steps: vec![],
        context: None,
        created_at: Utc::now().to_rfc3339(),
        acknowledged: false,
    });
    assert!(existing_issue_url(&entry).is_none());

    let md = entry.metadata.as_object_mut().expect("obj");
    md.insert(
        "github".into(),
        json!({"issue_url": "https://github.com/o/r/issues/42"}),
    );
    assert_eq!(
        existing_issue_url(&entry).as_deref(),
        Some("https://github.com/o/r/issues/42")
    );
}

#[test]
fn test_gc_expired_handoff_memories() {
    let mut store = test_store();

    let old_ack = HandoffMemo {
        id: "old-ack".to_string(),
        from_agent: "agent-a".to_string(),
        target_agent: None,
        summary: "old ack memo".to_string(),
        next_steps: vec![],
        context: None,
        created_at: (chrono::Utc::now() - chrono::Duration::days(31)).to_rfc3339(),
        acknowledged: true,
    };
    let mut entry_old_ack = test_entry(old_ack);
    entry_old_ack.metadata["status"] = json!("acknowledged");
    store.upsert(&entry_old_ack).expect("upsert old ack");

    let new_ack = HandoffMemo {
        id: "new-ack".to_string(),
        from_agent: "agent-a".to_string(),
        target_agent: None,
        summary: "new ack memo".to_string(),
        next_steps: vec![],
        context: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        acknowledged: true,
    };
    let mut entry_new_ack = test_entry(new_ack);
    entry_new_ack.metadata["status"] = json!("acknowledged");
    store.upsert(&entry_new_ack).expect("upsert new ack");

    let old_pending = HandoffMemo {
        id: "old-pending".to_string(),
        from_agent: "agent-a".to_string(),
        target_agent: None,
        summary: "old pending memo".to_string(),
        next_steps: vec![],
        context: None,
        created_at: (chrono::Utc::now() - chrono::Duration::days(31)).to_rfc3339(),
        acknowledged: false,
    };
    let entry_old_pending = test_entry(old_pending);
    store
        .upsert(&entry_old_pending)
        .expect("upsert old pending");

    let deleted = gc_expired_handoff_memories(&mut store, 30).expect("gc");
    assert_eq!(deleted, 1);

    assert!(store.get("handoff:old-ack").expect("get").is_none());
    assert!(store.get("handoff:new-ack").expect("get").is_some());
    assert!(store.get("handoff:old-pending").expect("get").is_some());
}

#[tokio::test]
async fn test_handoff_leave_supersedes_pending_duplicate() {
    let db_path =
        std::env::temp_dir().join(format!("handoff-dup-test-{}.sqlite", uuid::Uuid::new_v4()));
    let server = test_server(db_path.clone());
    server
        .agent_register(Parameters(AgentRegisterParams {
            agent_id: "agent-a".to_string(),
            display_name: None,
            capabilities: vec![],
            tool_filter: None,
            rate_limit_rpm: None,
            rate_limit_burst: None,
        }))
        .await
        .expect("register agent");

    let first_resp = server
        .handoff_leave(Parameters(HandoffLeaveParams {
            summary: "first pending memo".to_string(),
            next_steps: vec![],
            target_agent: Some("agent-b".to_string()),
            context: None,
        }))
        .await
        .expect("leave first");
    let first_json: serde_json::Value = serde_json::from_str(&first_resp).expect("json");
    let first_id = first_json["memo_id"].as_str().expect("id").to_string();

    let second_resp = server
        .handoff_leave(Parameters(HandoffLeaveParams {
            summary: "second pending memo".to_string(),
            next_steps: vec![],
            target_agent: Some("agent-b".to_string()),
            context: None,
        }))
        .await
        .expect("leave second");
    let second_json: serde_json::Value = serde_json::from_str(&second_resp).expect("json");
    let second_id = second_json["memo_id"].as_str().expect("id").to_string();

    server
        .with_global_store_read(|store| {
            let entry1 = store
                .get_with_options(&format!("handoff:{first_id}"), true)
                .unwrap()
                .unwrap();
            assert!(entry1.archived);
            assert_eq!(entry1.metadata["status"], "superseded");

            let entry2 = store.get(&format!("handoff:{second_id}")).unwrap().unwrap();
            assert!(!entry2.archived);
            assert_eq!(entry2.metadata["status"], "pending");
            Ok(())
        })
        .unwrap();

    let _ = std::fs::remove_file(db_path);
}

#[test]
fn supersede_pending_handoffs_propagates_db_errors() {
    use rusqlite::Connection;

    let db_path = std::env::temp_dir().join(format!(
        "handoff-supersede-db-error-{}.sqlite",
        uuid::Uuid::new_v4()
    ));

    // Seed a pending handoff entry.
    {
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        let memo = HandoffMemo {
            id: "memo-1".to_string(),
            from_agent: "agent-a".to_string(),
            target_agent: Some("agent-b".to_string()),
            summary: "pending".to_string(),
            next_steps: vec![],
            context: None,
            created_at: Utc::now().to_rfc3339(),
            acknowledged: false,
        };
        store.upsert(&test_entry(memo)).expect("seed pending");
    }

    // Reopen the store, then hold a RESERVED lock on the DB so the
    // supersede write fails instead of being swallowed.
    let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");
    let lock_path = db_path.clone();
    let (acquired_tx, acquired_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lock_handle = std::thread::spawn(move || {
        let conn = Connection::open(&lock_path).expect("open lock connection");
        conn.execute_batch("BEGIN IMMEDIATE;")
            .expect("begin immediate");
        let _ = acquired_tx.send(());
        let _ = release_rx.recv();
    });
    acquired_rx.recv().expect("lock acquired");

    let new_memo = HandoffMemo {
        id: "memo-2".to_string(),
        from_agent: "agent-a".to_string(),
        target_agent: Some("agent-b".to_string()),
        summary: "new".to_string(),
        next_steps: vec![],
        context: None,
        created_at: Utc::now().to_rfc3339(),
        acknowledged: false,
    };
    let new_entry = test_entry(new_memo.clone());

    let result = supersede_pending_handoffs(&mut store, &new_memo, &new_entry);
    assert!(
        result.is_err(),
        "expected supersede DB error to propagate, got {result:?}"
    );

    let _ = release_tx.send(());
    let _ = lock_handle.join();
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(db_path.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(db_path.with_extension("sqlite-shm"));
}
