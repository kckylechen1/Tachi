use super::*;

#[tool_router(router = tune_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Admin/operator tuning facade for route and recall policy evaluation. Available only to admin/full profiles by omission from the standard, delegate, and bundle pattern arrays. Actions: route_simulate, route_proposals, route_review, route_apply, recall_simulate, recall_proposals, recall_review, recall_apply."
    )]
    pub(crate) async fn tachi_tune(
        &self,
        Parameters(params): Parameters<TachiTuneParams>,
    ) -> Result<String, String> {
        handle_tachi_tune_bridge(self, params).await
    }
}

async fn handle_tachi_tune_bridge(
    server: &MemoryServer,
    params: TachiTuneParams,
) -> Result<String, String> {
    if !tachi_hub::tool_visible("tachi_tune", server.active_tool_profile(), None) {
        return Err(
            "tachi_tune is admin-only; route and recall tuning are not available to the active tool profile."
                .to_string(),
        );
    }

    match params.action {
        TachiTuneAction::RouteSimulate => {
            let task_params = remap_tune_params::<TachiTaskParams>(&params, "route_simulate")?;
            handle_tachi_task_facade(server, task_params).await
        }
        TachiTuneAction::RouteProposals => {
            let task_params = remap_tune_params::<TachiTaskParams>(&params, "proposals")?;
            handle_tachi_task_facade(server, task_params).await
        }
        TachiTuneAction::RouteReview => {
            let task_params = remap_tune_params::<TachiTaskParams>(&params, "review_proposal")?;
            handle_tachi_task_facade(server, task_params).await
        }
        TachiTuneAction::RouteApply => {
            let task_params = remap_tune_params::<TachiTaskParams>(&params, "apply_proposals")?;
            handle_tachi_task_facade(server, task_params).await
        }
        TachiTuneAction::RecallSimulate => {
            let memory_params = remap_tune_params::<TachiMemoryParams>(&params, "recall_simulate")?;
            crate::facade_memory_ops::handle_tachi_memory(server, memory_params).await
        }
        TachiTuneAction::RecallProposals => {
            let memory_params =
                remap_tune_params::<TachiMemoryParams>(&params, "recall_proposals")?;
            crate::facade_memory_ops::handle_tachi_memory(server, memory_params).await
        }
        TachiTuneAction::RecallReview => {
            let memory_params =
                remap_tune_params::<TachiMemoryParams>(&params, "review_recall_proposal")?;
            crate::facade_memory_ops::handle_tachi_memory(server, memory_params).await
        }
        TachiTuneAction::RecallApply => {
            let memory_params =
                remap_tune_params::<TachiMemoryParams>(&params, "apply_recall_proposals")?;
            crate::facade_memory_ops::handle_tachi_memory(server, memory_params).await
        }
    }
}

fn remap_tune_params<T>(params: &TachiTuneParams, action: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let mut value =
        serde_json::to_value(params).map_err(|err| format!("serialize tachi_tune params: {err}"))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| "serialize tachi_tune params: expected object".to_string())?;
    object.insert("action".to_string(), Value::String(action.to_string()));
    serde_json::from_value(value).map_err(|err| format!("map tachi_tune params: {err}"))
}
