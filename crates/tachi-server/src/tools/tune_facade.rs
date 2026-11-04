use super::*;

#[tool_router(router = tune_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Admin/operator tuning facade for route and recall policy evaluation. Available only through explicitly admitted owner management calls. Actions: route_simulate, route_proposals, route_review, route_apply, recall_simulate, recall_proposals, recall_review, recall_apply."
    )]
    pub(crate) async fn tachi_tune(
        &self,
        Parameters(params): Parameters<TachiTuneParams>,
    ) -> Result<String, String> {
        crate::tune_ops::handle_tachi_tune(self, params).await
    }
}
