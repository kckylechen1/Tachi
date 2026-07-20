use super::*;

mod convoy;
mod dispatch;
mod stage;
mod status;

#[cfg(test)]
pub(super) use self::convoy::resolve_slice_id;
use self::dispatch::handle_dispatch_action;
use self::stage::handle_stage_action;
pub(super) use self::status::handle_status_action;

// ─── Action handlers ─────────────────────────────────────────────────────────

/// Top-level dispatcher used by `MemoryServer::tachi_shell`.
pub(crate) async fn handle_tachi_shell(
    server: &MemoryServer,
    params: TachiShellParams,
) -> Result<String, String> {
    let action = params.action.to_ascii_lowercase();
    match action.as_str() {
        "brainstorm" | "plan" | "review" | "ship" => {
            handle_stage_action(server, &action, params).await
        }
        "dispatch" => handle_dispatch_action(server, params).await,
        "status" => handle_status_action(params).await,
        _ => Err(format!(
            "Invalid action '{}'. Use 'brainstorm', 'plan', 'dispatch', 'status', 'review', or 'ship'.",
            params.action
        )),
    }
}
