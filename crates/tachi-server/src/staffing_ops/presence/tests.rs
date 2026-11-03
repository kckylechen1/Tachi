use super::*;
use rmcp::handler::server::wrapper::Parameters;

fn assignment(flow: &str) -> Assignment {
    Assignment {
        summary: "Review the Staffing adapter".into(),
        issue_ref: Some("#2178".into()),
        flow_id: Some(flow.into()),
    }
}
fn items(value: &Value) -> &[Value] {
    value["items"].as_array().unwrap()
}

#[tokio::test]
async fn roster_and_board_share_observations_and_flow_assignments() {
    let (server, _) = crate::tests::make_server_with_project_fixture("presence-alpha");
    let session = server.clone_for_mcp_session();
    session.set_session_identity(
        Some("codex-client".into()),
        Some("presence-alpha".into()),
        None,
    );
    session.touch_activity();
    let (worker, guard) =
        register_worker(&session, "run-1", "opencode", None, assignment("flow-a")).unwrap();
    observe_runner(Some(worker), async {
        worker_event("busy", "process_spawned");
    })
    .await;
    let (_, other_guard) =
        register_worker(&session, "run-2", "codex", None, assignment("flow-b")).unwrap();
    let params = serde_json::from_value(json!({"action":"roster","format":"json"})).unwrap();
    let raw = session.tachi_staff(Parameters(params)).await.unwrap();
    // The facade renderer may wrap the projection; the shared body stays identical.
    assert!(raw.contains("run-1") && raw.contains("process_spawned"));
    let before = roster(&session, None, None, 20).unwrap();
    let board: Value = serde_json::from_str(
        &crate::dispatch_ops::handle_tachi_board(
            &session,
            serde_json::from_value(json!({"project":"presence-alpha"})).unwrap(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(board["staff"], before);
    let filtered = roster(&session, None, Some("flow-a"), 20).unwrap();
    assert!(!filtered.to_string().contains("run-2"));
    assert!(filtered.to_string().contains("#2178"));
    assert_eq!(
        items(&filtered)
            .iter()
            .find(|r| r["kind"] == "worker")
            .unwrap()["state"],
        "busy"
    );
    drop(guard);
    drop(other_guard);
    assert!(items(&roster(&session, None, None, 20).unwrap())
        .iter()
        .filter(|r| r["kind"] == "worker")
        .all(|r| r["state"] == "disconnected"));
}

#[test]
fn sightings_expire_without_fabricating_readiness_and_refresh_keeps_identity() {
    let (server, _) = crate::tests::make_server_with_project_fixture("presence-alpha");
    server.set_session_identity(Some("codex".into()), Some("presence-alpha".into()), None);
    server.touch_activity();
    let before = roster(&server, None, None, 20).unwrap();
    let id = before["items"][0]["presence_id"].clone();
    assert_eq!(before["items"][0]["state"], "recently_seen");
    assert!(before["items"][0]["accepts_new_work"].is_null());
    for row in server.staff_presence.rows.lock().unwrap().values_mut() {
        row.observed_at = Utc::now() - chrono::Duration::seconds(CLIENT_TTL_SECONDS + 1);
    }
    assert_eq!(
        roster(&server, None, None, 20).unwrap()["items"][0]["state"],
        "stale"
    );
    server.touch_activity();
    let after = roster(&server, None, None, 20).unwrap();
    assert_eq!(after["items"][0]["presence_id"], id);
    assert_eq!(after["items"][0]["state"], "recently_seen");
}

#[test]
fn projection_is_project_scoped_and_reports_missing_and_zero_inputs() {
    let (server, _) = crate::tests::make_server_with_project_fixture("presence-alpha");
    assert_eq!(
        roster(&server, None, None, 20).unwrap()["reason"],
        "project_binding_required"
    );
    assert!(register_worker(&server, "no-project", "codex", None, assignment("flow-a")).is_none());
    server.set_session_identity(Some("codex".into()), Some("presence-alpha".into()), None);
    let (_, guard) =
        register_worker(&server, "run-1", "codex", None, assignment("flow-a")).unwrap();
    assert!(items(&roster(&server, None, None, 0).unwrap()).is_empty());
    let foreign = server.clone_for_mcp_session();
    foreign.set_session_identity(
        Some("opencode".into()),
        Some("missing-project".into()),
        None,
    );
    assert!(roster(&foreign, Some("presence-alpha"), None, 20).is_err());
    let before = server.staff_presence.rows.lock().unwrap().len();
    for raw in [
        json!({"action":"roster","task":"launch"}),
        json!({"action":"roster","worker":"codex"}),
        json!({"action":"roster","dispatch_id":"run-1"}),
    ] {
        assert!(staff_roster(&server, &serde_json::from_value(raw).unwrap()).is_err());
    }
    assert_eq!(server.staff_presence.rows.lock().unwrap().len(), before);
    drop(guard);
}

#[tokio::test]
async fn cancellation_drops_presence_owner_and_acp_readiness_is_retained_as_evidence() {
    let (server, _) = crate::tests::make_server_with_project_fixture("presence-alpha");
    let (observer, guard) = register_worker(
        &server,
        "run-1",
        "codex",
        Some("presence-alpha"),
        assignment("flow-a"),
    )
    .unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(observe_runner(Some(observer), async move {
        let _guard = guard;
        worker_event("ready", "acp_session_initialized");
        worker_event("busy", "acp_prompt_sent");
        tx.send(()).unwrap();
        std::future::pending::<()>().await;
    }));
    rx.await.unwrap();
    let live = roster(&server, Some("presence-alpha"), None, 20).unwrap();
    assert_eq!(live["items"][0]["state"], "busy");
    assert!(!live["items"][0]["ready_observed_at"].is_null());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(
        roster(&server, Some("presence-alpha"), None, 20).unwrap()["items"][0]["state"],
        "disconnected"
    );
}

#[test]
fn registry_and_output_are_bounded_and_claim_read_failure_is_visible() {
    let (server, _) = crate::tests::make_server_with_project_fixture("presence-alpha");
    server.set_session_identity(
        Some("codex\n<instruction>".into()),
        Some("presence-alpha".into()),
        None,
    );
    server.touch_activity();
    let original = server
        .staff_presence
        .rows
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    for index in 0..CAPACITY + 5 {
        let mut row = original.clone();
        row.id = format!("row-{index}");
        server.staff_presence.insert(row.id.clone(), row);
    }
    assert_eq!(server.staff_presence.rows.lock().unwrap().len(), CAPACITY);
    let result = roster(&server, None, None, usize::MAX).unwrap();
    assert_eq!(items(&result).len(), DISPLAY_CAP);
    assert_eq!(result["incomplete"], true);
    assert_eq!(result["overflow"], CAPACITY - DISPLAY_CAP);
    assert!(!result["items"][0]["client"]
        .as_str()
        .unwrap()
        .contains('\n'));
    crate::test_support::with_unrestricted_fixture_connection(&server.global_db_path_buf(), |c| {
        c.execute_batch("DROP TABLE session_claims")
    })
    .unwrap();
    let unavailable = roster(&server, None, None, 20).unwrap();
    assert_eq!(unavailable["claim_error"], "project_claims_unavailable");
}
