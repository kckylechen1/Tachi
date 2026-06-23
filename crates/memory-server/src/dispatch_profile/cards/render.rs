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
