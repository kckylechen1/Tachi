//! GC-candidate lookups (kanban/handoff sweeps) on [`MemoryStore`].

use crate::{db, error::MemoryError, MemoryStore};

pub use crate::db::{CategoryPathPrefixMemoryRow, PathPrefixMemoryRow};

impl MemoryStore {
    /// Body-free, read-only discovery in one consistent SQLite snapshot.
    /// Prefix matches are literal; the result is never deletion authority.
    pub fn inspect_maintenance_candidates(
        &self,
        prefix: &str,
        as_of: &str,
        max_age_days: u64,
    ) -> Result<db::MaintenanceCandidateReport, MemoryError> {
        let tx = self.conn.unchecked_transaction()?;
        let report = db::maintenance_candidates(&tx, self.profile, prefix, as_of, max_age_days)?;
        tx.commit()?;
        Ok(report)
    }

    /// Recheck lifecycle, retention and references and canonically delete only
    /// eligible Kanban rows under the same immediate transaction.
    pub fn gc_expired_kanban_cards(
        &mut self,
        as_of: &str,
        max_age_days: u64,
    ) -> Result<usize, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::sweep_kanban_gc(
            &mut self.conn,
            self.profile,
            self.vec_available,
            as_of,
            max_age_days,
        )
    }

    /// List `memories` rows whose `path` matches a SQL `LIKE` pattern (caller
    /// supplies the full pattern, e.g. `"{prefix}%"`). Used by GC sweeps that
    /// scope by path namespace (e.g. kanban).
    pub fn list_memories_by_path_prefix(
        &self,
        path_like_pattern: &str,
    ) -> Result<Vec<PathPrefixMemoryRow>, MemoryError> {
        db::list_memories_by_path_prefix(&self.conn, path_like_pattern)
    }

    /// List `memories` rows matching an exact `category` and a `path` `LIKE`
    /// pattern. Used by GC sweeps that scope by category + path namespace
    /// (e.g. handoff memos).
    pub fn list_memories_by_category_and_path_prefix(
        &self,
        category: &str,
        path_like_pattern: &str,
    ) -> Result<Vec<CategoryPathPrefixMemoryRow>, MemoryError> {
        db::list_memories_by_category_and_path_prefix(&self.conn, category, path_like_pattern)
    }
}
