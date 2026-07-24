use super::super::*;
use super::simulation::{route_simulation_caveats, simulate_route_policy};
use sha2::{Digest, Sha256};
use tachi_dispatch::policy::{
    build_loadout_evolution_proposals, build_route_policy_proposals,
    route_policy_v2_identity_payload, LoadoutEvalEntry, ProfileCardRiskInputs,
    ProfilePositiveEvolutionInputs, ROUTE_POLICY_PROPOSAL_POLICY_VERSION,
    ROUTE_POLICY_PROPOSAL_TARGET,
};

/// SHA-256 hex of the canonical identity payload. Used as the content-addressed
/// half of the v2 proposal id and stored verbatim as `content_digest` so apply
/// can re-validate the persisted row was not mutated after review.
pub(super) fn content_digest_hex(identity_payload: &Value) -> String {
    // `serde_json` does not guarantee key order across rebuilds; serialize the
    // value through `route_policy_v2_identity_payload`'s canonical form first
    // so the hash is stable regardless of how the caller assembled the input.
    let canonical = identity_payload.to_string();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let bytes = hasher.finalize();
    let mut out = String::with_capacity(2 * bytes.len());
    for byte in bytes {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

/// v2 proposal id: human-readable kind prefix + the content digest. The digest
/// is the SHA-256 of the canonical identity payload, so any change to the
/// apply payload, evidence, policy version, or target rotates the id and
/// forces a fresh pending row instead of inheriting an old approval.
fn route_policy_v2_proposal_id(identity_payload: &Value) -> String {
    let digest = content_digest_hex(identity_payload);
    let short = &digest[..16.min(digest.len())];
    format!("route_policy:v2:{short}")
}

/// `true` iff the persisted row carries the additive v2 schema marker
/// (`schema_version: 2`). Legacy rows predate the content-addressed identity
/// and are refused at review/apply as `legacy_unbound_proposal`.
fn is_v2_proposal(value: &Value) -> bool {
    value
        .get("schema_version")
        .and_then(Value::as_u64)
        .map(|version| version >= 2)
        .unwrap_or(false)
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
    let rows = load_live_eval_rows(server, limit.max(1))?;
    let performance_matrix = aggregate_performance_matrix(&rows);
    let current = simulate_route_policy("current", &performance_matrix, None);
    let variants = ["cost_sensitive", "quality_first"]
        .iter()
        .map(|policy| simulate_route_policy(policy, &performance_matrix, None))
        .collect::<Vec<_>>();
    let generated_at = Utc::now().to_rfc3339();
    let mut proposals =
        build_route_policy_proposals(&current, &variants, rows.len(), limit.max(1), &generated_at);
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
        for proposal in proposals {
            // The dispatch layer emits the legacy deterministic id
            // (`route_policy:<policy>:<task>:<profile>`); we replace it with
            // the v2 content-addressed id derived from the canonical identity
            // payload. Two regenerations of the same content hash to the same
            // id and thus preserve any prior approval; any change to apply
            // payload / evidence / policy version / target rotates the id and
            // starts a fresh pending row.
            let legacy_id = proposal
                .get("legacy_proposal_id")
                .and_then(Value::as_str)
                .or_else(|| proposal.get("proposal_id").and_then(Value::as_str))
                .ok_or_else(|| "route policy proposal missing id".to_string())?
                .to_string();
            let identity_payload = match proposal.get("identity_payload") {
                Some(value) => value.clone(),
                None => {
                    // Build the identity payload from the apply payload +
                    // evidence for any proposal shape that did not pre-bind
                    // one (defensive: should not happen for route_policy kind
                    // after the dispatch-layer change, but loadout_evolution
                    // proposals do not yet carry identity_payload in this PR).
                    let apply_payload = proposal.get("policy_rule").cloned().unwrap_or(json!({}));
                    let evidence_review = proposal.get("evidence").cloned().unwrap_or(json!({}));
                    route_policy_v2_identity_payload(
                        &apply_payload,
                        &evidence_review,
                        ROUTE_POLICY_PROPOSAL_POLICY_VERSION,
                        ROUTE_POLICY_PROPOSAL_TARGET,
                    )
                }
            };
            let content_digest = content_digest_hex(&identity_payload);
            let id = match proposal.get("kind").and_then(Value::as_str) {
                Some("route_policy") => route_policy_v2_proposal_id(&identity_payload),
                // loadout_evolution proposals are out of scope for v2 in this
                // PR; keep their legacy id so existing tests still match.
                _ => legacy_id.clone(),
            };

            let mut next = proposal.clone();
            next["proposal_id"] = json!(id);
            next["legacy_proposal_id"] = json!(legacy_id);
            if matches!(
                proposal.get("kind").and_then(Value::as_str),
                Some("route_policy")
            ) {
                next["content_digest"] = json!(content_digest);
            }

            if let Some((existing, _version)) = store
                .get_state_kv(DISPATCH_POLICY_PROPOSAL_NS, &id)
                .map_err(|e| format!("load route policy proposal: {e}"))?
            {
                if let Ok(existing_json) = serde_json::from_str::<Value>(&existing) {
                    // Preserve a prior review/apply decision ONLY when the
                    // persisted row carries the SAME content digest as the
                    // freshly regenerated proposal. For v2 route_policy rows
                    // the id already only collides with itself when content
                    // is identical, so this is a belt-and-braces guard against
                    // any path that writes the same id with different content
                    // (an in-flight schema change, a hand-edited row). For
                    // loadout_evolution rows (still on legacy ids in this PR)
                    // the digest check is skipped — their behavior is
                    // unchanged.
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
        // Legacy proposals (those that predate the v2 schema) remain listable
        // but their review/apply paths refuse loudly; surface the marker here
        // so a caller can see *why* before they hit the refusal.
        if !is_v2_proposal(&value) {
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
        let (raw, version) = store
            .get_state_kv(DISPATCH_POLICY_PROPOSAL_NS, proposal_id)
            .map_err(|e| format!("load route policy proposal: {e}"))?
            .ok_or_else(|| format!("route policy proposal not found: {proposal_id}"))?;
        let mut value: Value =
            serde_json::from_str(&raw).map_err(|e| format!("parse route policy proposal: {e}"))?;
        // Legacy route_policy proposals (pre-v2 schema) carry no content-
        // addressed binding between what the human reviewed and what apply
        // will persist, so an old approval cannot be trusted to cover the
        // current payload. Refuse loudly rather than silently inheriting that
        // approval. Loadout-evolution proposals are still on legacy ids in
        // this PR and stay reviewable.
        let kind = value
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("route_policy");
        if kind == "route_policy" && !is_v2_proposal(&value) {
            return Err(format!(
                "legacy_unbound_proposal: {proposal_id} predates the v2 content-addressed identity and cannot be reviewed; regenerate with action='proposals' to mint a fresh pending v2 proposal"
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
            let stored_digest = value
                .get("content_digest")
                .and_then(Value::as_str)
                .unwrap_or("");
            let identity_payload = value.get("identity_payload").cloned().unwrap_or(json!({}));
            let recomputed = content_digest_hex(&identity_payload);
            if stored_digest.is_empty() || recomputed != stored_digest {
                return Err(format!(
                    "content_digest_mismatch: route policy proposal {proposal_id} stored digest {stored_digest:?} does not match recomputed {recomputed}; refusing to review a proposal that drifted from what was generated"
                ));
            }
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
        let updated =
            store
                .set_state_if_version(DISPATCH_POLICY_PROPOSAL_NS, proposal_id, &next, version)
                .map_err(|e| format!("persist route policy review: {e}"))?;
        if !updated {
            return Err(format!(
                "stale_state_version: route policy proposal {proposal_id} changed before review; reload and retry"
            ));
        }
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
