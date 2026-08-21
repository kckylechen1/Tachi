use tachi_params::ExecutionGrant;

pub(super) fn dispatch_can_self_complete(grant: &ExecutionGrant) -> bool {
    grant.mcp_access.as_ref().is_some_and(|access| {
        access.inject_tachi_mcp == Some(true)
            || access
                .allowed_facades
                .iter()
                .any(|facade| matches!(facade.as_str(), "tachi_task" | "tachi_complete"))
    })
}
