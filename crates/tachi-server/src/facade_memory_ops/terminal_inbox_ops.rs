use serde_json::json;

use crate::{claims_ops::resolve_session_client, tool_params::TerminalInboxParams, MemoryServer};

/// Read/ack facade. Both operations resolve identity at the server boundary;
/// supplied parameters cannot select another recipient's inbox.
pub(crate) fn handle_terminal_inbox(
    server: &MemoryServer,
    params: TerminalInboxParams,
) -> Result<String, String> {
    let recipient = resolve_session_client(server);
    match params.action.to_ascii_lowercase().as_str() {
        "list" => {
            let receipts = server.with_global_store_read(|store| {
                memcore::list_terminal_receipts(
                    store.connection(),
                    &recipient,
                    params.include_acknowledged,
                    params.limit.clamp(1, 100),
                )
                .map_err(|e| e.to_string())
            })?;
            serde_json::to_string(
                &json!({"status":"completed", "action":"list", "receipts":receipts}),
            )
            .map_err(|e| e.to_string())
        }
        "ack" => {
            let dispatch_id = params
                .dispatch_id
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| "dispatch_id is required when action='ack'".to_string())?;
            let acknowledged = server.with_global_store(|store| {
                memcore::acknowledge_terminal_receipt(store.connection(), &dispatch_id, &recipient)
                    .map_err(|e| e.to_string())
            })?;
            serde_json::to_string(&json!({"status":"completed", "action":"ack", "dispatch_id":dispatch_id, "acknowledged":acknowledged})).map_err(|e| e.to_string())
        }
        _ => Err("action must be 'list' or 'ack'".to_string()),
    }
}
