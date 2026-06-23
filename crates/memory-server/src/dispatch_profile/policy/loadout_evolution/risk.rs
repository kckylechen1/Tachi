use super::*;

pub(super) fn build_card_risk_evolution_proposals(
    profile: &DispatchProfileDef,
    profile_rows: &[AgentPerformanceMatrixRow],
    entries: &[memory_core::MemoryEntry],
    profile_samples: u32,
    existing_weak_against: &HashSet<String>,
    existing_demotion_targets: &HashSet<String>,
    profile_required_skills: &[String],
    limit: usize,
) -> Vec<Value> {
    let mut out = Vec::new();
    let mut proposed_weaknesses = HashSet::new();
    let mut bad_task_types = HashSet::new();
    for row in profile_rows {
        if row.samples < MIN_CARD_RISK_EVOLUTION_SAMPLES {
            continue;
        }
        let risk_signal =
            row.failure_count >= 2 || row.human_override_rate >= 0.25 || row.avg_retry_count >= 1.5;
        if !risk_signal {
            continue;
        }
        bad_task_types.insert(row.task_type.clone());
        let weakness_id = row.task_type.clone();
        if !existing_weak_against.contains(&weakness_id)
            && proposed_weaknesses.insert(weakness_id.clone())
        {
            let id = format!(
                "loadout_evolution:{}:add_card_weakness:{}",
                sanitize_policy_key(profile.name),
                sanitize_policy_key(&weakness_id)
            );
            out.push(json!({
                "proposal_id": id,
                "kind": "loadout_evolution",
                "status": "pending",
                "requires_human_approval": true,
                "created_or_refreshed_at": Utc::now().to_rfc3339(),
                "profile": profile.name,
                "operation": "add_card_weakness",
                "weakness_id": weakness_id,
                "weakness_label": format!("Repeated friction on {}", row.task_type),
                "current_card": profile_json(profile).get("mbit_card").cloned().unwrap_or(Value::Null),
                "proposed_patch": {
                    "add_weak_against": [row.task_type],
                    "preserve_baseline_weak_against": profile.weak_against,
                },
                "evidence": {
                    "source": "live_memory_eval",
                    "limit": limit,
                    "profile_samples": profile_samples,
                    "min_samples_for_card_risk_evolution": MIN_CARD_RISK_EVOLUTION_SAMPLES,
                    "task_type": row.task_type,
                    "task_samples": row.samples,
                    "failure_count": row.failure_count,
                    "human_override_rate": round2(row.human_override_rate),
                    "avg_retry_count": round2(row.avg_retry_count),
                    "profile_summary": summarize_matrix_rows(profile_rows),
                },
                "rationale": format!(
                    "{} has repeated friction on {}; add it to weak_against so recommendation can explain/deprioritize the match",
                    profile.name, row.task_type
                ),
                "projection": {
                    "status": "pending_profile_card_projection",
                    "note": "Human approval records the proposal; apply_proposals projects approved weakness markers into the MBIT/profile card overlay."
                }
            }));
        }
    }

    if bad_task_types.is_empty() {
        return out;
    }

    let current_skills = profile_required_skills.iter().collect::<HashSet<_>>();
    let mut skill_hits: HashMap<String, u32> = HashMap::new();
    for entry in entries {
        let Some(meta) = entry.metadata.as_object() else {
            continue;
        };
        if meta.get("profile").and_then(Value::as_str) != Some(profile.name) {
            continue;
        }
        let task_type = meta
            .get("task_type")
            .and_then(Value::as_str)
            .unwrap_or("other");
        if !bad_task_types.contains(task_type) {
            continue;
        }
        let outcome = meta
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let risky = matches!(outcome.as_str(), "failure" | "failed" | "partial")
            || meta
                .get("human_override")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || meta.get("retry_count").and_then(Value::as_u64).unwrap_or(0) >= 2;
        if !risky {
            continue;
        }
        let skills = meta
            .get("skills_used")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|skill| !skill.is_empty())
            .filter(|skill| current_skills.contains(&skill.to_string()))
            .map(str::to_string)
            .collect::<Vec<_>>();
        for skill in skills {
            *skill_hits.entry(skill).or_insert(0) += 1;
        }
    }

    let min_skill_hits = MIN_CARD_RISK_EVOLUTION_SAMPLES;
    for (skill, hits) in skill_hits {
        if hits < min_skill_hits || existing_demotion_targets.contains(&skill) {
            continue;
        }
        let id = format!(
            "loadout_evolution:{}:demote_skill:{}",
            sanitize_policy_key(profile.name),
            sanitize_policy_key(&skill)
        );
        out.push(json!({
            "proposal_id": id,
            "kind": "loadout_evolution",
            "status": "pending",
            "requires_human_approval": true,
            "created_or_refreshed_at": Utc::now().to_rfc3339(),
            "profile": profile.name,
            "operation": "mark_skill_demotion_target",
            "skill_id": skill,
            "current_loadout": profile_skill_loadout_json(profile),
            "proposed_patch": {
                "demotion_targets": [skill],
                "preserve_signature_skills": profile.signature_skills,
            },
            "evidence": {
                "source": "live_memory_eval",
                "limit": limit,
                "profile_samples": profile_samples,
                "min_samples_for_card_risk_evolution": MIN_CARD_RISK_EVOLUTION_SAMPLES,
                "skill_hits": hits,
                "bad_task_types": bad_task_types,
                "profile_summary": summarize_matrix_rows(profile_rows),
            },
            "rationale": format!(
                "{} repeatedly appeared in failed/overridden/retried {} runs; mark as a demotion target for human review",
                skill, profile.name
            ),
            "projection": {
                "status": "pending_profile_card_projection",
                "note": "Human approval records the proposal; apply_proposals projects approved demotion targets into the MBIT/profile card overlay without mutating baseline skills."
            }
        }));
    }

    out
}
