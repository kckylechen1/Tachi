use super::*;

pub(in crate::dispatch_profile) use tachi_dispatch::{
    profile_demotion_targets_from_overlay, profile_projected_evidence_required_from_overlay,
    profile_projected_passive_traits_from_overlay, profile_projected_signature_skills_from_overlay,
    profile_projected_weak_against_from_overlay,
};

pub(crate) fn profile_required_skill_ids(profile: &DispatchProfileDef) -> Vec<String> {
    tachi_dispatch::profile_required_skill_ids(profile)
}

pub(crate) fn profile_required_skill_ids_for_server(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Vec<String>, String> {
    let overlay = load_profile_overlay(server, profile.name)?;
    Ok(tachi_dispatch::profile_required_skill_ids_with_overlay(
        profile,
        overlay.as_ref(),
    ))
}

pub(crate) fn profile_skill_loadout_json_for_server(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Value, String> {
    let overlay = load_profile_overlay(server, profile.name)?;
    Ok(tachi_dispatch::profile_skill_loadout_json_with_overlay(
        profile,
        overlay.as_ref(),
    ))
}

#[cfg(test)]
pub(crate) fn profile_evidence_required(profile: &DispatchProfileDef) -> Vec<String> {
    tachi_dispatch::profile_evidence_required(profile)
}

pub(crate) fn profile_evidence_required_for_server(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Vec<String>, String> {
    let overlay = load_profile_overlay(server, profile.name)?;
    Ok(tachi_dispatch::profile_evidence_required_with_overlay(
        profile,
        overlay.as_ref(),
    ))
}

pub(crate) fn profile_weak_against_for_server(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Vec<String>, String> {
    let overlay = load_profile_overlay(server, profile.name)?;
    Ok(tachi_dispatch::profile_weak_against_with_overlay(
        profile,
        overlay.as_ref(),
    ))
}

pub(in crate::dispatch_profile) fn profile_demotion_targets(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Vec<String>, String> {
    let overlay = load_profile_overlay(server, profile.name)?;
    Ok(tachi_dispatch::profile_demotion_targets_from_overlay(
        profile,
        overlay.as_ref(),
    ))
}

pub(crate) fn profile_evidence_contract_json_for_server(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Value, String> {
    let overlay = load_profile_overlay(server, profile.name)?;
    Ok(tachi_dispatch::profile_evidence_contract_json_with_overlay(
        profile,
        overlay.as_ref(),
    ))
}

pub(crate) fn profile_json(profile: &DispatchProfileDef) -> Value {
    tachi_dispatch::profile_json(profile)
}

pub(crate) fn profile_json_for_server(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Value, String> {
    let overlay = load_profile_overlay(server, profile.name)?;
    Ok(tachi_dispatch::profile_json_with_overlay(
        profile,
        overlay.as_ref(),
    ))
}

pub(crate) fn profile_eval_feedback_json(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
    limit: usize,
) -> Result<Value, String> {
    let limit = limit.max(1);
    let rows = load_live_eval_rows(server, limit)?;
    let performance_matrix = aggregate_performance_matrix(&rows);
    Ok(tachi_dispatch::policy::profile_eval_feedback_json(
        profile,
        rows.len(),
        &performance_matrix,
        limit,
    ))
}

fn load_profile_overlay(server: &MemoryServer, profile: &str) -> Result<Option<Value>, String> {
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
