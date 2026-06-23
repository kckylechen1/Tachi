use super::*;

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

pub(in crate::dispatch_profile) fn profile_demotion_targets(
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
