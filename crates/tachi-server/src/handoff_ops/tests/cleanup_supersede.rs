use super::*;

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
