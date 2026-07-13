//! Derived item and retention methods on [`MemoryStore`].

use crate::{db, error::MemoryError, types::GcConfig, MemoryStore};

impl MemoryStore {
    /// Save a derived item (causal extraction, distilled rule, etc.)
    #[allow(clippy::too_many_arguments)]
    pub fn save_derived(
        &self,
        text: &str,
        path: &str,
        summary: &str,
        importance: f64,
        source: &str,
        scope: &str,
        metadata: &serde_json::Value,
    ) -> Result<String, MemoryError> {
        db::save_derived(
            &self.conn, text, path, summary, importance, source, scope, metadata,
        )
    }

    /// Save or update a derived item with a stable caller-provided id.
    #[allow(clippy::too_many_arguments)]
    pub fn save_derived_with_id(
        &self,
        id: &str,
        text: &str,
        path: &str,
        summary: &str,
        importance: f64,
        source: &str,
        scope: &str,
        metadata: &serde_json::Value,
    ) -> Result<(), MemoryError> {
        db::save_derived_with_id(
            &self.conn, id, text, path, summary, importance, source, scope, metadata,
        )
    }

    /// List derived items by source and path prefix.
    pub fn list_derived_by_source(
        &self,
        source: &str,
        path_prefix: &str,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>, MemoryError> {
        db::list_derived_by_source(&self.conn, source, path_prefix, limit)
    }

    /// Archive a memory entry (set archived=1, used after merge).
    pub fn archive_memory(&self, id: &str) -> Result<bool, MemoryError> {
        db::archive_memory(&self.conn, id)
    }

    /// Mark a memory as superseded by a newer/canonical memory.
    pub fn supersede_memory(&self, id: &str, superseded_by: &str) -> Result<bool, MemoryError> {
        db::supersede_memory(&self.conn, id, superseded_by)
    }

    /// Run retention-based garbage collection on growing tables.
    /// Thresholds are driven by `GcConfig` (replaces previously hardcoded literals).
    pub fn gc_tables(&mut self, cfg: &GcConfig) -> Result<serde_json::Value, MemoryError> {
        let db_label = self.db_label.clone();
        db::retry_memory_locked("gc_tables", &db_label, || db::gc_tables(&mut self.conn, cfg))
    }

    /// Archive low-importance memories not accessed in `stale_days`.
    pub fn archive_stale_memories(&self, stale_days: u32) -> Result<u64, MemoryError> {
        db::archive_stale_memories(&self.conn, stale_days)
    }
}
