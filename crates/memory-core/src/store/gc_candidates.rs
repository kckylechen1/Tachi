//! GC-candidate lookups (kanban/handoff sweeps) on [`MemoryStore`].

use crate::{db, error::MemoryError, MemoryStore};

pub use crate::db::{CategoryPathPrefixMemoryRow, PathPrefixMemoryRow};

impl MemoryStore {
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
