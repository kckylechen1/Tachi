use super::*;

pub(super) async fn handle_kanban_action(
    server: &MemoryServer,
    params: TachiShellParams,
) -> Result<String, String> {
    let bp = TachiBoardParams {
        state_filter: params.state_filter.clone(),
        limit: params.limit,
        project: params.project.clone(),
        flow_id: params.flow_id.clone(),
    };
    crate::dispatch_ops::handle_tachi_board(server, bp).await
}
