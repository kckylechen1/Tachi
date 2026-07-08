use super::*;

#[tool_router(router = component_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Component governance read model (Issue #796). action='list': compact records of governed components (component_id, type, owner, summary). action='show': full record (owner, consumers, upstream prereqs, known drift/backflow) plus relation edges (owns/consumes/blocked_by/backflow_candidate). action='check' (Issue #797): read-only downstream classifier — given a local repo checkout path, classifies it against declared component records (kernel_drift/allowed_adapter_policy/bridge/frontend_shell/unknown) using the git origin remote and declared owner_path evidence. Records are seeded from the v0 governance fixture under /components/v0/<component_id>. Read-only."
    )]
    pub(crate) async fn tachi_component(
        &self,
        Parameters(params): Parameters<TachiComponentParams>,
    ) -> Result<String, String> {
        let action = params.action.to_ascii_lowercase();
        // Only list/show are forwarded to a remote daemon host: "check" takes a
        // client-local filesystem path (`repo`) that only makes sense classified
        // against the daemon's OWN host filesystem, not the caller's. Forwarding it
        // would silently classify the wrong checkout. Do not add "check" to this
        // allowlist — it must stay local (default-deny is correct here, Issue #797).
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
