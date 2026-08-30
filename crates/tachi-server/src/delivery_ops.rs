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
use crate::tool_params::TachiDeliveryParams;
use memcore::{
    ack_delivered, claim_ready_delivery, dismiss_delivery, get_delivery_intent,
    reject_or_block, resume_requester_operation, DeliveryCaller, DeliveryClaimRequest,
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
    let (admitted_host, _connection_id, admission_state) = server
        .work_claim_connection()
        .ok_or_else(|| "no active host admission; the delivery seam requires an admitted host connection".to_string())?;
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
            return Err(format!("unknown requester agent identity {agent_identity_id}"));
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
    let action = params.action.trim().to_ascii_lowercase();
    if !tachi_hub::facade_action_allowed(
        "tachi_delivery",
        Some(&action),
        server.active_tool_profile(),
    ) {
        return Err(format!(
            "tool action '{action}' is not allowed for the active Tachi profile"
        ));
    }
    let caller = verify_caller(server, &params)?;
    match action.as_str() {
        "claim_ready_delivery" => {
            let claim_key = required(params.claim_key.clone(), "claim_key")?;
            let request = DeliveryClaimRequest {
                caller: caller.clone(),
                claim_key,
                lease_seconds: params.lease_seconds.unwrap_or(0),
                only_delivery_id: None,
            };
            let outcome = server
                .with_global_store(|store| {
                    claim_ready_delivery(store.connection(), &request)
                        .map_err(|error| error.to_string())
                })?;
            let (outcome_token, delivery) = match outcome {
                memcore::DeliveryClaimOutcome::Claimed(view) => {
                    ("claimed", Some(serde_json::to_value(&view).expect("view serializes")))
                }
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
        "ack_delivered" => {
            let delivery_id = required(params.delivery_id.clone(), "delivery_id")?;
            let ack_key = required(params.ack_key.clone(), "ack_key")?;
            let outcome = server
                .with_global_store(|store| {
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
        "reject_or_block" => {
            let delivery_id = required(params.delivery_id.clone(), "delivery_id")?;
            let blocker_class = required(params.blocker_class.clone(), "blocker_class")?;
            let intent = server
                .with_global_store(|store| {
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
        "resume_requester_operation" => {
            let intents = server
                .with_global_store(|store| {
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
        "dismiss" => {
            let delivery_id = required(params.delivery_id.clone(), "delivery_id")?;
            let actor = caller.agent_identity_id.clone();
            let intent = server
                .with_global_store(|store| {
                    dismiss_delivery(
                        store.connection(),
                        &delivery_id,
                        &actor,
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
        "get" => {
            let delivery_id = required(params.delivery_id.clone(), "delivery_id")?;
            let intent = server
                .with_global_store(|store| {
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
        other => Err(format!("unknown tachi_delivery action '{other}'")),
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
