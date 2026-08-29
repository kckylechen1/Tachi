//! Attached-session receipt spine routes (#1678): authoritative event
//! ingest, the canonical state projection, host connection facts, reconnect
//! receipts, and capability advertisements.
//!
//! Like the v33 attachment admission, every route records receipts only.
//! None of them spawns, signals, waits on, or reaps a host-owned session,
//! and none of them reads or stores a transcript.

use crate::agent_eval::attachment::{current_host_admission, required};
use crate::server_state::MemoryServer;
use crate::tool_params::TachiAgentEvalParams;
use memcore::{
    advertise_harness_session_capabilities, get_harness_session_state,
    ingest_harness_session_event, mark_harness_session_connection, reconnect_harness_session,
    HarnessSessionAttachmentCapabilities, HarnessSessionAttachmentSelector,
    HarnessSessionConnectionFact, HarnessSessionEventKind, HarnessSessionTerminalOutcome,
    NewHarnessSessionEvent,
};
use serde_json::{json, Value};

pub(crate) fn attachment_selector(
    params: &TachiAgentEvalParams,
    host_identity: &str,
) -> Result<HarnessSessionAttachmentSelector, String> {
    if let Some(attachment_id) = params.attachment_id.clone() {
        return Ok(HarnessSessionAttachmentSelector::AttachmentId(required(
            Some(attachment_id),
            "attachment_id",
        )?));
    }
    Ok(HarnessSessionAttachmentSelector::NaturalKey {
        host_identity: host_identity.to_string(),
        protocol_version: "1".to_string(),
        adapter_connection_identity: required(
            params.adapter_connection_identity.clone(),
            "adapter_connection_identity",
        )?,
        remote_session_id: required(params.remote_session_id.clone(), "remote_session_id")?,
    })
}

fn event_kind(raw: Option<String>) -> Result<HarnessSessionEventKind, String> {
    let raw = required(raw, "session_event_kind")?;
    HarnessSessionEventKind::parse(raw.trim()).map_err(|error| error.to_string())
}

fn terminal_outcome(raw: Option<String>) -> Result<Option<HarnessSessionTerminalOutcome>, String> {
    match raw.filter(|value| !value.trim().is_empty()) {
        None => Ok(None),
        Some(raw) => match raw.trim() {
            "completed" => Ok(Some(HarnessSessionTerminalOutcome::Completed)),
            "failed" => Ok(Some(HarnessSessionTerminalOutcome::Failed)),
            "cancelled" => Ok(Some(HarnessSessionTerminalOutcome::Cancelled)),
            other => Err(format!("unknown session_event_outcome '{other}'")),
        },
    }
}

pub(crate) fn handle_ingest_session_event(
    server: &MemoryServer,
    params: TachiAgentEvalParams,
) -> Result<String, String> {
    let (host, admission_receipt_ref) = current_host_admission(
        server,
        params.host_identity.clone(),
        params.admission_receipt_ref.clone(),
    )?;
    let selector = attachment_selector(&params, &host.host_identity)?;
    let kind = event_kind(params.session_event_kind.clone())?;
    let outcome = terminal_outcome(params.session_event_outcome.clone())?;
    let input = NewHarnessSessionEvent {
        event_id: required(params.session_event_id.clone(), "session_event_id")?,
        kind,
        outcome,
        source_revision: params
            .source_revision
            .ok_or_else(|| "source_revision is required".to_string())?,
        authority_confirmation_ref: params
            .authority_confirmation_ref
            .clone()
            .filter(|value| !value.trim().is_empty()),
        summary: params
            .event_summary
            .clone()
            .filter(|value| !value.trim().is_empty()),
        payload_digest: params
            .payload_digest
            .clone()
            .filter(|value| !value.trim().is_empty()),
        occurred_at: required(params.event_occurred_at.clone(), "event_occurred_at")?,
    };
    let receipt = server.with_global_store(|store| {
        ingest_harness_session_event(
            store.connection_mut(),
            &selector,
            &input,
            &host,
            &admission_receipt_ref,
        )
        .map_err(|error| error.to_string())
    })?;
    serde_json::to_string(&json!({
        "status": "completed",
        "action": "ingest_session_event",
        "attachment_id": receipt.attachment_id,
        "event_id": receipt.event_id,
        "admission": match receipt.admission {
            memcore::HarnessSessionEventAdmission::Journaled => "journaled",
            memcore::HarnessSessionEventAdmission::Replayed => "replayed",
        },
        "disposition": receipt.disposition.as_str(),
        "canonical_state": session_state_json(&receipt.state),
    }))
    .map_err(|error| format!("serialize session event receipt: {error}"))
}

pub(crate) fn session_state_json(state: &memcore::HarnessSessionStateProjection) -> Value {
    json!({
        "canonical_state": state
            .canonical_state
            .map(memcore::HarnessSessionCanonicalState::as_str),
        "canonical_revision": state.canonical_revision,
        "cleanup_recorded": state.cleanup_recorded,
        "conflicting_terminal": state.conflicting_terminal,
        "last_event_id": state.last_event_id,
    })
}

pub(crate) fn handle_get_session_state(
    server: &MemoryServer,
    params: TachiAgentEvalParams,
) -> Result<String, String> {
    let (host, admission_receipt_ref) = current_host_admission(
        server,
        params.host_identity.clone(),
        params.admission_receipt_ref.clone(),
    )?;
    let selector = attachment_selector(&params, &host.host_identity)?;
    let state = server.with_global_store_read(|store| {
        get_harness_session_state(store.connection(), &selector, &host, &admission_receipt_ref)
            .map_err(|error| error.to_string())
    })?;
    serde_json::to_string(&json!({
        "status": "completed",
        "action": "get_session_state",
        "attachment_id": state.attachment_id,
        "canonical_state": session_state_json(&state),
    }))
    .map_err(|error| format!("serialize session state projection: {error}"))
}

pub(crate) fn handle_mark_session_connection(
    server: &MemoryServer,
    params: TachiAgentEvalParams,
) -> Result<String, String> {
    let (host, admission_receipt_ref) = current_host_admission(
        server,
        params.host_identity.clone(),
        params.admission_receipt_ref.clone(),
    )?;
    let selector = attachment_selector(&params, &host.host_identity)?;
    let fact = match required(params.connection_fact.clone(), "connection_fact")?.trim() {
        "disconnected" => HarnessSessionConnectionFact::Disconnected,
        "reconnect_failed" => HarnessSessionConnectionFact::ReconnectFailed,
        other => {
            return Err(format!(
                "unknown connection_fact '{other}' (expected 'disconnected' or 'reconnect_failed')"
            ))
        }
    };
    let receipt = server.with_global_store(|store| {
        mark_harness_session_connection(
            store.connection_mut(),
            &selector,
            fact,
            &host,
            &admission_receipt_ref,
        )
        .map_err(|error| error.to_string())
    })?;
    serde_json::to_string(&json!({
        "status": "completed",
        "action": "mark_session_connection",
        "attachment_id": receipt.attachment.attachment_id,
        "fact": fact.as_str(),
        "attachment_state": receipt.attachment.state.as_str(),
        "previous_attachment_state": receipt.previous_attachment_state.as_str(),
        "changed": receipt.changed,
        "canonical_state": session_state_json(&receipt.state),
    }))
    .map_err(|error| format!("serialize connection receipt: {error}"))
}

pub(crate) fn handle_reconnect_session(
    server: &MemoryServer,
    params: TachiAgentEvalParams,
) -> Result<String, String> {
    let (host, admission_receipt_ref) = current_host_admission(
        server,
        params.host_identity.clone(),
        params.admission_receipt_ref.clone(),
    )?;
    let selector = attachment_selector(&params, &host.host_identity)?;
    let receipt = server.with_global_store(|store| {
        reconnect_harness_session(
            store.connection_mut(),
            &selector,
            &host,
            &admission_receipt_ref,
            crate::claims_ops::CLAIM_TTL_SECONDS,
        )
        .map_err(|error| error.to_string())
    })?;
    serde_json::to_string(&json!({
        "status": "completed",
        "action": "reconnect_session",
        "attachment_id": receipt.attachment.attachment_id,
        "attachment_state": receipt.attachment.state.as_str(),
        "previous_attachment_state": receipt.previous_attachment_state.as_str(),
        "reconnected": receipt.reconnected,
        "resume_from_revision": receipt.resume_from_revision,
        "canonical_state": session_state_json(&receipt.state),
    }))
    .map_err(|error| format!("serialize reconnect receipt: {error}"))
}

pub(crate) fn handle_advertise_session_capabilities(
    server: &MemoryServer,
    params: TachiAgentEvalParams,
) -> Result<String, String> {
    let (host, admission_receipt_ref) = current_host_admission(
        server,
        params.host_identity.clone(),
        params.admission_receipt_ref.clone(),
    )?;
    let selector = attachment_selector(&params, &host.host_identity)?;
    let capabilities =
        HarnessSessionAttachmentCapabilities::from_names(&params.session_capabilities)
            .map_err(|error| error.to_string())?;
    let (sequence, capabilities_json) = server.with_global_store(|store| {
        advertise_harness_session_capabilities(
            store.connection_mut(),
            &selector,
            &capabilities,
            &host,
            &admission_receipt_ref,
        )
        .map_err(|error| error.to_string())
    })?;
    serde_json::to_string(&json!({
        "status": "completed",
        "action": "advertise_session_capabilities",
        "advertisement_seq": sequence,
        "session_capabilities": serde_json::from_str::<Value>(&capabilities_json)
            .map_err(|error| format!("stored capability JSON is not canonical: {error}"))?,
    }))
    .map_err(|error| format!("serialize capability advertisement: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_params::TachiAgentEvalParams;
    use memcore::{
        insert_agent_identity, insert_work_claim, record_unverified_admission, AgentIdentity,
        NewWorkClaim, UnverifiedAdmissionState, WorkClaimMode,
    };

    fn test_server() -> MemoryServer {
        let db_path = crate::utils::test_fixture_path(format!(
            "session-spine-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let server = MemoryServer::new(db_path, None).expect("test memory server");
        server.set_tool_profile(Some(tachi_hub::ToolProfile::coordinate()));
        server
    }

    fn seed_valid_admission(server: &MemoryServer, grant: &str) {
        server
            .with_global_store(|store| {
                insert_agent_identity(
                    store.connection(),
                    &AgentIdentity {
                        agent_identity_id: "host-1".to_string(),
                        display_name: None,
                        seat: None,
                        capability_json: None,
                        created_at: String::new(),
                    },
                )
                .map_err(|error| error.to_string())?;
                insert_agent_identity(
                    store.connection(),
                    &AgentIdentity {
                        agent_identity_id: "agent-1".to_string(),
                        display_name: None,
                        seat: None,
                        capability_json: Some(grant.to_string()),
                        created_at: String::new(),
                    },
                )
                .map_err(|error| error.to_string())?;
                record_unverified_admission(
                    store.connection(),
                    "admission-1",
                    "host-1",
                    "connection-1",
                    UnverifiedAdmissionState::SelfAsserted,
                )
                .map_err(|error| error.to_string())?;
                insert_work_claim(
                    store.connection_mut(),
                    &NewWorkClaim {
                        claim_id: "claim-1".to_string(),
                        agent_identity_id: "agent-1".to_string(),
                        session_client: Some("connection-1".to_string()),
                        issue_ref: None,
                        flow_id: None,
                        dispatch_id: None,
                        branch: "branch".to_string(),
                        worktree_path: "/tmp/session-spine-claim".to_string(),
                        declared_file_scope: "[\"src/lib.rs\"]".to_string(),
                        role: "executor".to_string(),
                        mode: WorkClaimMode::Writable,
                        expected_head: "head".to_string(),
                        lease_expires_at: "2030-01-01T00:00:00Z".to_string(),
                        created_at: String::new(),
                    },
                )
                .map_err(|error| error.to_string())?;
                Ok(())
            })
            .expect("seed valid spine admission");
        server.set_work_claim_connection(
            Some("host-1".to_string()),
            "connection-1".to_string(),
            "self_asserted".to_string(),
        );
    }

    const GRANT_DELEGATE: &str =
        r#"{"acp":{"tool_profiles":["delegate"],"capability_classes":["tachi"]}}"#;

    fn attach_params(idempotency_key: &str, capabilities: &[&str]) -> TachiAgentEvalParams {
        TachiAgentEvalParams {
            action: "attach_session".to_string(),
            host_identity: Some("host-1".to_string()),
            agent_identity_id: Some("agent-1".to_string()),
            work_claim_id: Some("claim-1".to_string()),
            expected_transition_revision: Some(0),
            protocol_version: Some(1),
            adapter_connection_identity: Some("adapter-1".to_string()),
            remote_session_id: Some("remote-1".to_string()),
            contract_digest: Some("contract-digest".to_string()),
            session_capabilities: capabilities.iter().map(|s| s.to_string()).collect(),
            tool_profile: Some("delegate".to_string()),
            capability_class: Some("tachi".to_string()),
            idempotency_key: Some(idempotency_key.to_string()),
            admission_receipt_ref: Some("admission-1".to_string()),
            ..Default::default()
        }
    }

    fn spine_params(action: &str, attachment_id: &str) -> TachiAgentEvalParams {
        TachiAgentEvalParams {
            action: action.to_string(),
            host_identity: Some("host-1".to_string()),
            admission_receipt_ref: Some("admission-1".to_string()),
            attachment_id: Some(attachment_id.to_string()),
            ..Default::default()
        }
    }

    fn event_params(
        attachment_id: &str,
        event_id: &str,
        kind: &str,
        revision: i64,
    ) -> TachiAgentEvalParams {
        TachiAgentEvalParams {
            action: "ingest_session_event".to_string(),
            host_identity: Some("host-1".to_string()),
            admission_receipt_ref: Some("admission-1".to_string()),
            attachment_id: Some(attachment_id.to_string()),
            session_event_id: Some(event_id.to_string()),
            session_event_kind: Some(kind.to_string()),
            source_revision: Some(revision),
            event_summary: Some("public-safe progress note".to_string()),
            event_occurred_at: Some("2026-08-29T00:00:00Z".to_string()),
            ..Default::default()
        }
    }

    async fn attach(server: &MemoryServer, idempotency_key: &str, capabilities: &[&str]) -> String {
        let raw = crate::agent_eval::handle_agent_eval(
            server,
            attach_params(idempotency_key, capabilities),
        )
        .await
        .expect("attach");
        let value: Value = serde_json::from_str(&raw).expect("attach JSON");
        value["attachment_id"].as_str().unwrap().to_string()
    }

    async fn eval(server: &MemoryServer, params: TachiAgentEvalParams) -> Value {
        let raw = crate::agent_eval::handle_agent_eval(server, params)
            .await
            .expect("spine action");
        serde_json::from_str(&raw).expect("spine JSON")
    }

    #[tokio::test]
    async fn event_spine_flows_over_the_facade_with_replay_and_stale_protection() {
        let server = test_server();
        seed_valid_admission(&server, GRANT_DELEGATE);
        let attachment_id = attach(&server, "spine-idem", &["observe", "events"]).await;

        let accepted = eval(&server, event_params(&attachment_id, "ev-1", "accepted", 1)).await;
        assert_eq!(accepted["canonical_state"]["canonical_state"], "accepted");
        let started = eval(&server, event_params(&attachment_id, "ev-2", "started", 2)).await;
        assert_eq!(started["canonical_state"]["canonical_state"], "started");
        let terminal = eval(
            &server,
            TachiAgentEvalParams {
                session_event_outcome: Some("completed".to_string()),
                ..event_params(&attachment_id, "ev-3", "terminal", 3)
            },
        )
        .await;
        assert_eq!(terminal["canonical_state"]["canonical_state"], "completed");
        assert_eq!(terminal["disposition"], "advanced");

        // Replay of the same event id is idempotent; material equality
        // includes the terminal outcome.
        let replay = eval(
            &server,
            TachiAgentEvalParams {
                session_event_outcome: Some("completed".to_string()),
                ..event_params(&attachment_id, "ev-3", "terminal", 3)
            },
        )
        .await;
        assert_eq!(replay["admission"], "replayed");
        // Out-of-order stale progress cannot regress the terminal.
        let stale = eval(&server, event_params(&attachment_id, "ev-4", "progress", 2)).await;
        assert_eq!(stale["disposition"], "journaled_stale");
        assert_eq!(
            stale["canonical_state"]["canonical_state"], "completed",
            "canonical state must not regress"
        );

        let state = eval(&server, spine_params("get_session_state", &attachment_id)).await;
        assert_eq!(state["canonical_state"]["canonical_state"], "completed");
        assert_eq!(state["canonical_state"]["canonical_revision"], 3);
    }

    #[tokio::test]
    async fn conflicting_terminal_facts_reconcile_over_the_facade() {
        let server = test_server();
        seed_valid_admission(&server, GRANT_DELEGATE);
        let attachment_id = attach(&server, "conflict-idem", &["observe", "events"]).await;
        let done = eval(
            &server,
            TachiAgentEvalParams {
                session_event_outcome: Some("completed".to_string()),
                ..event_params(&attachment_id, "ev-1", "terminal", 1)
            },
        )
        .await;
        assert_eq!(done["canonical_state"]["canonical_state"], "completed");
        let failed = eval(
            &server,
            TachiAgentEvalParams {
                session_event_outcome: Some("failed".to_string()),
                ..event_params(&attachment_id, "ev-2", "terminal", 2)
            },
        )
        .await;
        assert_eq!(failed["disposition"], "journaled_terminal_conflict");
        assert_eq!(
            failed["canonical_state"]["canonical_state"],
            "inconsistent_reconciling"
        );
        assert!(failed["canonical_state"]["conflicting_terminal"]
            .as_bool()
            .unwrap());
    }

    #[tokio::test]
    async fn disappearance_reconnect_and_recovery_flow_over_the_facade() {
        let server = test_server();
        seed_valid_admission(&server, GRANT_DELEGATE);
        let attachment_id = attach(&server, "reconnect-idem", &["observe", "events"]).await;
        eval(&server, event_params(&attachment_id, "ev-1", "started", 4)).await;

        let gone = eval(
            &server,
            TachiAgentEvalParams {
                connection_fact: Some("disconnected".to_string()),
                ..spine_params("mark_session_connection", &attachment_id)
            },
        )
        .await;
        assert_eq!(gone["attachment_state"], "unknown");
        assert_eq!(
            gone["canonical_state"]["canonical_state"],
            "unknown_orphaned"
        );

        let back = eval(&server, spine_params("reconnect_session", &attachment_id)).await;
        assert_eq!(back["attachment_state"], "attached");
        assert_eq!(back["resume_from_revision"], 4);
        assert_eq!(
            back["canonical_state"]["canonical_state"],
            "unknown_orphaned"
        );

        let done = eval(
            &server,
            TachiAgentEvalParams {
                session_event_outcome: Some("completed".to_string()),
                ..event_params(&attachment_id, "ev-2", "terminal", 5)
            },
        )
        .await;
        assert_eq!(done["canonical_state"]["canonical_state"], "completed");
    }

    #[tokio::test]
    async fn unsupported_cancel_over_the_facade_is_typed_and_zero_mutation() {
        let server = test_server();
        seed_valid_admission(&server, GRANT_DELEGATE);
        // observe-only fake harness profile: no cancel advertised, no
        // delegate policy.
        let attachment_id = attach(&server, "observe-idem", &["observe", "events"]).await;
        let rows_before: i64 = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM harness_session_interventions",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        let refusal = crate::agent_eval::handle_agent_eval(
            &server,
            TachiAgentEvalParams {
                action: "request_intervention".to_string(),
                host_identity: Some("host-1".to_string()),
                admission_receipt_ref: Some("admission-1".to_string()),
                attachment_id: Some(attachment_id.clone()),
                intervention_kind: Some("request_cancel".to_string()),
                intervention_request_id: Some("req-1".to_string()),
                intervention_reason: Some("operator asked".to_string()),
                expected_session_revision: Some(0),
                ..Default::default()
            },
        )
        .await
        .expect("unsupported must be a typed response, not an error");
        let value: Value = serde_json::from_str(&refusal).expect("typed refusal JSON");
        assert_eq!(value["status"], "unsupported_by_lifecycle_owner");
        assert_eq!(value["intervention_kind"], "request_cancel");
        assert_eq!(value["mutated"], false);
        let rows_after: i64 = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM harness_session_interventions",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        assert_eq!(rows_before, 0);
        assert_eq!(
            rows_after, rows_before,
            "unsupported refusal is zero mutation"
        );
    }

    /// #1678 Verification: "one ZeroClaw-style adapter fixture, no real
    /// vendor session required." A host-shaped adapter drives the full
    /// receipt spine end to end against fake ids; the vendor session itself
    /// never exists.
    #[tokio::test]
    async fn zeroclaw_style_adapter_fixture_full_receipt_spine() {
        let server = test_server();
        seed_valid_admission(&server, GRANT_DELEGATE);

        // 1. The adapter attaches its own host-owned session.
        let attachment_id = attach(
            &server,
            "zc-fixture-idem",
            &["observe", "prompt", "cancel", "resume", "events"],
        )
        .await;

        // 2. The adapter advertises the capabilities it actually supports.
        let advertised = eval(
            &server,
            TachiAgentEvalParams {
                action: "advertise_session_capabilities".to_string(),
                host_identity: Some("host-1".to_string()),
                admission_receipt_ref: Some("admission-1".to_string()),
                attachment_id: Some(attachment_id.clone()),
                session_capabilities: vec![
                    "observe".to_string(),
                    "prompt".to_string(),
                    "cancel".to_string(),
                ],
                ..Default::default()
            },
        )
        .await;
        assert_eq!(advertised["advertisement_seq"], 1);

        // 3. Session facts stream in; the agent gets stuck on input.
        eval(&server, event_params(&attachment_id, "ev-1", "accepted", 1)).await;
        eval(&server, event_params(&attachment_id, "ev-2", "started", 2)).await;
        let stuck = eval(
            &server,
            TachiAgentEvalParams {
                session_event_kind: Some("input_required".to_string()),
                ..event_params(&attachment_id, "ev-3", "input_required", 3)
            },
        )
        .await;
        assert_eq!(
            stuck["canonical_state"]["canonical_state"],
            "input_required"
        );

        // 4. A typed correction is requested; the receipt is not a state.
        let request = eval(
            &server,
            TachiAgentEvalParams {
                action: "request_intervention".to_string(),
                host_identity: Some("host-1".to_string()),
                admission_receipt_ref: Some("admission-1".to_string()),
                attachment_id: Some(attachment_id.clone()),
                intervention_kind: Some("prompt_or_correct".to_string()),
                intervention_request_id: Some("req-correct-1".to_string()),
                intervention_reason: Some("missing required test command".to_string()),
                expected_session_revision: Some(3),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(request["status"], "completed");
        assert_eq!(request["admission"], "created");
        assert_eq!(request["capability_source"], "advertised");
        assert_eq!(
            request["canonical_state"]["canonical_state"], "input_required",
            "a request must not move lifecycle state"
        );
        let replay = eval(
            &server,
            TachiAgentEvalParams {
                action: "request_intervention".to_string(),
                host_identity: Some("host-1".to_string()),
                admission_receipt_ref: Some("admission-1".to_string()),
                attachment_id: Some(attachment_id.clone()),
                intervention_kind: Some("prompt_or_correct".to_string()),
                intervention_request_id: Some("req-correct-1".to_string()),
                intervention_reason: Some("missing required test command".to_string()),
                expected_session_revision: Some(3),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(replay["admission"], "replayed");

        // 5. The harness authoritatively reports the correction was taken.
        let result = eval(
            &server,
            TachiAgentEvalParams {
                action: "record_intervention_result".to_string(),
                host_identity: Some("host-1".to_string()),
                admission_receipt_ref: Some("admission-1".to_string()),
                attachment_id: Some(attachment_id.clone()),
                intervention_request_id: Some("req-correct-1".to_string()),
                intervention_disposition: Some("accepted".to_string()),
                intervention_detail: Some("harness applied the correction".to_string()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result["status"], "completed");
        assert_eq!(result["result"]["request_kind"], "prompt_or_correct");

        // 6. The adapter then cancels with real confirmation; the terminal
        // fact binds the confirmation reference; cleanup closes it out.
        let cancel_request = eval(
            &server,
            TachiAgentEvalParams {
                action: "request_intervention".to_string(),
                host_identity: Some("host-1".to_string()),
                admission_receipt_ref: Some("admission-1".to_string()),
                attachment_id: Some(attachment_id.clone()),
                intervention_kind: Some("request_cancel".to_string()),
                intervention_request_id: Some("req-cancel-1".to_string()),
                intervention_reason: Some("operator stopped the session".to_string()),
                expected_session_revision: Some(3),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(cancel_request["status"], "completed");
        let cancel_result = eval(
            &server,
            TachiAgentEvalParams {
                action: "record_intervention_result".to_string(),
                host_identity: Some("host-1".to_string()),
                admission_receipt_ref: Some("admission-1".to_string()),
                attachment_id: Some(attachment_id.clone()),
                intervention_request_id: Some("req-cancel-1".to_string()),
                intervention_disposition: Some("accepted".to_string()),
                authority_confirmation_ref: Some("zc-confirm-42".to_string()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            cancel_result["canonical_state"]["canonical_state"], "input_required",
            "recording a result never mints the cancelled lifecycle state"
        );
        let cancelled = eval(
            &server,
            TachiAgentEvalParams {
                session_event_outcome: Some("cancelled".to_string()),
                authority_confirmation_ref: Some("zc-confirm-42".to_string()),
                ..event_params(&attachment_id, "ev-4", "terminal", 4)
            },
        )
        .await;
        assert_eq!(cancelled["canonical_state"]["canonical_state"], "cancelled");
        let cleanup = eval(
            &server,
            TachiAgentEvalParams {
                session_event_kind: Some("cleanup".to_string()),
                ..event_params(&attachment_id, "ev-5", "cleanup", 5)
            },
        )
        .await;
        assert_eq!(cleanup["disposition"], "advanced");
        assert!(cleanup["canonical_state"]["cleanup_recorded"]
            .as_bool()
            .unwrap());

        // 7. The full projection is public-safe: no transcript, no reasoning,
        // no vendor identifiers anywhere in the stored spine.
        let state = eval(&server, spine_params("get_session_state", &attachment_id)).await;
        let state_json = serde_json::to_string(&state).unwrap();
        for forbidden in [
            "transcript",
            "reasoning",
            "chain-of-thought",
            "zeroclaw-local-secret",
            "api_key",
        ] {
            assert!(!state_json.contains(forbidden), "{forbidden} leaked");
        }
        let stored_events: i64 = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row("SELECT COUNT(*) FROM harness_session_events", [], |row| {
                        row.get(0)
                    })
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        assert_eq!(stored_events, 5);
    }

    #[tokio::test]
    async fn oversize_summary_and_unknown_kinds_refuse_without_journaling() {
        let server = test_server();
        seed_valid_admission(&server, GRANT_DELEGATE);
        let attachment_id = attach(&server, "bound-idem", &["observe", "events"]).await;
        let oversize = TachiAgentEvalParams {
            event_summary: Some("x".repeat(2001)),
            ..event_params(&attachment_id, "ev-big", "progress", 1)
        };
        let error = crate::agent_eval::handle_agent_eval(&server, oversize)
            .await
            .expect_err("oversize summary must refuse");
        assert!(error.contains("bounded evidence"), "{error}");
        let error = crate::agent_eval::handle_agent_eval(
            &server,
            event_params(&attachment_id, "ev-kind", "submit", 1),
        )
        .await
        .expect_err("worker submit is not a session fact");
        assert!(
            error.contains("unknown harness session event kind"),
            "{error}"
        );
        let stored: i64 = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row("SELECT COUNT(*) FROM harness_session_events", [], |row| {
                        row.get(0)
                    })
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        assert_eq!(stored, 0, "refusals must not journal");
    }
}
