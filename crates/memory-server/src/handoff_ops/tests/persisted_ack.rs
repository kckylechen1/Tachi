use super::*;

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
