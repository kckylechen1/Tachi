// lib.rs — Public API for memory-core
//
// Re-exports all primary types and provides a MemoryStore handle that
// bundles a rusqlite::Connection with convenience methods.

pub mod db;
pub mod error;
pub mod foundry;
pub mod hub;
pub mod noise;
pub mod pack;
pub mod path_router;
pub mod scorer;
pub mod search;
pub mod store;
pub mod types;
pub mod vault;

pub use db::foundry_config::{get_foundry_config, set_foundry_config, PerDbConfig};
pub use db::row_to_entry;
pub use db::foundry_jobs::{
    find_foundry_jobs_for_memory, gc_foundry_jobs, insert_foundry_job, job_status_histogram,
    load_pending_foundry_jobs, update_foundry_job_status_with_reason, FoundryJobSummary,
    JobStatusHistogram, PersistedFoundryJob,
};
pub use error::MemoryError;
pub use foundry::{
    AgentEvolutionProposal, AgentEvolutionSynthesis, AgentProfileDocument,
    AgentProfileDocumentKind, FoundryEvidence, FoundryEvidenceKind, FoundryJobKind, FoundryJobSpec,
    FoundryJobStatus, FoundryModelLane,
};
pub use hub::{HubCapability, VirtualCapabilityBinding};
pub use noise::{is_noise_text, should_skip_query};
pub use pack::{
    AgentKind, AgentProjection, Pack, PackAssetRef, PackManifest, PackManifestMeta, PackOverlay,
};
pub use scorer::HybridWeights;
pub use search::{hybrid_search, SearchOptions};
pub use types::{
    DomainConfig, GcConfig, GraphExpandResult, HybridScore, MemoryEdge, MemoryEntry,
    RetentionPolicy, SearchResult, StatsResult,
};
pub use vault::{SecretType, VaultConfig, VaultEntry, VaultKeyRotation};

use rusqlite::{Connection, OpenFlags};
use std::time::Duration;

/// Test/operator escape hatch: when set to a truthy value, the path-routing
/// validation in `MemoryStore::upsert` is bypassed entirely. Useful for test
/// fixtures that intentionally write across the canonical layout.
fn path_validation_disabled() -> bool {
    matches!(
        std::env::var("TACHI_DISABLE_PATH_VALIDATION")
            .ok()
            .as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes")
    )
}

/// High-level handle that owns a database connection.
/// Language bindings (NAPI, PyO3) will wrap this struct.
///
/// Method definitions are split across `crate::store::*` extension modules
/// (agent state, hub, sandbox, pack, vault, audit) to keep this file focused on
/// core CRUD/search/maintenance. Fields are `pub(crate)` so those sibling
/// modules can construct `MemoryStore` and access the connection directly;
/// they remain private to the crate.
pub struct MemoryStore {
    pub(crate) conn: Connection,
    pub vec_available: bool,
    /// Manifest label for this DB ("global", "wiki", a project name, or
    /// "unknown"). Used by path validation at write time.
    pub(crate) db_label: String,
    /// Whether path validation is enforced for this store. Disabled when
    /// db_label is unknown to avoid breaking unlabeled callers.
    pub(crate) path_validation: bool,
}

impl MemoryStore {
    /// Open (or create) a memory database at the given path.
    pub fn open(db_path: &str) -> Result<Self, MemoryError> {
        Self::open_with_label_inner(db_path, "unknown", false)
    }

    /// Open (or create) a memory database with a known manifest label.
    /// Enables path-routing validation at write time and runs data migrations.
    pub fn open_with_label(db_path: &str, db_label: &str) -> Result<Self, MemoryError> {
        Self::open_with_label_inner(db_path, db_label, true)
    }

    fn open_with_label_inner(
        db_path: &str,
        db_label: &str,
        path_validation: bool,
    ) -> Result<Self, MemoryError> {
        // Register extensions BEFORE opening the connection.
        libsimple::enable_auto_extension()
            .map_err(|e| MemoryError::InvalidArg(format!("simple tokenizer init: {e}")))?;
        db::register_sqlite_vec();
        let mut conn = Connection::open(db_path)?;
        conn.busy_timeout(Duration::from_millis(5_000))?;
        if path_validation {
            let p = std::path::PathBuf::from(db_path);
            let _ = db::init_schema_with_label_mut(&mut conn, db_label, &p)?;
        } else {
            db::init_schema(&conn)?;
        }
        let vec_available = db::try_load_sqlite_vec(&conn);
        Ok(Self {
            conn,
            vec_available,
            db_label: db_label.to_string(),
            path_validation,
        })
    }

    /// Open an existing memory database for pure read-only operations.
    ///
    /// This intentionally skips schema initialization because init paths can
    /// write. Use it for CLI search/stats and other diagnostics that must work
    /// even when the DB file is not writable.
    pub fn open_read_only(db_path: &str) -> Result<Self, MemoryError> {
        libsimple::enable_auto_extension()
            .map_err(|e| MemoryError::InvalidArg(format!("simple tokenizer init: {e}")))?;
        db::register_sqlite_vec();
        let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(Duration::from_millis(5_000))?;
        let vec_available = db::try_load_sqlite_vec(&conn);
        Ok(Self {
            conn,
            vec_available,
            db_label: "unknown".to_string(),
            path_validation: false,
        })
    }

    /// In-memory database (useful for tests and scripts).
    pub fn open_in_memory() -> Result<Self, MemoryError> {
        libsimple::enable_auto_extension()
            .map_err(|e| MemoryError::InvalidArg(format!("simple tokenizer init: {e}")))?;
        db::register_sqlite_vec();
        let conn = Connection::open_in_memory()?;
        conn.busy_timeout(Duration::from_millis(5_000))?;
        db::init_schema(&conn)?;
        let vec_available = db::try_load_sqlite_vec(&conn);
        Ok(Self {
            conn,
            vec_available,
            db_label: "unknown".to_string(),
            path_validation: false,
        })
    }

    /// Insert or update a memory entry (with optional embedding vector).
    pub fn upsert(&mut self, entry: &MemoryEntry) -> Result<(), MemoryError> {
        if self.path_validation && !path_validation_disabled() {
            let allow_cross = entry
                .metadata
                .get("allow_cross_project")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if let Err(e) =
                path_router::validate_path_for_db(&entry.path, &self.db_label, allow_cross)
            {
                eprintln!(
                    "warning: path-routing validation rejected write db_label={} path={} error={}",
                    self.db_label, entry.path, e
                );
                return Err(MemoryError::InvalidArg(e.to_string()));
            }
        }
        db::upsert(&mut self.conn, entry, self.vec_available)
    }

    /// Check whether an event hash has already been processed by a worker.
    pub fn is_event_processed(&self, event_hash: &str, worker: &str) -> Result<bool, MemoryError> {
        db::is_event_processed(&self.conn, event_hash, worker)
    }

    /// Mark an event hash as processed by a worker.
    pub fn mark_event_processed(
        &self,
        event_hash: &str,
        event_id: &str,
        worker: &str,
    ) -> Result<(), MemoryError> {
        db::mark_event_processed(&self.conn, event_hash, event_id, worker)
    }

    /// Atomically try to claim an event for processing.
    /// Returns true if claimed (first processor), false if already processed.
    pub fn try_claim_event(
        &self,
        event_hash: &str,
        event_id: &str,
        worker: &str,
    ) -> Result<bool, MemoryError> {
        db::try_claim_event(&self.conn, event_hash, event_id, worker)
    }

    /// Release a claimed event on processing failure (at-least-once delivery).
    pub fn release_event_claim(&self, event_hash: &str, worker: &str) -> Result<(), MemoryError> {
        db::release_event_claim(&self.conn, event_hash, worker)
    }

    /// Revision-checked update used for optimistic locking in merge flows.
    #[allow(clippy::too_many_arguments)]
    pub fn update_with_revision(
        &mut self,
        id: &str,
        new_text: &str,
        new_summary: &str,
        new_source: &str,
        new_metadata: &serde_json::Value,
        new_vec: Option<&[f32]>,
        expected_revision: i64,
    ) -> Result<bool, MemoryError> {
        let metadata_json = serde_json::to_string(new_metadata)?;
        let vec_blob = if self.vec_available {
            new_vec.map(db::serialize_f32)
        } else {
            None
        };

        db::update_with_revision(
            &mut self.conn,
            id,
            new_text,
            new_summary,
            new_source,
            &metadata_json,
            vec_blob.as_deref(),
            expected_revision,
        )
    }

    /// Update only enrichment fields (summary + vector) with revision check.
    /// Returns false if revision mismatch (entry was updated since enrichment started).
    pub fn update_enrichment_fields(
        &mut self,
        id: &str,
        new_summary: Option<&str>,
        new_vec: Option<&[f32]>,
        expected_revision: i64,
    ) -> Result<bool, MemoryError> {
        let vec_blob = if self.vec_available {
            new_vec.map(db::serialize_f32)
        } else {
            None
        };
        db::update_enrichment_fields(
            &mut self.conn,
            id,
            new_summary,
            vec_blob.as_deref(),
            expected_revision,
        )
    }

    /// List entries that are missing vector embeddings.
    /// Returns (id, text, summary, revision) tuples.
    pub fn entries_missing_vectors(
        &self,
    ) -> Result<Vec<(String, String, String, i64)>, MemoryError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, text, summary, revision FROM memories
             WHERE id NOT IN (SELECT id FROM memories_vec)
             ORDER BY rowid",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// List entries that are missing summaries.
    /// Returns (id, text, revision) tuples.
    pub fn entries_missing_summaries(&self) -> Result<Vec<(String, String, i64)>, MemoryError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, text, revision FROM memories
             WHERE trim(summary) = ''
             ORDER BY rowid",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Get total memory count and FTS index count.
    pub fn fts_stats(&self) -> Result<(i64, i64), MemoryError> {
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))?;
        let with_fts: i64 =
            self.conn
                .query_row("SELECT COUNT(DISTINCT id) FROM memories_fts", [], |r| {
                    r.get(0)
                })?;
        Ok((total, with_fts))
    }

    /// Backfill FTS index for entries missing from memories_fts.
    /// Returns the number of rows inserted.
    pub fn backfill_fts_missing(&mut self) -> Result<usize, MemoryError> {
        let inserted = self.conn.execute(
            r#"INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
               SELECT
                 id, path, summary, text,
                 trim(replace(replace(replace(keywords, '[', ' '), ']', ' '), '"', ' ')),
                 trim(replace(replace(replace(entities, '[', ' '), ']', ' '), '"', ' '))
               FROM memories
               WHERE id NOT IN (SELECT id FROM memories_fts)"#,
            [],
        )?;
        Ok(inserted)
    }

    /// Full FTS rebuild. Use this when the FTS table is stale or corrupted.
    pub fn rebuild_fts_full(&mut self) -> Result<usize, MemoryError> {
        self.conn
            .execute_batch("DROP TABLE IF EXISTS memories_fts;")?;
        self.conn.execute_batch(
            r#"CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                   id UNINDEXED,
                   path,
                   summary,
                   text,
                   keywords,
                   entities,
                   tokenize = 'simple'
               );"#,
        )?;
        let inserted = self.conn.execute(
            r#"INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
               SELECT
                 id, path, summary, text,
                 trim(replace(replace(replace(keywords, '[', ' '), ']', ' '), '"', ' ')),
                 trim(replace(replace(replace(entities, '[', ' '), ']', ' '), '"', ' '))
               FROM memories"#,
            [],
        )?;
        Ok(inserted)
    }

    /// Get total memory count and vector count.
    pub fn vector_stats(&self) -> Result<(i64, i64), MemoryError> {
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))?;
        let with_vec: i64 =
            self.conn
                .query_row("SELECT COUNT(DISTINCT id) FROM memories_vec", [], |r| {
                    r.get(0)
                })?;
        Ok((total, with_vec))
    }

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

    /// Delete a memory entry by ID. Returns true if found and deleted.
    pub fn delete(&mut self, id: &str) -> Result<bool, MemoryError> {
        db::delete(&mut self.conn, id, self.vec_available)
    }

    /// Run PRAGMA quick_check to detect database corruption early.
    /// Returns Ok(true) if healthy, Ok(false) if corrupt.
    pub fn quick_check(&self) -> Result<bool, MemoryError> {
        let result: String = self
            .conn
            .query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        Ok(result == "ok")
    }

    /// Flush SQLite state before process shutdown.
    pub fn prepare_shutdown(&self) -> Result<(), MemoryError> {
        self.conn
            .execute_batch("PRAGMA optimize;\nPRAGMA wal_checkpoint(PASSIVE);")?;
        Ok(())
    }

    /// Get aggregate statistics about the memory store.
    pub fn stats(&self, include_archived: bool) -> Result<StatsResult, MemoryError> {
        db::stats(&self.conn, include_archived)
    }

    // ─── Graph Operations ────────────────────────────────────────────────────

    /// Add or update an edge in the memory graph.
    pub fn add_edge(&self, edge: &MemoryEdge) -> Result<(), MemoryError> {
        db::add_edge(&self.conn, edge)
    }

    /// Remove a specific edge.
    pub fn remove_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relation: &str,
    ) -> Result<bool, MemoryError> {
        db::remove_edge(&self.conn, source_id, target_id, relation)
    }

    /// Get edges connected to a memory entry.
    pub fn get_edges(
        &self,
        memory_id: &str,
        direction: &str,
        relation_filter: Option<&str>,
    ) -> Result<Vec<MemoryEdge>, MemoryError> {
        db::get_edges(&self.conn, memory_id, direction, relation_filter)
    }

    /// BFS expansion from seed IDs through the memory graph.
    pub fn graph_expand(
        &self,
        seed_ids: &[String],
        max_hops: u32,
        relation_filter: Option<&str>,
    ) -> Result<GraphExpandResult, MemoryError> {
        db::graph_expand(&self.conn, seed_ids, max_hops, relation_filter)
    }

    // ─── Derived Items Operations ──────────────────────────────────────────────

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

    /// Count derived items by source and path prefix.
    pub fn count_derived_by_source(
        &self,
        source: &str,
        path_prefix: &str,
    ) -> Result<u64, MemoryError> {
        db::count_derived_by_source(&self.conn, source, path_prefix)
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

    /// Run retention-based garbage collection on growing tables.
    /// Thresholds are driven by `GcConfig` (replaces previously hardcoded literals).
    pub fn gc_tables(&mut self, cfg: &GcConfig) -> Result<serde_json::Value, MemoryError> {
        db::gc_tables(&mut self.conn, cfg)
    }

    /// Archive low-importance memories not accessed in `stale_days`.
    pub fn archive_stale_memories(&self, stale_days: u32) -> Result<u64, MemoryError> {
        db::archive_stale_memories(&self.conn, stale_days)
    }

    // ─── Domain Operations ───────────────────────────────────────────────────

    /// Register or update a domain configuration.
    pub fn register_domain(&self, domain: &DomainConfig) -> Result<(), MemoryError> {
        db::register_domain(&self.conn, domain)
    }

    /// Get a domain configuration by name.
    pub fn get_domain(&self, name: &str) -> Result<Option<DomainConfig>, MemoryError> {
        db::get_domain(&self.conn, name)
    }

    /// List all registered domain configurations.
    pub fn list_domains(&self) -> Result<Vec<DomainConfig>, MemoryError> {
        db::list_domains(&self.conn)
    }

    /// Delete a domain configuration by name. Returns true if deleted.
    pub fn delete_domain(&self, name: &str) -> Result<bool, MemoryError> {
        db::delete_domain(&self.conn, name)
    }

    // ─── Hard State Operations ────────────────────────────────────────────────

    /// Set a deterministic key-value state.
    pub fn set_state(
        &self,
        namespace: &str,
        key: &str,
        value_json: &str,
    ) -> Result<u32, MemoryError> {
        db::set_state(&self.conn, namespace, key, value_json)
    }

    /// Get a deterministic key-value state.
    pub fn get_state_kv(
        &self,
        namespace: &str,
        key: &str,
    ) -> Result<Option<(String, u32)>, MemoryError> {
        db::get_state(&self.conn, namespace, key)
    }

    // Hub, audit, agent state, sandbox, pack, and vault methods live in
    // `crate::store::*` extension modules so this file stays focused on core
    // CRUD/search/maintenance. They contribute to this same `impl MemoryStore`
    // block — the public API on `MemoryStore` is unchanged.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/facts/readonly".to_string(),
            summary: "Readonly search fixture".to_string(),
            text: "Hermes rate limit was caused by routing to the ZAI global endpoint".to_string(),
            importance: 0.8,
            timestamp: chrono::Utc::now().to_rfc3339(),
            category: "fact".to_string(),
            topic: "hermes-rate-limit".to_string(),
            keywords: vec!["hermes".to_string(), "rate-limit".to_string()],
            persons: vec![],
            entities: vec!["Hermes".to_string(), "ZAI".to_string()],
            location: String::new(),
            source: "test".to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: serde_json::json!({}),
            vector: None,
            retention_policy: Some("durable".to_string()),
            domain: Some("coding".to_string()),
        }
    }

    #[test]
    fn read_only_store_supports_stats_and_search_without_access_writes() {
        let temp = tempfile::NamedTempFile::new().expect("temp db");
        let db_path = temp.path().to_string_lossy().to_string();

        {
            let mut store = MemoryStore::open(&db_path).expect("open writable store");
            store
                .upsert(&test_entry("readonly-search"))
                .expect("seed memory");
        }

        let store = MemoryStore::open_read_only(&db_path).expect("open read-only store");
        assert_eq!(store.stats(false).expect("stats").total, 1);

        let rows = store
            .search(
                "Hermes rate limit",
                Some(SearchOptions {
                    record_access: false,
                    ..Default::default()
                }),
            )
            .expect("read-only search");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entry.id, "readonly-search");
    }
}
