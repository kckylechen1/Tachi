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
        // TachiShellParams (the CLI-facing kanban action) has no verbose
        // knob of its own; this surface gets the new compact-by-default view
        // for free (tachi#1173 item 3).
        verbose: None,
    };
    crate::dispatch_ops::handle_tachi_board(server, bp).await
}
