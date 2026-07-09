use super::*;

#[tool_router(router = component_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Component governance read model (Issue #796/#797/#798/#799). action='list': compact records of governed components (component_id, type, owner, summary). action='show': full record (owner, consumers, upstream prereqs, known drift/backflow) plus relation edges (owns/consumes/blocked_by/backflow_candidate). action='check': read-only downstream classifier — classify a checked-out repo path (repo=<path>) against declared records as kernel_drift/allowed_adapter_policy/bridge/frontend_shell/unknown with evidence gaps; pass component_id to scope a match. action='plan': read-only cutover checklist from component_id (--from) to repo/path or consumer id (--to via repo=); distinguishes pull/adapt/backflow/delete_retire outcomes; no auto code changes or sync. Records are seeded from the v0 governance fixture under /components/v0/<component_id>. Read-only."
    )]
    pub(crate) async fn tachi_component(
        &self,
        Parameters(params): Parameters<TachiComponentParams>,
    ) -> Result<String, String> {
        let action = params.action.to_ascii_lowercase();
        // Only list/show are forwarded to a remote daemon host: "check" and "plan"
        // take a client-local filesystem path (`repo`) that only makes sense against
        // the caller's own host filesystem. Forwarding would silently classify the
        // wrong checkout. Do not add "check"/"plan" to this allowlist — they must
        // stay local (default-deny is correct here, Issues #797/#798).
        if matches!(action.as_str(), "list" | "show") {
            if let Some(body) =
                crate::cli_client::maybe_forward_server_read(self, "tachi_component", &params)
                    .await?
            {
                return Ok(body);
            }
        }
        crate::component_governance_ops::handle_tachi_component(self, params).await
    }
}
