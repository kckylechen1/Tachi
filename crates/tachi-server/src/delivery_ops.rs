//! Durable delivery seam routes (#1679): `claim_ready_delivery`,
//! `ack_delivered`, `reject_or_block`, `resume_requester_operation`,
//! `dismiss`, and the projection observation `get`.
//!
//! Every route is receipts-only over the v36 delivery spine. None of them
//! writes execution or adjudication truth, none accepts a destination, and
//! every route binds the caller to the CURRENT admitted host connection
//! plus a registered requester AgentIdentity before touching the spine —
//! the same admission bar the attached-session seam (#1678) applies.

use crate::server_state::MemoryServer;
use crate::tool_params::{TachiDeliveryAction, TachiDeliveryParams};
use memcore::{
    ack_delivered, claim_ready_delivery, dismiss_delivery, get_delivery_intent, reject_or_block,
    resume_requester_operation, DeliveryCaller, DeliveryClaimRequest,
};
use serde_json::{json, Value};

fn required(value: Option<String>, name: &str) -> Result<String, String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{name} is required"))
}

/// The caller identity every seam call binds: the admitted host connection
/// (param must match the CURRENT connection, mirroring
/// `agent_eval::attachment::current_host_admission`) plus a registered
/// requester AgentIdentity.
fn verify_caller(
    server: &MemoryServer,
    params: &TachiDeliveryParams,
) -> Result<DeliveryCaller, String> {
    let host_identity = required(params.host_identity.clone(), "host_identity")?;
    let agent_identity_id = required(params.agent_identity_id.clone(), "agent_identity_id")?;
    let (admitted_host, _connection_id, admission_state) =
        server.work_claim_connection().ok_or_else(|| {
            "no active host admission; the delivery seam requires an admitted host connection"
                .to_string()
        })?;
    if !matches!(admission_state.as_str(), "self_asserted" | "verified") {
        return Err("current host admission is not active".to_string());
    }
    if let Some(admitted) = admitted_host {
        if !admitted.trim().is_empty() && admitted != host_identity {
            return Err("host_identity does not match the current host connection".to_string());
        }
    }
    server.with_global_store(|store| {
        let known: bool = store
            .connection()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_identity_id = ?1)",
                [&agent_identity_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if !known {
            return Err(format!(
                "unknown requester agent identity {agent_identity_id}"
            ));
        }
        Ok(())
    })?;
    Ok(DeliveryCaller {
        agent_identity_id,
        host_identity,
    })
}

/// No existence signal: an unauthorized or absent read is the same generic
/// refusal, never a distinct "exists but not yours".
fn not_found() -> String {
    "delivery intent not found".to_string()
}

pub(crate) fn handle_tachi_delivery(
    server: &MemoryServer,
    params: TachiDeliveryParams,
) -> Result<String, String> {
    let action = params.action.as_str();
    if !tachi_hub::facade_action_allowed(
        "tachi_delivery",
        Some(action),
        server.active_tool_profile(),
    ) {
        return Err(format!(
            "tool action '{action}' is not allowed for the active Tachi profile"
        ));
    }
    let caller = verify_caller(server, &params)?;
    match params.action {
        TachiDeliveryAction::ClaimReadyDelivery => {
            let claim_key = required(params.claim_key.clone(), "claim_key")?;
            let request = DeliveryClaimRequest {
                caller: caller.clone(),
                claim_key,
                lease_seconds: params.lease_seconds.unwrap_or(0),
                only_delivery_id: None,
            };
            let outcome = server.with_global_store(|store| {
                claim_ready_delivery(store.connection(), &request)
                    .map_err(|error| error.to_string())
            })?;
            let (outcome_token, delivery) = match outcome {
                memcore::DeliveryClaimOutcome::Claimed(view) => (
                    "claimed",
                    Some(serde_json::to_value(&view).expect("view serializes")),
                ),
                memcore::DeliveryClaimOutcome::ReplayedClaim(view) => (
                    "replayed_claim",
                    Some(serde_json::to_value(&view).expect("view serializes")),
                ),
                memcore::DeliveryClaimOutcome::NoneReady => ("none_ready", None),
            };
            Ok(json!({
                "status": "completed",
                "action": "claim_ready_delivery",
                "outcome": outcome_token,
                "delivery": delivery,
            })
            .to_string())
        }
        TachiDeliveryAction::AckDelivered => {
            let delivery_id = required(params.delivery_id.clone(), "delivery_id")?;
            let ack_key = required(params.ack_key.clone(), "ack_key")?;
            let outcome = server.with_global_store(|store| {
                ack_delivered(
                    store.connection(),
                    &delivery_id,
                    &caller,
                    &ack_key,
                    params.expected_revision,
                )
                .map_err(|error| error.to_string())
            })?;
            let (outcome_token, revision) = match outcome {
                memcore::DeliveryAckOutcome::Acknowledged { revision } => {
                    ("acknowledged", Some(revision))
                }
                memcore::DeliveryAckOutcome::AlreadyDelivered => ("already_delivered", None),
            };
            Ok(json!({
                "status": "completed",
                "action": "ack_delivered",
                "outcome": outcome_token,
                "revision": revision,
            })
            .to_string())
        }
        TachiDeliveryAction::RejectOrBlock => {
            let delivery_id = required(params.delivery_id.clone(), "delivery_id")?;
            let blocker_class = required(params.blocker_class.clone(), "blocker_class")?;
            let intent = server.with_global_store(|store| {
                reject_or_block(
                    store.connection(),
                    &delivery_id,
                    &caller,
                    &blocker_class,
                    params.detail.as_deref(),
                    params.retry_in_seconds,
                    params.expected_revision,
                )
                .map_err(|error| error.to_string())
            })?;
            Ok(json!({
                "status": "completed",
                "action": "reject_or_block",
                "delivery_id": intent.delivery_id,
                "delivery_state": intent.delivery_state,
                "blocker_class": intent.blocker_class,
                "revision": intent.revision,
            })
            .to_string())
        }
        TachiDeliveryAction::ResumeRequesterOperation => {
            let intents = server.with_global_store(|store| {
                resume_requester_operation(store.connection(), &caller)
                    .map_err(|error| error.to_string())
            })?;
            let deliveries: Vec<Value> = intents
                .iter()
                .map(|intent| delivery_summary(intent))
                .collect();
            Ok(json!({
                "status": "completed",
                "action": "resume_requester_operation",
                "deliveries": deliveries,
            })
            .to_string())
        }
        TachiDeliveryAction::Dismiss => {
            let delivery_id = required(params.delivery_id.clone(), "delivery_id")?;
            let intent = server.with_global_store(|store| {
                dismiss_delivery(
                    store.connection(),
                    &delivery_id,
                    &caller,
                    params.expected_revision,
                )
                .map_err(|error| error.to_string())
            })?;
            Ok(json!({
                "status": "completed",
                "action": "dismiss",
                "delivery_id": intent.delivery_id,
                "delivery_state": intent.delivery_state,
                "revision": intent.revision,
            })
            .to_string())
        }
        TachiDeliveryAction::Get => {
            let delivery_id = required(params.delivery_id.clone(), "delivery_id")?;
            let intent = server.with_global_store(|store| {
                get_delivery_intent(store.connection(), &delivery_id)
                    .map_err(|error| error.to_string())
            })?;
            match intent {
                Some(intent)
                    if intent.visibility_class != "private"
                        || intent.requester_agent_identity_id.as_deref()
                            == Some(caller.agent_identity_id.as_str()) =>
                {
                    Ok(json!({
                        "status": "completed",
                        "action": "get",
                        "delivery": delivery_summary(&intent),
                    })
                    .to_string())
                }
                // Missing and unauthorized are indistinguishable here: no
                // forbidden existence signal (discrimination 6).
                _ => Err(not_found()),
            }
        }
    }
}

/// Bounded delivery summary for seam responses: state, revision, and
/// policy-visible refs. No raw content exists on the spine to leak.
fn delivery_summary(intent: &memcore::DeliveryIntent) -> Value {
    json!({
        "delivery_id": intent.delivery_id,
        "delivery_state": intent.delivery_state,
        "blocker_class": intent.blocker_class,
        "result_ref": intent.result_ref,
        "result_revision": intent.result_revision,
        "payload_digest": intent.payload_digest,
        "visibility_class": intent.visibility_class,
        "delivery_policy": intent.delivery_policy,
        "protocol_capability": intent.protocol_capability,
        "execution_source": intent.execution_source,
        "execution_ref": intent.execution_ref,
        "work_claim_id": intent.work_claim_id,
        "requester_session_ref": intent.requester_session_ref,
        "attempt_count": intent.attempt_count,
        "next_retry_at": intent.next_retry_at,
        "revision": intent.revision,
        "created_at": intent.created_at,
        "ready_at": intent.ready_at,
        "delivered_at": intent.delivered_at,
        "dismissed_at": intent.dismissed_at,
    })
}

// ---------------------------------------------------------------------------
// terminal-plane mint (server-side; no seam action can mint)
// ---------------------------------------------------------------------------

/// Wall-clock base for managed correction revisions (epoch millis of NOW,
/// not of the receipt timestamp): a corrected mint must strictly exceed
/// every revision the wall clock could have produced before it.
fn revision_wall_clock_base() -> i64 {
    chrono::Utc::now().timestamp_millis().max(1)
}

fn digest_token(parts: &[&str]) -> String {
    let joined = parts.join("\u{1f}");
    let digest = <sha2::Sha256 as sha2::Digest>::digest(joined.as_bytes());
    let hex: String = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("");
    format!("sha256-{hex}")
}

/// Mint (or idempotently reconcile) the durable delivery intent for a
/// managed dispatch's canonical terminal receipt (#1679). Delivery is a
/// separate plane: a mint failure logs a warning and never rewrites the
/// execution truth the outcome row carries.
pub(crate) fn mint_delivery_for_managed_outcome(
    server: &MemoryServer,
    outcome: &memcore::DispatchOutcomeRow,
    eval_memory_id: &str,
) {
    let task_type = outcome.task_type.as_deref().unwrap_or("-");
    let idempotency_key = format!("managed:{}:{}", outcome.dispatch_id, task_type);
    let payload_digest = digest_token(&[
        &outcome.outcome_id,
        &outcome.execution_outcome,
        eval_memory_id,
        &outcome.evidence_refs.to_string(),
    ]);
    // Content-aware correction revision: the FIRST mint takes the wall
    // clock; a content CHANGE (a corrected terminal receipt) takes
    // strictly-more-than any revision this intent has ever carried, so the
    // spine's supersede path re-arms delivery instead of hitting the
    // equal-revision idempotency conflict — even when two corrections land
    // within the same clock tick.
    let result_revision = match server.with_global_store(|store| {
        memcore::find_delivery_intent_by_idempotency_key(store.connection(), &idempotency_key)
            .map_err(|error| error.to_string())
    }) {
        Ok(Some(existing)) if existing.payload_digest != payload_digest => {
            (existing.result_revision + 1).max(revision_wall_clock_base())
        }
        Ok(Some(existing)) => existing.result_revision,
        Ok(None) => revision_wall_clock_base(),
        Err(error) => {
            tracing::warn!(
                error = %error,
                dispatch_id = %outcome.dispatch_id,
                "failed to read existing delivery intent before mint"
            );
            return;
        }
    };
    let mut new = memcore::NewDeliveryIntent {
        idempotency_key,
        execution_source: memcore::DeliveryExecutionSource::ManagedDispatch,
        execution_ref: outcome.dispatch_id.clone(),
        terminal_receipt_revision: 0,
        work_claim_id: None,
        result_ref: format!("memory:{eval_memory_id}"),
        result_revision,
        payload_digest,
        visibility_class: memcore::DeliveryVisibilityClass::Public,
        delivery_policy: memcore::DeliveryPolicy::ReturnToCurrentCall,
        protocol_capability: "result-ref-v1".to_string(),
        requester: memcore::DeliveryRequesterBinding::default(),
        expires_at: None,
    };
    // Bind the admitted requester from the owning WorkClaim when one names
    // this dispatch. A bound intent is private to that requester (fail-
    // closed default); unbound stays public/pull-only.
    if let Ok(Some((claim_id, agent_identity_id, session_client))) =
        server.with_global_store(|store| {
            memcore::find_claim_requester_for_dispatch(store.connection(), &outcome.dispatch_id)
                .map_err(|error| error.to_string())
        })
    {
        new.visibility_class = memcore::DeliveryVisibilityClass::Private;
        new.work_claim_id = Some(claim_id);
        new.requester = memcore::DeliveryRequesterBinding {
            agent_identity_id: Some(agent_identity_id),
            host_identity: None,
            session_ref: session_client,
        };
    }
    if let Err(error) = server.with_global_store(|store| {
        memcore::mint_delivery_intent(store.connection(), &new).map_err(|error| error.to_string())
    }) {
        tracing::warn!(
            error = %error,
            dispatch_id = %outcome.dispatch_id,
            "failed to mint delivery intent for terminal outcome"
        );
    }
}

/// Mint (or idempotently reconcile) the durable delivery intent for an
/// attached session's terminal event receipt (#1678 spine, #1679 delivery).
/// The requester binding comes from the admitted attachment, never from the
/// host-reported event payload.
#[allow(clippy::too_many_arguments)]
pub(crate) fn mint_delivery_for_attached_terminal(
    server: &MemoryServer,
    attachment_id: &str,
    event_id: &str,
    source_revision: i64,
    outcome_token: &str,
    summary: Option<&str>,
    payload_digest: Option<&str>,
    agent_identity_id: &str,
    host_identity: &str,
    remote_session_id: &str,
    work_claim_id: &str,
) {
    let digest = payload_digest
        .map(str::to_string)
        .unwrap_or_else(|| digest_token(&[outcome_token, summary.unwrap_or("")]));
    // One durable intent per attached RUN (keyed on the attachment): a
    // redundant terminal event for the same run reconciles against the SAME
    // intent instead of minting a sibling. The digest excludes the event id
    // for the same reason.
    let new = memcore::NewDeliveryIntent {
        idempotency_key: format!("attached:{attachment_id}"),
        execution_source: memcore::DeliveryExecutionSource::AttachedSession,
        execution_ref: attachment_id.to_string(),
        terminal_receipt_revision: source_revision.max(0),
        work_claim_id: (!work_claim_id.is_empty()).then(|| work_claim_id.to_string()),
        result_ref: format!("harness_session:{attachment_id}:{event_id}"),
        result_revision: source_revision.max(1),
        payload_digest: digest,
        visibility_class: memcore::DeliveryVisibilityClass::Private,
        delivery_policy: memcore::DeliveryPolicy::ResumeRequesterOperation,
        protocol_capability: "result-ref-v1".to_string(),
        requester: memcore::DeliveryRequesterBinding {
            agent_identity_id: Some(agent_identity_id.to_string()),
            host_identity: Some(host_identity.to_string()),
            session_ref: Some(remote_session_id.to_string()),
        },
        expires_at: None,
    };
    if let Err(error) = server.with_global_store(|store| {
        memcore::mint_delivery_intent(store.connection(), &new).map_err(|error| error.to_string())
    }) {
        tracing::warn!(
            error = %error,
            attachment_id = %attachment_id,
            event_id = %event_id,
            "failed to mint delivery intent for attached terminal event"
        );
    }
}
