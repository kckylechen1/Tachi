use crate::tool_params::AuditLogParams;
use crate::MemoryServer;

pub(crate) async fn handle_tachi_audit_log(
    server: &MemoryServer,
    params: AuditLogParams,
) -> Result<String, String> {
    server.with_global_store(|store| {
        let entries = store
            .audit_log_list(params.limit, params.server_filter.as_deref())
            .map_err(|e| format!("audit log: {e}"))?;
        serde_json::to_string(&entries).map_err(|e| format!("serialize: {e}"))
    })
}
