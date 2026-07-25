//! Evidence-backed RecallConfig proposal review/apply loop.

use super::evidence_format::{json_string, wants_json};
use super::recall_simulate_ops::build_recall_simulation_report;
use crate::tool_params::*;
use crate::MemoryServer;
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
#[cfg(unix)]
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use tachi_dispatch::policy::{
    canonical_json_eq, recall_config_v3_identity_payload, RECALL_CONFIG_PROPOSAL_KIND,
    RECALL_CONFIG_PROPOSAL_POLICY_VERSION, RECALL_CONFIG_PROPOSAL_SCHEMA_VERSION,
    RECALL_CONFIG_PROPOSAL_TARGET,
};

const RECALL_CONFIG_PROPOSAL_NS: &str = "recall_config_proposals";
const EPSILON: f64 = 0.000_001;

/// `true` iff the row's unbound top-level DISPLAY field (`config_env` — the
/// exact field `handle_recall_config_proposals`/`handle_recall_config_review`
/// return verbatim to a caller) has drifted from its digest-bound
/// `identity_payload.apply_payload.config_env` counterpart. Compares through
/// the SAME `canonical_json_eq` the identity hash itself normalizes through —
/// the only comparison rule this repo has for "these two JSON values
/// represent the same content," never a second one invented here.
///
/// A DIFFERENT question from `content_digest_mismatch`: that check proves
/// `identity_payload` is internally self-consistent with `content_digest`; it
/// says nothing about whether the DISPLAY copy a human actually reviewed
/// still matches it. A row can pass the digest check and still have a
/// drifted display copy if only `config_env` was hand-edited or partially
/// written after generation — a human who approves based on that (wrong)
/// display copy has not actually approved what `identity_payload` binds.
fn recall_config_display_drifted(value: &Value, identity_payload: &Value) -> bool {
    let bound_config_env = identity_payload
        .get("apply_payload")
        .and_then(|apply_payload| apply_payload.get("config_env"))
        .cloned()
        .unwrap_or(json!({}));
    let display_config_env = value.get("config_env").cloned().unwrap_or(json!({}));
    !canonical_json_eq(&display_config_env, &bound_config_env)
}

/// Deterministic test seam for the otherwise-uncooperative config.env writer
/// in the review protocol. It exists only in test builds and runs after the
/// proposal CAS has executed inside its still-uncommitted transaction, but
/// before the post-CAS source digest is read.
#[cfg(test)]
fn run_recall_review_post_cas_test_hook(
    params: &TachiMemoryParams,
    config_env_path: &Path,
) -> Result<(), String> {
    let Some(body) = params
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("test_config_env_after_review_cas"))
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    std::fs::write(config_env_path, body).map_err(|e| {
        format!(
            "test hook edit config.env {} after review CAS: {e}",
            config_env_path.display()
        )
    })
}

#[cfg(not(test))]
fn run_recall_review_post_cas_test_hook(
    _params: &TachiMemoryParams,
    _config_env_path: &Path,
) -> Result<(), String> {
    Ok(())
}

/// Deterministic test seam for a non-cooperating writer in the former
/// post-read/pre-rename window. Production has no hook; tests use it to mutate
/// the already-open descriptor's source immediately before final validation.
#[cfg(test)]
fn run_recall_apply_pre_append_test_hook(
    params: &TachiMemoryParams,
    config_env_path: &Path,
) -> Result<(), String> {
    let Some(body) = params
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("test_config_env_before_recall_append"))
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    std::fs::write(config_env_path, body).map_err(|e| {
        format!(
            "test hook edit config.env {} before recall append: {e}",
            config_env_path.display()
        )
    })
}

#[cfg(not(test))]
fn run_recall_apply_pre_append_test_hook(
    _params: &TachiMemoryParams,
    _config_env_path: &Path,
) -> Result<(), String> {
    Ok(())
}

pub(crate) async fn handle_recall_config_proposals(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let generated = if has_eval_input(params) {
        let app_home = crate::cli_client::app_home_from_global_db(&server.global_db_path_buf());
        let config_env_path = app_home.join("config.env");
        let source_revision = compute_recall_digest(&config_env_path)?;
        let simulation = build_recall_simulation_report(server, params).await?;
        let proposals =
            build_proposals_from_simulation(&simulation, params.force, &source_revision)?;
        let live_source_revision = compute_recall_digest(&config_env_path)?;
        if live_source_revision != source_revision {
            return Err(format!(
                "source_state_drift: recall config.env changed while proposals were generated; no proposals were persisted, regenerate from source revision {live_source_revision}"
            ));
        }
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
    let app_home = crate::cli_client::app_home_from_global_db(&server.global_db_path_buf());
    let config_env_path = app_home.join("config.env");
    // config.env cannot participate in SQLite atomicity. Read its relevant
    // digest immediately before opening the review transaction, then read it
    // again after the review CAS while that CAS is still uncommitted. Drift
    // returns an error and drops the transaction, restoring the exact pending
    // row and version rather than durably blessing either file state.
    let pre_review_source_revision = compute_recall_digest(&config_env_path)?;
    let updated = server.with_global_store(|store| {
        let tx = store
            .connection_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| format!("open bracketed recall review tx: {e}"))?;
        let (raw, version) = memcore::db::get_state(&tx, RECALL_CONFIG_PROPOSAL_NS, proposal_id)
            .map_err(|e| format!("load recall config proposal: {e}"))?
            .ok_or_else(|| format!("recall config proposal not found: {proposal_id}"))?;
        let mut value: Value =
            serde_json::from_str(&raw).map_err(|e| format!("parse recall config proposal: {e}"))?;
        // Legacy proposals (pre-v3 schema) carry no content-addressed binding
        // between what the human reviewed and what apply will persist, so an
        // old approval cannot be trusted to cover the current config_env.
        // Refuse loudly rather than silently inheriting that approval.
        let is_v3 = value
            .get("schema_version")
            .and_then(Value::as_u64)
            .map(|v| v == RECALL_CONFIG_PROPOSAL_SCHEMA_VERSION)
            .unwrap_or(false);
        if !is_v3 {
            return Err(format!(
                "legacy_unbound_proposal: {proposal_id} predates the v3 content-addressed identity and cannot be reviewed; regenerate with action='recall_proposals' to mint a fresh pending v3 proposal"
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
        // Distinct from the digest check above: prove the DISPLAY copy
        // (top-level `config_env` — what this action's own response returns)
        // still matches what `identity_payload` binds. A reviewer approves
        // based on the display copy, not `identity_payload` — if it drifted,
        // the approval about to be recorded would not actually cover the
        // bound content. Refuse rather than silently reviewing content the
        // caller never saw.
        if recall_config_display_drifted(&value, &identity_payload) {
            return Err(format!(
                "display_copy_drift: recall config proposal {proposal_id} top-level `config_env` does not match its digest-bound identity_payload copy; refusing to record a review decision against display content that has diverged from what was actually generated"
                ));
        }
        validate_recall_config_proposal(
            proposal_id,
            &value,
            Some(&pre_review_source_revision),
        )?;
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
        let cas_ok = memcore::db::set_state_if_version(
            &tx,
            RECALL_CONFIG_PROPOSAL_NS,
            proposal_id,
            &next,
            version,
        )
            .map_err(|e| format!("persist recall config review: {e}"))?;
        if !cas_ok {
            return Err(format!(
                "stale_state_version: recall config proposal {proposal_id} changed before review; reload and retry"
            ));
        }
        run_recall_review_post_cas_test_hook(params, &config_env_path)?;
        let post_review_source_revision = compute_recall_digest(&config_env_path)?;
        if post_review_source_revision != pre_review_source_revision {
            // Returning before commit drops/rolls back the transaction. This
            // is the compensating action: unlike a second durable CAS, it
            // restores the byte-identical pending row and its exact version.
            return Err(format!(
                "review_source_drift: recall config.env changed across the review CAS from {pre_review_source_revision} to {post_review_source_revision}; the review transition was rolled back and the proposal remains pending"
            ));
        }
        tx.commit()
            .map_err(|e| format!("commit bracketed recall review tx: {e}"))?;
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
        drive_recall_apply_state_machine(server, params, proposal_id, &config_env_path)?;

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
    /// Recovery where the file already matched `after_digest` (append landed,
    /// finalize CAS did not). Finalized idempotently; no file mutation.
    FinalizedExisting { attempt_id: String },
    /// Recovery where the file still matched `before_digest` (append did not
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

#[derive(Clone, Debug)]
struct RecallAppendPlan {
    before_len: usize,
    append_payload: String,
    after_digest: String,
}

/// Drive one proposal through the recoverable apply state machine. Returns
/// the terminal `applied` proposal row, the keys that the proposal updates on
/// `config.env`, and the outcome that describes which recovery branch was
/// taken. All mutations are gated by hard_state version CAS, so two
/// concurrent applies yield exactly one terminal receipt (the loser's CAS
/// fails with `stale_state_version`).
///
/// State graph:
///   approved  --(CAS)-->  applying  --(append, CAS)-->  applied
///                 |                |
///                 |                +-- recovery: observed == after  -> finalize
///                 |                +-- recovery: observed == before -> retry
///                 |                +-- recovery: observed == other  -> REFUSE
///                 +-- anything else -> REFUSE (legacy / pending / etc.)
fn drive_recall_apply_state_machine(
    server: &MemoryServer,
    params: &TachiMemoryParams,
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

    // Legacy refusal: a pre-v3 row carries no content-addressed binding, so
    // its approval does not cover the current config_env payload.
    let is_v3 = proposal
        .get("schema_version")
        .and_then(Value::as_u64)
        .map(|v| v == RECALL_CONFIG_PROPOSAL_SCHEMA_VERSION)
        .unwrap_or(false);
    if !is_v3 {
        return Err(format!(
            "legacy_unbound_proposal: {proposal_id} predates the v3 content-addressed identity and cannot be applied; regenerate with action='recall_proposals' to mint a fresh pending v3 proposal"
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
    // Distinct from the digest check above: prove the DISPLAY copy
    // (top-level `config_env` — what a reviewer's UI/response actually shows)
    // still matches what `identity_payload` binds. Reading the bound copy
    // below (never the display copy) already makes it impossible for a
    // drifted display copy to reach config.env — but a drifted display copy
    // still means the human's review was recorded against content that
    // differs from what actually gets applied, which is a fact worth
    // refusing loudly on rather than silently overriding.
    if recall_config_display_drifted(&proposal, &identity_payload) {
        return Err(format!(
            "display_copy_drift: recall config proposal {proposal_id} top-level `config_env` does not match its digest-bound identity_payload copy; refusing to apply content that diverged from what was reviewed"
        ));
    }
    let (identity_payload, bound_source_revision) =
        validate_recall_config_proposal(proposal_id, &proposal, None)?;

    // Consume the BOUND copy (identity_payload.apply_payload.config_env),
    // never the unbound top-level `proposal.config_env` display field. The
    // digest check above only proves `identity_payload` is internally
    // consistent with `content_digest`; it says nothing about whether the
    // top-level `config_env` field (which the reviewer's UI/response shows)
    // still matches it. Reading the bound copy here makes that question moot
    // by construction — there is no second copy in the trust path to drift.
    let patch = parse_config_env_patch(&identity_payload)?;
    if patch.is_empty() {
        return Err(format!(
            "recall config proposal {proposal_id} has no TACHI_RECALL_* config_env values"
        ));
    }

    let status = proposal
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");

    match status {
        "approved" => {
            let observed_source_revision = compute_recall_digest(config_env_path)?;
            if observed_source_revision != bound_source_revision {
                return Err(format!(
                    "source_state_drift: recall config proposal {proposal_id} was approved against source revision {bound_source_revision}, but config.env is now {observed_source_revision}; refusing without changing the proposal or file"
                ));
            }
            // The approved identity, not a fresh apply-time observation, is
            // the receipt's before state. A post-approval edit must refuse
            // above rather than being silently blessed as this attempt's new
            // baseline.
            let before_digest = bound_source_revision.as_str();
            let append_plan = compute_recall_append_plan(config_env_path, &patch)?;
            let after_digest = append_plan.after_digest.as_str();
            // Fresh apply: stamp an applying receipt, do the descriptor append,
            // then finalize.
            let attempt_id = uuid::Uuid::new_v4().to_string();
            stamp_applying_receipt(
                server,
                proposal_id,
                &attempt_id,
                before_digest,
                after_digest,
                &patch,
                append_plan.before_len,
                &append_plan.append_payload,
            )?;
            append_recall_config_env(
                config_env_path,
                before_digest,
                after_digest,
                append_plan.before_len,
                &append_plan.append_payload,
                params,
            )?;
            let outcome = RecallApplyOutcome::Fresh { attempt_id };
            finalize_recall_apply(
                server,
                proposal_id,
                &outcome,
                &patch,
                after_digest,
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
            let receipt_before_len = receipt
                .get("before_len")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok());
            let receipt_append_payload = receipt
                .get("append_payload")
                .and_then(Value::as_str)
                .map(str::to_string);
            if receipt_attempt_id.is_empty()
                || receipt_before.is_empty()
                || receipt_after.is_empty()
                || receipt_before_len.is_none()
                || receipt_append_payload.is_none()
            {
                return Err(format!(
                    "recall config proposal {proposal_id} applying_receipt is missing attempt_id/before_digest/after_digest/before_len/append_payload; refusing to guess — operator must reconcile the row"
                ));
            }
            let receipt_before_len = receipt_before_len.expect("checked above");
            let receipt_append_payload = receipt_append_payload.expect("checked above");
            validate_receipt_append_payload(&patch, &receipt_append_payload)?;
            if receipt_before != bound_source_revision {
                return Err(format!(
                    "source_revision_mismatch: recall config proposal {proposal_id} applying receipt baseline {receipt_before} does not match its approved bound source revision {bound_source_revision}; refusing recovery"
                ));
            }
            let observed = compute_recall_digest(config_env_path)?;
            if observed == receipt_after {
                // Append already landed before the crash; finalize idempotently.
                let outcome =
                    RecallApplyOutcome::FinalizedExisting { attempt_id: receipt_attempt_id };
                finalize_recall_apply(
                    server,
                    proposal_id,
                    &outcome,
                    &patch,
                    &receipt_after,
                    config_env_path,
                )
            } else if recall_append_progress(
                &read_config_env_body(config_env_path)?,
                &receipt_before,
                receipt_before_len,
                &receipt_append_payload,
            )
            .is_some()
            {
                // The append did not land or only a receipt-bound prefix
                // landed before a crash. Resume only the missing suffix.
                append_recall_config_env(
                    config_env_path,
                    &receipt_before,
                    &receipt_after,
                    receipt_before_len,
                    &receipt_append_payload,
                    params,
                )?;
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
                    &receipt_after,
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
    before_len: usize,
    append_payload: &str,
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
            "before_len": before_len,
            "append_payload": append_payload,
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
/// the append and this finalize cannot stamp `applied` on a row whose file is
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
    // A newly created file can be durable as file data while its directory
    // entry is not. Confirm parent-directory durability before the terminal
    // CAS on every finalize path, including recovery after a process crash.
    // An unsupported directory sync is loud and leaves the proposal applying.
    sync_recall_config_parent(config_env_path)?;
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
    source_revision: &str,
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
        let identity_payload = recall_config_v3_identity_payload(
            &config_env_value,
            &evidence_review,
            RECALL_CONFIG_PROPOSAL_POLICY_VERSION,
            RECALL_CONFIG_PROPOSAL_TARGET,
            source_revision,
        );
        let content_digest = content_digest_hex(&identity_payload);
        let id = format!("recall_config:v3:{content_digest}");
        out.push(json!({
            "proposal_id": id,
            "legacy_proposal_id": legacy_id,
            "kind": RECALL_CONFIG_PROPOSAL_KIND,
            "schema_version": RECALL_CONFIG_PROPOSAL_SCHEMA_VERSION,
            "policy_version": RECALL_CONFIG_PROPOSAL_POLICY_VERSION,
            "target": RECALL_CONFIG_PROPOSAL_TARGET,
            "source_revision": source_revision,
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
                    // stored row is v3 AND carries the SAME content digest as
                    // the freshly regenerated proposal. The v3 id already
                    // collides only with itself when content is identical, so
                    // this is a belt-and-braces guard against any path that
                    // writes the same id with different content. A legacy row
                    // (pre-v3) donates nothing — its approval was not bound to
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
        // Legacy proposals (pre-v3 schema) carry no content-addressed binding
        // and are refused at review/apply; surface the marker here so callers
        // see *why* before they hit the refusal.
        let is_v3 = value
            .get("schema_version")
            .and_then(Value::as_u64)
            .map(|version| version == RECALL_CONFIG_PROPOSAL_SCHEMA_VERSION)
            .unwrap_or(false);
        if !is_v3 {
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
/// scope. Both must serialize through the same `recall_config_v3_identity_payload`
/// canonical form first.
fn content_digest_hex(identity_payload: &Value) -> String {
    let canonical = tachi_dispatch::policy::canonical_json(identity_payload).to_string();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hex_lower(&hasher.finalize())
}

/// Reject rows that remain self-consistent after a hand edit but no longer
/// implement the current recall proposal contract. The optional live source
/// revision is supplied at review and on the fresh approved->applying path;
/// recovery from an already-stamped applying receipt validates that receipt's
/// bound baseline separately because the file may legitimately equal `after`.
fn validate_recall_config_proposal(
    proposal_id: &str,
    value: &Value,
    live_source_revision: Option<&str>,
) -> Result<(Value, String), String> {
    if value.get("schema_version").and_then(Value::as_u64)
        != Some(RECALL_CONFIG_PROPOSAL_SCHEMA_VERSION)
    {
        return Err(format!(
            "schema_version_mismatch: recall config proposal {proposal_id} must use current schema version {RECALL_CONFIG_PROPOSAL_SCHEMA_VERSION}; regenerate a fresh pending proposal"
        ));
    }
    if value.get("kind").and_then(Value::as_str) != Some(RECALL_CONFIG_PROPOSAL_KIND) {
        return Err(format!(
            "kind_mismatch: recall config proposal {proposal_id} is not the current {RECALL_CONFIG_PROPOSAL_KIND} kind"
        ));
    }
    if value.get("policy_version").and_then(Value::as_str)
        != Some(RECALL_CONFIG_PROPOSAL_POLICY_VERSION)
    {
        return Err(format!(
            "current_policy_mismatch: recall config proposal {proposal_id} does not use current policy version {RECALL_CONFIG_PROPOSAL_POLICY_VERSION}; regenerate before review or apply"
        ));
    }
    if value.get("target").and_then(Value::as_str) != Some(RECALL_CONFIG_PROPOSAL_TARGET) {
        return Err(format!(
            "target_mismatch: recall config proposal {proposal_id} does not target {RECALL_CONFIG_PROPOSAL_TARGET}"
        ));
    }
    let identity_payload = value
        .get("identity_payload")
        .cloned()
        .unwrap_or(Value::Null);
    if identity_payload.get("kind").and_then(Value::as_str) != Some(RECALL_CONFIG_PROPOSAL_KIND)
        || identity_payload
            .get("policy_version")
            .and_then(Value::as_str)
            != Some(RECALL_CONFIG_PROPOSAL_POLICY_VERSION)
        || identity_payload.get("target").and_then(Value::as_str)
            != Some(RECALL_CONFIG_PROPOSAL_TARGET)
    {
        return Err(format!(
            "current_policy_mismatch: recall config proposal {proposal_id} identity payload does not bind the current kind, policy version, and target"
        ));
    }
    let bound_source_revision = identity_payload
        .get("source_revision")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "legacy_unbound_proposal: recall config proposal {proposal_id} has no bound source revision; regenerate a fresh pending proposal"
            )
        })?;
    if value.get("source_revision").and_then(Value::as_str) != Some(bound_source_revision.as_str())
    {
        return Err(format!(
            "source_revision_mismatch: recall config proposal {proposal_id} display revision does not match its bound source revision"
        ));
    }
    let stored_digest = value
        .get("content_digest")
        .and_then(Value::as_str)
        .unwrap_or("");
    let recomputed_digest = content_digest_hex(&identity_payload);
    if stored_digest.is_empty() || stored_digest != recomputed_digest {
        return Err(format!(
            "content_digest_mismatch: recall config proposal {proposal_id} stored digest {stored_digest:?} does not match recomputed {recomputed_digest}; refusing unreviewed content"
        ));
    }
    let expected_id = format!("recall_config:v3:{recomputed_digest}");
    if proposal_id != expected_id.as_str()
        || value.get("proposal_id").and_then(Value::as_str) != Some(expected_id.as_str())
    {
        return Err(format!(
            "proposal_identity_mismatch: recall config proposal {proposal_id} is not stored under its canonical full content-address key {expected_id}"
        ));
    }
    if let Some(live_source_revision) = live_source_revision {
        if live_source_revision != bound_source_revision.as_str() {
            return Err(format!(
                "source_state_drift: recall config proposal {proposal_id} was generated against source revision {bound_source_revision}, but config.env is now {live_source_revision}; regenerate and re-review"
            ));
        }
    }
    if recall_config_display_drifted(value, &identity_payload) {
        return Err(format!(
            "display_copy_drift: recall config proposal {proposal_id} top-level `config_env` does not match its digest-bound identity_payload copy; refusing content that diverged from what was reviewed"
        ));
    }
    Ok((identity_payload, bound_source_revision))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(2 * bytes.len());
    for byte in bytes {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

/// SHA-256 hex of the complete config.env source currently on disk at `path`.
/// Used as the `before_digest` of a fresh apply and as the `observed` digest
/// the recovery path compares against the receipt's before/after digests. It
/// covers provider/Vault lines, comments, and formatting so any non-recall
/// source edit rotates the proposal identity and blocks stale application.
fn compute_recall_digest(path: &Path) -> Result<String, String> {
    Ok(digest_config_env_source(&read_config_env_body(path)?))
}

/// SHA-256 hex of the complete config.env source at `path` *after* `patch` is
/// applied as trailing assignments. Both shipped dotenv readers use
/// last-assignment-wins semantics, so the append changes effective recall
/// values without replacing any pre-existing provider/Vault bytes.
fn compute_recall_append_plan(
    path: &Path,
    patch: &BTreeMap<String, String>,
) -> Result<RecallAppendPlan, String> {
    let mut source = read_config_env_body(path)?;
    let before_len = source.len();
    let append_payload = recall_config_append_payload(&source, patch);
    source.push_str(&append_payload);
    Ok(RecallAppendPlan {
        before_len,
        append_payload,
        after_digest: digest_config_env_source(&source),
    })
}

#[cfg(unix)]
fn read_config_env_body(path: &Path) -> Result<String, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            validate_recall_config_metadata(path, &metadata)?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(err) => {
            return Err(format!(
                "read config.env metadata {}: {err}",
                path.display()
            ))
        }
    }

    let mut file = open_existing_recall_config_read(path)?;
    let descriptor_metadata = file
        .metadata()
        .map_err(|err| format!("read open config.env metadata {}: {err}", path.display()))?;
    validate_recall_config_metadata(path, &descriptor_metadata)?;
    assert_recall_config_path_identity(path, &descriptor_metadata)?;
    let source = read_recall_config_descriptor(&mut file, path)?;
    assert_recall_config_path_identity(path, &descriptor_metadata)?;
    Ok(source)
}

#[cfg(not(unix))]
fn read_config_env_body(path: &Path) -> Result<String, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "symlink_config_refused: config.env {} is a symlink; refusing to read or apply without a stable object identity",
            path.display()
        )),
        Ok(metadata) if !metadata.file_type().is_file() => Err(format!(
            "unsupported_config_type: config.env {} is not a regular file",
            path.display()
        )),
        Ok(_) => std::fs::read_to_string(path)
            .map_err(|err| format!("read config.env {}: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(format!(
            "read config.env metadata {}: {err}",
            path.display()
        )),
    }
}

fn digest_config_env_source(source: &str) -> String {
    hex_lower(&Sha256::digest(source.as_bytes()))
}

fn recall_config_append_payload(source: &str, values: &BTreeMap<String, String>) -> String {
    let mut body = String::new();
    if !source.is_empty() && !source.ends_with('\n') {
        body.push('\n');
    }
    for (key, value) in values {
        body.push_str(key);
        body.push('=');
        body.push_str(value);
        body.push('\n');
    }
    body
}

fn validate_receipt_append_payload(
    values: &BTreeMap<String, String>,
    append_payload: &str,
) -> Result<(), String> {
    let assignments = recall_config_append_payload("", values);
    if append_payload == assignments || append_payload == format!("\n{assignments}") {
        return Ok(());
    }
    Err("invalid_applying_receipt: append_payload does not encode exactly the approved recall assignments"
        .to_string())
}

fn recall_append_progress(
    source: &str,
    expected_before_revision: &str,
    expected_before_len: usize,
    expected_append_payload: &str,
) -> Option<usize> {
    let bytes = source.as_bytes();
    if bytes.len() < expected_before_len
        || bytes.len() > expected_before_len + expected_append_payload.len()
    {
        return None;
    }
    let before = &bytes[..expected_before_len];
    if hex_lower(&Sha256::digest(before)) != expected_before_revision {
        return None;
    }
    let landed = &bytes[expected_before_len..];
    expected_append_payload
        .as_bytes()
        .starts_with(landed)
        .then_some(landed.len())
}

// Reads the BOUND apply payload (`identity_payload.apply_payload.config_env`),
// not the unbound top-level `proposal.config_env` display field. Callers must
// pass the `identity_payload` sub-value (already digest-validated by the
// caller), not the whole proposal — see the call site in
// `drive_recall_apply_state_machine` for why.
fn parse_config_env_patch(identity_payload: &Value) -> Result<BTreeMap<String, String>, String> {
    let config_env = identity_payload
        .get("apply_payload")
        .and_then(|apply_payload| apply_payload.get("config_env"))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            "recall config proposal identity_payload missing apply_payload.config_env".to_string()
        })?;
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

/// Append recall assignments through a descriptor for the exact regular file
/// whose full source revision was approved. This never replaces existing
/// bytes. The same descriptor and final path identity are checked immediately
/// before and after the append, so a non-cooperating replacement survives and
/// is refused rather than overwritten.
#[cfg(unix)]
fn append_recall_config_env(
    path: &Path,
    expected_source_revision: &str,
    expected_after_revision: &str,
    expected_before_len: usize,
    expected_append_payload: &str,
    params: &TachiMemoryParams,
) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create config.env parent {}: {e}", parent.display()))?;
    }

    let mut file = match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            validate_recall_config_metadata(path, &metadata)?;
            open_existing_recall_config(path)?
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let mut options = std::fs::OpenOptions::new();
            options
                .read(true)
                .append(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
            match options.open(path) {
                Ok(file) => file,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    open_existing_recall_config(path)?
                }
                Err(err) => return Err(format!("create config.env {}: {err}", path.display())),
            }
        }
        Err(err) => {
            return Err(format!(
                "read config.env metadata {}: {err}",
                path.display()
            ))
        }
    };
    let descriptor_metadata = file
        .metadata()
        .map_err(|err| format!("read open config.env metadata {}: {err}", path.display()))?;
    validate_recall_config_metadata(path, &descriptor_metadata)?;
    assert_recall_config_path_identity(path, &descriptor_metadata)?;

    let existing = read_recall_config_descriptor(&mut file, path)?;
    let Some(initial_progress) = recall_append_progress(
        &existing,
        expected_source_revision,
        expected_before_len,
        expected_append_payload,
    ) else {
        let observed_source_revision = digest_config_env_source(&existing);
        return Err(format!(
            "source_state_drift: config.env {} changed before descriptor append; expected source revision {expected_source_revision}, observed {observed_source_revision}; refusing to modify the current source",
            path.display()
        ));
    };
    let mut projected = existing.as_bytes()[..expected_before_len].to_vec();
    projected.extend_from_slice(expected_append_payload.as_bytes());
    let projected_revision = hex_lower(&Sha256::digest(&projected));
    if projected_revision != expected_after_revision {
        return Err(format!(
            "projected_digest_mismatch: config.env {} append projects revision {projected_revision}, but applying receipt expects {expected_after_revision}; refusing to write",
            path.display()
        ));
    }

    run_recall_apply_pre_append_test_hook(params, path)?;
    assert_recall_config_path_identity(path, &descriptor_metadata)?;
    let revalidated = read_recall_config_descriptor(&mut file, path)?;
    let Some(revalidated_progress) = recall_append_progress(
        &revalidated,
        expected_source_revision,
        expected_before_len,
        expected_append_payload,
    ) else {
        let revalidated_revision = digest_config_env_source(&revalidated);
        return Err(format!(
            "source_state_drift: config.env {} changed immediately before descriptor append; expected source revision {expected_source_revision}, observed {revalidated_revision}; refusing to modify the current source",
            path.display()
        ));
    };
    if revalidated_progress < initial_progress {
        return Err(format!(
            "source_state_drift: config.env {} lost receipt-bound append bytes during final validation; refusing to modify the current source",
            path.display()
        ));
    }

    file.write_all(&expected_append_payload.as_bytes()[revalidated_progress..])
        .map_err(|err| format!("append recall config.env {}: {err}", path.display()))?;
    file.sync_all()
        .map_err(|err| format!("fsync recall config.env {}: {err}", path.display()))?;
    assert_recall_config_path_identity(path, &descriptor_metadata)?;
    let appended = read_recall_config_descriptor(&mut file, path)?;
    let appended_revision = digest_config_env_source(&appended);
    if appended_revision != expected_after_revision {
        return Err(format!(
            "append_source_drift: config.env {} revision after append is {appended_revision}, expected {expected_after_revision}; refusing to finalize",
            path.display()
        ));
    }
    sync_recall_config_parent(path)
}

#[cfg(unix)]
fn open_existing_recall_config(path: &Path) -> Result<std::fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    options.open(path).map_err(|err| {
        if err.raw_os_error() == Some(libc::ELOOP) {
            format!(
                "symlink_config_refused: config.env {} is a symlink; refusing to apply without a stable object identity",
                path.display()
            )
        } else {
            format!("open config.env {} for append: {err}", path.display())
        }
    })
}

#[cfg(unix)]
fn open_existing_recall_config_read(path: &Path) -> Result<std::fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    options.open(path).map_err(|err| {
        if err.raw_os_error() == Some(libc::ELOOP) {
            format!(
                "symlink_config_refused: config.env {} is a symlink; refusing to read without a stable object identity",
                path.display()
            )
        } else {
            format!("open config.env {} for read: {err}", path.display())
        }
    })
}

#[cfg(unix)]
fn validate_recall_config_metadata(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), String> {
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "symlink_config_refused: config.env {} is a symlink; refusing to apply without a stable object identity",
            path.display()
        ));
    }
    if !metadata.file_type().is_file() {
        return Err(format!(
            "unsupported_config_type: config.env {} is not a regular file",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn assert_recall_config_path_identity(
    path: &Path,
    descriptor_metadata: &std::fs::Metadata,
) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let path_metadata = std::fs::symlink_metadata(path).map_err(|err| {
        format!(
            "source_identity_drift: config.env {} cannot be inspected after opening: {err}",
            path.display()
        )
    })?;
    validate_recall_config_metadata(path, &path_metadata)?;
    if path_metadata.dev() != descriptor_metadata.dev()
        || path_metadata.ino() != descriptor_metadata.ino()
    {
        return Err(format!(
            "source_identity_drift: config.env {} no longer names the opened object; refusing to append or finalize",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn read_recall_config_descriptor(file: &mut std::fs::File, path: &Path) -> Result<String, String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|err| format!("seek config.env {}: {err}", path.display()))?;
    let mut source = String::new();
    file.read_to_string(&mut source)
        .map_err(|err| format!("read open config.env {}: {err}", path.display()))?;
    Ok(source)
}

#[cfg(not(unix))]
fn append_recall_config_env(
    path: &Path,
    _expected_source_revision: &str,
    _expected_after_revision: &str,
    _expected_before_len: usize,
    _expected_append_payload: &str,
    _params: &TachiMemoryParams,
) -> Result<(), String> {
    Err(format!(
        "unsupported_platform: recall config apply for {} requires descriptor identity and no-follow guarantees",
        path.display()
    ))
}

fn sync_recall_config_parent(path: &Path) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| {
        format!(
            "fsync config.env parent: {} has no parent directory",
            path.display()
        )
    })?;
    let directory = std::fs::File::open(parent)
        .map_err(|e| format!("open config.env parent {} for fsync: {e}", parent.display()))?;
    directory
        .sync_all()
        .map_err(|e| format!("fsync config.env parent {}: {e}", parent.display()))
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
