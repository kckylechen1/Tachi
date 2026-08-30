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
use rusqlite::params;
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
            let rearm_blocked = params.rearm_blocked.unwrap_or(false);
            let intents = server.with_global_store(|store| {
                resume_requester_operation(store.connection(), &caller, rearm_blocked)
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
                    if intent.visibility_class != "private" || {
                        // A private intent is readable only by its FULL
                        // admitted binding: agent identity AND the host it
                        // was bound to (host rotation does not inherit
                        // another host's reads).
                        intent.requester_agent_identity_id.as_deref()
                            == Some(caller.agent_identity_id.as_str())
                            && intent
                                .requester_host_identity
                                .as_deref()
                                .is_none_or(|bound| bound == caller.host_identity)
                    } =>
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
/// managed dispatch's canonical terminal receipt (#1679).
///
/// Store topology law: delivery intents are GLOBAL (requester-facing — the
/// claiming host must find them regardless of which project store holds the
/// outcome), while the canonical outcome row may live in a project store.
/// The caller therefore reads the canonical row INSIDE the write closure's
/// lock and passes the snapshot here; this helper then binds the requester
/// from the GLOBAL WorkClaim table and mints into the GLOBAL delivery
/// spine, so the seam's claim/get/ack surface and the mint share one
/// store. Delivery is a separate plane: a mint failure logs a warning and
/// never rewrites the execution truth above.
pub(crate) fn mint_delivery_for_managed_outcome(
    server: &MemoryServer,
    outcome: &memcore::DispatchOutcomeRow,
    result_ref: String,
) {
    let _ = server.with_global_store(|store| {
        mint_delivery_for_managed_outcome_in_store(store, outcome, result_ref);
        Ok::<(), String>(())
    });
}

/// In-store core: the caller already holds the right store scope (the
/// global-scope write closure). Locks are non-reentrant, so the global
/// scope branch calls THIS directly; project-scope branches go through the
/// server wrapper's nested global acquisition.
pub(crate) fn mint_delivery_for_managed_outcome_in_store(
    store: &mut memcore::MemoryStore,
    outcome: &memcore::DispatchOutcomeRow,
    result_ref: String,
) {
    // Correction authority: the spine serializes corrections inside the
    // mint transaction (equal-revision + different content supersedes at
    // prev+1), so two corrections in the same clock tick can never be
    // silently lost. The wall clock only provides the first-mint floor.
    let result_revision = revision_wall_clock_base();
    let idempotency_key = managed_delivery_key(&outcome.dispatch_id);

    let mint_result = || {
        let conn = store.connection();
        let mut new = memcore::NewDeliveryIntent {
            idempotency_key: idempotency_key.clone(),
            execution_source: memcore::DeliveryExecutionSource::ManagedDispatch,
            execution_ref: outcome.dispatch_id.clone(),
            terminal_receipt_revision: 0,
            work_claim_id: None,
            result_ref: result_ref.clone(),
            result_revision,
            payload_digest: digest_token(&[
                &outcome.outcome_id,
                &outcome.execution_outcome,
                &result_ref,
                &outcome.evidence_refs.to_string(),
            ]),
            visibility_class: memcore::DeliveryVisibilityClass::Public,
            delivery_policy: memcore::DeliveryPolicy::ReturnToCurrentCall,
            protocol_capability: "result-ref-v1".to_string(),
            requester: memcore::DeliveryRequesterBinding::default(),
            expires_at: None,
            correction: true,
        };
        match memcore::find_claim_requester_for_dispatch(conn, &outcome.dispatch_id)
            .map_err(|error| error.to_string())
        {
            Ok(Some(binding)) if binding.state == "active" => {
                new.visibility_class = memcore::DeliveryVisibilityClass::Private;
                new.work_claim_id = Some(binding.claim_id);
                new.requester = memcore::DeliveryRequesterBinding {
                    agent_identity_id: Some(binding.agent_identity_id),
                    host_identity: None,
                    session_ref: None,
                };
            }
            // The claim exists but is NOT active (released/handed off): the
            // requester context is gone. A mint now would either keep the
            // old binding (harmless reconcile) or - if no intent exists
            // yet - create an UNBOUND public delivery for work whose owner
            // left. Only reconcile an existing intent; never mint fresh.
            Ok(Some(_)) => {
                if memcore::find_delivery_intent_by_idempotency_key(conn, &idempotency_key)
                    .map_err(|error| error.to_string())?
                    .is_none()
                {
                    return Ok(None);
                }
            }
            // No claim names this dispatch: honestly unbound (pull-only).
            Ok(None) => {}
            Err(error) => return Err(error),
        }
        memcore::mint_delivery_intent(conn, &new)
            .map(|intent| Some(intent))
            .map_err(|error| error.to_string())
    };
    if let Err(error) = mint_result() {
        tracing::warn!(
            error = %error,
            dispatch_id = %outcome.dispatch_id,
            "failed to mint delivery intent for terminal outcome"
        );
    }
}

/// The managed mint idempotency key: one delivery intent per DISPATCH.
pub(crate) fn managed_delivery_key(dispatch_id: &str) -> String {
    format!("managed:{dispatch_id}")
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
    // Older-information guard + canonical-terminal re-check + mint run
    // under ONE store lock: no ingest/receipt interleaving can mint from a
    // receipt the canonical projection no longer supports.
    let mint_result = server.with_global_store(|store| {
        let conn = store.connection();

        // 1. The canonical projection must RIGHT NOW be a consistent
        //    terminal: a mint while inconsistent_reconciling or
        //    unknown_orphaned would let an unresolved (possibly wrong)
        //    result reach a requester. #1623 adjudication owns those
        //    states; delivery waits.
        let canonical: Option<String> = conn
            .query_row(
                "SELECT canonical_state FROM harness_session_state WHERE attachment_id = ?1",
                params![attachment_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if !matches!(
            canonical.as_deref(),
            Some("completed") | Some("failed") | Some("cancelled")
        ) {
            return Ok(None);
        }

        // 2. Older-information guard, against the AUTHORITATIVE canonical
        //    revision (not just the intent's): the stored projection's
        //    canonical_revision is the freshest truth the receipt spine
        //    committed, so an event below it is stale even when no intent
        //    exists yet (a faster sibling may have committed projection
        //    but not yet minted).
        let canonical_revision: i64 = conn
            .query_row(
                "SELECT COALESCE(canonical_revision, 0) FROM harness_session_state
                 WHERE attachment_id = ?1",
                params![attachment_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if canonical_revision > source_revision {
            return Ok(None);
        }
        let intent_key = format!("attached:{attachment_id}");
        let existing = memcore::find_delivery_intent_by_idempotency_key(conn, &intent_key)
            .map_err(|error| error.to_string())?;
        if let Some(existing) = &existing {
            if existing.result_revision > source_revision.max(1) {
                return Ok(None);
            }
        }

        let digest = payload_digest
            .map(str::to_string)
            .unwrap_or_else(|| digest_token(&[outcome_token, summary.unwrap_or("")]));
        // One durable intent per attached RUN (keyed on the attachment): a
        // redundant terminal event for the same run reconciles against the
        // SAME intent instead of minting a sibling. The digest excludes the
        // event id for the same reason; the result ref is a run-stable
        // locator, so an identical payload compares unchanged no matter
        // which event id journaled it.
        let new = memcore::NewDeliveryIntent {
            idempotency_key: intent_key,
            execution_source: memcore::DeliveryExecutionSource::AttachedSession,
            execution_ref: attachment_id.to_string(),
            terminal_receipt_revision: source_revision.max(0),
            work_claim_id: (!work_claim_id.is_empty()).then(|| work_claim_id.to_string()),
            result_ref: format!("harness_session:{attachment_id}"),
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
            correction: true,
        };
        memcore::mint_delivery_intent(conn, &new)
            .map(Some)
            .map_err(|error| error.to_string())
    });
    if let Err(error) = mint_result {
        tracing::warn!(
            error = %error,
            attachment_id = %attachment_id,
            event_id = %event_id,
            "failed to mint delivery intent for attached terminal event"
        );
    }
}
