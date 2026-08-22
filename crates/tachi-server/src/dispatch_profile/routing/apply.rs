use super::*;

#[cfg(test)]
pub(crate) fn resolve_and_apply_dispatch_profile(
    params: &mut TachiDispatchParams,
) -> Result<ResolvedDispatchProfile, String> {
    tachi_dispatch::resolve_and_apply_dispatch_profile(
        params,
        |profile| Ok(profile_required_skill_ids(profile)),
        |profile| Ok(profile_evidence_required(profile)),
        |profile| Ok(profile_json(profile)),
        |server_url| crate::dispatch_ops::harness_server_attach_ready(server_url),
    )
}

pub(crate) fn resolve_and_apply_dispatch_profile_for_server(
    server: &MemoryServer,
    params: &mut TachiDispatchParams,
) -> Result<ResolvedDispatchProfile, String> {
    tachi_dispatch::resolve_and_apply_dispatch_profile(
        params,
        |profile| profile_required_skill_ids_for_server(server, profile),
        |profile| profile_evidence_required_for_server(server, profile),
        |profile| profile_json_for_server(server, profile),
        |server_url| crate::dispatch_ops::harness_server_attach_ready(server_url),
    )
}
