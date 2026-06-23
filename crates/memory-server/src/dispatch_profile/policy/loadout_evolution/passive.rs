use super::*;

pub(super) fn build_passive_trait_evolution_proposals(
    profile: &DispatchProfileDef,
    profile_rows: &[AgentPerformanceMatrixRow],
    profile_samples: u32,
    existing_passive_traits: &HashSet<String>,
    limit: usize,
    min_task_hits: u32,
) -> Vec<Value> {
    let mut out = Vec::new();
    let mut proposed_traits = HashSet::new();
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
        let Some((trait_id, trait_label)) = passive_trait_for_task_type(&row.task_type) else {
            continue;
        };
        if existing_passive_traits.contains(trait_id) {
            continue;
        }
        if !proposed_traits.insert(trait_id.to_string()) {
            continue;
        }
        let id = format!(
            "loadout_evolution:{}:add_passive_trait:{}",
            sanitize_policy_key(profile.name),
            sanitize_policy_key(trait_id)
        );
        out.push(json!({
            "proposal_id": id,
            "kind": "loadout_evolution",
            "status": "pending",
            "requires_human_approval": true,
            "created_or_refreshed_at": Utc::now().to_rfc3339(),
            "profile": profile.name,
            "operation": "add_evidence_backed_passive_trait",
            "trait_id": trait_id,
            "trait_label": trait_label,
            "current_loadout": profile_skill_loadout_json(profile),
            "proposed_patch": {
                "add_passive_traits": [trait_id],
                "preserve_signature_skills": profile.signature_skills,
                "preserve_forbidden_skills": profile.forbidden_skills,
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
                "{} has {} clean verified {} samples; add passive trait {}",
                profile.name, row.samples, row.task_type, trait_id
            ),
            "projection": {
                "status": "pending_profile_card_projection",
                "note": "Human approval records the proposal; apply_proposals projects approved passive traits into the profile/card overlay."
            }
        }));
    }
    out
}
