//! Hub capability + virtual-capability binding methods on [`MemoryStore`].
//!
//! Pure delegation to `crate::db::*`. Lives here (rather than in lib.rs) so
//! the hub surface can grow without bloating the crate root.

use crate::db;
use crate::error::MemoryError;
use crate::hub::{HubCapability, VirtualCapabilityBinding};
use crate::MemoryStore;

impl MemoryStore {
    /// Register or update a hub capability (skill, plugin, or MCP server).
    pub fn hub_register(&self, cap: &HubCapability) -> Result<(), MemoryError> {
        db::hub_upsert(&self.conn, cap)
    }

    /// Get a single hub capability by ID.
    pub fn hub_get(&self, id: &str) -> Result<Option<HubCapability>, MemoryError> {
        db::hub_get(&self.conn, id)
    }

    /// List hub capabilities, optionally filtered by type and enabled status.
    pub fn hub_list(
        &self,
        cap_type: Option<&str>,
        enabled_only: bool,
    ) -> Result<Vec<HubCapability>, MemoryError> {
        db::hub_list(&self.conn, cap_type, enabled_only)
    }

    /// Search hub capabilities by name/description.
    pub fn hub_search(
        &self,
        query: &str,
        cap_type: Option<&str>,
    ) -> Result<Vec<HubCapability>, MemoryError> {
        db::hub_search(&self.conn, query, cap_type)
    }

    /// Enable or disable a hub capability.
    pub fn hub_set_enabled(&self, id: &str, enabled: bool) -> Result<bool, MemoryError> {
        db::hub_set_enabled(&self.conn, id, enabled)
    }

    /// Set governance review status for a capability.
    pub fn hub_set_review(
        &self,
        id: &str,
        review_status: &str,
        enabled: Option<bool>,
    ) -> Result<bool, MemoryError> {
        db::hub_set_review(&self.conn, id, review_status, enabled)
    }

    /// Map an alias capability id to an active concrete capability id.
    pub fn hub_set_active_version_route(
        &self,
        alias_id: &str,
        active_capability_id: &str,
    ) -> Result<(), MemoryError> {
        db::hub_set_active_version_route(&self.conn, alias_id, active_capability_id)
    }

    /// Resolve an alias capability id to concrete active capability id.
    pub fn hub_get_active_version_route(
        &self,
        alias_id: &str,
    ) -> Result<Option<String>, MemoryError> {
        db::hub_get_active_version_route(&self.conn, alias_id)
    }

    /// Record invocation outcome for governance health tracking.
    pub fn hub_record_call_outcome(
        &self,
        id: &str,
        success: bool,
        error_kind: Option<&str>,
        open_threshold: u32,
    ) -> Result<(), MemoryError> {
        db::hub_record_call_outcome(&self.conn, id, success, error_kind, open_threshold)
    }

    /// Record feedback for a hub capability invocation.
    pub fn hub_record_feedback(
        &self,
        id: &str,
        success: bool,
        rating: Option<f64>,
    ) -> Result<bool, MemoryError> {
        db::hub_record_feedback(&self.conn, id, success, rating)
    }

    /// Delete a hub capability.
    pub fn hub_delete(&self, id: &str) -> Result<bool, MemoryError> {
        db::hub_delete(&self.conn, id)
    }

    /// Upsert one binding from virtual capability to concrete capability.
    pub fn vc_upsert_binding(&self, binding: &VirtualCapabilityBinding) -> Result<(), MemoryError> {
        db::vc_upsert_binding(&self.conn, binding)
    }

    /// List bindings for a virtual capability.
    pub fn vc_list_bindings(
        &self,
        vc_id: &str,
    ) -> Result<Vec<VirtualCapabilityBinding>, MemoryError> {
        db::vc_list_bindings(&self.conn, vc_id)
    }
}
