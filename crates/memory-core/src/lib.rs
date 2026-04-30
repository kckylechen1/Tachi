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
pub mod scorer;
pub mod search;
pub mod types;
pub mod vault;

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
pub use db::foundry_jobs::{
    find_foundry_jobs_for_memory, gc_foundry_jobs, insert_foundry_job, job_status_histogram,
    load_pending_foundry_jobs, update_foundry_job_status_with_reason,
    FoundryJobSummary, JobStatusHistogram, PersistedFoundryJob,
};

use rusqlite::{Connection, OpenFlags};
use std::time::Duration;

/// High-level handle that owns a database connection.
/// Language bindings (NAPI, PyO3) will wrap this struct.
pub struct MemoryStore {
    conn: Connection,
    pub vec_available: bool,
}

impl MemoryStore {
    /// Open (or create) a memory database at the given path.
    pub fn open(db_path: &str) -> Result<Self, MemoryError> {
        // Register extensions BEFORE opening the connection.
        libsimple::enable_auto_extension()
            .map_err(|e| MemoryError::InvalidArg(format!("simple tokenizer init: {e}")))?;
        db::register_sqlite_vec();
        let conn = Connection::open(db_path)?;
        conn.busy_timeout(Duration::from_millis(5_000))?;
        db::init_schema(&conn)?;
        let vec_available = db::try_load_sqlite_vec(&conn);
        Ok(Self {
            conn,
            vec_available,
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
        })
    }

    /// Insert or update a memory entry (with optional embedding vector).
    pub fn upsert(&mut self, entry: &MemoryEntry) -> Result<(), MemoryError> {
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
        let with_fts: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT id) FROM memories_fts",
            [],
            |r| r.get(0),
        )?;
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

    // ─── Hub Operations ──────────────────────────────────────────────────────

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

    // ─── Audit Log Operations ────────────────────────────────────────────────

    /// Insert an audit log entry.
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

    // ─── Ghost Persistence Operations ─────────────────────────────────────────

    /// Persist one ghost message and advance topic index.
    pub fn ghost_publish_message(
        &mut self,
        id: &str,
        topic: &str,
        payload_json: &str,
        publisher: &str,
        timestamp: &str,
    ) -> Result<u64, MemoryError> {
        db::ghost_publish_message(
            &mut self.conn,
            id,
            topic,
            payload_json,
            publisher,
            timestamp,
        )
    }

    /// Fetch topic messages after cursor index.
    pub fn ghost_fetch_messages_since(
        &self,
        topic: &str,
        after_index: u64,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>, MemoryError> {
        db::ghost_fetch_messages_since(&self.conn, topic, after_index, limit)
    }

    /// Upsert one subscription row.
    pub fn ghost_upsert_subscription(
        &self,
        agent_id: &str,
        topic: &str,
    ) -> Result<(), MemoryError> {
        db::ghost_upsert_subscription(&self.conn, agent_id, topic)
    }

    /// Read one agent-topic cursor.
    pub fn ghost_get_cursor(&self, agent_id: &str, topic: &str) -> Result<u64, MemoryError> {
        db::ghost_get_cursor(&self.conn, agent_id, topic)
    }

    /// Upsert one agent-topic cursor.
    pub fn ghost_set_cursor(
        &self,
        agent_id: &str,
        topic: &str,
        last_seen_index: u64,
    ) -> Result<(), MemoryError> {
        db::ghost_set_cursor(&self.conn, agent_id, topic, last_seen_index)
    }

    /// Resolve one message id to topic/index.
    pub fn ghost_get_message_topic_index(
        &self,
        message_id: &str,
    ) -> Result<Option<(String, u64)>, MemoryError> {
        db::ghost_get_message_topic_index(&self.conn, message_id)
    }

    /// Read total published count for topic.
    pub fn ghost_get_topic_total(&self, topic: &str) -> Result<u64, MemoryError> {
        db::ghost_get_topic_total(&self.conn, topic)
    }

    /// List active ghost topics.
    pub fn ghost_list_topics(&self, limit: usize) -> Result<Vec<serde_json::Value>, MemoryError> {
        db::ghost_list_topics(&self.conn, limit)
    }

    /// Insert one reflection row.
    pub fn ghost_insert_reflection(
        &self,
        id: &str,
        agent_id: &str,
        topic: Option<&str>,
        summary: &str,
        metadata_json: &str,
        timestamp: &str,
    ) -> Result<(), MemoryError> {
        db::ghost_insert_reflection(
            &self.conn,
            id,
            agent_id,
            topic,
            summary,
            metadata_json,
            timestamp,
        )
    }

    /// Fetch one ghost message by id.
    pub fn ghost_get_message(
        &self,
        message_id: &str,
    ) -> Result<Option<serde_json::Value>, MemoryError> {
        db::ghost_get_message(&self.conn, message_id)
    }

    /// Mark ghost message promoted.
    pub fn ghost_mark_message_promoted(
        &self,
        message_id: &str,
        importance: Option<f64>,
    ) -> Result<bool, MemoryError> {
        db::ghost_mark_message_promoted(&self.conn, message_id, importance)
    }

    // ─── Agent Known State (Context Diffing) ─────────────────────────────────

    /// Get the known revisions for a set of memory IDs for a given agent.
    pub fn get_agent_known_revisions(
        &self,
        agent_id: &str,
        memory_ids: &[String],
    ) -> Result<std::collections::HashMap<String, i64>, MemoryError> {
        db::get_agent_known_revisions(&self.conn, agent_id, memory_ids)
    }

    /// Update the agent's known state for a set of memory entries.
    pub fn update_agent_known_state(
        &self,
        agent_id: &str,
        entries: &[(String, i64)],
    ) -> Result<(), MemoryError> {
        db::update_agent_known_state(&self.conn, agent_id, entries)
    }

    // ─── Semantic Sandboxing ─────────────────────────────────────────────────

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
    // Sandbox enforcement is implemented in memory-server's handle_search_memory
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

    // ─── Pack Operations ─────────────────────────────────────────────────────

    /// Register or update a pack in the registry.
    pub fn pack_register(&self, pack: &Pack) -> Result<(), MemoryError> {
        db::pack_upsert(&self.conn, pack)
    }

    /// Get a pack by ID.
    pub fn pack_get(&self, id: &str) -> Result<Option<Pack>, MemoryError> {
        db::pack_get(&self.conn, id)
    }

    /// List all packs, optionally filtering by enabled status.
    pub fn pack_list(&self, enabled_only: bool) -> Result<Vec<Pack>, MemoryError> {
        db::pack_list(&self.conn, enabled_only)
    }

    /// Delete a pack by ID (also removes associated projections).
    pub fn pack_delete(&self, id: &str) -> Result<bool, MemoryError> {
        db::pack_delete(&self.conn, id)
    }

    /// Enable or disable a pack.
    pub fn pack_set_enabled(&self, id: &str, enabled: bool) -> Result<bool, MemoryError> {
        db::pack_set_enabled(&self.conn, id, enabled)
    }

    /// Upsert an agent projection record.
    pub fn projection_upsert(&self, proj: &AgentProjection) -> Result<(), MemoryError> {
        db::projection_upsert(&self.conn, proj)
    }

    /// List agent projections, optionally filtering by agent and/or pack_id.
    pub fn projection_list(
        &self,
        agent: Option<&str>,
        pack_id: Option<&str>,
    ) -> Result<Vec<AgentProjection>, MemoryError> {
        db::projection_list(&self.conn, agent, pack_id)
    }

    /// Delete an agent projection.
    pub fn projection_delete(&self, agent: &str, pack_id: &str) -> Result<bool, MemoryError> {
        db::projection_delete(&self.conn, agent, pack_id)
    }

    // Vault operations

    /// Get vault configuration (returns None if not initialized).
    pub fn vault_get_config(&self) -> Result<Option<vault::VaultConfig>, MemoryError> {
        db::vault_get_config(&self.conn)
    }

    /// Set vault configuration.
    pub fn vault_set_config(&self, config: &vault::VaultConfig) -> Result<(), MemoryError> {
        db::vault_set_config(&self.conn, config)
    }

    /// Store or update an encrypted secret.
    pub fn vault_upsert_entry(&self, entry: &vault::VaultEntry) -> Result<(), MemoryError> {
        db::vault_upsert_entry(&self.conn, entry)
    }

    /// Get a secret by name (returns None if not found).
    pub fn vault_get_entry(&self, name: &str) -> Result<Option<vault::VaultEntry>, MemoryError> {
        db::vault_get_entry(&self.conn, name)
    }

    /// List all secrets.
    pub fn vault_list_entries(&self) -> Result<Vec<vault::VaultEntry>, MemoryError> {
        db::vault_list_entries(&self.conn)
    }

    /// List secrets filtered by type.
    pub fn vault_list_entries_by_type(
        &self,
        secret_type: &str,
    ) -> Result<Vec<vault::VaultEntry>, MemoryError> {
        db::vault_list_entries_by_type(&self.conn, secret_type)
    }

    /// Delete a secret by name. Returns true if deleted, false if not found.
    pub fn vault_delete_entry(&self, name: &str) -> Result<bool, MemoryError> {
        db::vault_delete_entry(&self.conn, name)
    }

    /// Touch a secret (update accessed_at and increment access_count).
    pub fn vault_touch_entry(&self, name: &str) -> Result<(), MemoryError> {
        db::vault_touch_entry(&self.conn, name)
    }

    /// Insert a vault audit record.
    pub fn vault_insert_audit(
        &self,
        timestamp: &str,
        operation: &str,
        secret_name: Option<&str>,
        success: bool,
        detail: Option<&str>,
    ) -> Result<(), MemoryError> {
        db::vault_insert_audit(
            &self.conn,
            timestamp,
            operation,
            secret_name,
            success,
            detail,
        )
    }

    /// Count total number of secrets.
    pub fn vault_count_entries(&self) -> Result<i64, MemoryError> {
        db::vault_count_entries(&self.conn)
    }

    /// Check if a secret exists.
    pub fn vault_entry_exists(&self, name: &str) -> Result<bool, MemoryError> {
        db::vault_entry_exists(&self.conn, name)
    }

    // Key rotation operations

    /// Get rotation state for a prefix.
    pub fn vault_get_rotation(
        &self,
        prefix: &str,
    ) -> Result<Option<vault::VaultKeyRotation>, MemoryError> {
        db::vault_get_rotation(&self.conn, prefix)
    }

    /// Set rotation state for a prefix.
    pub fn vault_set_rotation(
        &self,
        rotation: &vault::VaultKeyRotation,
    ) -> Result<(), MemoryError> {
        db::vault_set_rotation(&self.conn, rotation)
    }

    /// List all rotation configurations.
    pub fn vault_list_rotations(&self) -> Result<Vec<vault::VaultKeyRotation>, MemoryError> {
        db::vault_list_rotations(&self.conn)
    }

    /// Delete rotation configuration for a prefix.
    pub fn vault_delete_rotation(&self, prefix: &str) -> Result<bool, MemoryError> {
        db::vault_delete_rotation(&self.conn, prefix)
    }
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
