use super::*;

pub(crate) fn profile_json(profile: &DispatchProfileDef) -> Value {
    profile_json_with_loadout_and_evidence_contract(
        profile,
        profile_skill_loadout_json(profile),
        profile_evidence_contract_json(profile),
        profile_weak_against(profile),
        Vec::new(),
        Vec::new(),
    )
}

pub(crate) fn profile_json_for_server(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Value, String> {
    Ok(profile_json_with_loadout_and_evidence_contract(
        profile,
        profile_skill_loadout_json_for_server(server, profile)?,
        profile_evidence_contract_json_for_server(server, profile)?,
        profile_weak_against_for_server(server, profile)?,
        profile_projected_weak_against(server, profile)?,
        profile_demotion_targets(server, profile)?,
    ))
}

pub(super) fn profile_json_with_loadout_and_evidence_contract(
    profile: &DispatchProfileDef,
    skill_loadout: Value,
    evidence_contract: Value,
    weak_against: Vec<String>,
    projected_weak_against: Vec<String>,
    demotion_targets: Vec<String>,
) -> Value {
    let stats = profile_mbit_stats(profile);
    let authority = profile_card_authority_json(profile);
    let guidance = profile_card_guidance_json(&skill_loadout);
    let moves = profile_card_moves_json(&skill_loadout);
    let personality = profile_card_personality_json(&stats);
    let archetype = profile_card_archetype(profile);
    let card_projection = json!({
        "status": if projected_weak_against.is_empty() && demotion_targets.is_empty() {
            "baseline"
        } else {
            "applied_overlay"
        },
        "namespace": PROFILE_CARD_OVERLAY_NS,
        "key": profile.name,
    });
    json!({
        "name": profile.name,
        "display_name": profile.display_name,
        "backend": profile.backend,
        "role": profile.role,
        "stage": profile.stage,
        "card_archetype": archetype,
        "model": profile.model,
        "tool_profile": profile.tool_profile,
        "mcp_access": {
            "inject_tachi_mcp": profile.inject_tachi_mcp,
            "inject_hub_mcps": profile.inject_hub_mcps,
            "allowed_facades": profile.allowed_facades,
            "allowed_mcp_servers": profile.allowed_mcp_servers,
            "github_read": profile.github_read,
            "write_actions": profile.write_actions,
        },
        "credential_profiles": profile.credential_profiles,
        "skill_loadout": skill_loadout,
        "evidence_contract": evidence_contract,
        "weak_against": weak_against,
        "mbit_card": {
            "display_name": profile.display_name,
            "archetype": archetype,
            "type": [profile.role],
            "stats": stats,
            "authority": authority,
            "guidance": guidance,
            "moves": moves,
            "personality": personality,
            "strong_against": profile.strong_against,
            "weak_against": weak_against,
            "projected_weak_against": projected_weak_against,
            "demotion_targets": demotion_targets,
            "auto_capability_bundle": profile.auto_capability_bundle,
            "skill_loadout": skill_loadout,
            "evidence_contract": evidence_contract,
            "evolution": {
                "projection": card_projection,
            },
        }
    })
}

pub(super) fn profile_card_archetype(profile: &DispatchProfileDef) -> &'static str {
    let stage = profile.stage.unwrap_or_default();
    if profile.role == "explore" || stage == "explore" || stage == "probe" {
        "poke"
    } else if profile.role == "executor" || stage == "execute" || stage == "hotfix" {
        "scv"
    } else {
        "raven"
    }
}

pub(super) fn profile_mbit_stats(profile: &DispatchProfileDef) -> Value {
    let (precision, speed, cost, creativity, risk_control) = match profile.name {
        "claude_plan" => (86, 58, 65, 82, 88),
        "glm_51_impl" => (78, 76, 52, 70, 72),
        "opencode_builder" => (74, 82, 48, 68, 70),
        "codex_55_review" => (95, 55, 72, 60, 95),
        "codex_53_fast" => (72, 92, 35, 52, 58),
        "kimi_arch" => (88, 64, 58, 86, 84),
        "deepseek_explore" => (76, 88, 30, 72, 62),
        "kimi_ux" => (84, 70, 58, 88, 78),
        _ => (70, 70, 70, 70, 70),
    };
    json!({
        "precision": precision,
        "speed": speed,
        "cost": cost,
        "creativity": creativity,
        "risk_control": risk_control,
    })
}

pub(super) fn profile_card_authority_json(profile: &DispatchProfileDef) -> Value {
    json!({
        "write_code": profile.write_actions,
        "merge": false,
        "github_read": profile.github_read,
        "github_write": false,
        "can_dispatch_followup": false,
        "credential_profiles": profile.credential_profiles,
        "tool_profile": profile.tool_profile,
    })
}

pub(super) fn profile_card_guidance_json(skill_loadout: &Value) -> Value {
    json!({
        "superpowers": profile_card_skills_by_prefix(skill_loadout, "skill:superpowers-"),
    })
}

pub(super) fn profile_card_moves_json(skill_loadout: &Value) -> Value {
    let skills = profile_card_skill_ids_from_loadout(skill_loadout);
    let mut tachi_native = skills
        .iter()
        .filter(|skill| {
            skill.starts_with("skill:")
                && !skill.starts_with("skill:superpowers-")
                && !skill.starts_with("skill:waza-")
        })
        .cloned()
        .collect::<Vec<_>>();
    crate::skill_policy::dedupe_preserve_order(&mut tachi_native);
    json!({
        "waza": profile_card_skills_by_prefix(skill_loadout, "skill:waza-"),
        "external": [],
        "tachi_native": tachi_native,
    })
}

pub(super) fn profile_card_personality_json(stats: &Value) -> Value {
    json!({
        "curiosity": stats.get("creativity").and_then(Value::as_i64).unwrap_or(70),
        "caution": stats.get("risk_control").and_then(Value::as_i64).unwrap_or(70),
        "speed": stats.get("speed").and_then(Value::as_i64).unwrap_or(70),
        "risk_control": stats.get("risk_control").and_then(Value::as_i64).unwrap_or(70),
    })
}

pub(super) fn profile_card_skills_by_prefix(skill_loadout: &Value, prefix: &str) -> Vec<String> {
    let mut skills = profile_card_skill_ids_from_loadout(skill_loadout)
        .into_iter()
        .filter(|skill| skill.starts_with(prefix))
        .collect::<Vec<_>>();
    crate::skill_policy::dedupe_preserve_order(&mut skills);
    skills
}

pub(super) fn profile_card_skill_ids_from_loadout(skill_loadout: &Value) -> Vec<String> {
    let mut skills = Vec::new();
    for key in [
        "common_skills",
        "signature_skills",
        "projected_signature_skills",
    ] {
        let Some(items) = skill_loadout.get(key).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            if let Some(skill) = item.as_str() {
                skills.push(skill.to_string());
            }
        }
    }
    crate::skill_policy::dedupe_preserve_order(&mut skills);
    skills
}

pub(crate) fn profile_evidence_required(profile: &DispatchProfileDef) -> Vec<String> {
    let mut evidence = profile
        .evidence_required
        .iter()
        .map(|item| item.to_string())
        .collect::<Vec<_>>();
    crate::skill_policy::dedupe_preserve_order(&mut evidence);
    evidence
}

pub(crate) fn profile_evidence_required_for_server(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Vec<String>, String> {
    let overlay = load_profile_overlay(server, profile.name)?;
    let mut evidence = profile_evidence_required(profile);
    evidence.extend(profile_projected_evidence_required_from_overlay(
        profile,
        overlay.as_ref(),
    ));
    crate::skill_policy::dedupe_preserve_order(&mut evidence);
    Ok(evidence)
}

pub(super) fn profile_weak_against(profile: &DispatchProfileDef) -> Vec<String> {
    let mut weak = profile
        .weak_against
        .iter()
        .map(|item| item.to_string())
        .collect::<Vec<_>>();
    crate::skill_policy::dedupe_preserve_order(&mut weak);
    weak
}

pub(crate) fn profile_weak_against_for_server(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Vec<String>, String> {
    let overlay = load_profile_overlay(server, profile.name)?;
    let mut weak = profile_weak_against(profile);
    weak.extend(profile_projected_weak_against_from_overlay(
        profile,
        overlay.as_ref(),
    ));
    crate::skill_policy::dedupe_preserve_order(&mut weak);
    Ok(weak)
}

pub(super) fn profile_projected_weak_against(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Vec<String>, String> {
    let overlay = load_profile_overlay(server, profile.name)?;
    Ok(profile_projected_weak_against_from_overlay(
        profile,
        overlay.as_ref(),
    ))
}

pub(super) fn profile_demotion_targets(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Vec<String>, String> {
    let overlay = load_profile_overlay(server, profile.name)?;
    Ok(profile_demotion_targets_from_overlay(
        profile,
        overlay.as_ref(),
    ))
}

pub(crate) fn profile_evidence_contract_json(profile: &DispatchProfileDef) -> Value {
    json!({
        "required": profile_evidence_required(profile),
        "projected_required": [],
        "projection": {
            "status": "baseline",
        },
    })
}

pub(crate) fn profile_evidence_contract_json_for_server(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Value, String> {
    let overlay = load_profile_overlay(server, profile.name)?;
    let projected_required =
        profile_projected_evidence_required_from_overlay(profile, overlay.as_ref());
    let mut required = profile_evidence_required(profile);
    required.extend(projected_required.iter().cloned());
    crate::skill_policy::dedupe_preserve_order(&mut required);
    let source_proposal_ids = overlay
        .as_ref()
        .and_then(|overlay| overlay.get("source_proposal_ids"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(json!({
        "required": required,
        "projected_required": projected_required,
        "projection": {
            "status": if overlay.is_some() { "applied_overlay" } else { "baseline" },
            "namespace": PROFILE_CARD_OVERLAY_NS,
            "key": profile.name,
            "source_proposal_ids": source_proposal_ids,
        },
    }))
}

pub(crate) fn profile_required_skill_ids(profile: &DispatchProfileDef) -> Vec<String> {
    let mut skills = profile
        .common_skills
        .iter()
        .chain(profile.signature_skills.iter())
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    crate::skill_policy::dedupe_preserve_order(&mut skills);
    skills
}

pub(crate) fn profile_required_skill_ids_for_server(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Vec<String>, String> {
    let mut skills = profile
        .common_skills
        .iter()
        .chain(profile.signature_skills.iter())
        .map(|skill| skill.to_string())
        .collect::<Vec<_>>();
    skills.extend(profile_projected_signature_skills(server, profile)?);
    crate::skill_policy::dedupe_preserve_order(&mut skills);
    Ok(skills)
}

pub(crate) fn profile_skill_loadout_json(profile: &DispatchProfileDef) -> Value {
    json!({
        "common_skills": profile.common_skills,
        "signature_skills": profile.signature_skills,
        "projected_signature_skills": [],
        "passive_traits": profile.passive_traits,
        "projected_passive_traits": [],
        "forbidden_skills": profile.forbidden_skills,
        "projection": {
            "status": "baseline",
        },
    })
}

pub(crate) fn profile_skill_loadout_json_for_server(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Value, String> {
    let overlay = load_profile_overlay(server, profile.name)?;
    let projected_signature_skills =
        profile_projected_signature_skills_from_overlay(profile, overlay.as_ref());
    let projected_passive_traits =
        profile_projected_passive_traits_from_overlay(profile, overlay.as_ref());
    let mut signature_skills = profile
        .signature_skills
        .iter()
        .map(|skill| skill.to_string())
        .collect::<Vec<_>>();
    signature_skills.extend(projected_signature_skills.iter().cloned());
    crate::skill_policy::dedupe_preserve_order(&mut signature_skills);
    let mut passive_traits = profile
        .passive_traits
        .iter()
        .map(|trait_id| trait_id.to_string())
        .collect::<Vec<_>>();
    passive_traits.extend(projected_passive_traits.iter().cloned());
    crate::skill_policy::dedupe_preserve_order(&mut passive_traits);
    let source_proposal_ids = overlay
        .as_ref()
        .and_then(|overlay| overlay.get("source_proposal_ids"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(json!({
        "common_skills": profile.common_skills,
        "signature_skills": signature_skills,
        "projected_signature_skills": projected_signature_skills,
        "passive_traits": passive_traits,
        "projected_passive_traits": projected_passive_traits,
        "forbidden_skills": profile.forbidden_skills,
        "projection": {
            "status": if overlay.is_some() { "applied_overlay" } else { "baseline" },
            "namespace": PROFILE_CARD_OVERLAY_NS,
            "key": profile.name,
            "source_proposal_ids": source_proposal_ids,
        },
    }))
}

pub(super) fn profile_projected_signature_skills(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Vec<String>, String> {
    let overlay = load_profile_overlay(server, profile.name)?;
    Ok(profile_projected_signature_skills_from_overlay(
        profile,
        overlay.as_ref(),
    ))
}

pub(super) fn profile_projected_signature_skills_from_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    let Some(overlay) = overlay else {
        return Vec::new();
    };
    let forbidden = profile.forbidden_skills.iter().collect::<HashSet<_>>();
    let mut skills = overlay
        .get("add_signature_skills")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|skill| !skill.is_empty())
        .filter(|skill| !forbidden.contains(skill))
        .map(str::to_string)
        .collect::<Vec<_>>();
    crate::skill_policy::dedupe_preserve_order(&mut skills);
    skills
}

pub(super) fn profile_projected_passive_traits_from_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    let Some(overlay) = overlay else {
        return Vec::new();
    };
    let baseline = profile.passive_traits.iter().collect::<HashSet<_>>();
    let mut traits = overlay
        .get("add_passive_traits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|trait_id| !trait_id.is_empty())
        .filter(|trait_id| !baseline.contains(trait_id))
        .map(str::to_string)
        .collect::<Vec<_>>();
    crate::skill_policy::dedupe_preserve_order(&mut traits);
    traits
}

pub(super) fn profile_projected_evidence_required_from_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    let Some(overlay) = overlay else {
        return Vec::new();
    };
    let baseline = profile.evidence_required.iter().collect::<HashSet<_>>();
    let mut evidence = overlay
        .get("add_evidence_required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|evidence_id| !evidence_id.is_empty())
        .filter(|evidence_id| !baseline.contains(evidence_id))
        .map(str::to_string)
        .collect::<Vec<_>>();
    crate::skill_policy::dedupe_preserve_order(&mut evidence);
    evidence
}

pub(super) fn profile_projected_weak_against_from_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    let Some(overlay) = overlay else {
        return Vec::new();
    };
    let baseline = profile.weak_against.iter().collect::<HashSet<_>>();
    let mut weak = overlay
        .get("add_weak_against")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|weakness_id| !weakness_id.is_empty())
        .filter(|weakness_id| !baseline.contains(weakness_id))
        .map(str::to_string)
        .collect::<Vec<_>>();
    crate::skill_policy::dedupe_preserve_order(&mut weak);
    weak
}

pub(super) fn profile_demotion_targets_from_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    let Some(overlay) = overlay else {
        return Vec::new();
    };
    let known_skills = profile_required_skill_ids(profile)
        .into_iter()
        .collect::<HashSet<_>>();
    let mut targets = overlay
        .get("demotion_targets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|skill_id| !skill_id.is_empty())
        .filter(|skill_id| known_skills.contains(*skill_id))
        .map(str::to_string)
        .collect::<Vec<_>>();
    crate::skill_policy::dedupe_preserve_order(&mut targets);
    targets
}

pub(super) fn load_profile_overlay(
    server: &MemoryServer,
    profile: &str,
) -> Result<Option<Value>, String> {
    server
        .with_global_store_read(|store| {
            store
                .get_state_kv(PROFILE_CARD_OVERLAY_NS, profile)
                .map_err(|e| format!("load profile/card overlay: {e}"))
        })?
        .map(|(raw, _version)| {
            serde_json::from_str::<Value>(&raw)
                .map_err(|e| format!("parse profile/card overlay: {e}"))
        })
        .transpose()
}

pub(crate) fn profile_eval_feedback_json(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
    limit: usize,
) -> Result<Value, String> {
    let limit = limit.max(1);
    let rows = load_live_eval_rows(server, limit)?;
    let performance_matrix = aggregate_performance_matrix(&rows);
    let profile_rows = performance_matrix
        .iter()
        .filter(|row| row.profile.as_deref() == Some(profile.name))
        .cloned()
        .collect::<Vec<_>>();
    let fallback_rows = performance_matrix
        .iter()
        .filter(|row| {
            row.profile.is_none()
                && (row.agent == profile.backend
                    || row
                        .role
                        .as_deref()
                        .is_some_and(|role| profile_role_matches(profile, role)))
        })
        .cloned()
        .collect::<Vec<_>>();

    let profile_samples = sum_matrix_samples(&profile_rows);
    let fallback_role_samples = sum_matrix_samples(&fallback_rows);
    let summary = summarize_matrix_rows(&profile_rows);
    let failure_count = sum_matrix_failures(&profile_rows);
    let human_override_rate =
        weighted_matrix_rate(&profile_rows, |row| Some(row.human_override_rate));
    let avg_retry_count = weighted_matrix_rate(&profile_rows, |row| Some(row.avg_retry_count));
    let verification_rate = weighted_matrix_rate(&profile_rows, |row| Some(row.verification_rate));
    let success_rate = weighted_matrix_rate(&profile_rows, |row| row.success_rate);
    let useful_rate = weighted_matrix_rate(&profile_rows, |row| row.useful_rate);

    let mut guidance = Vec::new();
    if profile_samples == 0 {
        guidance.push(
            "low_sample: no live /eval rows for this profile; keep deterministic MBIT loadout"
                .to_string(),
        );
    } else if profile_samples < MIN_LOADOUT_EVOLUTION_SAMPLES {
        guidance.push(format!(
            "low_sample: {} profile samples below evolution threshold {}",
            profile_samples, MIN_LOADOUT_EVOLUTION_SAMPLES
        ));
    } else {
        guidance.push(
            "evidence_available: profile has enough live samples for loadout review".to_string(),
        );
    }
    if failure_count > 0 {
        guidance.push(format!(
            "caution: {} failures present before promoting signature skills",
            failure_count
        ));
    }
    if human_override_rate.unwrap_or(0.0) >= 0.10 {
        guidance.push("review_required: human overrides are elevated for this profile".to_string());
    }
    if avg_retry_count.unwrap_or(0.0) >= 1.0 {
        guidance
            .push("review_required: retry count suggests loadout or prompt friction".to_string());
    }
    if verification_rate.unwrap_or(0.0) < 0.50 && profile_samples > 0 {
        guidance.push("evidence_gap: completions need stronger verification evidence".to_string());
    }
    if profile_samples >= MIN_LOADOUT_EVOLUTION_SAMPLES
        && failure_count == 0
        && human_override_rate.unwrap_or(0.0) < 0.10
        && avg_retry_count.unwrap_or(0.0) < 1.0
        && success_rate.or(useful_rate).unwrap_or(0.0) >= 0.80
    {
        guidance.push(
            "promotion_candidate: stable profile feedback can seed a reviewed loadout proposal"
                .to_string(),
        );
    }

    Ok(json!({
        "source": "live_eval",
        "limit": limit,
        "row_count": rows.len(),
        "matrix_rows": performance_matrix.len(),
        "profile_samples": profile_samples,
        "fallback_role_samples": fallback_role_samples,
        "min_samples_for_evolution": MIN_LOADOUT_EVOLUTION_SAMPLES,
        "summary": summary,
        "performance_by_task": profile_rows,
        "role_backend_fallback": fallback_rows,
        "guidance": guidance,
    }))
}

pub(super) fn profile_role_matches(profile: &DispatchProfileDef, role: &str) -> bool {
    role == profile.role
        || (profile.role == "planner" && role == "architect")
        || (profile.role == "architect" && role == "critic")
        || (profile.role == "executor" && role == "implementer")
        || (profile.role.contains("review") && role.contains("review"))
}

pub(super) fn sum_matrix_samples(rows: &[AgentPerformanceMatrixRow]) -> u32 {
    rows.iter().map(|row| row.samples).sum()
}

pub(super) fn sum_matrix_failures(rows: &[AgentPerformanceMatrixRow]) -> u32 {
    rows.iter().map(|row| row.failure_count).sum()
}

pub(super) fn weighted_matrix_rate<F>(rows: &[AgentPerformanceMatrixRow], value: F) -> Option<f64>
where
    F: Fn(&AgentPerformanceMatrixRow) -> Option<f64>,
{
    let mut weighted_sum = 0.0;
    let mut samples = 0_u32;
    for row in rows {
        let Some(value) = value(row) else {
            continue;
        };
        weighted_sum += value * row.samples as f64;
        samples += row.samples;
    }
    (samples > 0).then(|| round4(weighted_sum / samples as f64))
}

pub(super) fn summarize_matrix_rows(rows: &[AgentPerformanceMatrixRow]) -> Value {
    json!({
        "samples": sum_matrix_samples(rows),
        "success_rate": weighted_matrix_rate(rows, |row| row.success_rate),
        "useful_rate": weighted_matrix_rate(rows, |row| row.useful_rate),
        "verification_rate": weighted_matrix_rate(rows, |row| Some(row.verification_rate)),
        "failure_count": sum_matrix_failures(rows),
        "human_override_rate": weighted_matrix_rate(rows, |row| Some(row.human_override_rate)),
        "avg_retry_count": weighted_matrix_rate(rows, |row| Some(row.avg_retry_count)),
        "avg_latency_ms": weighted_matrix_rate(rows, |row| row.avg_latency_ms),
        "avg_cost_usd": weighted_matrix_rate(rows, |row| row.avg_cost_usd),
        "avg_quality_score": weighted_matrix_rate(rows, |row| row.avg_quality_score),
    })
}
