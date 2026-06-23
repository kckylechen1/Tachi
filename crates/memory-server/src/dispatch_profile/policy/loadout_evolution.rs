use super::super::*;
use super::simulation::sanitize_policy_key;

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

pub(super) fn passive_trait_for_task_type(task_type: &str) -> Option<(&'static str, &'static str)> {
    match task_type {
        "plan_request" => Some((
            "evidence_backed_planning",
            "Repeated verified planning success; keep plan-first behavior prominent.",
        )),
        "review_request" => Some((
            "evidence_backed_review_gate",
            "Repeated verified review success; keep blocker/evidence review behavior prominent.",
        )),
        "fix_request" | "refactor_request" | "migration_request" => Some((
            "evidence_backed_change_control",
            "Repeated verified change work; keep bounded-diff and regression-control behavior prominent.",
        )),
        "test_request" => Some((
            "evidence_backed_verification",
            "Repeated verified test work; keep verification-first behavior prominent.",
        )),
        _ => None,
    }
}

pub(super) fn evidence_contract_target_for_task_type(
    task_type: &str,
) -> Option<(&'static str, &'static str)> {
    match task_type {
        "plan_request" => Some((
            "acceptance_criteria",
            "Repeated verified planning success; require explicit acceptance criteria in handoffs.",
        )),
        "review_request" => Some((
            "severity_rationale",
            "Repeated verified review success; require severity rationale with findings.",
        )),
        "fix_request" | "refactor_request" | "migration_request" => Some((
            "regression_tests",
            "Repeated verified change work; require regression-test evidence with diffs.",
        )),
        "test_request" => Some((
            "test_evidence",
            "Repeated verified test work; require concrete test evidence and gaps.",
        )),
        _ => None,
    }
}

pub(super) fn load_live_eval_entries(
    server: &MemoryServer,
    limit: usize,
) -> Result<Vec<memory_core::MemoryEntry>, String> {
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
