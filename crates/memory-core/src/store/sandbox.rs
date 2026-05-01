//! Semantic sandbox rules + runtime sandbox policies + exec-audit on
//! [`MemoryStore`].
//!
//! Rule enforcement (path-pattern matching against agent role) lives in
//! `memory-server::handle_search_memory`; this layer is pure persistence.

use crate::db;
use crate::error::MemoryError;
use crate::MemoryStore;

impl MemoryStore {
    /// Set a sandbox access rule for an agent role + path pattern.
    pub fn set_sandbox_rule(
        &self,
        agent_role: &str,
        path_pattern: &str,
        access_level: &str,
    ) -> Result<(), MemoryError> {
        db::set_sandbox_rule(&self.conn, agent_role, path_pattern, access_level)
    }

    /// Check if an agent role can access a path for a given operation.
    /// Returns (allowed, matching_rule_description).
    pub fn check_sandbox_access(
        &self,
        agent_role: &str,
        path: &str,
        operation: &str,
    ) -> Result<(bool, Option<String>), MemoryError> {
        db::check_sandbox_access(&self.conn, agent_role, path, operation)
    }

    /// Set or update runtime sandbox policy for a capability.
    #[allow(clippy::too_many_arguments)]
    pub fn set_sandbox_policy(
        &self,
        capability_id: &str,
        runtime_type: &str,
        env_allowlist_json: &str,
        fs_read_roots_json: &str,
        fs_write_roots_json: &str,
        cwd_roots_json: &str,
        max_startup_ms: u64,
        max_tool_ms: u64,
        max_concurrency: u32,
        enabled: bool,
    ) -> Result<(), MemoryError> {
        db::set_sandbox_policy(
            &self.conn,
            capability_id,
            runtime_type,
            env_allowlist_json,
            fs_read_roots_json,
            fs_write_roots_json,
            cwd_roots_json,
            max_startup_ms,
            max_tool_ms,
            max_concurrency,
            enabled,
        )
    }

    /// Get runtime sandbox policy by capability id.
    pub fn get_sandbox_policy(
        &self,
        capability_id: &str,
    ) -> Result<Option<serde_json::Value>, MemoryError> {
        db::get_sandbox_policy(&self.conn, capability_id)
    }

    /// List runtime sandbox policies.
    pub fn list_sandbox_policies(
        &self,
        enabled_only: bool,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>, MemoryError> {
        db::list_sandbox_policies(&self.conn, enabled_only, limit)
    }

    /// Insert one sandbox execution audit row.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_sandbox_exec_audit(
        &self,
        timestamp: &str,
        capability_id: &str,
        stage: &str,
        decision: &str,
        reason: Option<&str>,
        duration_ms: u64,
        tool_name: Option<&str>,
        error_kind: Option<&str>,
        metadata_json: &str,
    ) -> Result<(), MemoryError> {
        db::insert_sandbox_exec_audit(
            &self.conn,
            timestamp,
            capability_id,
            stage,
            decision,
            reason,
            duration_ms,
            tool_name,
            error_kind,
            metadata_json,
        )
    }

    /// List sandbox execution audit rows.
    pub fn list_sandbox_exec_audit(
        &self,
        capability_id: Option<&str>,
        stage: Option<&str>,
        decision: Option<&str>,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>, MemoryError> {
        db::list_sandbox_exec_audit(&self.conn, capability_id, stage, decision, limit)
    }
}
