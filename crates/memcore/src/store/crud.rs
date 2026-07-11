//! Core CRUD, search, and diagnostics methods on [`MemoryStore`].

use rusqlite::Connection;

use crate::{
    db,
    error::MemoryError,
    search::{hybrid_search, SearchOptions},
    types::{MemoryEntry, SearchResult, StatsResult},
    MemoryStore,
};

impl MemoryStore {
    /// Hybrid search: Text + FTS5 + optional vector channel.
    pub fn search(
        &self,
        query: &str,
        opts: Option<SearchOptions>,
    ) -> Result<Vec<SearchResult>, MemoryError> {
        let mut options = opts.unwrap_or_default();
        options.vec_available = self.vec_available;
        hybrid_search(&self.conn, query, &options)
    }

    /// Fetch a single entry by ID.
    pub fn get(&self, id: &str) -> Result<Option<MemoryEntry>, MemoryError> {
        self.get_with_options(id, false)
    }

    /// Fetch a single entry by ID with archive visibility control.
    pub fn get_with_options(
        &self,
        id: &str,
        include_archived: bool,
    ) -> Result<Option<MemoryEntry>, MemoryError> {
        let ids = vec![id.to_string()];
        let mut map = db::fetch_by_ids(&self.conn, &ids, include_archived)?;
        Ok(map.remove(id))
    }

    /// Low-level access for bindings that need raw Connection reference.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Mutable low-level access for bindings that need to open a transaction
    /// (`Connection::transaction` requires `&mut`). Used by the exec-env lease
    /// reclaim path (#894 S1), whose flip is a single atomic transaction.
    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Run a TRUNCATE WAL checkpoint to reclaim the `-wal` file.
    ///
    /// Default PASSIVE auto-checkpoints merge WAL frames into the DB but never
    /// shrink the `-wal` file, so a write burst (or long-lived readers blocking
    /// truncation) lets it balloon — observed as a 25 MB orphaned WAL on a busy
    /// agent DB. A periodic TRUNCATE checkpoint reclaims it when readers are
    /// quiet. Best-effort: returns Ok even if SQLite reports a busy checkpoint.
    pub fn checkpoint_wal_truncate(&self) -> Result<(), MemoryError> {
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(MemoryError::from)
    }

    /// Fetch multiple newest entries up to a limit (used for dedup).
    pub fn get_all(&self, limit: usize) -> Result<Vec<MemoryEntry>, MemoryError> {
        self.get_all_with_options(limit, false)
    }

    /// Fetch newest entries with archive visibility control.
    pub fn get_all_with_options(
        &self,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        db::get_all(&self.conn, limit, include_archived)
    }

    /// List entries under a path (exact + descendants) with SQL pushdown.
    pub fn list_by_path(
        &self,
        path_prefix: &str,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        db::list_by_path(&self.conn, path_prefix, limit, include_archived)
    }

    /// List entries under a path (exact + descendants), newest-first by
    /// `timestamp`. Use this instead of `list_by_path` when the caller wants
    /// a recency-first view and applies `limit` as a hard cutoff — see
    /// `list_by_path_recent`'s doc comment for why `list_by_path`'s
    /// `path ASC` primary sort can silently drop the newest rows.
    pub fn list_by_path_recent(
        &self,
        path_prefix: &str,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        db::list_by_path_recent(&self.conn, path_prefix, limit, include_archived)
    }

    /// Delete a memory entry by ID. Returns true if found and deleted.
    pub fn delete(&mut self, id: &str) -> Result<bool, MemoryError> {
        db::retry_memory_locked(|| db::delete(&mut self.conn, id, self.vec_available))
    }

    pub fn list_wiki_duplicate_candidates(
        &self,
        path: &str,
        topic: &str,
        parent_path: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        db::list_wiki_duplicate_candidates(&self.conn, path, topic, parent_path, limit)
    }

    /// Run PRAGMA quick_check to detect database corruption early.
    /// Returns Ok(true) if healthy, Ok(false) if corrupt.
    pub fn quick_check(&self) -> Result<bool, MemoryError> {
        let result: String = self
            .conn
            .query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        Ok(result == "ok")
    }

    /// Get aggregate statistics about the memory store.
    pub fn stats(&self, include_archived: bool) -> Result<StatsResult, MemoryError> {
        db::stats(&self.conn, include_archived)
    }
}
