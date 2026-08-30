//! Server-level discriminations for the durable delivery seam (#1679):
//! requester restart re-claims the SAME intent, the three planes stay
//! independent, the worker cannot choose a destination, and a private
//! intent yields no existence signal to other requesters.

use crate::server_state::MemoryServer;
use crate::tool_params::{TachiAgentEvalParams, TachiDeliveryParams};
use memcore::{
    insert_agent_identity, insert_work_claim, mint_delivery_intent, AgentIdentity,
    DeliveryExecutionSource, DeliveryPolicy, DeliveryRequesterBinding, DeliveryVisibilityClass,
    NewDeliveryIntent, NewWorkClaim, WorkClaimMode,
};
use serde_json::json;

use crate::agent_eval::handle_agent_eval;

fn test_server() -> MemoryServer {
    let db_path =
        crate::utils::test_fixture_path(format!("delivery-seam-{}.sqlite", uuid::Uuid::new_v4()));
    let server = MemoryServer::new(db_path, None).expect("test memory server");
    server.set_tool_profile(Some(tachi_hub::ToolProfile::coordinate()));
    server
}

fn seed_admission_and_requester(server: &MemoryServer, grant: &str) {
    server
        .with_global_store(|store| {
            for identity in [
                ("host-1", None),
                ("agent-requester", Some(grant)),
                ("agent-other", None),
            ] {
                insert_agent_identity(
                    store.connection(),
                    &AgentIdentity {
                        agent_identity_id: identity.0.to_string(),
                        display_name: None,
                        seat: None,
                        capability_json: identity.1.map(str::to_string),
                        created_at: String::new(),
                    },
                )
                .map_err(|error| error.to_string())?;
            }
            Ok(())
        })
        .expect("seed identities");
    server.set_work_claim_connection(
        Some("host-1".to_string()),
        "connection-1".to_string(),
        "self_asserted".to_string(),
    );
}

fn seed_claim(server: &MemoryServer, dispatch_id: &str) {
    server
        .with_global_store(|store| {
            insert_work_claim(
                store.connection_mut(),
                &NewWorkClaim {
                    claim_id: format!("claim-{dispatch_id}"),
                    agent_identity_id: "agent-requester".to_string(),
                    session_client: Some("connection-1".to_string()),
                    issue_ref: None,
                    flow_id: None,
                    dispatch_id: Some(dispatch_id.to_string()),
                    branch: "branch".to_string(),
                    worktree_path: "/tmp/delivery-claim".to_string(),
                    declared_file_scope: "[\"src/lib.rs\"]".to_string(),
                    role: "executor".to_string(),
                    mode: WorkClaimMode::Writable,
                    expected_head: "head".to_string(),
                    lease_expires_at: "2030-01-01T00:00:00Z".to_string(),
                    created_at: String::new(),
                },
            )
            .map_err(|error| error.to_string())
        })
        .expect("seed claim");
}

fn delivery_params(value: serde_json::Value) -> TachiDeliveryParams {
    serde_json::from_value(value).expect("delivery params deserialize")
}

fn mint_new(idempotency_key: &str, private: bool) -> NewDeliveryIntent {
    NewDeliveryIntent {
        idempotency_key: idempotency_key.to_string(),
        execution_source: DeliveryExecutionSource::ManagedDispatch,
        execution_ref: "dispatch-1".to_string(),
        terminal_receipt_revision: 0,
        work_claim_id: None,
        result_ref: "memory:eval-1".to_string(),
        result_revision: 1,
        payload_digest: "sha256-AAAA".to_string(),
        visibility_class: if private {
            DeliveryVisibilityClass::Private
        } else {
            DeliveryVisibilityClass::Public
        },
        delivery_policy: DeliveryPolicy::ReturnToCurrentCall,
        protocol_capability: "result-ref-v1".to_string(),
        requester: DeliveryRequesterBinding::default(),
        expires_at: None,
        correction: false,
    }
}

fn seam_call(server: &MemoryServer, value: serde_json::Value) -> Result<String, String> {
    crate::delivery_ops::handle_tachi_delivery(server, delivery_params(value))
}

/// Discrimination 1/4: a requester that crashes mid-claim, restarts, and
/// re-claims recovers the SAME intent exactly once through the seam.
#[test]
fn requester_restart_reclaims_the_same_intent_through_the_seam() {
    let server = test_server();
    seed_admission_and_requester(&server, "{}");
    let intent = server
        .with_global_store(|store| {
            mint_delivery_intent(store.connection(), &mint_new("managed:dispatch-1", false))
                .map_err(|error| error.to_string())
        })
        .expect("mint");

    let claim = |key: &str| {
        seam_call(
            &server,
            json!({
                "action": "claim_ready_delivery",
                "agent_identity_id": "agent-requester",
                "host_identity": "host-1",
                "claim_key": key
            }),
        )
        .expect("claim")
    };
    let first = claim("ck-1");
    assert!(first.contains(&format!("\"delivery_id\":\"{}\"", intent.delivery_id)));
    assert!(first.contains("\"outcome\":\"claimed\""));

    // Crash: the claim lease expires (rewrite the clock column only).
    server
        .with_global_store(|store| {
            store
                .connection_mut()
                .execute(
                    "UPDATE delivery_intents SET claim_expires_at = '2026-08-29T00:00:00Z'",
                    [],
                )
                .map_err(|error| error.to_string())
        })
        .expect("expire claim");

    // Restart: resume + a fresh claim key re-claim the SAME intent.
    let resumed = seam_call(
        &server,
        json!({
            "action": "resume_requester_operation",
            "agent_identity_id": "agent-requester",
            "host_identity": "host-1"
        }),
    )
    .expect("resume");
    assert!(resumed.contains(&format!("\"delivery_id\":\"{}\"", intent.delivery_id)));
    let second = claim("ck-restart");
    assert!(second.contains("\"outcome\":\"claimed\""));
    assert!(second.contains(&format!("\"delivery_id\":\"{}\"", intent.delivery_id)));
    assert!(second.contains("\"attempt_count\":2"));

    // Exactly-once: ack, then a replayed ack is suppressed.
    let ack = seam_call(
        &server,
        json!({
            "action": "ack_delivered",
            "agent_identity_id": "agent-requester",
            "host_identity": "host-1",
            "delivery_id": intent.delivery_id,
            "ack_key": "ak-1"
        }),
    )
    .expect("ack");
    assert!(ack.contains("\"outcome\":\"acknowledged\""));
    let replay = seam_call(
        &server,
        json!({
            "action": "ack_delivered",
            "agent_identity_id": "agent-requester",
            "host_identity": "host-1",
            "delivery_id": intent.delivery_id,
            "ack_key": "ak-2"
        }),
    )
    .expect("ack replay");
    assert!(replay.contains("\"outcome\":\"already_delivered\""));

    // A delivered intent is never claimable again.
    let after = claim("ck-3");
    assert!(after.contains("\"outcome\":\"none_ready\""));
}

/// Frozen law: execution / delivery / adjudication read independently —
/// "message not delivered" is never "execution failed".
#[test]
fn delivery_failure_leaves_execution_and_adjudication_untouched() {
    let server = test_server();
    seed_admission_and_requester(&server, "{}");
    seed_claim(&server, "dispatch-1");
    let intent = server
        .with_global_store(|store| {
            mint_delivery_intent(store.connection(), &mint_new("managed:dispatch-1", false))
                .map_err(|error| error.to_string())
        })
        .expect("mint");

    // The execution and adjudication planes hold their own truth.
    server
        .with_global_store(|store| {
            store
                .connection_mut()
                .execute(
                    "INSERT INTO dispatch_outcomes
                        (outcome_id, dispatch_id, execution_outcome, idempotency_key, created_at, updated_at)
                     VALUES ('outcome-1', 'dispatch-1', 'completed', 'ik-1', '2026-08-30T00:00:00Z', '2026-08-30T00:00:00Z')",
                    [],
                )
                .map_err(|error| error.to_string())?;
            store
                .connection_mut()
                .execute(
                    "INSERT INTO dispatch_adjudications
                        (adjudication_id, outcome_id, event_key, verdict, actor, evidence_ref, created_at, insertion_seq)
                     VALUES ('adj-1', 'outcome-1', 'evt-1', 'accepted', 'reviewer', 'evidence-1', '2026-08-30T00:00:00Z', 1)",
                    [],
                )
                .map_err(|error| error.to_string())
        })
        .expect("seed execution + adjudication");

    // Claim then fail delivery: transport failure, retry scheduled.
    seam_call(
        &server,
        json!({
            "action": "claim_ready_delivery",
            "agent_identity_id": "agent-requester",
            "host_identity": "host-1",
            "claim_key": "ck-f1"
        }),
    )
    .expect("claim");
    let blocked = seam_call(
        &server,
        json!({
            "action": "reject_or_block",
            "agent_identity_id": "agent-requester",
            "host_identity": "host-1",
            "delivery_id": intent.delivery_id,
            "blocker_class": "transport_failure",
            "detail": "connection refused before send",
            "retry_in_seconds": 30
        }),
    )
    .expect("reject");
    assert!(blocked.contains("\"delivery_state\":\"retrying\""));

    // Execution truth: still `completed`. Adjudication truth: still
    // `accepted`. Delivery truth: `retrying`. Three planes, no rewrites.
    let planes: (String, String, String) = server
        .with_global_store(|store| {
            let execution: String = store
                .connection()
                .query_row(
                    "SELECT execution_outcome FROM dispatch_outcomes WHERE outcome_id = 'outcome-1'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            let adjudication: String = store
                .connection()
                .query_row(
                    "SELECT verdict FROM dispatch_adjudications WHERE outcome_id = 'outcome-1'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            let delivery: String = store
                .connection()
                .query_row(
                    "SELECT delivery_state FROM delivery_intents WHERE delivery_id = ?1",
                    [&intent.delivery_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            Ok((execution, adjudication, delivery))
        })
        .expect("read three planes");
    assert_eq!(
        planes,
        (
            "completed".to_string(),
            "accepted".to_string(),
            "retrying".to_string()
        )
    );

    // Dismiss changes only the delivery plane.
    let dismissed = seam_call(
        &server,
        json!({
            "action": "dismiss",
            "agent_identity_id": "agent-requester",
            "host_identity": "host-1",
            "delivery_id": intent.delivery_id
        }),
    )
    .expect("dismiss");
    assert!(dismissed.contains("\"delivery_state\":\"dismissed\""));
    let execution_still: String = server
        .with_global_store(|store| {
            store
                .connection()
                .query_row(
                    "SELECT execution_outcome FROM dispatch_outcomes WHERE outcome_id = 'outcome-1'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("read execution");
    assert_eq!(execution_still, "completed");
}

/// Discrimination 7: the worker cannot choose the user/channel. Unknown
/// wire fields (a destination, a channel, a policy) are deserialization
/// refusals before any handler runs.
#[test]
fn seam_wire_refuses_worker_chosen_destinations() {
    for forged in [
        json!({
            "action": "claim_ready_delivery",
            "agent_identity_id": "agent-requester",
            "host_identity": "host-1",
            "claim_key": "ck-w",
            "deliver_to_user": "wechat:bob"
        }),
        json!({
            "action": "claim_ready_delivery",
            "agent_identity_id": "agent-requester",
            "host_identity": "host-1",
            "claim_key": "ck-w",
            "channel": "wecom"
        }),
        json!({
            "action": "mint",
            "agent_identity_id": "agent-requester",
            "host_identity": "host-1"
        }),
        json!({
            "action": "claim_ready_delivery",
            "agent_identity_id": "agent-requester",
            "host_identity": "host-1",
            "claim_key": "ck-w",
            "delivery_policy": "announce_requester_session"
        }),
    ] {
        assert!(
            serde_json::from_value::<TachiDeliveryParams>(forged).is_err(),
            "worker-chosen destination/mint/policy fields must be refused at the wire"
        );
    }
}

/// Discrimination 6: a private intent yields no existence signal — not an
/// error shape, not a count — to a requester it is not bound to.
#[test]
fn private_intent_yields_no_existence_signal_to_other_requesters() {
    let server = test_server();
    seed_admission_and_requester(&server, "{}");
    let mut new = mint_new("managed:dispatch-private", true);
    new.visibility_class = DeliveryVisibilityClass::Private;
    new.requester = DeliveryRequesterBinding {
        agent_identity_id: Some("agent-requester".to_string()),
        host_identity: Some("host-1".to_string()),
        session_ref: None,
    };
    let intent = server
        .with_global_store(|store| {
            mint_delivery_intent(store.connection(), &new).map_err(|error| error.to_string())
        })
        .expect("mint private");

    // Wrong requester: claims see nothing, resume sees nothing, get is the
    // same generic not-found as a bogus id.
    let wrong_claim = seam_call(
        &server,
        json!({
            "action": "claim_ready_delivery",
            "agent_identity_id": "agent-other",
            "host_identity": "host-1",
            "claim_key": "ck-p1"
        }),
    )
    .expect("claim as wrong requester");
    assert!(wrong_claim.contains("\"outcome\":\"none_ready\""));

    let wrong_resume = seam_call(
        &server,
        json!({
            "action": "resume_requester_operation",
            "agent_identity_id": "agent-other",
            "host_identity": "host-1"
        }),
    )
    .expect("resume as wrong requester");
    assert!(wrong_resume.contains("\"deliveries\":[]"));

    let wrong_get = seam_call(
        &server,
        json!({
            "action": "get",
            "agent_identity_id": "agent-other",
            "host_identity": "host-1",
            "delivery_id": intent.delivery_id
        }),
    );
    assert_eq!(wrong_get, Err("delivery intent not found".to_string()));

    let bogus_get = seam_call(
        &server,
        json!({
            "action": "get",
            "agent_identity_id": "agent-other",
            "host_identity": "host-1",
            "delivery_id": "di-does-not-exist"
        }),
    );
    assert_eq!(bogus_get, Err("delivery intent not found".to_string()));

    // The bound requester claims it through the seam.
    let right = seam_call(
        &server,
        json!({
            "action": "claim_ready_delivery",
            "agent_identity_id": "agent-requester",
            "host_identity": "host-1",
            "claim_key": "ck-p2"
        }),
    )
    .expect("claim as bound requester");
    assert!(right.contains(&format!("\"delivery_id\":\"{}\"", intent.delivery_id)));
}

/// The managed terminal plane mints (and reconciles) a delivery intent with
/// the admitted requester binding from the owning WorkClaim.
#[test]
fn managed_terminal_outcome_mints_and_binds_the_requester() {
    let server = test_server();
    seed_admission_and_requester(&server, "{}");
    seed_claim(&server, "dispatch-1");

    let outcome = memcore::NewDispatchOutcome {
        outcome_id: "outcome-1".to_string(),
        dispatch_id: "dispatch-1".to_string(),
        eval_memory_id: None,
        model: None,
        vendor: "unknown".to_string(),
        role: None,
        seat: None,
        task_type: None,
        execution_outcome: "completed".to_string(),
        reported_outcome: Some("success".to_string()),
        retry_count: 0,
        error_class: None,
        issue_ref: None,
        pr_ref: None,
        flow_id: None,
        cost_tokens: None,
        cost_usd: None,
        verification_present: false,
        diff_present: false,
        evidence_refs: json!([]),
        identity_receipt: None,
        identity_attribution_basis: "unknown".to_string(),
    };
    let row = server
        .with_global_store(|store| {
            store
                .upsert_dispatch_outcome(&outcome)
                .map_err(|error| error.to_string())
        })
        .expect("record outcome");
    server
        .with_global_store(|store| {
            crate::delivery_ops::mint_delivery_for_managed_outcome(
                store,
                &row,
                "memory:eval-1".to_string(),
            );
            Ok::<(), String>(())
        })
        .expect("mint");

    // Idempotent reconcile on re-record.
    server
        .with_global_store(|store| {
            crate::delivery_ops::mint_delivery_for_managed_outcome(
                store,
                &row,
                "memory:eval-1".to_string(),
            );
            Ok::<(), String>(())
        })
        .expect("mint");

    // Managed binding law: the intent is bound to the AGENT identity the
    // owning WorkClaim carried (the fabric's registry identity). The seam
    // claim presents that identity over the admitted host connection.
    let claim = seam_call(
        &server,
        json!({
            "action": "claim_ready_delivery",
            "agent_identity_id": "agent-requester",
            "host_identity": "host-1",
            "claim_key": "ck-m1"
        }),
    )
    .expect("claim");
    assert!(
        claim.contains(r#""outcome":"claimed""#),
        "claim response: {claim}"
    );

    let observed: Vec<memcore::DeliveryIntent> = server
        .with_global_store(|store| {
            memcore::observe_delivery_for_execution(
                store.connection(),
                "managed_dispatch",
                "dispatch-1",
            )
            .map_err(|error| error.to_string())
        })
        .expect("observe");
    assert_eq!(observed.len(), 1, "one intent per terminal receipt");
    assert_eq!(
        observed[0].delivery_state, "requester_queued",
        "the seam claim above holds it"
    );
    assert_eq!(
        observed[0].requester_agent_identity_id.as_deref(),
        Some("agent-requester"),
        "requester bound from the owning WorkClaim, never from worker prose"
    );
    assert_eq!(observed[0].visibility_class, "private");
}

/// codex R2 round-2 regression: host rotation does not inherit private
/// reads. The SAME agent identity offered over a DIFFERENT admitted host
/// connection cannot read a delivery bound to the original host.
#[tokio::test]
async fn private_get_is_bound_to_the_admitting_host() {
    let server = test_server();
    seed_admission_and_requester(&server, "{}");
    let mut new = mint_new("managed:dispatch-bound", true);
    new.visibility_class = DeliveryVisibilityClass::Private;
    new.requester = DeliveryRequesterBinding {
        agent_identity_id: Some("agent-requester".to_string()),
        host_identity: Some("host-1".to_string()),
        session_ref: None,
    };
    let intent = server
        .with_global_store(|store| {
            mint_delivery_intent(store.connection(), &new).map_err(|error| error.to_string())
        })
        .expect("mint private");

    // The original admitted host reads it.
    let ok = seam_call(
        &server,
        json!({
            "action": "get",
            "agent_identity_id": "agent-requester",
            "host_identity": "host-1",
            "delivery_id": intent.delivery_id
        }),
    );
    assert!(ok.is_ok());

    // Admission rotates to host-2; the same agent over host-2 is refused
    // with the same generic not-found.
    server.set_work_claim_connection(
        Some("host-2".to_string()),
        "connection-2".to_string(),
        "self_asserted".to_string(),
    );
    let rotated = seam_call(
        &server,
        json!({
            "action": "get",
            "agent_identity_id": "agent-requester",
            "host_identity": "host-2",
            "delivery_id": intent.delivery_id
        }),
    );
    assert_eq!(rotated, Err("delivery intent not found".to_string()));
}

const GRANT_DELEGATE: &str =
    r#"{"acp":{"tool_profiles":["delegate"],"capability_classes":["tachi"]}}"#;

fn attach_params(idempotency_key: &str) -> TachiAgentEvalParams {
    TachiAgentEvalParams {
        action: "attach_session".to_string(),
        host_identity: Some("host-1".to_string()),
        agent_identity_id: Some("agent-requester".to_string()),
        work_claim_id: Some("claim-attached".to_string()),
        expected_transition_revision: Some(0),
        protocol_version: Some(1),
        adapter_connection_identity: Some("adapter-1".to_string()),
        remote_session_id: Some("remote-1".to_string()),
        contract_digest: Some("contract-digest".to_string()),
        session_capabilities: vec!["observe".to_string()],
        tool_profile: Some("delegate".to_string()),
        capability_class: Some("tachi".to_string()),
        idempotency_key: Some(idempotency_key.to_string()),
        admission_receipt_ref: Some("admission-1".to_string()),
        ..Default::default()
    }
}

/// The attached terminal plane (#1678 spine) mints a delivery intent bound
/// to the admitted attachment identity, through the real ingest route.
#[tokio::test]
async fn attached_terminal_event_mints_a_delivery_intent() {
    let server = test_server();
    seed_admission_and_requester(&server, GRANT_DELEGATE);
    // Seed the attached spine admission the same way the #1678 routes do.
    server
        .with_global_store(|store| {
            memcore::record_unverified_admission(
                store.connection(),
                "admission-1",
                "host-1",
                "connection-1",
                memcore::UnverifiedAdmissionState::SelfAsserted,
            )
            .map_err(|error| error.to_string())?;
            insert_work_claim(
                store.connection_mut(),
                &NewWorkClaim {
                    claim_id: "claim-attached".to_string(),
                    agent_identity_id: "agent-requester".to_string(),
                    session_client: Some("connection-1".to_string()),
                    issue_ref: None,
                    flow_id: None,
                    dispatch_id: None,
                    branch: "branch".to_string(),
                    worktree_path: "/tmp/delivery-attached".to_string(),
                    declared_file_scope: "[\"src/lib.rs\"]".to_string(),
                    role: "executor".to_string(),
                    mode: WorkClaimMode::Writable,
                    expected_head: "head".to_string(),
                    lease_expires_at: "2030-01-01T00:00:00Z".to_string(),
                    created_at: String::new(),
                },
            )
            .map_err(|error| error.to_string())
        })
        .expect("seed attached admission");

    let attached = handle_agent_eval(&server, attach_params("attach-1"))
        .await
        .expect("attach");
    assert!(attached.contains("attachment_id"));
    assert!(!attached.contains("\"attachment_id\":\"\""));

    let terminal = TachiAgentEvalParams {
        action: "ingest_session_event".to_string(),
        host_identity: Some("host-1".to_string()),
        admission_receipt_ref: Some("admission-1".to_string()),
        session_event_id: Some("evt-terminal-1".to_string()),
        session_event_kind: Some("terminal".to_string()),
        session_event_outcome: Some("completed".to_string()),
        source_revision: Some(7),
        event_summary: Some("run completed".to_string()),
        event_occurred_at: Some("2026-08-30T00:00:00Z".to_string()),
        ..attach_params("attach-1")
    };
    // The ingest route resolves the attachment by its natural key; the
    // attachment_id returned by attach feeds the selector.
    let attachment_id = serde_json::from_str::<serde_json::Value>(&attached)
        .expect("attach receipt json")["attachment_id"]
        .as_str()
        .expect("attachment id")
        .to_string();
    let mut terminal = terminal;
    terminal.attachment_id = Some(attachment_id.clone());
    let receipt = handle_agent_eval(&server, terminal)
        .await
        .expect("ingest terminal");
    assert!(receipt.contains("\"canonical_state\""));

    // The delivery intent exists for the attached receipt, private to the
    // admitted requester.
    let observed: Vec<memcore::DeliveryIntent> = server
        .with_global_store(|store| {
            memcore::observe_delivery_for_execution(
                store.connection(),
                "attached_session",
                serde_json::from_str::<serde_json::Value>(&attached).unwrap()["attachment_id"]
                    .as_str()
                    .unwrap(),
            )
            .map_err(|error| error.to_string())
        })
        .expect("observe attached");
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].delivery_state, "ready");
    assert_eq!(
        observed[0].requester_agent_identity_id.as_deref(),
        Some("agent-requester")
    );
    assert_eq!(
        observed[0].requester_session_ref.as_deref(),
        Some("remote-1")
    );
    assert_eq!(observed[0].result_revision, 7);

    // Disposition gate: a STALE terminal fact (lower source revision) is
    // journaled without advancing canonical state and mints NOTHING —
    // still exactly one intent for the run.
    let stale = TachiAgentEvalParams {
        action: "ingest_session_event".to_string(),
        host_identity: Some("host-1".to_string()),
        admission_receipt_ref: Some("admission-1".to_string()),
        attachment_id: Some(attachment_id.clone()),
        session_event_id: Some("evt-terminal-0".to_string()),
        session_event_kind: Some("terminal".to_string()),
        session_event_outcome: Some("completed".to_string()),
        source_revision: Some(3),
        event_summary: Some("stale terminal".to_string()),
        event_occurred_at: Some("2026-08-30T00:00:00Z".to_string()),
        ..attach_params("attach-1")
    };
    let stale_receipt = handle_agent_eval(&server, stale)
        .await
        .expect("stale ingest");
    // The receipt spine journals the redundant terminal without advancing
    // anything; the delivery reconcile against the same run key is a no-op.
    assert!(stale_receipt.contains("journaled_redundant_terminal"));

    let after: Vec<memcore::DeliveryIntent> = server
        .with_global_store(|store| {
            memcore::observe_delivery_for_execution(
                store.connection(),
                "attached_session",
                serde_json::from_str::<serde_json::Value>(&attached).unwrap()["attachment_id"]
                    .as_str()
                    .unwrap(),
            )
            .map_err(|error| error.to_string())
        })
        .expect("observe after stale");
    // Freeze the run's delivery exactly as the first canonical terminal set
    // it: one intent, same state, same result revision, same result ref,
    // same revision counter, same event count. Nothing about a stale or
    // redundant terminal may re-arm or supersede it.
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].delivery_state, "ready");
    assert_eq!(after[0].result_revision, 7);
    assert_eq!(
        after[0].result_ref,
        format!("harness_session:{attachment_id}"),
        "run-stable result locator"
    );
    let events: i64 = server
        .with_global_store(|store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM delivery_events WHERE delivery_id = ?1",
                    [&after[0].delivery_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("event count");
    assert_eq!(events, 2, "mint receipts only; no supersede/re-arm events");

    // Exact replay of the SAME terminal event id: Replayed admission, mint
    // re-attempted idempotently (the recovery path if the first mint
    // failed), zero duplication.
    let exact_replay = TachiAgentEvalParams {
        action: "ingest_session_event".to_string(),
        host_identity: Some("host-1".to_string()),
        admission_receipt_ref: Some("admission-1".to_string()),
        attachment_id: Some(attachment_id.clone()),
        session_event_id: Some("evt-terminal-1".to_string()),
        session_event_kind: Some("terminal".to_string()),
        session_event_outcome: Some("completed".to_string()),
        source_revision: Some(7),
        event_summary: Some("run completed".to_string()),
        event_occurred_at: Some("2026-08-30T00:00:00Z".to_string()),
        ..attach_params("attach-1")
    };
    let replayed = handle_agent_eval(&server, exact_replay)
        .await
        .expect("exact replay");
    assert!(replayed.contains(r#""admission":"replayed""#));
    let after_replay: Vec<memcore::DeliveryIntent> = server
        .with_global_store(|store| {
            memcore::observe_delivery_for_execution(
                store.connection(),
                "attached_session",
                &attachment_id,
            )
            .map_err(|error| error.to_string())
        })
        .expect("observe after replay");
    assert_eq!(after_replay.len(), 1, "replay never mints a sibling");
    assert_eq!(after_replay[0].result_revision, 7);

    // A newer-revision redundant terminal carrying a CORRECTED payload is
    // fresher delivery truth: the same intent supersedes to the new
    // payload.
    let corrected = TachiAgentEvalParams {
        action: "ingest_session_event".to_string(),
        host_identity: Some("host-1".to_string()),
        admission_receipt_ref: Some("admission-1".to_string()),
        attachment_id: Some(attachment_id.clone()),
        session_event_id: Some("evt-terminal-2".to_string()),
        session_event_kind: Some("terminal".to_string()),
        session_event_outcome: Some("completed".to_string()),
        source_revision: Some(11),
        event_summary: Some("run completed with corrected artifact".to_string()),
        event_occurred_at: Some("2026-08-30T00:00:01Z".to_string()),
        ..attach_params("attach-1")
    };
    let corrected_receipt = handle_agent_eval(&server, corrected)
        .await
        .expect("corrected terminal");
    assert!(corrected_receipt.contains("journaled_redundant_terminal"));
    let after_corrected: Vec<memcore::DeliveryIntent> = server
        .with_global_store(|store| {
            memcore::observe_delivery_for_execution(
                store.connection(),
                "attached_session",
                &attachment_id,
            )
            .map_err(|error| error.to_string())
        })
        .expect("observe after corrected");
    assert_eq!(after_corrected.len(), 1, "still one intent per run");
    assert_eq!(
        after_corrected[0].result_revision, 11,
        "mirrors fresher payload"
    );
    assert_eq!(
        after_corrected[0].delivery_state, "ready",
        "corrected payload re-arms delivery"
    );
}
