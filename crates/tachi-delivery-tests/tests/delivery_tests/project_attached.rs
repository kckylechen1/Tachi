//! Project reads through the actual facade, without the server lib-test binary.
use super::*;
use serde_json::Value;

const PROJECT: &str = "delivery-project";
const ISSUE: &str = "org/repo#1693";

fn project_server() -> TestFixture {
    let server = test_server();
    seed_admission_and_requester(&server, GRANT_DELEGATE);
    server.set_session_project(PROJECT);
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
}
