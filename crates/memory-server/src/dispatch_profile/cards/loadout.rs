use super::*;

pub(crate) fn profile_required_skill_ids(profile: &DispatchProfileDef) -> Vec<String> {
    tachi_dispatch::profile_required_skill_ids(profile)
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
    tachi_dispatch::profile_skill_loadout_json(profile)
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

pub(in crate::dispatch_profile) fn profile_projected_signature_skills_from_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    tachi_dispatch::profile_projected_signature_skills_from_overlay(profile, overlay)
}

pub(in crate::dispatch_profile) fn profile_projected_passive_traits_from_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    tachi_dispatch::profile_projected_passive_traits_from_overlay(profile, overlay)
}

pub(in crate::dispatch_profile) fn profile_projected_evidence_required_from_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    tachi_dispatch::profile_projected_evidence_required_from_overlay(profile, overlay)
}

pub(in crate::dispatch_profile) fn profile_projected_weak_against_from_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    tachi_dispatch::profile_projected_weak_against_from_overlay(profile, overlay)
}

pub(in crate::dispatch_profile) fn profile_demotion_targets_from_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    tachi_dispatch::profile_demotion_targets_from_overlay(profile, overlay)
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
