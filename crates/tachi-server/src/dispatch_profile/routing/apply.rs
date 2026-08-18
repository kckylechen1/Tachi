use super::*;

#[cfg(test)]
pub(crate) fn resolve_and_apply_dispatch_profile(
    params: &mut TachiDispatchParams,
) -> Result<ResolvedDispatchProfile, String> {
    tachi_dispatch::resolve_and_apply_dispatch_profile(
        params,
        |profile| Ok(profile_required_skill_ids(profile)),
        |profile| Ok(profile_evidence_required(profile)),
        |server_url| crate::dispatch_ops::harness_server_attach_ready(server_url),
    )
}

pub(crate) fn resolve_and_apply_dispatch_profile_for_server(
    _server: &MemoryServer,
    params: &mut TachiDispatchParams,
) -> Result<ResolvedDispatchProfile, String> {
    // #1690 C3: skills are the STATIC reviewed baseline — the legacy
    // `add_signature_skills` overlay merge is retired, so the resolution no
    // longer loads a profile overlay for the skill set.
    tachi_dispatch::resolve_and_apply_dispatch_profile(
        params,
        |profile| Ok(profile_required_skill_ids(profile)),
        |profile| profile_evidence_required_for_server(_server, profile),
        |server_url| crate::dispatch_ops::harness_server_attach_ready(server_url),
    )
}
