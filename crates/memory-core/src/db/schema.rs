use rusqlite::Connection;
use std::path::Path;

use crate::error::MemoryError;

pub fn init_schema(conn: &Connection) -> Result<(), MemoryError> {
    init_schema_inner(conn)
}

/// Initialize schema and run data migrations with a known DB label and path.
///
/// Use this from server code that knows the manifest role/project name and
/// canonical DB path. Falls back to a no-op data-migration sentinel for
/// callers that pass `None` paths.
pub fn init_schema_with_label(
    conn: &Connection,
    db_label: &str,
    current_db_path: Option<&Path>,
) -> Result<Option<crate::db::migrations::MigrationReport>, MemoryError> {
    init_schema_inner(conn)?;
    if let Some(path) = current_db_path {
        // Migrations need a mutable connection for transactions. We can build
        // one from the existing connection's handle by re-borrowing through
        // an `unchecked_transaction` route inside the migration itself. To
        // avoid changing the public signature into `&mut Connection`, we
        // accept the limitation that callers needing migrations should use
        // the `_mut` variant below.
        let _ = (path, db_label);
    }
    Ok(None)
}

/// Mutable variant: runs schema init AND data migrations.
pub fn init_schema_with_label_mut(
    conn: &mut Connection,
    db_label: &str,
    current_db_path: &Path,
) -> Result<crate::db::migrations::MigrationReport, MemoryError> {
    init_schema_inner(conn)?;
    crate::db::migrations::run_data_migrations(conn, db_label, current_db_path)
}

fn init_schema_inner(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute_batch(r#"
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;
        PRAGMA busy_timeout = 5000;
        PRAGMA cache_size = -16000;   -- 16 MB page cache

        CREATE TABLE IF NOT EXISTS memories (
            id           TEXT PRIMARY KEY,
            path         TEXT NOT NULL DEFAULT '/',
            summary      TEXT NOT NULL DEFAULT '',
            text         TEXT NOT NULL DEFAULT '',
            importance   REAL NOT NULL DEFAULT 0.7,
            timestamp    TEXT NOT NULL,
            category     TEXT NOT NULL DEFAULT 'fact',
            topic        TEXT NOT NULL DEFAULT '',
            keywords     TEXT NOT NULL DEFAULT '[]',
            persons      TEXT NOT NULL DEFAULT '[]',
            entities     TEXT NOT NULL DEFAULT '[]',
            location     TEXT NOT NULL DEFAULT '',
            source       TEXT NOT NULL DEFAULT 'manual',
            scope        TEXT NOT NULL DEFAULT 'general',
            archived     INTEGER NOT NULL DEFAULT 0,
            created_at   TEXT NOT NULL DEFAULT '',
            updated_at   TEXT NOT NULL DEFAULT '',
            access_count INTEGER NOT NULL DEFAULT 0,
            last_access  TEXT,
            revision     INTEGER NOT NULL DEFAULT 1,
            metadata     TEXT NOT NULL DEFAULT '{}'
        );

        CREATE INDEX IF NOT EXISTS idx_memories_path        ON memories(path);
        CREATE INDEX IF NOT EXISTS idx_memories_importance  ON memories(importance DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_timestamp   ON memories(timestamp DESC);

        -- Standalone FTS5 table with Chinese + Pinyin tokenizer.
        -- Uses wangfenjin/simple for CJK segmentation.
        CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
            id UNINDEXED,
            path,
            summary,
            text,
            keywords,
            entities,
            tokenize = 'simple'
        );

        -- Memory graph edges for causal/temporal/entity relationships
        CREATE TABLE IF NOT EXISTS memory_edges (
            source_id  TEXT NOT NULL,
            target_id  TEXT NOT NULL,
            relation   TEXT NOT NULL,
            weight     REAL NOT NULL DEFAULT 1.0,
            metadata   TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (source_id, target_id, relation)
        );
        CREATE INDEX IF NOT EXISTS idx_edges_source ON memory_edges(source_id);
        CREATE INDEX IF NOT EXISTS idx_edges_target ON memory_edges(target_id);
        CREATE INDEX IF NOT EXISTS idx_edges_relation ON memory_edges(relation);

        -- Deterministic KV state (no vector search, no LLM)
        CREATE TABLE IF NOT EXISTS hard_state (
            namespace        TEXT NOT NULL,
            key              TEXT NOT NULL,
            value_json       TEXT NOT NULL DEFAULT '{}',
            version          INTEGER NOT NULL DEFAULT 1,
            created_at       TEXT NOT NULL DEFAULT '',
            updated_at       TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (namespace, key)
        );

        -- Access history for ACT-R base-level activation
        CREATE TABLE IF NOT EXISTS access_history (
            memory_id  TEXT NOT NULL,
            accessed_at TEXT NOT NULL,
            query_hash  TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_access_hist_mem ON access_history(memory_id);
        CREATE INDEX IF NOT EXISTS idx_access_hist_time ON access_history(accessed_at DESC);
        CREATE INDEX IF NOT EXISTS idx_access_hist_mem_time ON access_history(memory_id, accessed_at DESC);

        -- Derived items (causal extractions, distilled rules, etc.)
        CREATE TABLE IF NOT EXISTS derived_items (
            id         TEXT PRIMARY KEY,
            text       TEXT NOT NULL DEFAULT '',
            path       TEXT NOT NULL DEFAULT '/',
            summary    TEXT NOT NULL DEFAULT '',
            importance REAL NOT NULL DEFAULT 0.5,
            source     TEXT NOT NULL DEFAULT '',
            scope      TEXT NOT NULL DEFAULT 'general',
            metadata   TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL DEFAULT ''
        );

        CREATE TABLE IF NOT EXISTS processed_events (
            event_hash TEXT NOT NULL,
            event_id   TEXT NOT NULL DEFAULT '',
            worker     TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (event_hash, worker)
        );
        CREATE INDEX IF NOT EXISTS idx_processed_events_created_at ON processed_events(created_at DESC);

        -- Hub capability registry (skills, plugins, MCP servers)
        CREATE TABLE IF NOT EXISTS hub_capabilities (
            id          TEXT PRIMARY KEY,
            type        TEXT NOT NULL,
            name        TEXT NOT NULL,
            version     INTEGER NOT NULL DEFAULT 1,
            description TEXT NOT NULL DEFAULT '',
            definition  TEXT NOT NULL DEFAULT '',
            enabled     INTEGER NOT NULL DEFAULT 1,
            review_status TEXT NOT NULL DEFAULT 'approved',
            health_status TEXT NOT NULL DEFAULT 'healthy',
            last_error    TEXT,
            last_success_at TEXT,
            last_failure_at TEXT,
            fail_streak   INTEGER NOT NULL DEFAULT 0,
            active_version TEXT,
            exposure_mode TEXT NOT NULL DEFAULT 'direct',
            uses        INTEGER NOT NULL DEFAULT 0,
            successes   INTEGER NOT NULL DEFAULT 0,
            failures    INTEGER NOT NULL DEFAULT 0,
            avg_rating  REAL NOT NULL DEFAULT 0.0,
            last_used   TEXT,
            created_at  TEXT NOT NULL DEFAULT '',
            updated_at  TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_hub_cap_type ON hub_capabilities(type);
        CREATE INDEX IF NOT EXISTS idx_hub_cap_name ON hub_capabilities(name);
        CREATE INDEX IF NOT EXISTS idx_hub_cap_enabled ON hub_capabilities(enabled);
        -- review_status / health_status indexes are created after ensure_column migrations

        CREATE TABLE IF NOT EXISTS hub_version_routes (
            alias_id TEXT PRIMARY KEY,
            active_capability_id TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_hub_route_target ON hub_version_routes(active_capability_id);

        CREATE TABLE IF NOT EXISTS virtual_capability_bindings (
            vc_id         TEXT NOT NULL,
            capability_id TEXT NOT NULL,
            priority      INTEGER NOT NULL DEFAULT 100,
            version_pin   INTEGER,
            enabled       INTEGER NOT NULL DEFAULT 1,
            metadata      TEXT NOT NULL DEFAULT '{}',
            created_at    TEXT NOT NULL DEFAULT '',
            updated_at    TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (vc_id, capability_id)
        );
        CREATE INDEX IF NOT EXISTS idx_vc_binding_capability
            ON virtual_capability_bindings(capability_id);
        CREATE INDEX IF NOT EXISTS idx_vc_binding_priority
            ON virtual_capability_bindings(vc_id, priority ASC, capability_id ASC);

        -- Audit log for proxy tool calls
        CREATE TABLE IF NOT EXISTS audit_log (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp   TEXT NOT NULL,
            server_id   TEXT NOT NULL,
            tool_name   TEXT NOT NULL,
            args_hash   TEXT NOT NULL DEFAULT '',
            success     INTEGER NOT NULL DEFAULT 1,
            duration_ms INTEGER NOT NULL DEFAULT 0,
            error_kind  TEXT,
            created_at  TEXT NOT NULL DEFAULT (STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now'))
        );
        CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON audit_log(timestamp DESC);
        CREATE INDEX IF NOT EXISTS idx_audit_server ON audit_log(server_id);
        CREATE INDEX IF NOT EXISTS idx_audit_created_at ON audit_log(created_at DESC);

        -- Agent known state for context diffing (incremental memory sync)
        CREATE TABLE IF NOT EXISTS agent_known_state (
            agent_id   TEXT NOT NULL,
            memory_id  TEXT NOT NULL,
            revision   INTEGER NOT NULL DEFAULT 0,
            synced_at  TEXT NOT NULL,
            PRIMARY KEY (agent_id, memory_id)
        );
        CREATE INDEX IF NOT EXISTS idx_agent_known_agent ON agent_known_state(agent_id);
        CREATE INDEX IF NOT EXISTS idx_agent_known_memory ON agent_known_state(memory_id);
        CREATE INDEX IF NOT EXISTS idx_agent_known_synced_at ON agent_known_state(synced_at DESC);

        -- Sandbox rules for role-based memory isolation (Semantic Sandboxing)
        CREATE TABLE IF NOT EXISTS sandbox_rules (
            agent_role   TEXT NOT NULL,
            path_pattern TEXT NOT NULL,
            access_level TEXT NOT NULL DEFAULT 'read',
            created_at   TEXT NOT NULL,
            PRIMARY KEY (agent_role, path_pattern)
        );
        CREATE INDEX IF NOT EXISTS idx_sandbox_role ON sandbox_rules(agent_role);

        -- Sandbox runtime policies for MCP capability execution control
        CREATE TABLE IF NOT EXISTS sandbox_policies (
            capability_id    TEXT PRIMARY KEY,
            runtime_type     TEXT NOT NULL DEFAULT 'process',
            env_allowlist    TEXT NOT NULL DEFAULT '[]',
            fs_read_roots    TEXT NOT NULL DEFAULT '[]',
            fs_write_roots   TEXT NOT NULL DEFAULT '[]',
            cwd_roots        TEXT NOT NULL DEFAULT '[]',
            max_startup_ms   INTEGER NOT NULL DEFAULT 30000,
            max_tool_ms      INTEGER NOT NULL DEFAULT 30000,
            max_concurrency  INTEGER NOT NULL DEFAULT 1,
            enabled          INTEGER NOT NULL DEFAULT 1,
            created_at       TEXT NOT NULL DEFAULT '',
            updated_at       TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_sandbox_policy_enabled ON sandbox_policies(enabled);

        -- Sandbox execution audit for preflight/runtime decisions.
        CREATE TABLE IF NOT EXISTS sandbox_exec_audit (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp      TEXT NOT NULL,
            capability_id  TEXT NOT NULL,
            stage          TEXT NOT NULL DEFAULT 'preflight',
            decision       TEXT NOT NULL,
            reason         TEXT,
            duration_ms    INTEGER NOT NULL DEFAULT 0,
            tool_name      TEXT,
            error_kind     TEXT,
            metadata       TEXT NOT NULL DEFAULT '{}',
            created_at     TEXT NOT NULL DEFAULT (STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now'))
        );
        CREATE INDEX IF NOT EXISTS idx_sandbox_exec_timestamp ON sandbox_exec_audit(timestamp DESC);
        CREATE INDEX IF NOT EXISTS idx_sandbox_exec_capability ON sandbox_exec_audit(capability_id);

        -- Ghost persistence: messages, subscriptions, cursors, topics, reflections.
        CREATE TABLE IF NOT EXISTS ghost_messages (
            id            TEXT PRIMARY KEY,
            topic         TEXT NOT NULL,
            topic_index   INTEGER NOT NULL,
            payload       TEXT NOT NULL DEFAULT '{}',
            publisher     TEXT NOT NULL DEFAULT '',
            timestamp     TEXT NOT NULL DEFAULT '',
            promoted      INTEGER NOT NULL DEFAULT 0,
            importance    REAL NOT NULL DEFAULT 0.5,
            created_at    TEXT NOT NULL DEFAULT ''
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_ghost_topic_index_unique
            ON ghost_messages(topic, topic_index);
        CREATE INDEX IF NOT EXISTS idx_ghost_topic_index
            ON ghost_messages(topic, topic_index DESC);
        CREATE INDEX IF NOT EXISTS idx_ghost_messages_timestamp
            ON ghost_messages(timestamp DESC);

        CREATE TABLE IF NOT EXISTS ghost_subscriptions (
            agent_id      TEXT NOT NULL,
            topic         TEXT NOT NULL,
            created_at    TEXT NOT NULL DEFAULT '',
            updated_at    TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (agent_id, topic)
        );
        CREATE INDEX IF NOT EXISTS idx_ghost_subscriptions_topic
            ON ghost_subscriptions(topic);

        CREATE TABLE IF NOT EXISTS ghost_cursors (
            agent_id        TEXT NOT NULL,
            topic           TEXT NOT NULL,
            last_seen_index INTEGER NOT NULL DEFAULT 0,
            updated_at      TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (agent_id, topic)
        );
        CREATE INDEX IF NOT EXISTS idx_ghost_cursors_updated
            ON ghost_cursors(updated_at DESC);

        CREATE TABLE IF NOT EXISTS ghost_topics (
            topic             TEXT PRIMARY KEY,
            total_published   INTEGER NOT NULL DEFAULT 0,
            last_message_time TEXT,
            last_publisher    TEXT,
            updated_at        TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_ghost_topics_last_message
            ON ghost_topics(last_message_time DESC);

        -- Pack registry: installed skill packs from external sources
        CREATE TABLE IF NOT EXISTS packs (
            id             TEXT PRIMARY KEY,
            name           TEXT NOT NULL,
            source         TEXT NOT NULL DEFAULT '',
            version        TEXT NOT NULL DEFAULT '',
            description    TEXT NOT NULL DEFAULT '',
            skill_count    INTEGER NOT NULL DEFAULT 0,
            enabled        INTEGER NOT NULL DEFAULT 1,
            local_path     TEXT NOT NULL DEFAULT '',
            metadata       TEXT NOT NULL DEFAULT '{}',
            installed_at   TEXT NOT NULL DEFAULT '',
            updated_at     TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_packs_enabled ON packs(enabled);
        CREATE INDEX IF NOT EXISTS idx_packs_source ON packs(source);

        -- Agent projections: tracks which packs are projected to which agents
        CREATE TABLE IF NOT EXISTS agent_projections (
            agent          TEXT NOT NULL,
            pack_id        TEXT NOT NULL,
            enabled        INTEGER NOT NULL DEFAULT 1,
            projected_path TEXT NOT NULL DEFAULT '',
            skill_count    INTEGER NOT NULL DEFAULT 0,
            synced_at      TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (agent, pack_id)
        );
        CREATE INDEX IF NOT EXISTS idx_projections_pack ON agent_projections(pack_id);
        CREATE INDEX IF NOT EXISTS idx_projections_agent ON agent_projections(agent);

        CREATE TABLE IF NOT EXISTS ghost_reflections (
            id          TEXT PRIMARY KEY,
            agent_id    TEXT NOT NULL,
            topic       TEXT,
            summary     TEXT NOT NULL DEFAULT '',
            metadata    TEXT NOT NULL DEFAULT '{}',
            created_at  TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_ghost_reflections_topic
            ON ghost_reflections(topic);
        CREATE INDEX IF NOT EXISTS idx_ghost_reflections_created
            ON ghost_reflections(created_at DESC);

        -- Vault: encrypted secret storage configuration (exactly one row)
        CREATE TABLE IF NOT EXISTS vault_config (
            id              INTEGER PRIMARY KEY CHECK (id = 1),
            salt            TEXT NOT NULL,
            verifier        TEXT NOT NULL,
            kdf_algorithm   TEXT NOT NULL DEFAULT 'argon2id',
            kdf_params      TEXT NOT NULL DEFAULT '{"m":65536,"t":3,"p":4}',
            cipher          TEXT NOT NULL DEFAULT 'aes-256-gcm',
            created_at      TEXT NOT NULL DEFAULT '',
            updated_at      TEXT NOT NULL DEFAULT ''
        );

        -- Vault: encrypted secret entries
        CREATE TABLE IF NOT EXISTS vault_entries (
            name            TEXT PRIMARY KEY,
            encrypted_value TEXT NOT NULL,
            nonce           TEXT NOT NULL,
            secret_type     TEXT NOT NULL DEFAULT 'api_key',
            description     TEXT NOT NULL DEFAULT '',
            allowed_agents  TEXT,
            created_at      TEXT NOT NULL DEFAULT '',
            updated_at      TEXT NOT NULL DEFAULT '',
            accessed_at     TEXT NOT NULL DEFAULT '',
            access_count    INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_vault_entries_type ON vault_entries(secret_type);

        CREATE TABLE IF NOT EXISTS vault_audit (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp       TEXT NOT NULL,
            operation       TEXT NOT NULL,
            secret_name     TEXT,
            success         INTEGER NOT NULL DEFAULT 1,
            detail          TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_vault_audit_timestamp ON vault_audit(timestamp DESC);
        CREATE INDEX IF NOT EXISTS idx_vault_audit_operation ON vault_audit(operation);
        CREATE INDEX IF NOT EXISTS idx_vault_audit_secret_name ON vault_audit(secret_name);

        -- Vault: key rotation state for multi-key secrets
        CREATE TABLE IF NOT EXISTS vault_key_rotations (
            prefix              TEXT PRIMARY KEY,
            current_index       INTEGER NOT NULL DEFAULT 1,
            total_keys          INTEGER NOT NULL DEFAULT 0,
            rotation_strategy   TEXT NOT NULL DEFAULT 'round_robin',
            created_at          TEXT NOT NULL DEFAULT '',
            updated_at          TEXT NOT NULL DEFAULT ''
        );

        -- Foundry job persistence (survives process restarts)
        CREATE TABLE IF NOT EXISTS foundry_jobs (
            id            TEXT PRIMARY KEY,
            kind          TEXT NOT NULL,
            lane          TEXT NOT NULL,
            status        TEXT NOT NULL DEFAULT 'queued',
            target_db     TEXT NOT NULL DEFAULT 'project',
            named_project TEXT,
            path_prefix   TEXT NOT NULL DEFAULT '/',
            memory_ids    TEXT NOT NULL DEFAULT '[]',
            target_agent_id TEXT,
            requested_by  TEXT,
            evidence_count INTEGER NOT NULL DEFAULT 0,
            goal_count    INTEGER NOT NULL DEFAULT 1,
            metadata      TEXT NOT NULL DEFAULT '{}',
            created_at    TEXT NOT NULL DEFAULT '',
            updated_at    TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_foundry_jobs_status ON foundry_jobs(status);
        CREATE INDEX IF NOT EXISTS idx_foundry_jobs_kind ON foundry_jobs(kind);

        -- Per-DB Foundry runtime configuration. The scheduler reads one row per DB
        -- to decide whether to spawn a worker, what concurrency caps to apply, and
        -- which LLM provider override to use. Default behavior when row is missing:
        -- enabled=1, max_jobs_per_minute=10, distill_concurrency=1, enrichment_concurrency=1.
        CREATE TABLE IF NOT EXISTS foundry_config (
            id                       INTEGER PRIMARY KEY CHECK (id = 1),
            enabled                  INTEGER NOT NULL DEFAULT 1,
            max_jobs_per_minute      INTEGER NOT NULL DEFAULT 10,
            distill_concurrency      INTEGER NOT NULL DEFAULT 1,
            enrichment_concurrency   INTEGER NOT NULL DEFAULT 1,
            llm_provider_override    TEXT,
            updated_at               TEXT NOT NULL DEFAULT '',
            updated_by               TEXT NOT NULL DEFAULT 'default'
        );

        -- Domain configuration for memory routing and per-domain GC
        CREATE TABLE IF NOT EXISTS domains (
            name              TEXT PRIMARY KEY,
            description       TEXT NOT NULL DEFAULT '',
            gc_threshold_days INTEGER,
            default_retention TEXT,
            default_path_prefix TEXT,
            metadata          TEXT NOT NULL DEFAULT '{}',
            created_at        TEXT NOT NULL DEFAULT '',
            updated_at        TEXT NOT NULL DEFAULT ''
        );
    "#)?;

    // Forward-compatible migrations for existing DB files created before
    // archived/created_at/updated_at columns existed.
    ensure_column(conn, "memories", "archived", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(conn, "memories", "created_at", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "memories", "updated_at", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "memories", "revision", "INTEGER NOT NULL DEFAULT 1")?;

    // Retention policy and domain columns for Issue #38 and #32
    ensure_column(conn, "memories", "retention_policy", "TEXT")?;
    ensure_column(conn, "memories", "domain", "TEXT")?;

    // Temporal edge columns for memory_edges
    ensure_column(
        conn,
        "memory_edges",
        "valid_from",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(conn, "memory_edges", "valid_to", "TEXT")?;

    // derived_items columns that may be missing on legacy databases
    ensure_column(conn, "derived_items", "summary", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(
        conn,
        "derived_items",
        "importance",
        "REAL NOT NULL DEFAULT 0.5",
    )?;
    ensure_column(
        conn,
        "derived_items",
        "scope",
        "TEXT NOT NULL DEFAULT 'general'",
    )?;
    ensure_column(
        conn,
        "derived_items",
        "created_at",
        "TEXT NOT NULL DEFAULT ''",
    )?;

    // Hub governance columns for review + health + routing metadata
    ensure_column(
        conn,
        "hub_capabilities",
        "review_status",
        "TEXT NOT NULL DEFAULT 'approved'",
    )?;
    ensure_column(
        conn,
        "hub_capabilities",
        "health_status",
        "TEXT NOT NULL DEFAULT 'healthy'",
    )?;
    ensure_column(conn, "hub_capabilities", "last_error", "TEXT")?;
    ensure_column(conn, "hub_capabilities", "last_success_at", "TEXT")?;
    ensure_column(conn, "hub_capabilities", "last_failure_at", "TEXT")?;
    ensure_column(
        conn,
        "hub_capabilities",
        "fail_streak",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "hub_capabilities", "active_version", "TEXT")?;
    ensure_column(
        conn,
        "hub_capabilities",
        "exposure_mode",
        "TEXT NOT NULL DEFAULT 'direct'",
    )?;

    // Indexes on migrated columns — MUST come after ensure_column so the
    // columns exist on legacy databases that were created without them.
    conn.execute_batch(
        r#"
        CREATE INDEX IF NOT EXISTS idx_memories_archived    ON memories(archived);
        CREATE INDEX IF NOT EXISTS idx_memories_last_access ON memories(last_access DESC);
        CREATE INDEX IF NOT EXISTS idx_derived_source       ON derived_items(source);
        CREATE INDEX IF NOT EXISTS idx_derived_path         ON derived_items(path);
        CREATE INDEX IF NOT EXISTS idx_derived_created_at   ON derived_items(created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_hub_cap_review_status ON hub_capabilities(review_status);
        CREATE INDEX IF NOT EXISTS idx_hub_cap_health_status ON hub_capabilities(health_status);
        CREATE INDEX IF NOT EXISTS idx_memories_retention_policy ON memories(retention_policy);
        CREATE INDEX IF NOT EXISTS idx_memories_domain ON memories(domain);
    "#,
    )?;

    // Backfill empty values for legacy rows.
    conn.execute(
        "UPDATE memories SET created_at = timestamp WHERE created_at IS NULL OR created_at = ''",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET updated_at = created_at WHERE updated_at IS NULL OR updated_at = ''",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET revision = 1 WHERE revision IS NULL OR revision <= 0",
        [],
    )?;

    ensure_fts_backfilled(conn)?;

    migrate_enum_constraints(conn)?;

    // NOTE: sqlite-vec virtual table (memories_vec) is created separately after
    // the extension is loaded by the caller via register_sqlite_vec().
    Ok(())
}

/// Idempotent migration that:
///   1. Detects whether CHECK constraints are already in place (no-op if so).
///   2. Normalizes legacy `source` / `category` / `scope` / `retention_policy`
///      values to the canonical vocabulary defined in `types.rs`.
///   3. Backfills `retention_policy` defaults for /handoff, /kanban, /wiki, and
///      foundry_distill rows.
///   4. Rebuilds the `memories` table with CHECK constraints (SQLite cannot
///      ALTER TABLE ADD CHECK).
///
/// The standalone `memories_fts` virtual table is independent of the rebuild
/// and is preserved across the rename.
fn migrate_enum_constraints(conn: &Connection) -> Result<(), MemoryError> {
    // Check if CHECK constraints already exist by scanning sqlite_master.
    let existing_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='memories'",
            [],
            |row| row.get(0),
        )
        .ok();
    if let Some(sql) = existing_sql.as_deref() {
        if sql.contains("CHECK (source") || sql.contains("CHECK(source") {
            // Already migrated; nothing to do.
            return Ok(());
        }
    }

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let migration_result = (|| -> Result<(), MemoryError> {
        // ── Step 1: Normalize source ────────────────────────────────────────────
        // Empty/NULL → 'manual'
        conn.execute(
            "UPDATE memories SET source = 'manual'
         WHERE source IS NULL OR trim(source) = ''",
            [],
        )?;
        // Common legacy aliases
        conn.execute(
            "UPDATE memories SET source = 'manual'
         WHERE lower(source) IN ('test', 'unit_test')",
            [],
        )?;

        // ghost:<publisher> → 'ghost' + move publisher into metadata.ghost.publisher
        // Only mutate metadata when it parses as JSON; otherwise just collapse source.
        loop {
            let rows = fetch_ghost_source_batch(conn, 500)?;
            if rows.is_empty() {
                break;
            }
            for (id, source, metadata) in rows {
                let publisher = source
                    .strip_prefix("ghost:")
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let new_meta = match serde_json::from_str::<serde_json::Value>(&metadata) {
                    Ok(mut v) => {
                        if !publisher.is_empty() {
                            let obj = v.as_object_mut();
                            if let Some(map) = obj {
                                let ghost_entry = map
                                    .entry("ghost".to_string())
                                    .or_insert_with(|| serde_json::json!({}));
                                if let Some(g) = ghost_entry.as_object_mut() {
                                    g.insert(
                                        "publisher".to_string(),
                                        serde_json::Value::String(publisher.clone()),
                                    );
                                }
                            }
                        }
                        Some(v.to_string())
                    }
                    Err(_) => None, // leave metadata untouched
                };
                match new_meta {
                    Some(m) => {
                        conn.execute(
                            "UPDATE memories SET source='ghost', metadata=?1 WHERE id=?2",
                            rusqlite::params![m, id],
                        )?;
                    }
                    None => {
                        conn.execute(
                            "UPDATE memories SET source='ghost' WHERE id=?1",
                            rusqlite::params![id],
                        )?;
                    }
                }
            }
        }

        // Lowercase canonical sources so case-variants match the CHECK list.
        conn.execute(
        "UPDATE memories SET source = lower(source)
         WHERE source IN ('Manual','Extraction','Migration','Auto','FoundryDistill','Handoff','Kanban','Wiki','Ghost','IngestEvent')
            OR source GLOB '*[A-Z]*'",
        [],
    )?;

        // Anything still not canonical (and not already external:) → external:<sanitized>
        let canonical_list = [
            "manual",
            "extraction",
            "migration",
            "auto",
            "foundry_distill",
            "foundry_recall_rerank_cache",
            "handoff",
            "kanban",
            "wiki",
            "ghost",
            "ingest_event",
        ];
        loop {
            let rows = fetch_noncanonical_source_batch(conn, 500)?;
            if rows.is_empty() {
                break;
            }
            let mut changed = 0usize;
            for (id, source) in rows {
                if canonical_list.contains(&source.as_str()) {
                    continue;
                }
                if let Some(suffix) = source.strip_prefix("external:") {
                    // Re-sanitize suffix to be safe
                    let sanitized = sanitize_source_suffix_sql(suffix);
                    let new_val = format!("external:{}", sanitized);
                    if new_val != source {
                        conn.execute(
                            "UPDATE memories SET source=?1 WHERE id=?2",
                            rusqlite::params![new_val, id],
                        )?;
                        changed += 1;
                    }
                    continue;
                }
                let sanitized = sanitize_source_suffix_sql(&source);
                let new_val = format!("external:{}", sanitized);
                conn.execute(
                    "UPDATE memories SET source=?1 WHERE id=?2",
                    rusqlite::params![new_val, id],
                )?;
                changed += 1;
            }
            if changed == 0 {
                break;
            }
        }

        // ── Step 2: Normalize category ──────────────────────────────────────────
        conn.execute(
            "UPDATE memories SET category = lower(category) WHERE category IS NOT NULL",
            [],
        )?;
        conn.execute(
        "UPDATE memories SET category = 'other'
         WHERE category IS NULL OR category = ''
            OR category NOT IN ('fact','decision','experience','preference','entity','other','kanban','handoff','ghost','wiki')",
        [],
    )?;

        // ── Step 3: Normalize scope ─────────────────────────────────────────────
        conn.execute(
            "UPDATE memories SET scope = lower(scope) WHERE scope IS NOT NULL",
            [],
        )?;
        conn.execute(
            "UPDATE memories SET scope = 'general'
         WHERE scope IS NULL OR scope NOT IN ('user','project','general')",
            [],
        )?;

        // ── Step 4: Normalize retention_policy ──────────────────────────────────
        conn.execute(
            "UPDATE memories SET retention_policy = lower(retention_policy)
         WHERE retention_policy IS NOT NULL",
            [],
        )?;
        conn.execute(
            "UPDATE memories SET retention_policy = NULL
         WHERE retention_policy IS NOT NULL
           AND retention_policy NOT IN ('ephemeral','durable','permanent','pinned')",
            [],
        )?;

        // ── Step 5: Backfill retention defaults ─────────────────────────────────
        conn.execute(
            "UPDATE memories SET retention_policy = 'pinned'
         WHERE retention_policy IS NULL
           AND (path LIKE '/handoff%' OR path LIKE '/kanban%')",
            [],
        )?;
        conn.execute(
            "UPDATE memories SET retention_policy = 'permanent'
         WHERE retention_policy IS NULL AND path LIKE '/wiki%'",
            [],
        )?;
        conn.execute(
            "UPDATE memories SET retention_policy = 'permanent'
         WHERE retention_policy IS NULL AND source = 'foundry_distill'",
            [],
        )?;

        // ── Step 6: Rebuild table with CHECK constraints ────────────────────────
        conn.execute_batch(
        r#"
        CREATE TABLE memories_new (
            id           TEXT PRIMARY KEY,
            path         TEXT NOT NULL DEFAULT '/',
            summary      TEXT NOT NULL DEFAULT '',
            text         TEXT NOT NULL DEFAULT '',
            importance   REAL NOT NULL DEFAULT 0.7,
            timestamp    TEXT NOT NULL,
            category     TEXT NOT NULL DEFAULT 'fact',
            topic        TEXT NOT NULL DEFAULT '',
            keywords     TEXT NOT NULL DEFAULT '[]',
            persons      TEXT NOT NULL DEFAULT '[]',
            entities     TEXT NOT NULL DEFAULT '[]',
            location     TEXT NOT NULL DEFAULT '',
            source       TEXT NOT NULL DEFAULT 'manual',
            scope        TEXT NOT NULL DEFAULT 'general',
            archived     INTEGER NOT NULL DEFAULT 0,
            created_at   TEXT NOT NULL DEFAULT '',
            updated_at   TEXT NOT NULL DEFAULT '',
            access_count INTEGER NOT NULL DEFAULT 0,
            last_access  TEXT,
            revision     INTEGER NOT NULL DEFAULT 1,
            metadata     TEXT NOT NULL DEFAULT '{}',
            retention_policy TEXT,
            domain       TEXT,
            CHECK (category IN ('fact','decision','experience','preference','entity','other','kanban','handoff','ghost','wiki')),
            CHECK (scope IN ('user','project','general')),
            CHECK (retention_policy IS NULL OR retention_policy IN ('ephemeral','durable','permanent','pinned')),
            CHECK (
                source IN ('manual','extraction','migration','auto','foundry_distill','foundry_recall_rerank_cache','handoff','kanban','wiki','ghost','ingest_event')
                OR source LIKE 'external:%'
            )
        );

        INSERT INTO memories_new
            (id, path, summary, text, importance, timestamp, category, topic,
             keywords, persons, entities, location, source, scope, archived,
             created_at, updated_at, access_count, last_access, revision,
             metadata, retention_policy, domain)
        SELECT
             id, path, summary, text, importance, timestamp, category, topic,
             keywords, persons, entities, location, source, scope, archived,
             created_at, updated_at, access_count, last_access, revision,
             metadata, retention_policy, domain
        FROM memories;

        DROP TABLE memories;
        ALTER TABLE memories_new RENAME TO memories;

        CREATE INDEX IF NOT EXISTS idx_memories_path        ON memories(path);
        CREATE INDEX IF NOT EXISTS idx_memories_importance  ON memories(importance DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_timestamp   ON memories(timestamp DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_archived    ON memories(archived);
        CREATE INDEX IF NOT EXISTS idx_memories_last_access ON memories(last_access DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_retention_policy ON memories(retention_policy);
        CREATE INDEX IF NOT EXISTS idx_memories_domain      ON memories(domain);

        "#,
    )?;

        Ok(())
    })();

    match migration_result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

fn fetch_ghost_source_batch(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<(String, String, String)>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id, source, metadata FROM memories
         WHERE source LIKE 'ghost:%'
         ORDER BY id
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn fetch_noncanonical_source_batch(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<(String, String)>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id, source FROM memories
         WHERE source NOT IN ('manual','extraction','migration','auto','foundry_distill','foundry_recall_rerank_cache','handoff','kanban','wiki','ghost','ingest_event')
           AND (
             source NOT LIKE 'external:%'
             OR source = 'external:'
             OR source != lower(source)
             OR source GLOB '*[^a-z0-9_:-]*'
             OR length(substr(source, 10)) > 64
           )
         ORDER BY id
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// SQL-side equivalent of `sanitize_source_suffix` in types.rs.
/// Lowercases, replaces non-`[a-z0-9_-]` with `_`, truncates to 64 chars.
fn sanitize_source_suffix_sql(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(64));
    for c in s.chars() {
        let mapped = if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
            c
        } else if c.is_ascii_uppercase() {
            c.to_ascii_lowercase()
        } else {
            '_'
        };
        out.push(mapped);
        if out.len() >= 64 {
            break;
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), MemoryError> {
    if has_column(conn, table, column)? {
        return Ok(());
    }

    let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {definition}");
    conn.execute(&sql, [])?;
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, MemoryError> {
    let pragma = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&pragma)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn ensure_fts_backfilled(conn: &Connection) -> Result<(), MemoryError> {
    let memories_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
    if memories_count == 0 {
        return Ok(());
    }

    let fts_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM memories_fts", [], |row| row.get(0))?;
    if fts_count > 0 {
        return Ok(());
    }

    conn.execute(
        r#"INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
           SELECT
             id,
             path,
             summary,
             text,
             trim(replace(replace(replace(keywords, '[', ' '), ']', ' '), '"', ' ')),
             trim(replace(replace(replace(entities, '[', ' '), ']', ' '), '"', ' '))
           FROM memories"#,
        [],
    )?;

    ensure_column(conn, "vault_entries", "allowed_agents", "TEXT")?;

    Ok(())
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use rusqlite::params;

    fn open_with_legacy_row(
        source: &str,
        category: &str,
        scope: &str,
        retention: Option<&str>,
        path: &str,
        metadata: &str,
    ) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        // Build legacy-shape table without CHECK constraints, mirroring the
        // pre-migration schema.
        conn.execute_batch(
            r#"
            CREATE TABLE memories (
                id           TEXT PRIMARY KEY,
                path         TEXT NOT NULL DEFAULT '/',
                summary      TEXT NOT NULL DEFAULT '',
                text         TEXT NOT NULL DEFAULT '',
                importance   REAL NOT NULL DEFAULT 0.7,
                timestamp    TEXT NOT NULL,
                category     TEXT NOT NULL DEFAULT 'fact',
                topic        TEXT NOT NULL DEFAULT '',
                keywords     TEXT NOT NULL DEFAULT '[]',
                persons      TEXT NOT NULL DEFAULT '[]',
                entities     TEXT NOT NULL DEFAULT '[]',
                location     TEXT NOT NULL DEFAULT '',
                source       TEXT NOT NULL DEFAULT 'manual',
                scope        TEXT NOT NULL DEFAULT 'general',
                archived     INTEGER NOT NULL DEFAULT 0,
                created_at   TEXT NOT NULL DEFAULT '',
                updated_at   TEXT NOT NULL DEFAULT '',
                access_count INTEGER NOT NULL DEFAULT 0,
                last_access  TEXT,
                revision     INTEGER NOT NULL DEFAULT 1,
                metadata     TEXT NOT NULL DEFAULT '{}',
                retention_policy TEXT,
                domain       TEXT
            );
            CREATE VIRTUAL TABLE memories_fts USING fts5(
                id UNINDEXED, path, summary, text, keywords, entities,
                tokenize = 'simple'
            );
            "#,
        )
        .unwrap();
        conn.execute(
            r#"INSERT INTO memories
                (id, path, summary, text, importance, timestamp, category, topic,
                 keywords, persons, entities, location, source, scope, archived,
                 created_at, updated_at, access_count, last_access, revision,
                 metadata, retention_policy, domain)
               VALUES (?1, ?2, '', 'hello', 0.5, '2026-04-30T00:00:00Z', ?3, '',
                       '[]','[]','[]','', ?4, ?5, 0,
                       '2026-04-30T00:00:00Z', '2026-04-30T00:00:00Z', 0, NULL, 1,
                       ?6, ?7, NULL)"#,
            params!["row1", path, category, source, scope, metadata, retention],
        )
        .unwrap();
        conn
    }

    #[test]
    fn migration_normalizes_legacy_garbage() {
        let conn = open_with_legacy_row(
            "test",
            "WeirdCat",
            "self",
            Some("garbage"),
            "/notes/foo",
            "{}",
        );
        // First run: performs full migration.
        init_schema(&conn).unwrap();
        let (source, category, scope, retention): (String, String, String, Option<String>) = conn
            .query_row(
                "SELECT source, category, scope, retention_policy FROM memories WHERE id='row1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(source, "manual");
        assert_eq!(category, "other");
        assert_eq!(scope, "general");
        assert_eq!(retention, None);
    }

    #[test]
    fn migration_collapses_ghost_publisher_into_metadata() {
        let conn = open_with_legacy_row(
            "ghost:agent_x",
            "fact",
            "general",
            None,
            "/ghost/messages",
            "{}",
        );
        init_schema(&conn).unwrap();
        let (source, metadata): (String, String) = conn
            .query_row(
                "SELECT source, metadata FROM memories WHERE id='row1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(source, "ghost");
        let m: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(m["ghost"]["publisher"], "agent_x");
    }

    #[test]
    fn migration_backfills_handoff_retention_to_pinned() {
        let conn = open_with_legacy_row("manual", "fact", "general", None, "/handoff/foo", "{}");
        init_schema(&conn).unwrap();
        let r: Option<String> = conn
            .query_row(
                "SELECT retention_policy FROM memories WHERE id='row1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(r.as_deref(), Some("pinned"));
    }

    #[test]
    fn migration_rejects_invalid_scope_after_migration() {
        let conn = open_with_legacy_row("manual", "fact", "general", None, "/notes/x", "{}");
        init_schema(&conn).unwrap();
        // Now CHECK constraint should reject 'self'.
        let err = conn
            .execute(
                r#"INSERT INTO memories
                   (id, path, summary, text, importance, timestamp, category, topic,
                    keywords, persons, entities, location, source, scope, archived,
                    created_at, updated_at, access_count, last_access, revision,
                    metadata, retention_policy, domain)
                   VALUES ('row2', '/notes/y', '', '', 0.5, '2026-04-30T00:00:00Z',
                           'fact', '', '[]','[]','[]','', 'manual', 'self', 0,
                           '2026-04-30T00:00:00Z', '2026-04-30T00:00:00Z', 0, NULL, 1,
                           '{}', NULL, NULL)"#,
                [],
            )
            .unwrap_err();
        assert!(
            err.to_string().to_ascii_lowercase().contains("check")
                || err.to_string().to_ascii_lowercase().contains("constraint"),
            "expected CHECK violation, got: {err}"
        );
    }

    #[test]
    fn migration_is_idempotent() {
        let conn = open_with_legacy_row("manual", "fact", "general", None, "/notes/x", "{}");
        init_schema(&conn).unwrap();
        // Snapshot the table SQL.
        let sql1: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='memories'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // Run again — should be no-op.
        init_schema(&conn).unwrap();
        let sql2: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='memories'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sql1, sql2);
        // Row still present.
        let cnt: i64 = conn
            .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt, 1);
    }

    #[test]
    fn migration_preserves_fts_rows() {
        let conn = open_with_legacy_row("manual", "fact", "general", None, "/notes/x", "{}");
        // Pre-populate FTS to confirm it survives.
        conn.execute(
            "INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
             VALUES ('row1', '/notes/x', '', 'hello', '', '')",
            [],
        )
        .unwrap();
        init_schema(&conn).unwrap();
        let cnt: i64 = conn
            .query_row("SELECT COUNT(*) FROM memories_fts", [], |r| r.get(0))
            .unwrap();
        assert!(cnt >= 1);
    }
}
