use super::*;

/// #1099: `handoff_leave` (the MCP tool) is retired, so tests that need a
/// promotable handoff memo now seed the store directly — mirroring exactly
/// what `handle_handoff_leave` used to persist (`handoff:<id>`, category
/// "handoff", `status: "pending"` metadata) via the pre-existing
/// `test_entry` helper. `promote_issue` only ever *read* this shape; it
/// never depended on the leave handler itself.
fn seed_pending_memo(server: &MemoryServer, memo: HandoffMemo) -> String {
    let memo_id = memo.id.clone();
    let entry = test_entry(memo);
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| e.to_string()))
        .expect("seed pending handoff memo");
    memo_id
}

fn pending_memo(id: &str, summary: &str, next_steps: Vec<String>) -> HandoffMemo {
    HandoffMemo {
        id: id.to_string(),
        from_agent: "agent-a".to_string(),
        target_agent: Some("next-agent".to_string()),
        summary: summary.to_string(),
        next_steps,
        context: Some(json!({"risk": "medium"})),
        created_at: Utc::now().to_rfc3339(),
        acknowledged: false,
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn promote_handoff_issue_updates_memory_and_flow_artifacts() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
    let db_path =
        crate::utils::test_fixture_path(format!("handoff-promote-{}.sqlite", uuid::Uuid::new_v4()));
    let run_root =
        crate::utils::test_fixture_path(format!("handoff-promote-runs-{}", uuid::Uuid::new_v4()));
    let flow_id = "flow_handoff-promote";
    std::fs::create_dir_all(run_root.join(flow_id)).expect("create run dir");
    std::env::set_var("TACHI_RUN_ROOT", &run_root);

    let server = test_server(db_path.clone());
    let memo_id = seed_pending_memo(
        &server,
        pending_memo(
            "memo-promote-1",
            "Promote this memo",
            vec!["Create a tracked GitHub issue".to_string()],
        ),
    );
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
        Some(memcore::RetentionPolicy::Pinned.as_str())
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
    let db_path = crate::utils::test_fixture_path(format!(
        "handoff-invalid-flow-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = test_server(db_path.clone());
    let memo_id = seed_pending_memo(
        &server,
        pending_memo("memo-invalid-flow", "Invalid flow id test", vec![]),
    );
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

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn promote_dedup_returns_already_promoted_without_force() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
    let db_path =
        crate::utils::test_fixture_path(format!("handoff-dedup-{}.sqlite", uuid::Uuid::new_v4()));
    let run_root =
        crate::utils::test_fixture_path(format!("handoff-dedup-runs-{}", uuid::Uuid::new_v4()));
    std::env::set_var("TACHI_RUN_ROOT", &run_root);

    let server = test_server(db_path.clone());
    let memo_id = seed_pending_memo(
        &server,
        pending_memo("memo-dedup", "Dedup test memo", vec!["step".to_string()]),
    );
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
        crate::utils::test_fixture_path(format!("handoff-force-{}.sqlite", uuid::Uuid::new_v4()));
    let run_root =
        crate::utils::test_fixture_path(format!("handoff-force-runs-{}", uuid::Uuid::new_v4()));
    std::env::set_var("TACHI_RUN_ROOT", &run_root);

    let server = test_server(db_path.clone());
    let memo_id = seed_pending_memo(
        &server,
        pending_memo("memo-force", "Force re-promote test", vec![]),
    );
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
