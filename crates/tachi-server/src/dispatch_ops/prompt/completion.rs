use crate::tool_params::TachiDispatchParams;

pub(super) fn dispatch_can_self_complete(params: &TachiDispatchParams) -> bool {
    params.inject_tachi_mcp == Some(true)
        || params.mcp_access.as_ref().is_some_and(|access| {
            access.inject_tachi_mcp == Some(true)
                || access
                    .allowed_facades
                    .iter()
                    .any(|facade| matches!(facade.as_str(), "tachi_task" | "tachi_complete"))
        })
}
