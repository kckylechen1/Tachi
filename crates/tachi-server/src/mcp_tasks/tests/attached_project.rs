//! Real attachment/event producers feed the project read surfaces.
use super::*;

const WORKER: &str = "agent.attached-worker";
const CLAIM: &str = "claim-attached-project";
const ISSUE: &str = "org/repo#1693";

fn seed_worker(server: &MemoryServer, claim: &str, project: &str, issue: &str) {
    server.set_published_daemon_mcp_url("http://127.0.0.1:6919/mcp".into());
    server
        .with_global_store(|store| {
            let conn = store.connection();
            let known: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_identity_id=?1)",
                    [WORKER],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            if !known {
                memcore::insert_agent_identity(
                conn,
                &memcore::AgentIdentity {
                    agent_identity_id: WORKER.into(),
                    display_name: None,
                    seat: None,
                    capability_json: Some(
                        r#"{"acp":{"tool_profiles":["delegate"],"capability_classes":["tachi"]}}"#
                            .into(),
                    ),
                    created_at: String::new(),
                },
            )
            .map_err(|e| e.to_string())?;
                memcore::record_unverified_admission(
                    conn,
                    "worker-admission",
                    WORKER,
                    "worker-connection",
                    memcore::UnverifiedAdmissionState::SelfAsserted,
                )
                .map_err(|e| e.to_string())?;
            }
            memcore::db::session_claims::insert_local_project_work_claim(
                conn,
                &memcore::NewWorkClaim {
                    claim_id: claim.into(),
                    agent_identity_id: WORKER.into(),
                    session_client: Some("worker-connection".into()),
                    issue_ref: Some(issue.into()),
                    flow_id: None,
                    dispatch_id: None,
                    branch: claim.into(),
                    worktree_path: server
                        .global_db_path_buf()
                        .parent()
                        .unwrap()
                        .join(claim)
                        .to_string_lossy()
                        .into_owned(),
                    declared_file_scope: "[\"src/attached.rs\"]".into(),
                    role: "observer".into(),
                    mode: memcore::WorkClaimMode::ReadOnly,
                    expected_head: "1234567".into(),
                    lease_expires_at: "2035-01-01T00:00:00Z".into(),
                    created_at: String::new(),
                },
                "worker-connection",
                project,
            )
            .map_err(|e| e.to_string())
        })
        .unwrap();
}

async fn eval(server: &MemoryServer, params: Value) -> Value {
    let raw = crate::agent_eval::handle_agent_eval(server, serde_json::from_value(params).unwrap())
        .await
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

async fn attach(server: &MemoryServer, claim: &str, remote: &str) -> String {
    let (_, connection_id, _) = server.work_claim_connection().unwrap();
    eval(
        server,
        json!({
            "action":"attach_session", "host_identity":"agent.owner", "agent_identity_id":WORKER,
            "work_claim_id":claim, "expected_transition_revision":0, "protocol_version":1,
            "adapter_connection_identity":"adapter-project", "remote_session_id":remote,
            "contract_digest":"contract-project", "session_capabilities":["observe"],
            "mcp_transport":"http", "mcp_http_capable":true, "tool_profile":"delegate",
            "capability_class":"tachi", "idempotency_key":remote,
            "admission_receipt_ref":connection_id,
        }),
    )
    .await["attachment_id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn event_params(
    server: &MemoryServer,
    id: &str,
    event: &str,
    revision: i64,
    outcome: Option<&str>,
    summary: &str,
) -> Value {
    json!({
        "action":"ingest_session_event", "host_identity":"agent.owner", "protocol_version":1,
        "attachment_id":id, "admission_receipt_ref":server.work_claim_connection().unwrap().1,
        "session_event_id":event, "session_event_kind":if outcome.is_some() {"terminal"} else {"cleanup"},
        "session_event_outcome":outcome, "source_revision":revision,
        "event_summary":summary, "event_occurred_at":"2026-11-01T00:00:00Z",
    })
}

fn intent(server: &MemoryServer, attachment: &str) -> memcore::DeliveryIntent {
    server
        .with_global_store(|store| {
            memcore::find_delivery_intent_by_idempotency_key(
                store.connection(),
                &format!("attached:{attachment}"),
            )
            .map_err(|e| e.to_string())
        })
        .unwrap()
        .unwrap()
}

fn changes(server: &MemoryServer) -> u64 {
    server
        .with_global_store(|store| Ok(store.connection().total_changes()))
        .unwrap()
}

async fn assert_attached_views(
    server: &MemoryServer,
    attachment: &str,
    state: &str,
    delivery: Option<&str>,
) -> Value {
    let before = changes(server);
    let mut items = Vec::new();
    for action in ["board", "status", "brief"] {
        let section = project_view(server, action, Some(ISSUE), 100).await;
        assert_eq!(
            section["sources"]["attached_sessions"]["state"], "available",
            "{section}"
        );
        assert!(!section.to_string().contains("PRIVATE_ATTACHED_SUMMARY"));
        let mut item = section["items"][0].clone();
        item.as_object_mut().unwrap().remove("read_at");
        assert_eq!(item["attached_sessions"][0]["attachment_id"], attachment);
        assert_eq!(item["attached_sessions"][0]["canonical_state"], state);
        assert_eq!(
            item["status"]["attached_sessions"],
            item["brief"]["sections"]["attached_sessions"]
        );
        assert_eq!(item["status"]["adjudication"], "unavailable");
        assert_eq!(item["status"]["success_shaped"], false);
        if let Some(delivery) = delivery {
            assert_eq!(item["delivery_intents"][0]["delivery_state"], delivery);
            assert_eq!(item["status"]["delivery"], "observed");
        } else {
            assert_eq!(item["delivery_intents"][0]["availability"], "unavailable");
            assert_eq!(section["sources"]["delivery"]["state"], "unavailable");
        }
        items.push(item);
    }
    assert_eq!(items[0], items[1]);
    assert_eq!(items[1], items[2]);
    assert_eq!(
        changes(server),
        before,
        "project reads must not write any source"
    );
    items.remove(0)
}

fn deliver(server: &MemoryServer, id: &str) {
    server
        .with_global_store(|store| {
            let caller = memcore::DeliveryCaller {
                agent_identity_id: WORKER.into(),
                host_identity: "agent.owner".into(),
            };
            memcore::claim_ready_delivery(
                store.connection(),
                &memcore::DeliveryClaimRequest {
                    caller: caller.clone(),
                    claim_key: "attached-project-claim".into(),
                    lease_seconds: 60,
                    only_delivery_id: Some(id.into()),
                },
            )
            .map_err(|e| e.to_string())?;
            memcore::ack_delivered(
                store.connection(),
                id,
                &caller,
                "attached-project-ack",
                None,
            )
            .map_err(|e| e.to_string())
        })
        .unwrap();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_attached_results_share_views_and_survive_claim_release_without_writes() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, _) = fixture();
    project_claim(&server, None, "org/repo#1700");
    seed_worker(&server, CLAIM, PROJECT, ISSUE);
    let id = attach(&server, CLAIM, "remote-project").await;
    eval(
        &server,
        event_params(
            &server,
            &id,
            "done",
            7,
            Some("completed"),
            "PRIVATE_ATTACHED_SUMMARY",
        ),
    )
    .await;
    let ready = assert_attached_views(&server, &id, "completed", Some("ready")).await;
    assert_eq!(
        ready["status"]["run"], "unavailable",
        "attached evidence never forges a managed dispatch"
    );
    let delivery = intent(&server, &id);
    deliver(&server, &delivery.delivery_id);
    let delivered = assert_attached_views(&server, &id, "completed", Some("delivered")).await;
    assert_ne!(ready["revision"], delivered["revision"]);
    // Unchanged content at a higher event revision leaves settled delivery alone.
    eval(
        &server,
        event_params(
            &server,
            &id,
            "same",
            8,
            Some("completed"),
            "PRIVATE_ATTACHED_SUMMARY",
        ),
    )
    .await;
    eval(
        &server,
        event_params(&server, &id, "cleanup", 9, None, "cleaned"),
    )
    .await;
    let cleanup = assert_attached_views(&server, &id, "completed", Some("delivered")).await;
    assert_eq!(cleanup["attached_sessions"][0]["canonical_revision"], 9);
    assert_eq!(cleanup["attached_sessions"][0]["cleanup_recorded"], true);
    assert_eq!(
        cleanup["delivery_intents"][0]["terminal_receipt_revision"],
        7
    );
    server
        .with_global_store(|store| {
            memcore::release_work_claim(store.connection(), CLAIM, WORKER, 0, "complete")
                .map_err(|e| e.to_string())
        })
        .unwrap();
    assert_attached_views(&server, &id, "completed", Some("delivered")).await;
    eval(
        &server,
        event_params(
            &server,
            &id,
            "corrected",
            11,
            Some("completed"),
            "PRIVATE_ATTACHED_SUMMARY corrected",
        ),
    )
    .await;
    let corrected = assert_attached_views(&server, &id, "completed", Some("ready")).await;
    assert_eq!(
        corrected["delivery_intents"][0]["delivery_id"],
        delivery.delivery_id
    );
    assert_eq!(corrected["delivery_intents"][0]["result_revision"], 11);
    eval(
        &server,
        event_params(
            &server,
            &id,
            "conflict",
            12,
            Some("failed"),
            "conflicting outcome",
        ),
    )
    .await;
    let conflict = assert_attached_views(&server, &id, "inconsistent_reconciling", None).await;
    assert_eq!(
        conflict["attached_sessions"][0]["conflicting_terminal"],
        true
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_attached_correction_window_cannot_inherit_old_delivered_state() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, _) = fixture();
    project_claim(&server, None, "org/repo#1700");
    seed_worker(&server, CLAIM, PROJECT, ISSUE);
    let id = attach(&server, CLAIM, "remote-window").await;
    eval(
        &server,
        event_params(&server, &id, "done", 1, Some("completed"), "original"),
    )
    .await;
    deliver(&server, &intent(&server, &id).delivery_id);
    // Commit the real event writer before the server's separate delivery mint.
    let (_, connection_id, _) = server.work_claim_connection().unwrap();
    server
        .with_global_store(|store| {
            memcore::ingest_harness_session_event(
                store.connection_mut(),
                &memcore::HarnessSessionAttachmentSelector::AttachmentId(id.clone()),
                &memcore::NewHarnessSessionEvent {
                    event_id: "corrected".into(),
                    kind: memcore::HarnessSessionEventKind::Terminal,
                    outcome: Some(memcore::HarnessSessionTerminalOutcome::Completed),
                    source_revision: 2,
                    authority_confirmation_ref: None,
                    summary: Some("corrected".into()),
                    payload_digest: None,
                    occurred_at: "2026-11-01T00:00:00Z".into(),
                },
                &memcore::HarnessSessionHostAdmission {
                    host_identity: "agent.owner".into(),
                    connection_id: connection_id.clone(),
                },
                &connection_id,
            )
            .map_err(|e| e.to_string())
        })
        .unwrap();
    assert_attached_views(&server, &id, "completed", None).await;
    eval(
        &server,
        event_params(&server, &id, "corrected", 2, Some("completed"), "corrected"),
    )
    .await;
    assert_attached_views(&server, &id, "completed", Some("ready")).await;
    // Optional delivery storage failure must not erase canonical execution.
    let fixture_db = rusqlite::Connection::open(server.global_db_path_buf()).unwrap();
    fixture_db
        .execute_batch("ALTER TABLE delivery_intents RENAME TO offline_delivery_intents")
        .unwrap();
    assert_attached_views(&server, &id, "completed", None).await;
    fixture_db
        .execute_batch("ALTER TABLE harness_session_state RENAME TO offline_session_state")
        .unwrap();
    let offline = project_view(&server, "board", Some(ISSUE), 100).await;
    assert_eq!(offline["available"], true);
    assert_eq!(
        offline["sources"]["attached_sessions"]["state"],
        "unavailable"
    );
    assert_eq!(offline["items"][0]["attached_sessions"], json!([]));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_attached_discovery_masks_foreign_bindings_and_stale_admission() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, _) = fixture();
    project_claim(&server, None, "org/repo#1700");
    seed_worker(&server, CLAIM, PROJECT, ISSUE);
    let mut missing = project_view(&server, "board", Some(ISSUE), 100).await;
    for item in missing["items"].as_array_mut().unwrap() {
        item.as_object_mut().unwrap().remove("read_at");
    }
    let id = attach(&server, CLAIM, "remote-hidden").await;
    eval(
        &server,
        event_params(
            &server,
            &id,
            "done",
            1,
            Some("failed"),
            "PRIVATE_ATTACHED_SUMMARY",
        ),
    )
    .await;
    let (_, connection_id, _) = server.work_claim_connection().unwrap();
    // Independent binding dimensions must not grant an existence signal.
    for (column, wrong, original) in [
        ("host_identity", "foreign-host", "agent.owner"),
        ("agent_identity_id", "foreign-worker", WORKER),
        (
            "admission_receipt_ref",
            "old-connection",
            connection_id.as_str(),
        ),
    ] {
        server.with_global_store(|store| {
            store.connection().execute(&format!("UPDATE harness_session_attachments SET {column}=?1 WHERE attachment_id=?2"),
                rusqlite::params![wrong,id]).map_err(|e| e.to_string())
        }).unwrap();
        let mut hidden = project_view(&server, "board", Some(ISSUE), 100).await;
        for item in hidden["items"].as_array_mut().unwrap() {
            item.as_object_mut().unwrap().remove("read_at");
        }
        assert_eq!(
            hidden["items"], missing["items"],
            "denied attachment must equal absent: {column}"
        );
        assert_eq!(
            hidden["sources"]["attached_sessions"],
            missing["sources"]["attached_sessions"]
        );
        assert!(!hidden.to_string().contains(&id));
        server.with_global_store(|store| {
            store.connection().execute(&format!("UPDATE harness_session_attachments SET {column}=?1 WHERE attachment_id=?2"),
                rusqlite::params![original,id]).map_err(|e| e.to_string())
        }).unwrap();
    }
    seed_worker(
        &server,
        "foreign-project-claim",
        "foreign-project",
        "org/repo#1701",
    );
    server.set_session_identity(None, Some("foreign-project".into()), None);
    project_claim(&server, None, "org/repo#1702");
    let foreign = attach(&server, "foreign-project-claim", "remote-foreign-project").await;
    server.set_session_identity(None, Some(PROJECT.into()), None);
    let full = project_view(&server, "board", None, 100).await;
    assert!(!full.to_string().contains(&foreign));
    assert!(full.to_string().contains(&id));
    // The host re-admits on a new connection; prior attachments require rebind.
    bind_identity(&server, "agent.owner");
    let rotated = project_view(&server, "status", Some(ISSUE), 100).await;
    assert!(!rotated.to_string().contains(&id));
    let (_, current_connection_id, _) = server.work_claim_connection().unwrap();
    server
        .with_global_store(|store| {
            store
                .connection()
                .execute(
                    "UPDATE identity_admissions SET state='rejected' WHERE connection_id=?1",
                    [&current_connection_id],
                )
                .map_err(|e| e.to_string())
        })
        .unwrap();
    let revoked = project_view(&server, "brief", Some(ISSUE), 100).await;
    assert_eq!(revoked["available"], false);
    assert!(revoked["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_attached_delivery_requires_the_full_canonical_binding() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, _) = fixture();
    project_claim(&server, None, "org/repo#1700");
    seed_worker(&server, CLAIM, PROJECT, ISSUE);
    let id = attach(&server, CLAIM, "remote-binding").await;
    eval(
        &server,
        event_params(&server, &id, "done", 3, Some("completed"), "original"),
    )
    .await;
    let row = intent(&server, &id);
    for (column, wrong, original) in [
        ("execution_source", "managed_dispatch", "attached_session"),
        ("execution_ref", "foreign-attachment", id.as_str()),
        ("work_claim_id", "foreign-claim", CLAIM),
        ("requester_agent_identity_id", "foreign-worker", WORKER),
        ("requester_host_identity", "foreign-host", "agent.owner"),
        ("requester_session_ref", "foreign-session", "remote-binding"),
        ("result_ref", "foreign-result", row.result_ref.as_str()),
        (
            "payload_digest",
            "older-payload",
            row.payload_digest.as_str(),
        ),
        ("terminal_receipt_revision", "99", "3"),
    ] {
        server
            .with_global_store(|store| {
                store
                    .connection()
                    .execute(
                        &format!("UPDATE delivery_intents SET {column}=?1 WHERE delivery_id=?2"),
                        rusqlite::params![wrong, row.delivery_id],
                    )
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        assert_attached_views(&server, &id, "completed", None).await;
        server
            .with_global_store(|store| {
                store
                    .connection()
                    .execute(
                        &format!("UPDATE delivery_intents SET {column}=?1 WHERE delivery_id=?2"),
                        rusqlite::params![original, row.delivery_id],
                    )
                    .map_err(|e| e.to_string())
            })
            .unwrap();
    }
    assert_attached_views(&server, &id, "completed", Some("ready")).await;
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_attached_and_managed_delivery_share_one_snapshot_and_receipt_budget() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_runs, _env, server, run) = fixture();
    project_claim(&server, Some(DISPATCH), ISSUE);
    managed_status(&run, &server.controller_epoch, "TASK_STATE_COMPLETED", 1);
    let managed = mint_project_delivery(&server, None);
    seed_worker(&server, CLAIM, PROJECT, ISSUE);
    let id = attach(&server, CLAIM, "remote-mixed").await;
    eval(
        &server,
        event_params(&server, &id, "done", 1, Some("completed"), "original"),
    )
    .await;
    let attached = intent(&server, &id);
    let full = project_view(&server, "board", None, 100).await;
    let exact = project_view(&server, "status", Some(ISSUE), 100).await;
    assert_eq!(full["items"][0]["revision"], exact["items"][0]["revision"]);
    let item = &exact["items"][0];
    let deliveries = item["delivery_intents"].as_array().unwrap();
    assert_eq!(deliveries.len(), 2);
    for id in [managed.delivery_id, attached.delivery_id] {
        assert!(deliveries.iter().any(|d| d["delivery_id"] == id));
    }
    assert_eq!(item["status"]["delivery"], "observed");
    // The same eight-receipt budget covers managed runs and attachments.
    for n in 1..7 {
        attach(&server, CLAIM, &format!("remote-budget-{n}")).await;
    }
    let bounded = project_view(&server, "board", Some(ISSUE), 100).await;
    assert_eq!(bounded["available"], true);
    attach(&server, CLAIM, "remote-over-budget").await;
    let over = project_view(&server, "board", Some(ISSUE), 100).await;
    assert_eq!(
        over["sources"]["work_claims"]["reason"],
        "project_receipt_limit"
    );
    assert!(over["items"].as_array().unwrap().is_empty());
}
