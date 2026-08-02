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
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
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
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
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
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::archive_memory(&self.conn, id)
    }

    /// Archive an active memory only when its revision is the one inspected.
    pub fn archive_memory_if_revision(
        &self,
        id: &str,
        expected_revision: i64,
    ) -> Result<bool, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::archive_memory_if_revision(&self.conn, id, expected_revision)
    }

    /// Restore an archived memory only when its archived revision is unchanged.
    pub fn restore_archived_if_revision(
        &self,
        id: &str,
        expected_revision: i64,
    ) -> Result<bool, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::restore_archived_if_revision(&self.conn, id, expected_revision)
    }

    /// Mark a memory as superseded by a newer/canonical memory.
    pub fn supersede_memory(&self, id: &str, superseded_by: &str) -> Result<bool, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::supersede_memory(&self.conn, id, superseded_by)
    }

    /// Mark a memory as superseded only when its inspected revision is still
    /// current. This keeps migration lifecycle edges revision-CAS protected.
    pub fn supersede_memory_if_revision(
        &self,
        id: &str,
        superseded_by: &str,
        expected_revision: i64,
    ) -> Result<bool, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::supersede_memory_if_revision(&self.conn, id, superseded_by, expected_revision)
    }

    /// Run retention-based garbage collection on growing tables.
    /// Thresholds are driven by `GcConfig` (replaces previously hardcoded literals).
    pub fn gc_tables(&mut self, cfg: &GcConfig) -> Result<serde_json::Value, MemoryError> {
        let db_label = self.db_label.clone();
        let authorization = self.reserved_reference_write.clone();
        db::retry_memory_locked("gc_tables", &db_label, || {
            let _authorization = db::authorize_reserved_reference_write(&authorization)?;
            db::gc_tables(&mut self.conn, cfg)
        })
    }

    /// Archive low-importance memories not accessed in `stale_days`.
    ///
    /// Writes a `memory.gc_archived` receipt to `tachi_events` naming the rows
    /// and thresholds whenever the sweep archives anything — see
    /// [`db::archive_stale_memories`] (tachi#1463).
    ///
    /// Retried on `SQLITE_BUSY` like [`Self::gc_tables`]: tachi#1463 made the
    /// sweep a single transaction so its receipt cannot describe a partially
    /// applied archival, which means it now holds the write lock across all
    /// four passes instead of releasing it between four autocommit statements.
    /// Retrying is safe because that transaction rolls back whole, and a replay
    /// re-selects from scratch — rows archived by a committed attempt no longer
    /// match `archived = 0`.
    pub fn archive_stale_memories(&self, stale_days: u32) -> Result<u64, MemoryError> {
        let db_label = self.db_label.clone();
        let authorization = self.reserved_reference_write.clone();
        db::retry_memory_locked("archive_stale_memories", &db_label, || {
            let _authorization = db::authorize_reserved_reference_write(&authorization)?;
            db::archive_stale_memories(&self.conn, stale_days)
        })
    }
}
