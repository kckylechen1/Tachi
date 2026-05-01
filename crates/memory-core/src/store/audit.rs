//! Audit-log methods on [`MemoryStore`].

use crate::db;
use crate::error::MemoryError;
use crate::MemoryStore;

impl MemoryStore {
    /// Insert an audit log entry.
    #[allow(clippy::too_many_arguments)]
    pub fn audit_log_insert(
        &self,
        timestamp: &str,
        server_id: &str,
        tool_name: &str,
        args_hash: &str,
        success: bool,
        duration_ms: u64,
        error_kind: Option<&str>,
    ) -> Result<(), MemoryError> {
        db::audit_log_insert(
            &self.conn,
            timestamp,
            server_id,
            tool_name,
            args_hash,
            success,
            duration_ms,
            error_kind,
        )
    }

    /// List recent audit log entries.
    pub fn audit_log_list(
        &self,
        limit: usize,
        server_filter: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, MemoryError> {
        db::audit_log_list(&self.conn, limit, server_filter)
    }
}
