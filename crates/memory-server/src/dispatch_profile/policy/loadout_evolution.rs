use super::super::*;
use super::simulation::sanitize_policy_key;

mod entries;
mod evidence;
mod passive;
mod risk;
mod targets;

use entries::load_live_eval_entries;
use evidence::build_evidence_contract_evolution_proposals;
use passive::build_passive_trait_evolution_proposals;
use risk::build_card_risk_evolution_proposals;
use targets::{evidence_contract_target_for_task_type, passive_trait_for_task_type};

#[derive(Default)]
pub(super) struct LoadoutSkillEvidence {
    hits: u32,
    verified: u32,
    success: u32,
    quality_sum: f64,
    quality_count: u32,
    task_types: HashMap<String, u32>,
    eval_refs: Vec<String>,
}

pub(super) fn build_loadout_evolution_proposals(
    server: &MemoryServer,
    performance_matrix: &[AgentPerformanceMatrixRow],
    limit: usize,
) -> Result<Vec<Value>, String> {
    let entries = load_live_eval_entries(server, limit)?;
    let mut out = Vec::new();

    for profile in DISPATCH_PROFILES {
        let profile_rows = performance_matrix
            .iter()
            .filter(|row| row.profile.as_deref() == Some(profile.name))
            .cloned()
            .collect::<Vec<_>>();
        let profile_samples = sum_matrix_samples(&profile_rows);
        if profile_samples < MIN_LOADOUT_EVOLUTION_SAMPLES {
            out.extend(build_card_risk_evolution_proposals(
                profile,
                &profile_rows,
                &entries,
                profile_samples,
                &profile_weak_against_for_server(server, profile)?
                    .into_iter()
                    .collect::<HashSet<_>>(),
                &profile_demotion_targets(server, profile)?
                    .into_iter()
                    .collect::<HashSet<_>>(),
                &profile_required_skill_ids_for_server(server, profile)?,
                limit,
            ));
            continue;
        }
        let failure_count = sum_matrix_failures(&profile_rows);
        let human_override_rate =
            weighted_matrix_rate(&profile_rows, |row| Some(row.human_override_rate)).unwrap_or(0.0);
        let avg_retry_count =
            weighted_matrix_rate(&profile_rows, |row| Some(row.avg_retry_count)).unwrap_or(0.0);
        let success_rate =
            weighted_matrix_rate(&profile_rows, |row| row.success_rate).unwrap_or(0.0);
        let useful_rate = weighted_matrix_rate(&profile_rows, |row| row.useful_rate).unwrap_or(0.0);
        let positive_rate = success_rate.max(useful_rate);

        let existing_weak_against = profile_weak_against_for_server(server, profile)?
            .into_iter()
            .collect::<HashSet<_>>();
        let existing_demotion_targets = profile_demotion_targets(server, profile)?
            .into_iter()
            .collect::<HashSet<_>>();
        let profile_required_skills = profile_required_skill_ids_for_server(server, profile)?;
        out.extend(build_card_risk_evolution_proposals(
            profile,
            &profile_rows,
            &entries,
            profile_samples,
            &existing_weak_against,
            &existing_demotion_targets,
            &profile_required_skills,
            limit,
        ));

        if failure_count > 0
            || human_override_rate >= 0.10
            || avg_retry_count >= 1.0
            || positive_rate < 0.80
        {
            continue;
        }

        let existing_skills = profile_required_skill_ids_for_server(server, profile)?
            .into_iter()
            .chain(
                profile
                    .forbidden_skills
                    .iter()
                    .map(|skill| skill.to_string()),
            )
            .collect::<HashSet<_>>();
        let existing_passive_traits = {
            let loadout = profile_skill_loadout_json_for_server(server, profile)?;
            loadout
                .get("passive_traits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<HashSet<_>>()
        };
        let existing_evidence_required = profile_evidence_required_for_server(server, profile)?
            .into_iter()
            .collect::<HashSet<_>>();
        let mut buckets: HashMap<String, LoadoutSkillEvidence> = HashMap::new();
        for entry in &entries {
            let Some(meta) = entry.metadata.as_object() else {
                continue;
            };
            if meta.get("profile").and_then(Value::as_str) != Some(profile.name) {
                continue;
            }
            let task_type = meta
                .get("task_type")
                .and_then(Value::as_str)
                .unwrap_or("other")
                .to_string();
            let outcome = meta
                .get("outcome")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let verified = meta
                .get("verification_present")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let quality = meta.get("quality_score").and_then(Value::as_f64);
            let skills = meta
                .get("skills_used")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|skill| !skill.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();

            for skill in skills {
                if existing_skills.contains(&skill) {
                    continue;
                }
                let evidence = buckets.entry(skill).or_default();
                evidence.hits += 1;
                if verified {
                    evidence.verified += 1;
                }
                if matches!(outcome.as_str(), "success" | "completed") {
                    evidence.success += 1;
                }
                if let Some(quality) = quality {
                    evidence.quality_sum += quality;
                    evidence.quality_count += 1;
                }
                *evidence.task_types.entry(task_type.clone()).or_insert(0) += 1;
                if evidence.eval_refs.len() < 5 {
                    evidence.eval_refs.push(entry.path.clone());
                }
            }
        }

        let min_skill_hits = (profile_samples / 2).max(3);
        for proposal in build_passive_trait_evolution_proposals(
            profile,
            &profile_rows,
            profile_samples,
            &existing_passive_traits,
            limit,
            min_skill_hits,
        ) {
            out.push(proposal);
        }
        for proposal in build_evidence_contract_evolution_proposals(
            profile,
            &profile_rows,
            profile_samples,
            &existing_evidence_required,
            limit,
            min_skill_hits,
        ) {
            out.push(proposal);
        }
        for (skill, evidence) in buckets {
            if evidence.hits < min_skill_hits {
                continue;
            }
            let verified_rate = evidence.verified as f64 / evidence.hits as f64;
            let success_rate = evidence.success as f64 / evidence.hits as f64;
            if verified_rate < 0.50 || success_rate < 0.80 {
                continue;
            }
            let avg_quality = (evidence.quality_count > 0)
                .then(|| evidence.quality_sum / evidence.quality_count as f64);
            let id = format!(
                "loadout_evolution:{}:promote_signature:{}",
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
                "operation": "promote_observed_skill_to_signature",
                "skill_id": skill.clone(),
                "current_loadout": profile_skill_loadout_json(profile),
                "proposed_patch": {
                    "add_signature_skills": [skill.clone()],
                    "preserve_common_skills": profile.common_skills,
                    "preserve_forbidden_skills": profile.forbidden_skills,
                },
                "evidence": {
                    "source": "live_memory_eval",
                    "limit": limit,
                    "profile_samples": profile_samples,
                    "min_samples_for_evolution": MIN_LOADOUT_EVOLUTION_SAMPLES,
                    "min_skill_hits": min_skill_hits,
                    "skill_hits": evidence.hits,
                    "verified_rate": round2(verified_rate),
                    "success_rate": round2(success_rate),
                    "avg_quality_score": avg_quality.map(round2),
                    "profile_summary": summarize_matrix_rows(&profile_rows),
                    "task_types": evidence.task_types,
                    "eval_refs": evidence.eval_refs,
                    "loadout_call": "tachi_skill(action='loadout', profile=..., limit=...)",
                },
                "rationale": format!(
                    "{} appeared in {}/{} verified successful {} runs and is not part of the current sparse loadout",
                    skill, evidence.hits, profile_samples, profile.name
                ),
                "projection": {
                    "status": "pending_profile_card_projection",
                    "note": "Human approval records the proposal; apply_proposals projects approved changes into the profile/card overlay."
                }
            }));
        }
    }

    Ok(out)
}
