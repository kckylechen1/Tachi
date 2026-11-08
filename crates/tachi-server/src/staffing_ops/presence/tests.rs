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

#[test]
fn roster_counts_expired_peers_before_display_limit_and_keeps_self() {
    let (server, _) = crate::tests::make_server_with_project_fixture("presence-alpha");
    server.set_session_identity(Some("codex".into()), Some("presence-alpha".into()), None);
    server.touch_activity();
    {
        let mut rows = server.staff_presence.rows.lock().unwrap();
        let template = rows.values().next().unwrap().clone();
        for index in 0..30 {
            let mut row = template.clone();
            row.id = format!("expired-presence-{index}");
            row.owner = format!("expired-owner-{index}");
            row.client = Some(format!("expired-client-{index}"));
            row.observed_at = Utc::now() - chrono::Duration::seconds(CLIENT_TTL_SECONDS + 1);
            rows.insert(format!("expired-{index}"), row);
        }
    }
    let view = roster(&server, None, None, 20).unwrap();
    assert_eq!(view["total_count"], 31);
    assert_eq!(view["stale_count"], 30);
    assert_eq!(view["foldable_inactive_count"], 30);
    assert_eq!(view["count"], 20);
    assert_eq!(view["overflow"], 11);
    assert!(items(&view).iter().any(|row| row["is_self"] == true));
}

fn grant_read(server: &MemoryServer, identity: &str) -> Value {
    let (project, _) = server
        .resolve_server_named_project_binding("presence-alpha")
        .unwrap();
    let client = server.session_client();
    server.set_session_identity(client, Some(project), None);
    crate::claims_ops::admit_agent_connection(server, Some(identity.into()), true).unwrap();
    crate::claims_ops::handle_task_claim(
        server,
        &serde_json::from_value(json!({
            "action":"claim", "claim_role":"reviewer", "claim_mode":"read_only",
            "claim_scope":["src"], "expected_head":"abc123",
            "lease_expires_at":"2099-01-01T00:00:00Z"
        }))
        .unwrap(),
    )
    .unwrap()
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
    grant_read(&server, "seat.bounds");
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

#[tokio::test]
async fn detached_child_cannot_resurrect_a_cancelled_owner() {
    let (server, _) = crate::tests::make_server_with_project_fixture("presence-alpha");
    let (observer, guard) = register_worker(
        &server,
        "run-1",
        "codex",
        Some("presence-alpha"),
        assignment("flow-a"),
    )
    .unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let owner = tokio::spawn(observe_runner(Some(observer), async move {
        let _guard = guard;
        let child = tokio::spawn(observe_runner(current_worker(), async move {
            release_rx.await.unwrap();
            worker_event("busy", "process_spawned");
        }));
        started_tx.send(child).unwrap();
        std::future::pending::<()>().await;
    }));
    let child = started_rx.await.unwrap();
    owner.abort();
    assert!(owner.await.unwrap_err().is_cancelled());
    release_tx.send(()).unwrap();
    child.await.unwrap();
    assert_eq!(
        roster(&server, Some("presence-alpha"), None, 20).unwrap()["items"][0]["state"],
        "disconnected"
    );
}

#[tokio::test]
async fn active_workers_and_assignments_survive_history_truncation() {
    let (server, _) = crate::tests::make_server_with_project_fixture("presence-alpha");
    server.set_session_identity(Some("codex".into()), Some("presence-alpha".into()), None);
    server.touch_activity();
    let (observer, live_guard) =
        register_worker(&server, "z-live", "codex", None, assignment("flow-a")).unwrap();
    observe_runner(Some(observer), async {
        worker_event("busy", "process_spawned");
    })
    .await;
    for index in 0..DISPLAY_CAP + 1 {
        let (_, guard) = register_worker(
            &server,
            &format!("a-history-{index:03}"),
            "codex",
            None,
            assignment("flow-a"),
        )
        .unwrap();
        drop(guard);
    }
    let value = roster(&server, None, None, DISPLAY_CAP).unwrap();
    assert_eq!(value["items"][0]["dispatch_id"], "z-live");
    let client = items(&value)
        .iter()
        .find(|row| row["kind"] == "client")
        .unwrap();
    assert_eq!(client["assignments"][0]["dispatch_id"], "z-live");
    assert!(client["assignment_overflow"].as_u64().unwrap() > 0);
    let board: Value = serde_json::from_str(
        &crate::dispatch_ops::handle_tachi_board(
            &server,
            serde_json::from_value(json!({})).unwrap(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(board["staff"]["items"][0]["dispatch_id"], "z-live");
    drop(live_guard);
}

#[test]
fn active_claim_selection_filters_history_and_flow_before_the_limit() {
    let (server, _) = crate::tests::make_server_with_project_fixture("presence-alpha");
    grant_read(&server, "seat.history");
    let (_, guard) = register_worker(
        &server,
        "z-live",
        "codex",
        Some("presence-alpha"),
        assignment("flow-a"),
    )
    .unwrap();
    let (project, _) = server
        .resolve_server_named_project_binding("presence-alpha")
        .unwrap();
    assert_ne!(
        project, "presence-alpha",
        "the fixture alias must exercise canonical project resolution"
    );
    crate::test_support::with_unrestricted_fixture_connection(&server.global_db_path_buf(), |conn| {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        for index in 0..CAPACITY + 1 {
            for (prefix, state, expiry, flow) in [
                ("a-released", "released", "2099-01-01T00:00:00Z", "flow-a"),
                ("b-expired", "active", "2000-01-01T00:00:00Z", "flow-a"),
                ("c-other-flow", "active", "2099-01-01T00:00:00Z", "flow-b"),
            ] {
                conn.execute("INSERT INTO session_claims (claim_id,canonical_project,dispatch_id,mode,state,lease_expires_at,flow_id) VALUES (?1,?5,'z-live','read_only',?2,?3,?4)", rusqlite::params![format!("{prefix}-{index:03}"),state,expiry,flow,project])?;
            }
        }
        conn.execute("INSERT INTO session_claims (claim_id,canonical_project,dispatch_id,mode,state,lease_expires_at,flow_id) VALUES ('z-current',?1,'z-live','read_only','active','2099-01-01T00:00:00Z','flow-a')", [&project])?;
        conn.execute_batch("COMMIT")?;
        Ok(())
    }).unwrap();
    let value = roster(&server, Some("presence-alpha"), Some("flow-a"), 20).unwrap();
    assert!(
        value["claim_error"].is_null(),
        "claim projection failed: {value}"
    );
    assert_eq!(value["project"], project);
    assert_eq!(value["claims_truncated"], false);
    assert_eq!(value["items"][0]["assignments"][0]["claim_id"], "z-current");
    drop(guard);
}

#[tokio::test]
async fn identified_clients_see_self_and_claim_lifecycle_on_the_public_board() {
    let (server, _) = crate::tests::make_server_with_project_fixture("presence-alpha");
    let (project, _) = server
        .resolve_server_named_project_binding("presence-alpha")
        .unwrap();
    let owner = server.clone_for_mcp_session();
    owner.set_session_identity(Some("claude-code".into()), Some(project.clone()), None);
    crate::claims_ops::admit_agent_connection(&owner, Some("seat.claude".into()), true).unwrap();
    owner.touch_activity();
    let observer = server.clone_for_mcp_session();
    observer.set_session_identity(Some("codex".into()), Some(project.clone()), None);
    crate::claims_ops::admit_agent_connection(&observer, Some("seat.codex".into()), true).unwrap();
    observer.touch_activity();
    let call = |value| serde_json::from_value(value).unwrap();
    let claimed = crate::claims_ops::handle_task_claim(
        &owner,
        &call(json!({
            "action":"claim", "claim_role":"reviewer", "claim_mode":"read_only",
            "issue_ref":"org/repo#1", "flow_id":"presence-flow",
            "claim_scope":["src"], "expected_head":"abc123",
            "lease_expires_at":"2099-01-01T00:00:00Z"
        })),
    )
    .unwrap();
    let denied = roster(&observer, None, None, 20).unwrap();
    assert_eq!(denied["claims_available"], false);
    assert!(!denied
        .to_string()
        .contains(claimed["claim_id"].as_str().unwrap()));
    let unbound = server.clone_for_mcp_session();
    unbound.set_session_identity(Some("global-client".into()), None, None);
    report_task_context(
        &unbound,
        &call(json!({
            "action":"board", "project":project, "session_id":"public-only", "task":"public summary"
        })),
    )
    .unwrap();
    let public = roster(&unbound, None, None, 20).unwrap();
    assert_eq!(public["claims_available"], false);
    assert!(!public
        .to_string()
        .contains(claimed["claim_id"].as_str().unwrap()));
    assert!(public.to_string().contains("public summary"));
    let observer_claim = grant_read(&observer, "seat.codex");
    let before = roster(&observer, None, None, 20).unwrap();
    assert_eq!(before["self"]["agent_identity_id"], "seat.codex");
    assert_eq!(
        items(&before)
            .iter()
            .filter(|r| r["is_self"] == true)
            .count(),
        1
    );
    let peer = items(&before)
        .iter()
        .find(|r| r["client"] == "claude-code")
        .unwrap();
    assert_eq!(peer["is_self"], false);
    assert_eq!(peer["assignments"][0]["claim_id"], claimed["claim_id"]);
    assert_eq!(peer["assignments"][0]["issue_ref"], r"org/repo\#1");
    assert_eq!(peer["assignments"][0]["online_evidence"], false);
    let board: Value = serde_json::from_str(
        &observer
            .tachi_task(Parameters(call(json!({
                "action":"board", "format":"json"
            }))))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(board["board"], "project_work");
    assert_eq!(
        board["staff"], before,
        "identified board must retain the same roster"
    );
    crate::claims_ops::handle_task_heartbeat(
        &owner,
        &call(json!({
            "action":"heartbeat", "claim_id":claimed["claim_id"], "transition_version":0,
            "lease_expires_at":"2099-01-02T00:00:00Z"
        })),
    )
    .unwrap();
    let refreshed = roster(&observer, None, None, 20).unwrap();
    let peer = items(&refreshed)
        .iter()
        .find(|r| r["client"] == "claude-code")
        .unwrap();
    assert_eq!(peer["assignments"][0]["transition_version"], 1);
    crate::claims_ops::handle_task_release(
        &owner,
        &call(json!({
            "action":"release", "claim_id":claimed["claim_id"], "transition_version":1
        })),
    )
    .unwrap();
    server.staff_presence.rows.lock().unwrap().remove(&format!(
        "client:{}",
        unbound.agent_runtime_read().presence_session_id
    ));
    crate::claims_ops::handle_task_release(
        &observer,
        &call(json!({
            "action":"release", "claim_id":observer_claim["claim_id"], "transition_version":0
        })),
    )
    .unwrap();
    assert!(items(&roster(&observer, None, None, 20).unwrap())
        .iter()
        .all(|r| r["assignments"].as_array().unwrap().is_empty()));
}

#[test]
fn roster_reports_missing_setup_and_distinguishes_connections_of_one_seat() {
    let (server, _) = crate::tests::make_server_with_project_fixture("presence-alpha");
    let unbound = roster(&server, None, None, 20).unwrap();
    assert_eq!(unbound["self"]["project_bound"], false);
    assert_eq!(unbound["self"]["identity_configured"], false);
    server.set_session_identity(Some("codex".into()), Some("presence-alpha".into()), None);
    crate::claims_ops::admit_agent_connection(&server, Some("seat.codex".into()), true).unwrap();
    server.touch_activity();
    let reconnect = server.clone_for_mcp_session();
    reconnect.touch_activity();
    let value = roster(&reconnect, None, None, 20).unwrap();
    assert_eq!(items(&value).len(), 2);
    assert_eq!(
        items(&value)
            .iter()
            .filter(|r| r["is_self"] == true)
            .count(),
        1
    );
    assert!(items(&value)
        .iter()
        .all(|r| r["agent_identity_id"] == "seat.codex"));
    let zero = roster(&reconnect, None, None, 0).unwrap();
    assert_eq!(zero["self"], value["self"]);
    assert!(items(&zero).is_empty());
}

#[tokio::test]
async fn task_board_reports_unbound_sessions_without_granting_project_authority() {
    let (server, _) = crate::tests::make_server_with_project_fixture("presence-alpha");
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("presence-alpha");
    std::fs::create_dir(&cwd).unwrap();
    server
        .resolve_or_register_workspace_root(cwd.to_str().unwrap())
        .unwrap();
    let a = server.clone_for_mcp_session();
    let b = server.clone_for_mcp_session();
    for client in [&a, &b] {
        client.set_session_identity(Some("zcode".into()), None, None);
        // A shared bucket must not collapse separate conversations.
        client.set_rate_limit_session_id("same-bucket".into());
    }
    for (client, session, task) in [(&a, "a", "Review parser"), (&b, "b", "Fix UI")] {
        let params = serde_json::from_value(json!({"action":"board", "session_id":session,
            "agent_id":"zcode-agent", "cwd":cwd, "task":task}))
        .unwrap();
        let response: Value =
            serde_json::from_str(&client.tachi_task(Parameters(params)).await.unwrap()).unwrap();
        assert_eq!(response["staff"]["self"]["reported"], true);
        assert_eq!(response["staff"]["self"]["reported_work"]["summary"], task);
        assert!(client.session_project().is_none());
        assert!(client.agent_runtime_read().work_claim_connection.is_none());
    }
    let rows = roster(&a, None, None, 20).unwrap();
    assert_eq!(items(&rows).len(), 2);
    assert_eq!(
        items(&rows)
            .iter()
            .filter(|row| row["is_self"] == true)
            .count(),
        1
    );
    assert!(rows.to_string().contains("Review parser"));
    assert!(rows.to_string().contains("Fix UI"));
    assert!(
        crate::claims_ops::work_claim_board(&a).unwrap()["work_claims"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let old_id = items(&rows).iter().find(|r| r["is_self"] == true).unwrap()["presence_id"].clone();
    let reconnect = server.clone_for_mcp_session();
    reconnect.set_session_identity(Some("zcode".into()), None, None);
    let update = serde_json::from_value(json!({"action":"board", "session_id":"a",
        "agent_id":"zcode-agent", "cwd":cwd, "task":"Parser done", "flow_id":"flow_a"}))
    .unwrap();
    report_task_context(&reconnect, &update).unwrap();
    reconnect.touch_activity();
    let updated = roster(&reconnect, None, None, 20).unwrap();
    assert_eq!(items(&updated).len(), 2);
    let own = items(&updated)
        .iter()
        .find(|r| r["is_self"] == true)
        .unwrap();
    assert_eq!(own["presence_id"], old_id);
    assert_eq!(own["reported_work"]["summary"], "Parser done");
    let filtered = roster(&reconnect, None, Some("flow_a"), 20).unwrap();
    assert_eq!(items(&filtered).len(), 1);
    assert_eq!(filtered["self"]["session_id"], "a");
    assert!(!filtered.to_string().contains("Fix UI"));
    let echo = serde_json::from_value(
        json!({"action":"board", "session_id":filtered["self"]["session_id"],
        "cwd":cwd, "task":"Echo update"}),
    )
    .unwrap();
    report_task_context(&reconnect, &echo).unwrap();
    assert_eq!(
        roster(&reconnect, None, None, 20).unwrap()["self"]["presence_session_key"],
        updated["self"]["presence_session_key"]
    );
    assert!(items(&roster(&reconnect, None, None, 0).unwrap()).is_empty());
}

#[test]
fn session_reporting_rejects_bad_context_and_preserves_existing_briefing_callers() {
    let (server, _) = crate::tests::make_server_with_project_fixture("presence-alpha");
    server.set_session_identity(Some("codex".into()), Some("presence-alpha".into()), None);
    for invalid in [
        json!({"session_id":""}),
        json!({"session_id":"bad/id"}),
        json!({"session_id":"good", "cwd":"relative"}),
        json!({"session_id":"good", "project":"foreign"}),
    ] {
        let mut args = json!({"action":"brief", "task":"working"});
        args.as_object_mut()
            .unwrap()
            .extend(invalid.as_object().unwrap().clone());
        assert!(report_task_context(&server, &serde_json::from_value(args).unwrap()).is_err());
        assert!(server.agent_runtime_read().presence_report.is_none());
    }
    let legacy =
        serde_json::from_value(json!({"action":"brief", "cwd":".", "task":"legacy"})).unwrap();
    report_task_context(&server, &legacy).unwrap();
    let result = roster(&server, None, None, 20).unwrap();
    assert!(result["self"]["report_error"].is_string());
    assert_eq!(result["self"]["reported"], false);
}

#[tokio::test]
async fn internal_briefing_does_not_publish_private_objectives() {
    let (server, _) = crate::tests::make_server_with_project_fixture("presence-alpha");
    server.set_session_identity(Some("codex".into()), Some("presence-alpha".into()), None);
    server.touch_activity();
    let params = serde_json::from_value(
        json!({"action":"brief", "task":"private-intake-objective", "format":"json"}),
    )
    .unwrap();
    crate::copilot_ops::handle_tachi_feature_briefing(&server, &params)
        .await
        .unwrap();
    assert!(server.agent_runtime_read().presence_report.is_none());
    assert!(!roster(&server, None, None, 20)
        .unwrap()
        .to_string()
        .contains("private-intake-objective"));
}

#[test]
fn optional_task_roster_handles_missing_project_and_same_database_aliases() {
    let (server, _) = crate::tests::make_server_with_project_fixture("presence-alpha");
    server.set_session_identity(Some("codex".into()), Some("missing-project".into()), None);
    assert_eq!(
        task_roster(&server, None, None, 20)["reason"],
        "presence_project_unavailable"
    );
    server.set_session_identity(Some("codex".into()), Some("presence-alpha".into()), None);
    let (canonical, _) = server
        .resolve_server_named_project_binding("presence-alpha")
        .unwrap();
    let params = serde_json::from_value(json!({"action":"board", "session_id":"alias-test", "project":canonical, "task":"public-summary"})).unwrap();
    report_task_context(&server, &params).unwrap();
    let peer = server.clone_for_mcp_session();
    peer.set_session_identity(Some("peer".into()), Some(canonical), None);
    assert!(roster(&peer, None, None, 20)
        .unwrap()
        .to_string()
        .contains("public-summary"));
}

#[test]
fn workspace_report_preserves_stable_legacy_binding_and_shares_canonical_peers() {
    let (server, _) = crate::tests::make_server_with_project_fixture("presence-alpha");
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("unique-presence-repo");
    std::fs::create_dir(&cwd).unwrap();
    let canonical = server
        .resolve_or_register_workspace_root(cwd.to_str().unwrap())
        .unwrap();
    let legacy = crate::path_utils::plan_c_legacy_dir_name_from_root(&cwd).unwrap();
    assert_ne!(canonical, legacy);
    assert_eq!(
        server
            .resolve_server_named_project_binding(&legacy)
            .unwrap()
            .0,
        legacy
    );
    server.set_session_identity(Some("codex".into()), Some(legacy.clone()), None);
    report_task_context(&server, &serde_json::from_value(json!({
        "action":"brief", "cwd":cwd, "project":canonical, "session_id":"legacy-session", "task":"public-alias-report"
    })).unwrap()).unwrap();
    assert_eq!(server.session_project().as_deref(), Some(legacy.as_str()));
    let peer = server.clone_for_mcp_session();
    peer.set_session_identity(Some("peer".into()), Some(canonical), None);
    let view = roster(&peer, None, None, 20).unwrap();
    assert!(view.to_string().contains("public-alias-report"));
}

#[test]
fn idle_stale_reconnects_collapse_to_the_newest_per_client_identity() {
    let row = |id: &str, client: &str, identity: Option<&str>, at: &str, state: &str| {
        json!({"presence_id": id, "kind": "client", "client": client, "is_self": false,
            "agent_identity_id": identity, "state": state, "observed_at": at,
            "reported_work": null, "assignments": [], "active_dispatch_ids": [],
            "assignment_overflow": 0, "active_dispatch_overflow": 0})
    };
    let mut rows = vec![
        row("a", "codex", None, "2026-11-08T10:00:00Z", "stale"),
        row("b", "codex", None, "2026-11-08T10:00:00.5Z", "stale"),
        row("c", "codex", None, "2026-11-08T09:00:00Z", "stale"),
        row("d", "codex", Some("codex"), "2026-11-08T08:00:00Z", "stale"),
        row("e", "codex", None, "2026-11-08T07:00:00Z", "ready"),
    ];
    let mut busy = row("f", "codex", None, "2026-11-08T06:00:00Z", "stale");
    busy["assignments"] = json!([{"summary": "x"}]);
    rows.push(busy);

    collapse_idle_stale_clients(&mut rows);
    let ids: Vec<&str> = rows
        .iter()
        .map(|r| r["presence_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["b", "d", "e", "f"]);
    assert_eq!(rows[0]["collapsed_stale_sessions"], 2);
    assert!(rows[1].get("collapsed_stale_sessions").is_none());
}
