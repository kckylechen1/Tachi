use super::super::*;
use super::simulation::{route_simulation_caveats, simulate_route_policy};
use sha2::{Digest, Sha256};
use tachi_dispatch::policy::{
    build_loadout_evolution_proposals, build_route_policy_proposals, canonical_json,
    canonical_json_eq, loadout_evolution_v3_apply_payload, route_policy_v3_identity_payload,
    LoadoutEvalEntry, ProfileCardRiskInputs, ProfilePositiveEvolutionInputs,
    LOADOUT_EVOLUTION_PROPOSAL_KIND, LOADOUT_EVOLUTION_PROPOSAL_POLICY_VERSION,
    LOADOUT_EVOLUTION_PROPOSAL_SCHEMA_VERSION, LOADOUT_EVOLUTION_PROPOSAL_TARGET,
    ROUTE_POLICY_PROPOSAL_KIND, ROUTE_POLICY_PROPOSAL_POLICY_VERSION,
    ROUTE_POLICY_PROPOSAL_SCHEMA_VERSION, ROUTE_POLICY_PROPOSAL_TARGET,
};

/// SHA-256 hex of the canonical identity payload. Used as the content-addressed
/// half of the v3 proposal id and stored verbatim as `content_digest` so apply
/// can re-validate the persisted row was not mutated after review.
pub(super) fn content_digest_hex(identity_payload: &Value) -> String {
    // `serde_json` does not guarantee key order across rebuilds; serialize the
    // value through `route_policy_v3_identity_payload`'s canonical form first
    // so the hash is stable regardless of how the caller assembled the input.
    let canonical = canonical_json(identity_payload).to_string();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let bytes = hasher.finalize();
    let mut out = String::with_capacity(2 * bytes.len());
    for byte in bytes {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

/// Which unbound top-level DISPLAY field (`policy_rule` or `evidence`) — the
/// exact fields `handle_route_policy_proposals`/`handle_route_policy_review`
/// return verbatim to a caller — diverged from its digest-bound
/// `identity_payload` counterpart, if any. `None` means both match after
/// canonicalizing through the SAME `canonical_json` the identity hash uses
/// (`canonical_json_eq`), which is the only comparison rule this repo has for
/// "these two JSON values represent the same content."
///
/// This is a DIFFERENT question from `content_digest_mismatch`:
/// `content_digest_hex(identity_payload) == content_digest` proves
/// `identity_payload` is internally self-consistent with the stored digest —
/// it says nothing about whether the row's DISPLAY copy (what a human
/// reviewing the proposal actually sees) still matches it. A row can pass the
/// digest check and still have a drifted display copy if only `policy_rule`/
/// `evidence` were hand-edited or partially written after generation; a human
/// who approves based on the (wrong) display copy has not actually approved
/// what `identity_payload` binds. Refuse loudly rather than silently either
/// applying the (safe) bound copy or trusting the (possibly wrong) display
/// copy — see the review/apply call sites for why silent correction is not
/// this repo's call to make on a human's behalf.
pub(super) fn route_policy_display_drift(
    value: &Value,
    identity_payload: &Value,
) -> Option<&'static str> {
    let apply_payload = identity_payload
        .get("apply_payload")
        .cloned()
        .unwrap_or(json!({}));
    let evidence_review = identity_payload
        .get("evidence_review")
        .cloned()
        .unwrap_or(json!({}));
    let policy_rule = value.get("policy_rule").cloned().unwrap_or(json!({}));
    let evidence = value.get("evidence").cloned().unwrap_or(json!({}));
    if !canonical_json_eq(&policy_rule, &apply_payload) {
        return Some("policy_rule");
    }
    if !canonical_json_eq(&evidence, &evidence_review) {
        return Some("evidence");
    }
    None
}

/// v3 proposal id: human-readable kind prefix + the content digest. The digest
/// is the SHA-256 of the canonical identity payload, so any change to the
/// apply payload, evidence, policy version, or target rotates the id and
/// forces a fresh pending row instead of inheriting an old approval.
pub(super) fn route_policy_v3_proposal_id(identity_payload: &Value) -> String {
    format!("route_policy:v3:{}", content_digest_hex(identity_payload))
}

pub(super) fn loadout_evolution_v3_proposal_id(identity_payload: &Value) -> String {
    format!(
        "loadout_evolution:v3:{}",
        content_digest_hex(identity_payload)
    )
}

/// `true` iff the persisted row carries the exact current v3 schema marker.
/// Older rows predate the source-revision content-addressed identity
/// and are refused at review/apply as `legacy_unbound_proposal`.
fn is_v3_proposal(value: &Value) -> bool {
    value
        .get("schema_version")
        .and_then(Value::as_u64)
        .map(|version| version == ROUTE_POLICY_PROPOSAL_SCHEMA_VERSION)
        .unwrap_or(false)
}

fn is_v3_loadout_evolution_proposal(value: &Value) -> bool {
    value
        .get("schema_version")
        .and_then(Value::as_u64)
        .map(|version| version == LOADOUT_EVOLUTION_PROPOSAL_SCHEMA_VERSION)
        .unwrap_or(false)
}

/// Deterministic revision of every active route-rule row. It deliberately
/// includes each hard-state version as well as canonical value content: a
/// third party writing the same JSON still advances the active configuration
/// revision and must invalidate an approval made against the earlier state.
pub(super) fn route_policy_source_revision(rows: &[memcore::db::StateRow]) -> String {
    let mut snapshot = rows
        .iter()
        .map(|row| {
            let value = serde_json::from_str::<Value>(&row.value_json)
                .map(|value| canonical_json(&value))
                .unwrap_or_else(|_| Value::String(row.value_json.clone()));
            json!({
                "key": row.key,
                "version": row.version,
                "value": value,
            })
        })
        .collect::<Vec<_>>();
    snapshot.sort_by(|left, right| left["key"].as_str().cmp(&right["key"].as_str()));
    content_digest_hex(&Value::Array(snapshot))
}

/// Reject rows that are internally self-consistent but no longer represent
/// the current proposal contract. `live_source_revision` is supplied at
/// review and inside route apply's write transaction; omitting it is only for
/// preliminary structural validation before that transaction is opened.
pub(super) fn validate_route_policy_proposal(
    proposal_id: &str,
    value: &Value,
    live_source_revision: Option<&str>,
) -> Result<Value, String> {
    if value.get("schema_version").and_then(Value::as_u64)
        != Some(ROUTE_POLICY_PROPOSAL_SCHEMA_VERSION)
    {
        return Err(format!(
            "schema_version_mismatch: route policy proposal {proposal_id} must use current schema version {ROUTE_POLICY_PROPOSAL_SCHEMA_VERSION}; regenerate a fresh pending proposal"
        ));
    }
    if value.get("kind").and_then(Value::as_str) != Some(ROUTE_POLICY_PROPOSAL_KIND) {
        return Err(format!(
            "kind_mismatch: route policy proposal {proposal_id} is not the current {ROUTE_POLICY_PROPOSAL_KIND} kind"
        ));
    }
    if value.get("policy_version").and_then(Value::as_str)
        != Some(ROUTE_POLICY_PROPOSAL_POLICY_VERSION)
    {
        return Err(format!(
            "current_policy_mismatch: route policy proposal {proposal_id} does not use current policy version {ROUTE_POLICY_PROPOSAL_POLICY_VERSION}; regenerate before review or apply"
        ));
    }
    if value.get("target").and_then(Value::as_str) != Some(ROUTE_POLICY_PROPOSAL_TARGET) {
        return Err(format!(
            "target_mismatch: route policy proposal {proposal_id} does not target {ROUTE_POLICY_PROPOSAL_TARGET}"
        ));
    }

    let identity_payload = value
        .get("identity_payload")
        .cloned()
        .unwrap_or(Value::Null);
    if identity_payload.get("kind").and_then(Value::as_str) != Some(ROUTE_POLICY_PROPOSAL_KIND)
        || identity_payload
            .get("policy_version")
            .and_then(Value::as_str)
            != Some(ROUTE_POLICY_PROPOSAL_POLICY_VERSION)
        || identity_payload.get("target").and_then(Value::as_str)
            != Some(ROUTE_POLICY_PROPOSAL_TARGET)
    {
        return Err(format!(
            "current_policy_mismatch: route policy proposal {proposal_id} identity payload does not bind the current kind, policy version, and target"
        ));
    }
    let bound_source_revision = identity_payload
        .get("source_revision")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!(
                "legacy_unbound_proposal: route policy proposal {proposal_id} has no bound source revision; regenerate a fresh pending proposal"
            )
        })?;
    if value.get("source_revision").and_then(Value::as_str) != Some(bound_source_revision) {
        return Err(format!(
            "source_revision_mismatch: route policy proposal {proposal_id} display revision does not match its bound source revision"
        ));
    }

    let stored_digest = value
        .get("content_digest")
        .and_then(Value::as_str)
        .unwrap_or("");
    let recomputed = content_digest_hex(&identity_payload);
    if stored_digest.is_empty() || recomputed != stored_digest {
        return Err(format!(
            "content_digest_mismatch: route policy proposal {proposal_id} stored digest {stored_digest:?} does not match recomputed {recomputed}; refusing unreviewed content"
        ));
    }
    let expected_id = route_policy_v3_proposal_id(&identity_payload);
    if proposal_id != expected_id.as_str()
        || value.get("proposal_id").and_then(Value::as_str) != Some(expected_id.as_str())
    {
        return Err(format!(
            "proposal_identity_mismatch: route policy proposal {proposal_id} is not stored under its canonical full content-address key {expected_id}"
        ));
    }
    if let Some(live_source_revision) = live_source_revision {
        if live_source_revision != bound_source_revision {
            return Err(format!(
                "source_state_drift: route policy proposal {proposal_id} was generated against source revision {bound_source_revision}, but active {ROUTE_POLICY_RULE_NS} is now {live_source_revision}; regenerate and re-review"
            ));
        }
    }
    if let Some(field) = route_policy_display_drift(value, &identity_payload) {
        return Err(format!(
            "display_copy_drift: route policy proposal {proposal_id} top-level `{field}` does not match its digest-bound identity_payload copy; refusing content that diverged from what was reviewed"
        ));
    }
    Ok(identity_payload)
}

/// Return the specific display surface that diverged from the digest-bound
/// loadout identity. The apply payload is reconstructed through the dispatch
/// crate's one shared shape, then compared with the same canonical equality
/// used for the digest; a reviewer never approves a mutable display copy.
pub(super) fn loadout_evolution_display_drift(
    value: &Value,
    identity_payload: &Value,
) -> Option<&'static str> {
    let apply_payload = identity_payload
        .get("apply_payload")
        .cloned()
        .unwrap_or(Value::Null);
    let evidence_review = identity_payload
        .get("evidence_review")
        .cloned()
        .unwrap_or(Value::Null);
    if !canonical_json_eq(&loadout_evolution_v3_apply_payload(value), &apply_payload) {
        return Some("apply_payload");
    }
    let evidence = value.get("evidence").cloned().unwrap_or(Value::Null);
    if !canonical_json_eq(&evidence, &evidence_review) {
        return Some("evidence");
    }
    None
}

pub(super) fn validate_loadout_evolution_proposal(
    proposal_id: &str,
    value: &Value,
) -> Result<Value, String> {
    if !is_v3_loadout_evolution_proposal(value) {
        return Err(format!(
            "legacy_unbound_proposal: loadout_evolution proposal {proposal_id} predates the v3 content-addressed identity; regenerate with action='proposals' to mint a fresh pending v3 proposal"
        ));
    }
    if value.get("kind").and_then(Value::as_str) != Some(LOADOUT_EVOLUTION_PROPOSAL_KIND) {
        return Err(format!(
            "kind_mismatch: loadout_evolution proposal {proposal_id} is not the current {LOADOUT_EVOLUTION_PROPOSAL_KIND} kind"
        ));
    }
    if value.get("policy_version").and_then(Value::as_str)
        != Some(LOADOUT_EVOLUTION_PROPOSAL_POLICY_VERSION)
    {
        return Err(format!(
            "current_policy_mismatch: loadout_evolution proposal {proposal_id} does not use current policy version {LOADOUT_EVOLUTION_PROPOSAL_POLICY_VERSION}; regenerate before review or apply"
        ));
    }
    if value.get("target").and_then(Value::as_str) != Some(LOADOUT_EVOLUTION_PROPOSAL_TARGET) {
        return Err(format!(
            "target_mismatch: loadout_evolution proposal {proposal_id} does not target {LOADOUT_EVOLUTION_PROPOSAL_TARGET}"
        ));
    }

    let identity_payload = value
        .get("identity_payload")
        .cloned()
        .unwrap_or(Value::Null);
    if identity_payload.get("kind").and_then(Value::as_str) != Some(LOADOUT_EVOLUTION_PROPOSAL_KIND)
        || identity_payload
            .get("policy_version")
            .and_then(Value::as_str)
            != Some(LOADOUT_EVOLUTION_PROPOSAL_POLICY_VERSION)
        || identity_payload.get("target").and_then(Value::as_str)
            != Some(LOADOUT_EVOLUTION_PROPOSAL_TARGET)
    {
        return Err(format!(
            "current_policy_mismatch: loadout_evolution proposal {proposal_id} identity payload does not bind the current kind, policy version, and target"
        ));
    }
    let stored_digest = value
        .get("content_digest")
        .and_then(Value::as_str)
        .unwrap_or("");
    let recomputed = content_digest_hex(&identity_payload);
    if stored_digest.is_empty() || recomputed != stored_digest {
        return Err(format!(
            "content_digest_mismatch: loadout_evolution proposal {proposal_id} stored digest {stored_digest:?} does not match recomputed {recomputed}; refusing unreviewed content"
        ));
    }
    let expected_id = loadout_evolution_v3_proposal_id(&identity_payload);
    if proposal_id != expected_id.as_str()
        || value.get("proposal_id").and_then(Value::as_str) != Some(expected_id.as_str())
    {
        return Err(format!(
            "proposal_id_mismatch: loadout_evolution proposal {proposal_id} does not match digest-bound id {expected_id}; regenerate before review or apply"
        ));
    }
    if let Some(field) = loadout_evolution_display_drift(value, &identity_payload) {
        return Err(format!(
            "display_copy_drift: loadout_evolution proposal {proposal_id} top-level `{field}` does not match its digest-bound identity_payload copy; refusing content that diverged from what was reviewed"
        ));
    }
    Ok(identity_payload)
}

pub(crate) fn handle_route_simulation(
    server: &MemoryServer,
    limit: usize,
    focus_task: Option<&str>,
    risk_override: Option<&str>,
    file_paths: &[String],
) -> Result<String, String> {
    let rows = load_live_eval_rows(server, limit.max(1))?;
    let performance_matrix = aggregate_performance_matrix(&rows);
    let focus = focus_task
        .filter(|task| !task.trim().is_empty())
        .map(|task| classify_dispatch_risk(task, risk_override, file_paths));

    let summaries = ["current", "cost_sensitive", "quality_first"]
        .iter()
        .map(|policy| simulate_route_policy(policy, &performance_matrix, focus.as_ref()))
        .collect::<Vec<_>>();

    serde_json::to_string(&json!({
        "action": "route_simulate",
        "read_only": true,
        "source": "live_memory_eval",
        "row_count": rows.len(),
        "matrix_rows": performance_matrix.len(),
        "limit": limit.max(1),
        "focus": focus.as_ref().map(|risk| json!({
            "task_type": risk.task_type,
            "risk": risk.risk,
            "risk_reasons": risk.reasons,
            "required_profiles": risk.required_profiles,
            "blocked_profiles": risk.blocked_profiles,
        })),
        "policies": summaries,
        "caveats": route_simulation_caveats(&rows, &performance_matrix),
    }))
    .map_err(|e| format!("serialize route simulation: {e}"))
}

pub(crate) fn handle_route_policy_proposals(
    server: &MemoryServer,
    limit: usize,
    status_filter: Option<&str>,
) -> Result<String, String> {
    let source_rows = server.with_global_store_read(|store| {
        store
            .list_state(ROUTE_POLICY_RULE_NS)
            .map_err(|e| format!("list active route policy rules: {e}"))
    })?;
    let source_revision = route_policy_source_revision(&source_rows);
    let rows = load_live_eval_rows(server, limit.max(1))?;
    let performance_matrix = aggregate_performance_matrix(&rows);
    let current = simulate_route_policy("current", &performance_matrix, None);
    let variants = ["cost_sensitive", "quality_first"]
        .iter()
        .map(|policy| simulate_route_policy(policy, &performance_matrix, None))
        .collect::<Vec<_>>();
    let generated_at = Utc::now().to_rfc3339();
    let mut proposals = build_route_policy_proposals(
        &current,
        &variants,
        rows.len(),
        limit.max(1),
        &generated_at,
        &source_revision,
    );
    let eval_entries = load_live_eval_entries(server, limit.max(1))?
        .into_iter()
        .map(|entry| LoadoutEvalEntry {
            path: entry.path,
            metadata: entry.metadata,
        })
        .collect::<Vec<_>>();
    proposals.extend(build_loadout_evolution_proposals(
        &performance_matrix,
        &eval_entries,
        limit.max(1),
        &generated_at,
        |profile| profile_card_risk_inputs(server, profile),
        |profile| profile_positive_evolution_inputs(server, profile),
    )?);

    server.with_global_store(|store| {
        let current_source_rows = store
            .list_state(ROUTE_POLICY_RULE_NS)
            .map_err(|e| format!("revalidate active route policy rules: {e}"))?;
        let current_source_revision = route_policy_source_revision(&current_source_rows);
        if current_source_revision != source_revision {
            return Err(format!(
                "source_state_drift: active {ROUTE_POLICY_RULE_NS} changed while route policy proposals were generated; no proposals were persisted, regenerate from the current source revision"
            ));
        }
        for proposal in proposals {
            // The dispatch layer supplies a legacy deterministic id for
            // display/history. Persist the current kinds under a v3
            // content-addressed id so a change to apply payload, evidence,
            // policy version, or target starts a fresh pending row.
            let legacy_id = proposal
                .get("legacy_proposal_id")
                .and_then(Value::as_str)
                .or_else(|| proposal.get("proposal_id").and_then(Value::as_str))
                .ok_or_else(|| "dispatch policy proposal missing id".to_string())?
                .to_string();
            let kind = proposal.get("kind").and_then(Value::as_str);
            let identity_payload = match kind {
                Some("route_policy") => match proposal.get("identity_payload") {
                    Some(value) => value.clone(),
                    // Build the identity payload from the apply payload +
                    // evidence for any proposal shape that did not pre-bind
                    // one (defensive: should not happen after the dispatch
                    // layer has minted the v3 payload).
                    None => {
                        let apply_payload = proposal.get("policy_rule").cloned().unwrap_or(json!({}));
                        let evidence_review = proposal.get("evidence").cloned().unwrap_or(json!({}));
                        route_policy_v3_identity_payload(
                            &apply_payload,
                            &evidence_review,
                            ROUTE_POLICY_PROPOSAL_POLICY_VERSION,
                            ROUTE_POLICY_PROPOSAL_TARGET,
                            &source_revision,
                        )
                    }
                },
                Some("loadout_evolution") => proposal
                    .get("identity_payload")
                    .cloned()
                    .ok_or_else(|| {
                        "loadout_evolution proposal missing v3 identity payload".to_string()
                    })?,
                _ => Value::Null,
            };
            let content_digest = content_digest_hex(&identity_payload);
            let id = match kind {
                Some("route_policy") => route_policy_v3_proposal_id(&identity_payload),
                Some("loadout_evolution") => loadout_evolution_v3_proposal_id(&identity_payload),
                _ => legacy_id.clone(),
            };

            let mut next = proposal.clone();
            next["proposal_id"] = json!(id);
            next["legacy_proposal_id"] = json!(legacy_id);
            if matches!(kind, Some("route_policy" | "loadout_evolution")) {
                next["content_digest"] = json!(content_digest);
            }

            if let Some((existing, _version)) = store
                .get_state_kv(DISPATCH_POLICY_PROPOSAL_NS, &id)
                .map_err(|e| format!("load route policy proposal: {e}"))?
            {
                if let Ok(existing_json) = serde_json::from_str::<Value>(&existing) {
                    // Preserve a prior review/apply decision only when the
                    // persisted row carries the same digest as the regenerated
                    // content-addressed proposal. The comparison remains a
                    // defense-in-depth guard against a hand-edited collision.
                    let same_digest = match (
                        existing_json.get("content_digest").and_then(Value::as_str),
                        next.get("content_digest").and_then(Value::as_str),
                    ) {
                        (Some(a), Some(b)) => a == b,
                        _ => true,
                    };
                    if same_digest {
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
                    }
                }
            }
            let raw = serde_json::to_string(&next)
                .map_err(|e| format!("serialize route policy proposal: {e}"))?;
            store
                .set_state(DISPATCH_POLICY_PROPOSAL_NS, &id, &raw)
                .map_err(|e| format!("persist route policy proposal: {e}"))?;
        }
        Ok(())
    })?;

    let desired = status_filter
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "all")
        .map(|value| value.to_ascii_lowercase());
    let mut records = server.with_global_store_read(|store| {
        store
            .list_state(DISPATCH_POLICY_PROPOSAL_NS)
            .map_err(|e| format!("list route policy proposals: {e}"))
    })?;
    records.truncate(limit.max(1).min(100));
    let mut out = Vec::new();
    for row in records {
        let mut value: Value = serde_json::from_str(&row.value_json)
            .unwrap_or_else(|_| json!({ "proposal_id": row.key, "raw": row.value_json }));
        value["state_version"] = json!(row.version);
        value["updated_at"] = json!(row.updated_at);
        // Legacy current-kind proposals remain listable but their review/apply
        // paths refuse loudly; surface the marker so a caller can see why
        // before they hit the refusal.
        if (value.get("kind").and_then(Value::as_str) == Some("loadout_evolution")
            && !is_v3_loadout_evolution_proposal(&value))
            || (value.get("kind").and_then(Value::as_str) != Some("loadout_evolution")
                && !is_v3_proposal(&value))
        {
            value["legacy_unbound_proposal"] = json!(true);
        }
        let status = value
            .get("status")
            .and_then(|status| status.as_str())
            .unwrap_or("pending");
        if desired.as_deref().is_some_and(|wanted| wanted != status) {
            continue;
        }
        out.push(value);
    }

    serde_json::to_string(&json!({
        "action": "proposals",
        "kind": "dispatch_policy",
        "proposal_kinds": ["route_policy", "loadout_evolution"],
        "read_only": false,
        "requires_human_approval": true,
        "generated_from": {
            "source": "live_memory_eval",
            "row_count": rows.len(),
            "matrix_rows": performance_matrix.len(),
            "limit": limit.max(1),
        },
        "count": out.len(),
        "proposals": out,
        "next_actions": [
            "tachi_task(action='review_proposal', proposal_id=..., review_status='approved')",
            "tachi_task(action='apply_proposals', proposal_id=..., confirm=true) for approved route_policy rules",
            "tachi_task(action='apply_proposals', proposal_id=..., confirm=true) for approved loadout_evolution proposals to project reviewed profile/card loadout overlays"
        ],
    }))
    .map_err(|e| format!("serialize route policy proposals: {e}"))
}

pub(crate) fn handle_route_policy_review(
    server: &MemoryServer,
    proposal_id: &str,
    review_status: &str,
    note: Option<&str>,
) -> Result<String, String> {
    let proposal_id = proposal_id.trim();
    if proposal_id.is_empty() {
        return Err("proposal_id is required when action='review_proposal'".to_string());
    }
    let status = match review_status.trim().to_ascii_lowercase().as_str() {
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
        // BEGIN IMMEDIATE takes SQLite's write reservation before any review
        // input is read. Proposal load/version, active-rule census, source
        // validation, pending-state check, and the review CAS all use this
        // same connection and transaction, so no external route writer can
        // land after validation but before the lifecycle transition.
        let tx = store
            .connection_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| format!("open route policy review tx: {e}"))?;
        let (raw, version) = memcore::db::get_state(
            &tx,
            DISPATCH_POLICY_PROPOSAL_NS,
            proposal_id,
        )
            .map_err(|e| format!("load route policy proposal: {e}"))?
            .ok_or_else(|| format!("route policy proposal not found: {proposal_id}"))?;
        let mut value: Value =
            serde_json::from_str(&raw).map_err(|e| format!("parse route policy proposal: {e}"))?;
        // Legacy route_policy proposals (pre-v3 schema) carry no content-
        // addressed binding between what the human reviewed and what apply
        // will persist, so an old approval cannot be trusted to cover the
        // current payload. Refuse loudly rather than silently inheriting that
        // approval. Each current proposal kind validates its own bound shape
        // before a reviewer can decide it.
        let kind = value
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("route_policy");
        if kind == "route_policy" && !is_v3_proposal(&value) {
            return Err(format!(
                "legacy_unbound_proposal: {proposal_id} predates the v3 content-addressed identity and cannot be reviewed; regenerate with action='proposals' to mint a fresh pending v3 proposal"
            ));
        }
        if kind == "loadout_evolution" && !is_v3_loadout_evolution_proposal(&value) {
            return Err(format!(
                "legacy_unbound_proposal: loadout_evolution proposal {proposal_id} predates the v3 content-addressed identity and cannot be reviewed; regenerate with action='proposals' to mint a fresh pending v3 proposal"
            ));
        }
        // Re-validate the persisted content_digest against the identity_payload
        // still in the row *before* recording a review decision. Without this,
        // a proposal that drifted from what was generated (a hand-edit, a
        // partial write, a regeneration collision) could be approved at review
        // time and only get caught at apply — this closes that gap so
        // propose/review/apply drift is refused at the earliest point it can
        // be detected, not just the last one. Mirrors the same check apply.rs
        // runs immediately before mutating routing state.
        if kind == "route_policy" {
            let source_rows = memcore::db::list_state(&tx, ROUTE_POLICY_RULE_NS)
                .map_err(|e| format!("list active route policy rules for review: {e}"))?;
            let live_source_revision = route_policy_source_revision(&source_rows);
            validate_route_policy_proposal(proposal_id, &value, Some(&live_source_revision))?;
        } else if kind == "loadout_evolution" {
            validate_loadout_evolution_proposal(proposal_id, &value)?;
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
                "route policy proposal {proposal_id} is in terminal state '{current_status}'; only pending proposals can be reviewed"
            ));
        }
        value["status"] = json!(status);
        value["review"] = json!({
            "status": status,
            "note": note,
            "reviewed_at": reviewed_at,
        });
        let next = serde_json::to_string(&value)
            .map_err(|e| format!("serialize route policy review: {e}"))?;
        // hard_state version CAS: if another reviewer (or a regeneration that
        // happened to land on the same content-addressed id) raced us and the
        // row's version moved, refuse to overwrite — the caller must reload
        // and re-decide against the current state.
        let updated = memcore::db::set_state_if_version(
            &tx,
            DISPATCH_POLICY_PROPOSAL_NS,
            proposal_id,
            &next,
            version,
        )
        .map_err(|e| format!("persist route policy review: {e}"))?;
        if !updated {
            return Err(format!(
                "stale_state_version: route policy proposal {proposal_id} changed before review; reload and retry"
            ));
        }
        tx.commit()
            .map_err(|e| format!("commit route policy review tx: {e}"))?;
        Ok(value)
    })?;

    serde_json::to_string(&json!({
        "action": "review_proposal",
        "proposal_id": proposal_id,
        "proposal": updated,
    }))
    .map_err(|e| format!("serialize route policy review response: {e}"))
}

fn load_live_eval_entries(
    server: &MemoryServer,
    limit: usize,
) -> Result<Vec<memcore::MemoryEntry>, String> {
    let limit = limit.max(1);
    let mut entries = server.with_global_store_read(|store| {
        store
            .list_by_path("/eval", limit, false)
            .map_err(|e| format!("list global eval entries: {e}"))
    })?;
    if server.has_project_db() {
        let mut project_entries = server.with_project_store_read(|store| {
            store
                .list_by_path("/eval", limit, false)
                .map_err(|e| format!("list project eval entries: {e}"))
        })?;
        entries.append(&mut project_entries);
    }
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    entries.truncate(limit);
    Ok(entries)
}

fn profile_card_risk_inputs(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<ProfileCardRiskInputs, String> {
    Ok(ProfileCardRiskInputs {
        existing_weak_against: profile_weak_against_for_server(server, profile)?
            .into_iter()
            .collect(),
        existing_demotion_targets: profile_demotion_targets(server, profile)?
            .into_iter()
            .collect(),
        profile_required_skills: profile_required_skill_ids_for_server(server, profile)?,
    })
}

fn profile_positive_evolution_inputs(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<ProfilePositiveEvolutionInputs, String> {
    Ok(ProfilePositiveEvolutionInputs {
        profile_required_skills: profile_required_skill_ids_for_server(server, profile)?,
        existing_passive_traits: profile_skill_loadout_json_for_server(server, profile)
            .map(|loadout| {
                loadout
                    .get("passive_traits")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            })?
            .into_iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect(),
        existing_evidence_required: profile_evidence_required_for_server(server, profile)?
            .into_iter()
            .collect(),
    })
}
