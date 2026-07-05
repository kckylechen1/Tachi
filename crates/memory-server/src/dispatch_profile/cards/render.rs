use super::*;

pub(crate) fn profile_json(profile: &DispatchProfileDef) -> Value {
    tachi_dispatch::profile_json(profile)
}

pub(crate) fn profile_json_for_server(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Value, String> {
    Ok(
        tachi_dispatch::profile_json_with_loadout_and_evidence_contract(
            profile,
            profile_skill_loadout_json_for_server(server, profile)?,
            profile_evidence_contract_json_for_server(server, profile)?,
            profile_weak_against_for_server(server, profile)?,
            profile_projected_weak_against(server, profile)?,
            profile_demotion_targets(server, profile)?,
        ),
    )
}
