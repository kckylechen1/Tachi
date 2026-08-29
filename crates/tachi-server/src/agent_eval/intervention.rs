//! Typed intervention request/result routes for attached sessions (#1678).
//!
//! `request_intervention` records a typed, replay-idempotent receipt of an
//! ask; `record_intervention_result` records the host's authoritative
//! outcome. Neither route mutates canonical session state, sends a lifecycle
//! request, or holds a session handle. An unsupported lifecycle owner
//! produces the typed `unsupported_by_lifecycle_owner` response with zero
//! writes — never a fake `cancelled`.

use crate::agent_eval::attachment::{current_host_admission, required};
use crate::agent_eval::session_spine::{attachment_selector, session_state_json};
use crate::server_state::MemoryServer;
use crate::tool_params::TachiAgentEvalParams;
use memcore::{
    record_harness_session_intervention_result, request_harness_session_intervention,
    HarnessSessionInterventionDisposition, HarnessSessionInterventionKind,
    NewHarnessSessionIntervention, NewHarnessSessionInterventionResult,
};
use serde_json::json;

fn intervention_kind(raw: Option<String>) -> Result<HarnessSessionInterventionKind, String> {
    let raw = required(raw, "intervention_kind")?;
    HarnessSessionInterventionKind::parse(raw.trim()).map_err(|error| error.to_string())
}

pub(crate) fn handle_request_intervention(
    server: &MemoryServer,
    params: TachiAgentEvalParams,
) -> Result<String, String> {
    let (host, admission_receipt_ref) = current_host_admission(
        server,
        params.host_identity.clone(),
        params.admission_receipt_ref.clone(),
    )?;
    let selector = attachment_selector(&params, &host.host_identity)?;
    let input = NewHarnessSessionIntervention {
        request_id: required(
            params.intervention_request_id.clone(),
            "intervention_request_id",
        )?,
        kind: intervention_kind(params.intervention_kind.clone())?,
        reason: required(params.intervention_reason.clone(), "intervention_reason")?,
        expected_session_revision: params
            .expected_session_revision
            .ok_or_else(|| "expected_session_revision is required".to_string())?,
    };
    enum RequestOutcome {
        Receipt(memcore::HarnessSessionInterventionRequestReceipt),
        Unsupported {
            attachment_id: String,
            intervention_kind: String,
        },
    }
    let outcome = server.with_global_store(|store| {
        match request_harness_session_intervention(
            store.connection_mut(),
            &selector,
            &input,
            &host,
            &admission_receipt_ref,
        ) {
            Ok(receipt) => Ok(RequestOutcome::Receipt(receipt)),
            Err(memcore::MemoryError::UnsupportedByLifecycleOwner {
                attachment_id,
                intervention_kind,
            }) => Ok(RequestOutcome::Unsupported {
                attachment_id,
                intervention_kind,
            }),
            Err(error) => Err(error.to_string()),
        }
    })?;
    match outcome {
        RequestOutcome::Receipt(receipt) => serde_json::to_string(&json!({
            "status": "completed",
            "action": "request_intervention",
            "admission": match receipt.admission {
                memcore::HarnessSessionInterventionAdmission::Created => "created",
                memcore::HarnessSessionInterventionAdmission::Replayed => "replayed",
            },
            "request": {
                "attachment_id": receipt.intervention.attachment_id,
                "request_id": receipt.intervention.request_id,
                "kind": receipt.intervention.kind.as_str(),
                "reason": receipt.intervention.reason,
                "expected_session_revision": receipt.intervention.expected_session_revision,
                "requested_by": receipt.intervention.requested_by,
            },
            "capability_source": receipt.capability_source.as_str(),
            "canonical_state": session_state_json(&receipt.state),
            "note": "a request is not the resulting lifecycle state; the authoritative result arrives through record_intervention_result",
        }))
        .map_err(|error| format!("serialize intervention request receipt: {error}")),
        RequestOutcome::Unsupported {
            attachment_id,
            intervention_kind,
        } => serde_json::to_string(&json!({
            "status": "unsupported_by_lifecycle_owner",
            "action": "request_intervention",
            "attachment_id": attachment_id,
            "intervention_kind": intervention_kind,
            "mutated": false,
            "note": "the advertised lifecycle capability and admitted policy do not cover this kind; nothing was written",
        }))
        .map_err(|error| format!("serialize unsupported intervention refusal: {error}")),
    }
}

fn intervention_disposition(
    raw: Option<String>,
) -> Result<HarnessSessionInterventionDisposition, String> {
    let raw = required(raw, "intervention_disposition")?;
    match raw.trim() {
        "accepted" => Ok(HarnessSessionInterventionDisposition::Accepted),
        "refused" => Ok(HarnessSessionInterventionDisposition::Refused),
        "unsupported" => Ok(HarnessSessionInterventionDisposition::Unsupported),
        "failed" => Ok(HarnessSessionInterventionDisposition::Failed),
        other => Err(format!("unknown intervention_disposition '{other}'")),
    }
}

pub(crate) fn handle_record_intervention_result(
    server: &MemoryServer,
    params: TachiAgentEvalParams,
) -> Result<String, String> {
    let (host, admission_receipt_ref) = current_host_admission(
        server,
        params.host_identity.clone(),
        params.admission_receipt_ref.clone(),
    )?;
    let selector = attachment_selector(&params, &host.host_identity)?;
    let input = NewHarnessSessionInterventionResult {
        request_id: required(
            params.intervention_request_id.clone(),
            "intervention_request_id",
        )?,
        disposition: intervention_disposition(params.intervention_disposition.clone())?,
        // Exact-empty means absent; present-but-invalid text must reach
        // the typed writer validation.
        authority_confirmation_ref: params
            .authority_confirmation_ref
            .clone()
            .filter(|value| !value.is_empty()),
        detail: params
            .intervention_detail
            .clone()
            .filter(|value| !value.is_empty()),
    };
    let receipt = server.with_global_store(|store| {
        record_harness_session_intervention_result(
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
        "action": "record_intervention_result",
        "admission": match receipt.admission {
            memcore::HarnessSessionInterventionAdmission::Created => "created",
            memcore::HarnessSessionInterventionAdmission::Replayed => "replayed",
        },
        "result": {
            "attachment_id": receipt.result.attachment_id,
            "request_id": receipt.result.request_id,
            "request_kind": receipt.request_kind.as_str(),
            "disposition": receipt.result.disposition.as_str(),
            "authority_confirmation_ref": receipt.result.authority_confirmation_ref,
            "detail": receipt.result.detail,
        },
        "canonical_state": session_state_json(&receipt.state),
        "note": "canonical lifecycle state moves only through ingest_session_event terminal facts; a cancelled outcome must bind this confirmation reference",
    }))
    .map_err(|error| format!("serialize intervention result receipt: {error}"))
}
