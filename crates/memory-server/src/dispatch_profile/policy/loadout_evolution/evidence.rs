use super::*;

pub(super) fn build_evidence_contract_evolution_proposals(
    profile: &DispatchProfileDef,
    profile_rows: &[AgentPerformanceMatrixRow],
    profile_samples: u32,
    existing_evidence_required: &HashSet<String>,
    limit: usize,
    min_task_hits: u32,
) -> Vec<Value> {
    let mut out = Vec::new();
    let mut proposed_evidence = HashSet::new();
    for row in profile_rows {
        if row.samples < min_task_hits {
            continue;
        }
        if row.failure_count > 0
            || row.verification_rate < 0.50
            || row.human_override_rate >= 0.10
            || row.avg_retry_count >= 1.0
            || row.success_rate.or(row.useful_rate).unwrap_or(0.0) < 0.80
        {
            continue;
        }
        let Some((evidence_id, evidence_label)) =
            evidence_contract_target_for_task_type(&row.task_type)
        else {
            continue;
        };
        if existing_evidence_required.contains(evidence_id) {
            continue;
        }
        if !proposed_evidence.insert(evidence_id.to_string()) {
            continue;
        }
        let id = format!(
            "loadout_evolution:{}:add_evidence_required:{}",
            sanitize_policy_key(profile.name),
            sanitize_policy_key(evidence_id)
        );
        out.push(json!({
            "proposal_id": id,
            "kind": "loadout_evolution",
            "status": "pending",
            "requires_human_approval": true,
            "created_or_refreshed_at": Utc::now().to_rfc3339(),
            "profile": profile.name,
            "operation": "add_evidence_contract_required",
            "evidence_id": evidence_id,
            "evidence_label": evidence_label,
            "current_evidence_contract": profile_evidence_contract_json(profile),
            "proposed_patch": {
                "add_evidence_required": [evidence_id],
                "preserve_baseline_required": profile.evidence_required,
            },
            "evidence": {
                "source": "live_memory_eval",
                "limit": limit,
                "profile_samples": profile_samples,
                "min_samples_for_evolution": MIN_LOADOUT_EVOLUTION_SAMPLES,
                "min_task_hits": min_task_hits,
                "task_type": row.task_type,
                "task_samples": row.samples,
                "verification_rate": round2(row.verification_rate),
                "success_rate": row.success_rate.map(round2),
                "useful_rate": row.useful_rate.map(round2),
                "avg_retry_count": round2(row.avg_retry_count),
                "human_override_rate": round2(row.human_override_rate),
                "profile_summary": summarize_matrix_rows(profile_rows),
                "loadout_call": "tachi_skill(action='loadout', profile=..., limit=...)",
            },
            "rationale": format!(
                "{} has {} clean verified {} samples; require evidence artifact {}",
                profile.name, row.samples, row.task_type, evidence_id
            ),
            "projection": {
                "status": "pending_profile_card_projection",
                "note": "Human approval records the proposal; apply_proposals projects approved evidence requirements into the profile/card overlay."
            }
        }));
    }
    out
}
