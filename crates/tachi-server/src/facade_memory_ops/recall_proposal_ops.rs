//! Evidence-backed RecallConfig proposal review/apply loop.

use super::evidence_format::{json_string, wants_json};
use super::recall_simulate_ops::build_recall_simulation_report;
use crate::tool_params::*;
use crate::MemoryServer;
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use tachi_dispatch::policy::{
    recall_config_v2_identity_payload, RECALL_CONFIG_PROPOSAL_POLICY_VERSION,
    RECALL_CONFIG_PROPOSAL_TARGET,
};

const RECALL_CONFIG_PROPOSAL_NS: &str = "recall_config_proposals";
const EPSILON: f64 = 0.000_001;

pub(crate) async fn handle_recall_config_proposals(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let generated = if has_eval_input(params) {
        let simulation = build_recall_simulation_report(server, params).await?;
        let proposals = build_proposals_from_simulation(&simulation, params.force)?;
        persist_generated_proposals(server, proposals)?;
        Some(simulation)
    } else {
        None
    };

    let proposals = list_proposals(server, params.state_filter.as_deref())?;
    let response = json!({
        "status": "completed",
        "action": "recall_proposals",
        "kind": "recall_config",
        "read_only": false,
        "requires_human_approval": true,
        "generated": generated.as_ref().map(|simulation| json!({
            "source": "recall_simulate",
            "case_count": simulation["case_count"],
            "top_k": simulation["top_k"],
            "rerank": simulation["rerank"],
        })),
        "count": proposals.len(),
        "proposals": proposals,
        "next_actions": [
            "tachi_memory(action='review_recall_proposal', proposal_id=..., review_status='approved')",
            "tachi_memory(action='apply_recall_proposals', proposal_id=..., confirm=true) after approval; restart daemon to load new RecallConfig"
        ],
    });

    if wants_json(params.format.as_deref()) {
        return json_string(&response);
    }
    Ok(format_recall_proposals_markdown(&response))
}

pub(crate) fn handle_recall_config_review(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let proposal_id = required_proposal_id(params)?;
    let status = match params
        .review_status
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "approved" | "approve" => "approved",
        "rejected" | "reject" => "rejected",
        other => {
            return Err(format!(
                "Invalid review_status '{}'. Expected approved|rejected",
                other
            ))
        }
    };
    let reviewed_at = Utc::now().to_rfc3339();
    let updated = server.with_global_store(|store| {
        let (raw, version) = store
            .get_state_kv(RECALL_CONFIG_PROPOSAL_NS, proposal_id)
            .map_err(|e| format!("load recall config proposal: {e}"))?
            .ok_or_else(|| format!("recall config proposal not found: {proposal_id}"))?;
        let mut value: Value =
            serde_json::from_str(&raw).map_err(|e| format!("parse recall config proposal: {e}"))?;
        // Legacy proposals (pre-v2 schema) carry no content-addressed binding
        // between what the human reviewed and what apply will persist, so an
        // old approval cannot be trusted to cover the current config_env.
        // Refuse loudly rather than silently inheriting that approval.
        let is_v2 = value
            .get("schema_version")
            .and_then(Value::as_u64)
            .map(|v| v >= 2)
            .unwrap_or(false);
        if !is_v2 {
            return Err(format!(
                "legacy_unbound_proposal: {proposal_id} predates the v2 content-addressed identity and cannot be reviewed; regenerate with action='recall_proposals' to mint a fresh pending v2 proposal"
            ));
        }
        // Re-validate the persisted content_digest against the identity_payload
        // still in the row *before* recording a review decision. Without this,
        // a proposal that drifted from what was generated (a hand-edit, a
        // partial write, a regeneration collision) could be approved at review
        // time and only get caught at apply — this closes that gap so
        // propose/review/apply drift is refused at the earliest point it can
        // be detected, not just the last one. Mirrors the same check
        // `drive_recall_apply_state_machine` runs immediately before mutating
        // config.env.
        let stored_digest = value
            .get("content_digest")
            .and_then(Value::as_str)
            .unwrap_or("");
        let identity_payload = value
            .get("identity_payload")
            .cloned()
            .unwrap_or(Value::Null);
        let recomputed_digest = content_digest_hex(&identity_payload);
        if stored_digest.is_empty() || recomputed_digest != stored_digest {
            return Err(format!(
                "content_digest_mismatch: recall config proposal {proposal_id} stored digest {stored_digest:?} does not match recomputed {recomputed_digest}; refusing to review a proposal that drifted from what was generated"
            ));
        }
        // Review only permits pending -> approved | rejected. A terminal
        // (rejected/applied) row cannot be resurrected, and an already-approved
        // row cannot be silently re-decided without a fresh regeneration.
        let current_status = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        if current_status != "pending" {
            return Err(format!(
                "recall config proposal {proposal_id} is in terminal state '{current_status}'; only pending proposals can be reviewed"
            ));
        }
        value["status"] = json!(status);
        value["review"] = json!({
            "status": status,
            "note": params.notes.clone(),
            "reviewed_at": reviewed_at,
        });
        // `hard_state` TTL (#1342 follow-up): `rejected` is terminal — the
        // proposal will never be applied — so it gets a 30-day TTL here.
        // `approved` is NOT terminal (still awaits `handle_recall_config_apply`),
        // so it must stay TTL-less until that terminal write.
        if status == "rejected" {
            value["expires_at"] = json!((Utc::now() + Duration::days(30)).to_rfc3339());
        }
        let next = serde_json::to_string(&value)
            .map_err(|e| format!("serialize recall config review: {e}"))?;
        // hard_state version CAS: a concurrent review or a regeneration that
        // landed on the same content-addressed id must not be silently
        // overwritten. Refuse on version drift; the caller reloads and retries.
        let cas_ok = store
            .set_state_if_version(RECALL_CONFIG_PROPOSAL_NS, proposal_id, &next, version)
            .map_err(|e| format!("persist recall config review: {e}"))?;
        if !cas_ok {
            return Err(format!(
                "stale_state_version: recall config proposal {proposal_id} changed before review; reload and retry"
            ));
        }
        Ok(value)
    })?;

    let response = json!({
        "status": "completed",
        "action": "review_recall_proposal",
        "proposal_id": proposal_id,
        "proposal": updated,
    });
    if wants_json(params.format.as_deref()) {
        return json_string(&response);
    }
    Ok(format!(
        "Tachi recall proposal review\nstatus: completed\nproposal_id: `{proposal_id}`\nreview_status: {status}"
    ))
}

pub(crate) fn handle_recall_config_apply(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let proposal_id = required_proposal_id(params)?;
    if !params.confirm {
        return Err(
            "apply_recall_proposals requires confirm=true after human approval; no config.env changes applied"
                .to_string(),
        );
    }

    let app_home = crate::cli_client::app_home_from_global_db(&server.global_db_path_buf());
    let config_env_path = app_home.join("config.env");

    // Drive the apply through the explicit recoverable state machine. The
    // machine writes the proposal row + the config.env file through three
    // observable states (approved -> applying -> applied), each transition
    // CAS-guarded. config.env cannot be in the SQLite transaction, so the
    // `applying` row carries before/after digests that the recovery path
    // (and any concurrent caller) can re-derive from the live file to decide
    // idempotent-finalize vs. safe-retry vs. loud third-party-drift refusal.
    let (proposal, apply_result, outcome) =
        drive_recall_apply_state_machine(server, proposal_id, &config_env_path)?;

    let response = json!({
        "status": "completed",
        "action": "apply_recall_proposals",
        "proposal_id": proposal_id,
        "config_env_path": config_env_path.display().to_string(),
        "updated_keys": apply_result.updated_keys.clone(),
        "restart_required": true,
        "apply_outcome": outcome.label(),
        "attempt_id": outcome.attempt_id(),
        "proposal": proposal,
    });
    if wants_json(params.format.as_deref()) {
        return json_string(&response);
    }
    Ok(format!(
        "Tachi recall proposal apply
status: completed
proposal_id: `{proposal_id}`
updated_keys: {}
restart_required: true
outcome: {}",
        apply_result.updated_keys.join(", "),
        outcome.label(),
    ))
}

/// What `drive_recall_apply_state_machine` observed on disk and what it did.
/// Always carries the attempt_id of the receipt that ended up terminal so a
/// caller can correlate the proposal row with what landed on the config file.
#[derive(Clone, Debug)]
enum RecallApplyOutcome {
    /// Fresh apply: approved -> applying -> applied on this call.
    Fresh { attempt_id: String },
    /// Recovery where the file already matched `after_digest` (rename landed,
    /// finalize CAS did not). Finalized idempotently; no file mutation.
    FinalizedExisting { attempt_id: String },
    /// Recovery where the file still matched `before_digest` (rename did not
    /// land). Redid the file write and finalized.
    Retried { attempt_id: String },
}

impl RecallApplyOutcome {
    fn label(&self) -> &'static str {
        match self {
            RecallApplyOutcome::Fresh { .. } => "applied_fresh",
            RecallApplyOutcome::FinalizedExisting { .. } => "applied_finalized_existing",
            RecallApplyOutcome::Retried { .. } => "applied_retried",
        }
    }
    fn attempt_id(&self) -> &str {
        match self {
            RecallApplyOutcome::Fresh { attempt_id }
            | RecallApplyOutcome::FinalizedExisting { attempt_id }
            | RecallApplyOutcome::Retried { attempt_id } => attempt_id,
        }
    }
}

#[derive(Clone, Debug)]
struct RecallApplyResult {
    updated_keys: Vec<String>,
}

/// Drive one proposal through the recoverable apply state machine. Returns
/// the terminal `applied` proposal row, the keys that the proposal updates on
/// `config.env`, and the outcome that describes which recovery branch was
/// taken. All mutations are gated by hard_state version CAS, so two
/// concurrent applies yield exactly one terminal receipt (the loser's CAS
/// fails with `stale_state_version`).
///
/// State graph:
///   approved  --(CAS)-->  applying  --(rename, CAS)-->  applied
///                 |                |
///                 |                +-- recovery: observed == after  -> finalize
///                 |                +-- recovery: observed == before -> retry
///                 |                +-- recovery: observed == other  -> REFUSE
///                 +-- anything else -> REFUSE (legacy / pending / etc.)
fn drive_recall_apply_state_machine(
    server: &MemoryServer,
    proposal_id: &str,
    config_env_path: &Path,
) -> Result<(Value, RecallApplyResult, RecallApplyOutcome), String> {
    let proposal_json = server.with_global_store_read(|store| {
        store
            .get_state_kv(RECALL_CONFIG_PROPOSAL_NS, proposal_id)
            .map_err(|e| format!("load recall config proposal: {e}"))?
            .map(|(raw, _version)| raw)
            .ok_or_else(|| format!("recall config proposal not found: {proposal_id}"))
    })?;
    let proposal: Value = serde_json::from_str(&proposal_json)
        .map_err(|e| format!("parse recall config proposal: {e}"))?;

    // Legacy refusal: a pre-v2 row carries no content-addressed binding, so
    // its approval does not cover the current config_env payload.
    let is_v2 = proposal
        .get("schema_version")
        .and_then(Value::as_u64)
        .map(|v| v >= 2)
        .unwrap_or(false);
    if !is_v2 {
        return Err(format!(
            "legacy_unbound_proposal: {proposal_id} predates the v2 content-addressed identity and cannot be applied; regenerate with action='recall_proposals' to mint a fresh pending v2 proposal"
        ));
    }
    // Re-validate the persisted content_digest against the identity_payload
    // still in the row. A mismatch means the row was mutated after review.
    let stored_digest = proposal
        .get("content_digest")
        .and_then(Value::as_str)
        .unwrap_or("");
    let identity_payload = proposal
        .get("identity_payload")
        .cloned()
        .unwrap_or(Value::Null);
    let recomputed_digest = content_digest_hex(&identity_payload);
    if stored_digest.is_empty() || recomputed_digest != stored_digest {
        return Err(format!(
            "content_digest_mismatch: recall config proposal {proposal_id} stored digest {stored_digest:?} does not match recomputed {recomputed_digest}; refusing to apply unreviewed content"
        ));
    }

    let patch = parse_config_env_patch(&proposal)?;
    if patch.is_empty() {
        return Err(format!(
            "recall config proposal {proposal_id} has no TACHI_RECALL_* config_env values"
        ));
    }

    let status = proposal
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");

    let before_digest = compute_recall_digest(config_env_path)?;
    let after_digest = compute_projected_recall_digest(config_env_path, &patch)?;

    match status {
        "approved" => {
            // Fresh apply: stamp an applying receipt, do the rename, finalize.
            let attempt_id = uuid::Uuid::new_v4().to_string();
            stamp_applying_receipt(
                server,
                proposal_id,
                &attempt_id,
                &before_digest,
                &after_digest,
                &patch,
            )?;
            write_recall_config_env(config_env_path, &patch)?;
            let outcome = RecallApplyOutcome::Fresh { attempt_id };
            finalize_recall_apply(
                server,
                proposal_id,
                &outcome,
                &patch,
                &after_digest,
                config_env_path,
            )
        }
        "applying" => {
            // Recovery: examine the live config.env digest against the
            // receipt's before/after digests.
            let receipt = proposal
                .get("applying_receipt")
                .ok_or_else(|| {
                    format!(
                        "recall config proposal {proposal_id} is in 'applying' state but carries no applying_receipt; refusing to guess — operator must reconcile the row"
                    )
                })?
                .clone();
            let receipt_attempt_id = receipt
                .get("attempt_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let receipt_before = receipt
                .get("before_digest")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let receipt_after = receipt
                .get("after_digest")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if receipt_attempt_id.is_empty()
                || receipt_before.is_empty()
                || receipt_after.is_empty()
            {
                return Err(format!(
                    "recall config proposal {proposal_id} applying_receipt is missing attempt_id/before_digest/after_digest; refusing to guess — operator must reconcile the row"
                ));
            }
            let observed = compute_recall_digest(config_env_path)?;
            if observed == receipt_after {
                // Rename already landed before the crash; finalize idempotently.
                let outcome =
                    RecallApplyOutcome::FinalizedExisting { attempt_id: receipt_attempt_id };
                finalize_recall_apply(
                    server,
                    proposal_id,
                    &outcome,
                    &patch,
                    &after_digest,
                    config_env_path,
                )
            } else if observed == receipt_before {
                // Rename never landed; safe to redo the file write against the
                // known-clean before state, then finalize.
                write_recall_config_env(config_env_path, &patch)?;
                let observed_after = compute_recall_digest(config_env_path)?;
                if observed_after != receipt_after {
                    return Err(format!(
                        "recall config proposal {proposal_id} retry produced digest {observed_after} that does not match the receipt's after_digest {receipt_after}; refusing to finalize an unexpected file"
                    ));
                }
                let outcome = RecallApplyOutcome::Retried { attempt_id: receipt_attempt_id };
                finalize_recall_apply(
                    server,
                    proposal_id,
                    &outcome,
                    &patch,
                    &after_digest,
                    config_env_path,
                )
            } else {
                // The file drifted to something other than the receipt's before
                // or after state. A third party (or a divergent attempt)
                // touched the recall keys between the receipt and now; refuse
                // loudly rather than silently clobbering it.
                Err(format!(
                    "third_party_drift: recall config proposal {proposal_id} applying_receipt observed config.env digest {observed} that matches neither the receipt's before_digest nor its after_digest; refusing to finalize — operator must reconcile the config file"
                ))
            }
        }
        other => Err(format!(
            "recall config proposal {proposal_id} cannot be applied from status '{other}'; only approved (or applying for recovery) proposals can be applied"
        )),
    }
}

/// CAS approved -> applying, recording the attempt id and the before/after
/// config digests that the recovery path will check. Refuses on version drift
/// so two concurrent applies produce exactly one in-flight receipt.
fn stamp_applying_receipt(
    server: &MemoryServer,
    proposal_id: &str,
    attempt_id: &str,
    before_digest: &str,
    after_digest: &str,
    patch: &BTreeMap<String, String>,
) -> Result<u32, String> {
    server.with_global_store(|store| {
        let (raw, version) = store
            .get_state_kv(RECALL_CONFIG_PROPOSAL_NS, proposal_id)
            .map_err(|e| format!("load recall config proposal for applying stamp: {e}"))?
            .ok_or_else(|| {
                format!("recall config proposal not found for applying stamp: {proposal_id}")
            })?;
        let mut value: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("parse recall config proposal for applying stamp: {e}"))?;
        let current = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        if current != "approved" {
            return Err(format!(
                "stale_state_version: recall config proposal {proposal_id} is no longer 'approved' (now '{current}'); another apply won the race — reload and retry"
            ));
        }
        value["status"] = json!("applying");
        value["applying_receipt"] = json!({
            "attempt_id": attempt_id,
            "before_digest": before_digest,
            "after_digest": after_digest,
            "updated_keys": patch.keys().cloned().collect::<Vec<_>>(),
            "started_at": Utc::now().to_rfc3339(),
        });
        let next = serde_json::to_string(&value)
            .map_err(|e| format!("serialize applying recall config proposal: {e}"))?;
        let cas_ok = store
            .set_state_if_version(RECALL_CONFIG_PROPOSAL_NS, proposal_id, &next, version)
            .map_err(|e| format!("persist applying recall config proposal: {e}"))?;
        if !cas_ok {
            return Err(format!(
                "stale_state_version: recall config proposal {proposal_id} changed before applying stamp; reload and retry"
            ));
        }
        Ok(version + 1)
    })
}

/// CAS applying -> applied. Re-asserts the observed config.env digest equals
/// the receipt's `after_digest` immediately before the CAS, so a crash between
/// the rename and this finalize cannot stamp `applied` on a row whose file is
/// somehow not actually at the after state. Returns the terminal proposal row
/// and the apply result so the caller can surface them in the response.
fn finalize_recall_apply(
    server: &MemoryServer,
    proposal_id: &str,
    outcome: &RecallApplyOutcome,
    patch: &BTreeMap<String, String>,
    expected_after_digest: &str,
    config_env_path: &Path,
) -> Result<(Value, RecallApplyResult, RecallApplyOutcome), String> {
    let observed = compute_recall_digest(config_env_path)?;
    if observed != expected_after_digest {
        return Err(format!(
            "finalize_refused: recall config proposal {proposal_id} observed config.env digest {observed} does not match expected after_digest {expected_after_digest}; refusing to mark applied"
        ));
    }
    let applied_at = Utc::now().to_rfc3339();
    let terminal = server.with_global_store(|store| {
        let (raw, version) = store
            .get_state_kv(RECALL_CONFIG_PROPOSAL_NS, proposal_id)
            .map_err(|e| format!("load recall config proposal for finalize: {e}"))?
            .ok_or_else(|| {
                format!("recall config proposal not found for finalize: {proposal_id}")
            })?;
        let mut value: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("parse recall config proposal for finalize: {e}"))?;
        let current = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        if current != "applying" {
            return Err(format!(
                "stale_state_version: recall config proposal {proposal_id} is no longer 'applying' (now '{current}'); another finalize won the race — reload"
            ));
        }
        value["status"] = json!("applied");
        value["applied_at"] = json!(applied_at);
        // `hard_state` TTL (#1342 follow-up): `applied` is terminal — the
        // config patch already landed — so this write gets a 30-day TTL.
        value["expires_at"] = json!((Utc::now() + Duration::days(30)).to_rfc3339());
        value["apply_result"] = json!({
            "config_env_path": config_env_path.display().to_string(),
            "updated_keys": patch.keys().cloned().collect::<Vec<_>>(),
            "restart_required": true,
            "attempt_id": outcome.attempt_id(),
            "outcome": outcome.label(),
            "note": "RecallConfig is loaded once at process startup; restart the daemon/MCP server to apply these values.",
        });
        let next = serde_json::to_string(&value)
            .map_err(|e| format!("serialize applied recall config proposal: {e}"))?;
        let cas_ok = store
            .set_state_if_version(RECALL_CONFIG_PROPOSAL_NS, proposal_id, &next, version)
            .map_err(|e| format!("persist applied recall config proposal: {e}"))?;
        if !cas_ok {
            return Err(format!(
                "stale_state_version: recall config proposal {proposal_id} changed before finalize; reload and retry"
            ));
        }
        Ok(value)
    })?;
    let result = RecallApplyResult {
        updated_keys: patch.keys().cloned().collect::<Vec<_>>(),
    };
    Ok((terminal, result, outcome.clone()))
}

fn has_eval_input(params: &TachiMemoryParams) -> bool {
    params
        .text
        .as_deref()
        .is_some_and(|text| !text.trim().is_empty())
        || params.metadata.as_ref().is_some_and(|value| match value {
            Value::Array(items) => !items.is_empty(),
            Value::Object(map) => {
                map.contains_key("cases")
                    || map.contains_key("eval_cases")
                    || map.contains_key("case")
            }
            _ => false,
        })
}

fn build_proposals_from_simulation(
    simulation: &Value,
    include_non_improving: bool,
) -> Result<Vec<Value>, String> {
    let variants = simulation["variants"]
        .as_array()
        .ok_or_else(|| "recall simulation response missing variants".to_string())?;
    let Some(current) = variants
        .iter()
        .find(|variant| variant.get("name").and_then(Value::as_str) == Some("current"))
    else {
        return Err("recall simulation response missing current variant".to_string());
    };

    let current_recall = metric(current, "recall_at_k");
    let current_mrr = metric(current, "mrr");
    let mut out = Vec::new();
    for variant in variants {
        let name = variant
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("variant");
        if name == "current" {
            continue;
        }
        let proposed_recall = metric(variant, "recall_at_k");
        let proposed_mrr = metric(variant, "mrr");
        let recall_delta = proposed_recall - current_recall;
        let mrr_delta = proposed_mrr - current_mrr;
        let improved = variant_improves(recall_delta, mrr_delta);
        let config_env = variant
            .get("config_env")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if config_env.is_empty() || (!improved && !include_non_improving) {
            continue;
        }
        let config_env_value = Value::Object(config_env.clone());
        let legacy_id = format!(
            "recall_config:{}:{}",
            sanitize_key(name),
            stable_hex_hash(&config_env_value.to_string())
        );
        let recommendation = if improved {
            "metric_improved"
        } else {
            "forced_candidate"
        };
        let rationale = if improved {
            format!(
                "Recall replay variant {name} improved recall_at_k by {:.3} and MRR by {:.3}.",
                recall_delta, mrr_delta
            )
        } else {
            format!(
                "Recall replay variant {name} was forced into the review queue with recall_at_k delta {:.3} and MRR delta {:.3}.",
                recall_delta, mrr_delta
            )
        };
        let evidence_review = json!({
            "source": "recall_simulate",
            "case_count": simulation["case_count"],
            "top_k": simulation["top_k"],
            "rerank": simulation["rerank"],
            "variant_cases": variant["cases"],
            "baseline_metrics": current["metrics"],
            "proposed_metrics": variant["metrics"],
            "metric_delta": {
                "recall_at_k": round6(recall_delta),
                "mrr": round6(mrr_delta),
            },
            "limit_inputs": {
                "variant": name,
                "improved": improved,
                "forced": !improved && include_non_improving,
            },
        });
        // Identity binds (a) the complete apply payload (config_env the human
        // is approving for the daemon to load), (b) the evidence the human is
        // reviewing against, (c) the policy-version tag, (d) the apply target.
        // Any change to any of those rotates the SHA-256 id and starts a fresh
        // pending row, never inheriting an old approval.
        let identity_payload = recall_config_v2_identity_payload(
            &config_env_value,
            &evidence_review,
            RECALL_CONFIG_PROPOSAL_POLICY_VERSION,
            RECALL_CONFIG_PROPOSAL_TARGET,
        );
        let content_digest = content_digest_hex(&identity_payload);
        let id = format!(
            "recall_config:v2:{}",
            &content_digest[..16.min(content_digest.len())]
        );
        out.push(json!({
            "proposal_id": id,
            "legacy_proposal_id": legacy_id,
            "kind": "recall_config",
            "schema_version": 2,
            "policy_version": RECALL_CONFIG_PROPOSAL_POLICY_VERSION,
            "target": RECALL_CONFIG_PROPOSAL_TARGET,
            "identity_payload": identity_payload,
            "content_digest": content_digest,
            "status": "pending",
            "requires_human_approval": true,
            "created_or_refreshed_at": Utc::now().to_rfc3339(),
            "variant": name,
            "config_env": config_env_value,
            "baseline_metrics": current["metrics"],
            "proposed_metrics": variant["metrics"],
            "metric_delta": {
                "recall_at_k": round6(recall_delta),
                "mrr": round6(mrr_delta),
            },
            "recommendation": recommendation,
            "evidence": {
                "source": "recall_simulate",
                "case_count": simulation["case_count"],
                "top_k": simulation["top_k"],
                "rerank": simulation["rerank"],
                "variant_cases": variant["cases"],
            },
            "apply": {
                "target": "~/.tachi/config.env or TACHI_HOME/config.env",
                "restart_required": true,
            },
            "rationale": rationale,
        }));
    }
    Ok(out)
}

fn persist_generated_proposals(server: &MemoryServer, proposals: Vec<Value>) -> Result<(), String> {
    if proposals.is_empty() {
        return Ok(());
    }
    server.with_global_store(|store| {
        for proposal in proposals {
            let id = proposal["proposal_id"]
                .as_str()
                .ok_or_else(|| "recall config proposal missing proposal_id".to_string())?
                .to_string();
            let mut next = proposal;
            if let Some((existing, _version)) =
                store
                    .get_state_kv(RECALL_CONFIG_PROPOSAL_NS, &id)
                    .map_err(|e| format!("load recall config proposal: {e}"))?
            {
                if let Ok(existing_json) = serde_json::from_str::<Value>(&existing) {
                    // Preserve a prior review/apply decision ONLY when the
                    // stored row is v2 AND carries the SAME content digest as
                    // the freshly regenerated proposal. The v2 id already
                    // collides only with itself when content is identical, so
                    // this is a belt-and-braces guard against any path that
                    // writes the same id with different content. A legacy row
                    // (pre-v2) donates nothing — its approval was not bound to
                    // the current content.
                    let stored_digest = existing_json
                        .get("content_digest")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let new_digest = next
                        .get("content_digest")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let same_content = !stored_digest.is_empty() && stored_digest == new_digest;
                    if same_content {
                        let existing_status = existing_json
                            .get("status")
                            .and_then(|value| value.as_str())
                            .unwrap_or("pending");
                        if existing_status != "pending" {
                            next["status"] = json!(existing_status);
                        }
                        if let Some(review) = existing_json.get("review") {
                            next["review"] = review.clone();
                        }
                        if let Some(applied_at) = existing_json.get("applied_at") {
                            next["applied_at"] = applied_at.clone();
                        }
                        if let Some(apply_result) = existing_json.get("apply_result") {
                            next["apply_result"] = apply_result.clone();
                        }
                        if let Some(applying_receipt) = existing_json.get("applying_receipt") {
                            next["applying_receipt"] = applying_receipt.clone();
                        }
                        // #1342 follow-up (BUG, cross-vendor review): a
                        // re-generate rewrites the whole `next` value fresh off
                        // the freshly computed proposal, which carries no
                        // `expires_at` at all — without this preserve, a
                        // terminal (applied/rejected) row's TTL was silently
                        // erased on every refresh, and the next maintenance
                        // tick's idempotent backfill would then stamp a
                        // brand-new `now+30d` on it. Repeated refreshes before
                        // the TTL elapsed meant the row's expiry never actually
                        // arrived — the state-lifecycle-hygiene pass this
                        // namespace's TTL exists for was defeated by its own
                        // refresh path. Preserve the ORIGINAL timestamp, not a
                        // recomputed one.
                        if let Some(expires_at) = existing_json.get("expires_at") {
                            next["expires_at"] = expires_at.clone();
                        }
                    }
                }
            }
            let raw = serde_json::to_string(&next)
                .map_err(|e| format!("serialize recall config proposal: {e}"))?;
            store
                .set_state(RECALL_CONFIG_PROPOSAL_NS, &id, &raw)
                .map_err(|e| format!("persist recall config proposal: {e}"))?;
        }
        Ok(())
    })
}

fn list_proposals(
    server: &MemoryServer,
    status_filter: Option<&str>,
) -> Result<Vec<Value>, String> {
    let desired = status_filter
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "all")
        .map(|value| value.to_ascii_lowercase());
    let records = server.with_global_store_read(|store| {
        store
            .list_state(RECALL_CONFIG_PROPOSAL_NS)
            .map_err(|e| format!("list recall config proposals: {e}"))
    })?;
    let mut out = Vec::new();
    for row in records {
        let mut value: Value = serde_json::from_str(&row.value_json)
            .unwrap_or_else(|_| json!({ "proposal_id": row.key, "raw": row.value_json }));
        value["state_version"] = json!(row.version);
        value["updated_at"] = json!(row.updated_at);
        // Legacy proposals (pre-v2 schema) carry no content-addressed binding
        // and are refused at review/apply; surface the marker here so callers
        // see *why* before they hit the refusal.
        let is_v2 = value
            .get("schema_version")
            .and_then(Value::as_u64)
            .map(|version| version >= 2)
            .unwrap_or(false);
        if !is_v2 {
            value["legacy_unbound_proposal"] = json!(true);
        }
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        if desired.as_deref().is_some_and(|wanted| wanted != status) {
            continue;
        }
        out.push(value);
    }
    Ok(out)
}

/// SHA-256 hex of a canonical identity payload. Mirrors the helper in
/// `dispatch_profile/policy/handlers.rs`; kept local because the two modules
/// are in different crate sub-trees and a shared util would expand this PR's
/// scope. Both must serialize through the same `recall_config_v2_identity_payload`
/// canonical form first.
fn content_digest_hex(identity_payload: &Value) -> String {
    let canonical = identity_payload.to_string();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hex_lower(&hasher.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(2 * bytes.len());
    for byte in bytes {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

/// SHA-256 hex of the TACHI_RECALL_* keys currently on disk at `path`. Used as
/// the `before_digest` of a fresh apply and as the `observed` digest the
/// recovery path compares against the receipt's before/after digests. Only the
/// TACHI_RECALL_* keys are hashed — a third party changing VOYAGE_API_KEY must
/// not flip a recovery into a `third_party_drift` refusal, and a third party
/// changing any TACHI_RECALL_* key must.
fn compute_recall_digest(path: &Path) -> Result<String, String> {
    let pairs = read_recall_pairs(path)?;
    Ok(digest_of_pairs(&pairs))
}

/// SHA-256 hex of the TACHI_RECALL_* keys at `path` *after* `patch` is applied
/// (existing keys replaced, new keys appended). Used as the `after_digest` of
/// a fresh apply. Re-derived independently of `write_recall_config_env` so a
/// bug in the writer cannot silently produce a different file than the digest
/// promised.
fn compute_projected_recall_digest(
    path: &Path,
    patch: &BTreeMap<String, String>,
) -> Result<String, String> {
    let mut pairs = read_recall_pairs(path)?;
    let mut seen = BTreeSet::new();
    for pair in pairs.iter_mut() {
        if let Some(value) = patch.get(&pair.0) {
            pair.1 = value.clone();
        }
        seen.insert(pair.0.clone());
    }
    for (key, value) in patch {
        if !seen.contains(key) {
            pairs.push((key.clone(), value.clone()));
        }
    }
    pairs.sort();
    Ok(digest_of_pairs(&pairs))
}

fn read_recall_pairs(path: &Path) -> Result<Vec<(String, String)>, String> {
    let body = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(format!("read config.env {}: {err}", path.display())),
    };
    let mut pairs = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((raw_key, raw_value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = raw_key.trim();
        if !key.starts_with("TACHI_RECALL_") {
            continue;
        }
        pairs.push((key.to_string(), raw_value.trim().to_string()));
    }
    pairs.sort();
    Ok(pairs)
}

fn digest_of_pairs(pairs: &[(String, String)]) -> String {
    let mut hasher = Sha256::new();
    for (key, value) in pairs {
        hasher.update(key.as_bytes());
        hasher.update(b"=");
        hasher.update(value.as_bytes());
        hasher.update(b"\n");
    }
    hex_lower(&hasher.finalize())
}

fn parse_config_env_patch(proposal: &Value) -> Result<BTreeMap<String, String>, String> {
    let config_env = proposal
        .get("config_env")
        .and_then(Value::as_object)
        .ok_or_else(|| "recall config proposal missing config_env".to_string())?;
    let mut out = BTreeMap::new();
    for (key, value) in config_env {
        if !key.starts_with("TACHI_RECALL_") {
            return Err(format!(
                "recall config proposal contains non-recall config key: {key}"
            ));
        }
        let Some(value) = value.as_str() else {
            return Err(format!(
                "recall config proposal config_env.{key} must be a string"
            ));
        };
        out.insert(key.clone(), value.to_string());
    }
    Ok(out)
}

/// Write `values` into the config.env at `path`, replacing any existing
/// same-key lines and appending new ones. Durability:
/// * a fresh temp file is written and `fsync`'d (data + metadata) **before**
///   the rename, so the bytes are on stable storage when the atomic rename
///   exposes them;
/// * the rename is the only mutation visible to a concurrent reader, so the
///   file is either fully old or fully new, never partially rewritten;
/// * the temp name carries a per-attempt uuid so two concurrent applies (which
///   the upper CAS already serializes) cannot collide on the same temp path.
fn write_recall_config_env(path: &Path, values: &BTreeMap<String, String>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create config.env parent {}: {e}", parent.display()))?;
    }
    let existing = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(format!("read config.env {}: {err}", path.display())),
    };
    let mut seen = BTreeSet::new();
    let mut lines = Vec::new();
    for line in existing.lines() {
        let trimmed = line.trim_start();
        let Some((raw_key, _raw_value)) = trimmed.split_once('=') else {
            lines.push(line.to_string());
            continue;
        };
        let key = raw_key.trim();
        if let Some(value) = values.get(key) {
            lines.push(format!("{key}={value}"));
            seen.insert(key.to_string());
        } else {
            lines.push(line.to_string());
        }
    }
    for (key, value) in values {
        if !seen.contains(key) {
            lines.push(format!("{key}={value}"));
        }
    }
    let mut body = lines.join("\n");
    body.push('\n');
    let tmp = tmp_path_for(path);
    {
        let mut file = std::fs::File::create(&tmp)
            .map_err(|e| format!("create temp config.env {}: {e}", tmp.display()))?;
        file.write_all(body.as_bytes())
            .map_err(|e| format!("write temp config.env {}: {e}", tmp.display()))?;
        file.sync_all()
            .map_err(|e| format!("fsync temp config.env {}: {e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("replace config.env {}: {e}", path.display())
    })
}

fn tmp_path_for(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    PathBuf::from(tmp)
}

fn required_proposal_id(params: &TachiMemoryParams) -> Result<&str, String> {
    params
        .proposal_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "proposal_id is required".to_string())
}

fn metric(variant: &Value, name: &str) -> f64 {
    variant["metrics"][name].as_f64().unwrap_or(0.0)
}

fn variant_improves(recall_delta: f64, mrr_delta: f64) -> bool {
    recall_delta > EPSILON || (recall_delta.abs() <= EPSILON && mrr_delta > EPSILON)
}

fn sanitize_key(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "variant".to_string()
    } else {
        out
    }
}

fn stable_hex_hash(input: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn round6(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

fn format_recall_proposals_markdown(response: &Value) -> String {
    let mut out = vec![
        "Tachi recall proposals".to_string(),
        format!(
            "status: {}",
            response
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("completed")
        ),
        format!(
            "count: {}",
            response.get("count").and_then(Value::as_u64).unwrap_or(0)
        ),
    ];
    if let Some(proposals) = response.get("proposals").and_then(Value::as_array) {
        for proposal in proposals {
            let id = proposal
                .get("proposal_id")
                .and_then(Value::as_str)
                .unwrap_or("-");
            let status = proposal
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending");
            let variant = proposal
                .get("variant")
                .and_then(Value::as_str)
                .unwrap_or("-");
            out.push(format!("- `{id}` status={status} variant={variant}"));
        }
    }
    out.join("\n")
}
