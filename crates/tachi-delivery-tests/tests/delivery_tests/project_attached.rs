//! Project reads through the actual facade, without the server lib-test binary.
use super::*;
use serde_json::Value;

const PROJECT: &str = "delivery-project";
const ISSUE: &str = "org/repo#1693";

fn project_server() -> TestFixture {
    let server = test_server();
    seed_admission_and_requester(&server, GRANT_DELEGATE);
    server.set_session_project(Some(PROJECT));
    server
        .with_global_store(|store| {
            let conn = store.connection();
            for (admission, identity, connection) in [
                ("admission-1", "host-1", "connection-1"),
                ("worker-admission", "agent-requester", "worker-connection"),
            ] {
                memcore::record_unverified_admission(
                    conn,
                    admission,
                    identity,
                    connection,
                    memcore::UnverifiedAdmissionState::SelfAsserted,
                )
                .map_err(|e| e.to_string())?;
            }
            for (claim, identity, connection, issue) in [
                ("host-observer", "host-1", "connection-1", "org/repo#1700"),
                (
                    "claim-attached",
                    "agent-requester",
                    "worker-connection",
                    ISSUE,
                ),
            ] {
                memcore::db::session_claims::insert_local_project_work_claim(
                    conn,
                    &NewWorkClaim {
                        claim_id: claim.into(),
                        agent_identity_id: identity.into(),
                        session_client: Some(connection.into()),
                        issue_ref: Some(issue.into()),
                        flow_id: None,
                        dispatch_id: None,
                        branch: claim.into(),
                        worktree_path: server
                            ._dir
                            .path()
                            .join(claim)
                            .to_string_lossy()
                            .into_owned(),
                        declared_file_scope: "[\"src/lib.rs\"]".into(),
                        role: "observer".into(),
                        mode: WorkClaimMode::ReadOnly,
                        expected_head: "head".into(),
                        lease_expires_at: "2035-01-01T00:00:00Z".into(),
                        created_at: String::new(),
                    },
                    connection,
                    PROJECT,
                )
                .map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .unwrap();
    server
}

async fn attach_project(server: &DeliveryTestServer) -> String {
    let raw = server
        .handle_agent_eval_value(attach_params("project-attachment"))
        .await
        .unwrap();
    serde_json::from_str::<Value>(&raw).unwrap()["attachment_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn event(
    server: &DeliveryTestServer,
    attachment: &str,
    id: &str,
    revision: i64,
    outcome: Option<&str>,
    summary: &str,
) {
    let mut params = attach_params("project-attachment");
    params["action"] = json!("ingest_session_event");
    params["attachment_id"] = json!(attachment);
    params["session_event_id"] = json!(id);
    params["session_event_kind"] = json!(if outcome.is_some() {
        "terminal"
    } else {
        "cleanup"
    });
    params["session_event_outcome"] = json!(outcome);
    params["source_revision"] = json!(revision);
    params["event_summary"] = json!(summary);
    params["event_occurred_at"] = json!("2026-11-01T00:00:00Z");
    server.handle_agent_eval_value(params).await.unwrap();
}

async fn view(server: &DeliveryTestServer, action: &str) -> Value {
    let raw = server
        .handle_task_value(json!({"action":action,"issue_ref":ISSUE,"limit":100,"format":"json"}))
        .await
        .unwrap();
    assert!(!raw.contains("PRIVATE_PROJECT_RESULT"));
    let value: Value = serde_json::from_str(&raw).unwrap();
    value
        .get("work_read_model")
        .or_else(|| value.pointer("/cycle/work_read_model"))
        .unwrap()
        .clone()
}

fn write_count(server: &DeliveryTestServer) -> u64 {
    server
        .with_global_store(|store| Ok(store.connection().total_changes()))
        .unwrap()
}

async fn assert_views(
    server: &DeliveryTestServer,
    attachment: &str,
    canonical: &str,
    delivery: Option<&str>,
) -> Value {
    let before = write_count(server);
    let mut items = Vec::new();
    for action in ["board", "status", "brief"] {
        let section = view(server, action).await;
        assert_eq!(section["available"], true, "{section}");
        let mut item = section["items"][0].clone();
        item.as_object_mut().unwrap().remove("read_at");
        assert_eq!(item["attached_sessions"][0]["attachment_id"], attachment);
        assert_eq!(item["attached_sessions"][0]["canonical_state"], canonical);
        assert_eq!(item["status"]["run"], "unavailable");
        assert_eq!(item["status"]["success_shaped"], false);
        assert_eq!(item["status"]["adjudication"], "unavailable");
        assert_eq!(
            item["status"]["attached_sessions"],
            item["brief"]["sections"]["attached_sessions"]
        );
        match delivery {
            Some(state) => {
                assert_eq!(item["delivery_intents"][0]["delivery_state"], state);
                assert_eq!(item["status"]["delivery"], "observed");
            }
            None => assert_eq!(item["delivery_intents"][0]["availability"], "unavailable"),
        }
        items.push(item);
    }
    assert_eq!(items[0], items[1]);
    assert_eq!(items[1], items[2]);
    assert_eq!(
        write_count(server),
        before,
        "the read facade must not mutate any store"
    );
    items.remove(0)
}

#[tokio::test]
async fn attached_project_views_track_real_results_and_delivery_without_writes() {
    let server = project_server();
    let id = attach_project(&server).await;
    event(
        &server,
        &id,
        "done",
        7,
        Some("completed"),
        "PRIVATE_PROJECT_RESULT",
    )
    .await;
    let ready = assert_views(&server, &id, "completed", Some("ready")).await;
    let delivery_id = ready["delivery_intents"][0]["delivery_id"]
        .as_str()
        .unwrap();
    server
        .with_global_store(|store| {
            let caller = memcore::DeliveryCaller {
                agent_identity_id: "agent-requester".into(),
                host_identity: "host-1".into(),
            };
            memcore::claim_ready_delivery(
                store.connection(),
                &memcore::DeliveryClaimRequest {
                    caller: caller.clone(),
                    claim_key: "project-claim".into(),
                    lease_seconds: 60,
                    only_delivery_id: Some(delivery_id.into()),
                },
            )
            .map_err(|e| e.to_string())?;
            memcore::ack_delivered(
                store.connection(),
                delivery_id,
                &caller,
                "project-ack",
                None,
            )
            .map_err(|e| e.to_string())
        })
        .unwrap();
    let delivered = assert_views(&server, &id, "completed", Some("delivered")).await;
    assert_ne!(ready["revision"], delivered["revision"]);
    event(
        &server,
        &id,
        "same",
        8,
        Some("completed"),
        "PRIVATE_PROJECT_RESULT",
    )
    .await;
    event(&server, &id, "cleanup", 9, None, "cleaned").await;
    let cleanup = assert_views(&server, &id, "completed", Some("delivered")).await;
    assert_eq!(cleanup["attached_sessions"][0]["canonical_revision"], 9);
    assert_eq!(cleanup["attached_sessions"][0]["cleanup_recorded"], true);
    assert_eq!(
        cleanup["delivery_intents"][0]["terminal_receipt_revision"],
        7
    );
    server
        .with_global_store(|store| {
            memcore::release_work_claim(
                store.connection(),
                "claim-attached",
                "agent-requester",
                0,
                "complete",
            )
            .map_err(|e| e.to_string())
        })
        .unwrap();
    assert_views(&server, &id, "completed", Some("delivered")).await;
    event(
        &server,
        &id,
        "corrected",
        11,
        Some("completed"),
        "PRIVATE_PROJECT_RESULT corrected",
    )
    .await;
    let corrected = assert_views(&server, &id, "completed", Some("ready")).await;
    assert_eq!(corrected["delivery_intents"][0]["delivery_id"], delivery_id);
    assert_eq!(corrected["delivery_intents"][0]["result_revision"], 11);
    event(
        &server,
        &id,
        "conflict",
        12,
        Some("failed"),
        "conflicting terminal",
    )
    .await;
    assert_views(&server, &id, "inconsistent_reconciling", None).await;
}

#[tokio::test]
async fn attached_project_views_require_the_current_host_connection() {
    let server = project_server();
    let mut missing = view(&server, "board").await["items"][0].clone();
    missing.as_object_mut().unwrap().remove("read_at");
    let id = attach_project(&server).await;
    event(
        &server,
        &id,
        "done",
        1,
        Some("failed"),
        "PRIVATE_PROJECT_RESULT",
    )
    .await;
    assert_views(&server, &id, "failed", Some("ready")).await;
    server
        .with_global_store(|store| {
            memcore::record_unverified_admission(
                store.connection(),
                "new-admission",
                "host-1",
                "new-connection",
                memcore::UnverifiedAdmissionState::SelfAsserted,
            )
            .map_err(|e| e.to_string())
        })
        .unwrap();
    server.set_work_claim_connection(
        Some("host-1".into()),
        "new-connection".into(),
        "self_asserted".into(),
    );
    let before = write_count(&server);
    for action in ["board", "status", "brief"] {
        let section = view(&server, action).await;
        assert_eq!(section["available"], true);
        assert!(!section.to_string().contains(&id));
        let mut hidden = section["items"][0].clone();
        hidden.as_object_mut().unwrap().remove("read_at");
        assert_eq!(
            hidden, missing,
            "an old admission must be indistinguishable from no attachment"
        );
    }
    assert_eq!(write_count(&server), before);
    let mut reconnect = attach_params("project-attachment");
    reconnect["action"] = json!("reconnect_session");
    reconnect["attachment_id"] = json!(id);
    reconnect["admission_receipt_ref"] = json!("new-admission");
    let reconnected: Value =
        serde_json::from_str(&server.handle_agent_eval_value(reconnect).await.unwrap()).unwrap();
    assert_eq!(reconnected["reconnected"], true);
    assert_views(&server, &id, "failed", Some("ready")).await;
}

fn delivery_snapshot(
    server: &DeliveryTestServer,
    attachment: &str,
) -> (memcore::DeliveryIntent, i64) {
    server
        .with_global_store(|store| {
            let intent = memcore::find_delivery_intent_by_idempotency_key(
                store.connection(),
                &format!("attached:{attachment}"),
            )
            .map_err(|e| e.to_string())?
            .expect("terminal producer minted an intent");
            let events = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM delivery_events WHERE delivery_id=?1",
                    [&intent.delivery_id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            Ok((intent, events))
        })
        .unwrap()
}

#[tokio::test]
async fn attached_project_ignores_late_payloads_after_cleanup() {
    let server = project_server();
    let id = attach_project(&server).await;
    // Cleanup before any terminal is journaled without materializing state.
    event(&server, &id, "early-cleanup", 30, None, "not yet terminal").await;
    event(
        &server,
        &id,
        "done",
        7,
        Some("completed"),
        "PRIVATE_PROJECT_RESULT",
    )
    .await;
    let ready = assert_views(&server, &id, "completed", Some("ready")).await;
    let delivery_id = ready["delivery_intents"][0]["delivery_id"]
        .as_str()
        .unwrap();
    server
        .with_global_store(|store| {
            let caller = memcore::DeliveryCaller {
                agent_identity_id: "agent-requester".into(),
                host_identity: "host-1".into(),
            };
            memcore::claim_ready_delivery(
                store.connection(),
                &memcore::DeliveryClaimRequest {
                    caller: caller.clone(),
                    claim_key: "late-claim".into(),
                    lease_seconds: 60,
                    only_delivery_id: Some(delivery_id.into()),
                },
            )
            .map_err(|e| e.to_string())?;
            memcore::ack_delivered(store.connection(), delivery_id, &caller, "late-ack", None)
                .map_err(|e| e.to_string())
        })
        .unwrap();
    event(&server, &id, "cleanup", 9, None, "cleaned").await;
    let delivered = assert_views(&server, &id, "completed", Some("delivered")).await;
    let before = delivery_snapshot(&server, &id);
    assert_eq!(before.0.result_revision, 7);
    event(
        &server,
        &id,
        "late",
        8,
        Some("completed"),
        "PRIVATE_PROJECT_RESULT stale correction",
    )
    .await;
    assert_eq!(
        delivery_snapshot(&server, &id),
        before,
        "the producer must preserve the intent and delivery journal for stale information"
    );
    let after = assert_views(&server, &id, "completed", Some("delivered")).await;
    assert_eq!(after["attached_sessions"][0]["canonical_revision"], 9);
    assert_eq!(after["attached_sessions"][0]["cleanup_recorded"], true);
    assert_eq!(after["delivery_intents"], delivered["delivery_intents"]);

    // A valid correction committed before the separate mint must still hide A,
    // including when a later cleanup moves the canonical revision again.
    server
        .with_global_store(|store| {
            memcore::ingest_harness_session_event(
                store.connection_mut(),
                &memcore::HarnessSessionAttachmentSelector::AttachmentId(id.clone()),
                &memcore::NewHarnessSessionEvent {
                    event_id: "pending-correction".into(),
                    kind: memcore::HarnessSessionEventKind::Terminal,
                    outcome: Some(memcore::HarnessSessionTerminalOutcome::Completed),
                    source_revision: 11,
                    authority_confirmation_ref: None,
                    summary: Some("PRIVATE_PROJECT_RESULT valid correction".into()),
                    payload_digest: None,
                    occurred_at: "2026-11-01T00:00:00Z".into(),
                },
                &memcore::HarnessSessionHostAdmission {
                    host_identity: "host-1".into(),
                    connection_id: "connection-1".into(),
                },
                "admission-1",
            )
            .map_err(|e| e.to_string())
        })
        .unwrap();
    assert_eq!(delivery_snapshot(&server, &id), before);
    assert_views(&server, &id, "completed", None).await;
    event(&server, &id, "later-cleanup", 12, None, "cleaned again").await;
    assert_views(&server, &id, "completed", None).await;
    event(
        &server,
        &id,
        "fresh-correction",
        13,
        Some("completed"),
        "PRIVATE_PROJECT_RESULT valid correction",
    )
    .await;
    assert_views(&server, &id, "completed", Some("ready")).await;

    // Same-revision correction serializes at result_revision + 1. A subsequent
    // different payload below that floor is refused by the producer as well.
    event(
        &server,
        &id,
        "same-revision-correction",
        13,
        Some("completed"),
        "PRIVATE_PROJECT_RESULT corrected again",
    )
    .await;
    let corrected = assert_views(&server, &id, "completed", Some("ready")).await;
    assert_eq!(corrected["delivery_intents"][0]["result_revision"], 14);
    let settled = delivery_snapshot(&server, &id);
    event(
        &server,
        &id,
        "below-result-revision",
        13,
        Some("completed"),
        "PRIVATE_PROJECT_RESULT stale again",
    )
    .await;
    assert_eq!(delivery_snapshot(&server, &id), settled);
    let unchanged = assert_views(&server, &id, "completed", Some("ready")).await;
    assert_eq!(unchanged["delivery_intents"], corrected["delivery_intents"]);
}

#[tokio::test]
async fn attached_project_result_reference_reads_the_authorized_session() {
    let server = project_server();
    let id = attach_project(&server).await;
    event(
        &server,
        &id,
        "done",
        7,
        Some("completed"),
        "PRIVATE_PROJECT_RESULT",
    )
    .await;
    let section = view(&server, "board").await;
    let mut reference = section["items"][0]["attached_sessions"][0]["result_ref"].clone();
    assert_eq!(
        reference.as_object_mut().unwrap().remove("tool"),
        Some(json!("tachi_agent_eval"))
    );
    let before = write_count(&server);
    let raw = server
        .handle_agent_eval_value(reference)
        .await
        .expect("returned result reference must be callable");
    let state: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(state["attachment_id"], id);
    assert_eq!(state["canonical_state"]["canonical_state"], "completed");
    assert_eq!(state["canonical_state"]["canonical_revision"], 7);
    assert_eq!(write_count(&server), before);
}

async fn assert_fresh_attachment_actions_hide_target(server: &DeliveryTestServer, id: &str) {
    let before = write_count(server);
    for action in ["attach_session", "get_attachment", "reconnect_session"] {
        let mut target = attach_params("project-attachment");
        target["action"] = json!(action);
        target["attachment_id"] = json!(id);
        let mut missing = target.clone();
        missing["work_claim_id"] = json!("absent");
        missing["attachment_id"] = json!("absent");
        let absent = server.handle_agent_eval_value(missing).await.unwrap_err();
        let denied = server.handle_agent_eval_value(target).await.unwrap_err();
        assert_eq!(
            denied
                .replace("claim-attached", "absent")
                .replace(id, "absent"),
            absent,
            "{action} must not reveal a canonical attachment without project authority"
        );
    }
    assert_eq!(
        write_count(server),
        before,
        "refusal must not mutate the attachment"
    );
}

#[tokio::test]
async fn project_attachment_admission_preserves_legacy_and_masks_unqualified_targets() {
    let server = project_server();
    let id = attach_project(&server).await;
    let before = write_count(&server);
    let replay: Value = serde_json::from_str(
        &server
            .handle_agent_eval_value(attach_params("project-attachment"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(replay["admission"], "replayed");
    assert_eq!(replay["attachment_id"], id);
    let mut get = attach_params("project-attachment");
    get["action"] = json!("get_attachment");
    get["attachment_id"] = json!(id);
    let existing: Value =
        serde_json::from_str(&server.handle_agent_eval_value(get).await.unwrap()).unwrap();
    assert_eq!(existing["attachment_id"], id);
    assert_eq!(
        write_count(&server),
        before,
        "replay and get retain the original receipt"
    );

    for project in [None, Some("different-project")] {
        server.set_session_project(project);
        assert_fresh_attachment_actions_hide_target(&server, &id).await;
    }
    server.set_session_project(Some(PROJECT));
    server
        .with_global_store(|store| {
            memcore::release_work_claim(
                store.connection(),
                "host-observer",
                "host-1",
                0,
                "complete",
            )
            .map_err(|e| e.to_string())
        })
        .unwrap();
    assert_fresh_attachment_actions_hide_target(&server, &id).await;

    // A project routing hint must not withdraw the legacy NULL-claim grant.
    let legacy = test_server();
    seed_admission_and_requester(&legacy, GRANT_DELEGATE);
    seed_claim(&legacy, "attached");
    legacy
        .with_global_store(|store| {
            memcore::record_unverified_admission(
                store.connection(),
                "admission-1",
                "host-1",
                "connection-1",
                memcore::UnverifiedAdmissionState::SelfAsserted,
            )
            .map_err(|e| e.to_string())
        })
        .unwrap();
    legacy.set_session_project(Some(PROJECT));
    let raw = legacy
        .handle_agent_eval_value(attach_params("legacy-in-project"))
        .await
        .unwrap();
    let id = serde_json::from_str::<Value>(&raw).unwrap()["attachment_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut get = attach_params("legacy-in-project");
    get["action"] = json!("get_attachment");
    get["attachment_id"] = json!(id);
    assert_eq!(
        serde_json::from_str::<Value>(&legacy.handle_agent_eval_value(get).await.unwrap()).unwrap()
            ["attachment_id"],
        id
    );
}
