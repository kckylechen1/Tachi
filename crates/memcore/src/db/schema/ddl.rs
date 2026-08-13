/// Connection-level PRAGMAs that must run OUTSIDE any transaction.
/// `journal_mode` in particular is a no-op (and on some SQLite builds an
/// error) when issued mid-transaction, so this is executed before any
/// `BEGIN` — see `init_schema_with_label_mut` (#984 F1 round 3). SQLite busy
/// policy is deliberately configured by `db::open` before this batch so a
/// bounded one-shot writer cannot have its caller-owned budget overwritten.
pub(super) const CONNECTION_PRAGMA_SQL: &str = r#"
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;
        PRAGMA cache_size = -16000;   -- 16 MB page cache
"#;

pub(super) const RESERVED_REFERENCE_GUARD_SQL: &str = r#"
        DROP TRIGGER IF EXISTS memories_reserved_refs_insert_guard;
        DROP TRIGGER IF EXISTS memories_reserved_refs_update_guard;

        CREATE TRIGGER memories_reserved_refs_insert_guard
        BEFORE INSERT ON memories
        WHEN (
             json_type(NEW.metadata, '$.evidence_refs_v1') IS NOT NULL
             OR json_type(NEW.metadata, '$.source_refs') IS NOT NULL
         )
         AND NOT EXISTS (
             SELECT 1
             FROM memories AS current
             WHERE current.id = NEW.id
               AND json_type(current.metadata, '$.evidence_refs_v1')
                   IS json_type(NEW.metadata, '$.evidence_refs_v1')
               AND json_quote(json_extract(current.metadata, '$.evidence_refs_v1'))
                   IS json_quote(json_extract(NEW.metadata, '$.evidence_refs_v1'))
               AND json_type(current.metadata, '$.source_refs')
                   IS json_type(NEW.metadata, '$.source_refs')
               AND json_quote(json_extract(current.metadata, '$.source_refs'))
                   IS json_quote(json_extract(NEW.metadata, '$.source_refs'))
         )
         AND tachi_reserved_reference_write_enabled() = 0
        BEGIN
            SELECT RAISE(ABORT, 'reserved memory reference metadata requires typed mutation');
        END;

        CREATE TRIGGER memories_reserved_refs_update_guard
        BEFORE UPDATE OF metadata ON memories
        WHEN (
             json_type(NEW.metadata, '$.evidence_refs_v1')
                 IS NOT json_type(OLD.metadata, '$.evidence_refs_v1')
             OR json_quote(json_extract(NEW.metadata, '$.evidence_refs_v1'))
                 IS NOT json_quote(json_extract(OLD.metadata, '$.evidence_refs_v1'))
             OR json_type(NEW.metadata, '$.source_refs')
                 IS NOT json_type(OLD.metadata, '$.source_refs')
             OR json_quote(json_extract(NEW.metadata, '$.source_refs'))
                 IS NOT json_quote(json_extract(OLD.metadata, '$.source_refs'))
         )
         AND tachi_reserved_reference_write_enabled() = 0
        BEGIN
            SELECT RAISE(ABORT, 'reserved memory reference metadata requires typed mutation');
        END;
"#;

pub(super) const RESERVED_REFERENCE_INSERT_TRIGGER_NAME: &str =
    "memories_reserved_refs_insert_guard";
pub(super) const RESERVED_REFERENCE_UPDATE_TRIGGER_NAME: &str =
    "memories_reserved_refs_update_guard";

// Keep these definitions token-for-token aligned with the CREATE statements
// in RESERVED_REFERENCE_GUARD_SQL. Open-time trigger inventory validation
// compares normalized sqlite_schema SQL against these canonical definitions.
pub(super) const RESERVED_REFERENCE_INSERT_TRIGGER_SQL: &str = r#"
        CREATE TRIGGER memories_reserved_refs_insert_guard
        BEFORE INSERT ON memories
        WHEN (
             json_type(NEW.metadata, '$.evidence_refs_v1') IS NOT NULL
             OR json_type(NEW.metadata, '$.source_refs') IS NOT NULL
         )
         AND NOT EXISTS (
             SELECT 1
             FROM memories AS current
             WHERE current.id = NEW.id
               AND json_type(current.metadata, '$.evidence_refs_v1')
                   IS json_type(NEW.metadata, '$.evidence_refs_v1')
               AND json_quote(json_extract(current.metadata, '$.evidence_refs_v1'))
                   IS json_quote(json_extract(NEW.metadata, '$.evidence_refs_v1'))
               AND json_type(current.metadata, '$.source_refs')
                   IS json_type(NEW.metadata, '$.source_refs')
               AND json_quote(json_extract(current.metadata, '$.source_refs'))
                   IS json_quote(json_extract(NEW.metadata, '$.source_refs'))
         )
         AND tachi_reserved_reference_write_enabled() = 0
        BEGIN
            SELECT RAISE(ABORT, 'reserved memory reference metadata requires typed mutation');
        END
"#;

pub(super) const RESERVED_REFERENCE_UPDATE_TRIGGER_SQL: &str = r#"
        CREATE TRIGGER memories_reserved_refs_update_guard
        BEFORE UPDATE OF metadata ON memories
        WHEN (
             json_type(NEW.metadata, '$.evidence_refs_v1')
                 IS NOT json_type(OLD.metadata, '$.evidence_refs_v1')
             OR json_quote(json_extract(NEW.metadata, '$.evidence_refs_v1'))
                 IS NOT json_quote(json_extract(OLD.metadata, '$.evidence_refs_v1'))
             OR json_type(NEW.metadata, '$.source_refs')
                 IS NOT json_type(OLD.metadata, '$.source_refs')
             OR json_quote(json_extract(NEW.metadata, '$.source_refs'))
                 IS NOT json_quote(json_extract(OLD.metadata, '$.source_refs'))
         )
         AND tachi_reserved_reference_write_enabled() = 0
        BEGIN
            SELECT RAISE(ABORT, 'reserved memory reference metadata requires typed mutation');
        END
"#;

/// Whether a schema chunk belongs to the memory kernel every store has, or
/// to the Tachi product surface only a [`crate::db::StoreProfile::TachiFull`]
/// store carries (#1585 D3).
///
/// The split is *data*, not control flow: [`BASE_SCHEMA_CHUNKS`] and
/// [`MIGRATED_INDEXES_CHUNKS`] below are the same bytes the single
/// `BASE_SCHEMA_SQL` / `MIGRATED_INDEXES_SQL` literals used to be, cut into
/// one-table-plus-its-indexes pieces and tagged. The byte-equality goldens in
/// this module's test block assert that concatenating ALL chunks in order
/// reproduces the v28 text verbatim — so this refactor cannot have changed
/// what a `TachiFull` store gets, and a reviewer does not have to diff 860
/// lines of DDL by eye to believe that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SchemaScope {
    /// Executed for every store, whatever its profile.
    Portable,
    /// Executed only when the effective profile
    /// [`crate::db::StoreProfile::includes_product`].
    Product,
}

/// The v28 base schema, one `CREATE TABLE` (or virtual table) plus its
/// immediately-following indexes per chunk, in the original order.
///
/// Concatenated in order these bytes ARE the former `BASE_SCHEMA_SQL`; see
/// `base_schema_chunks_concatenate_to_the_v28_golden`.
pub(super) const BASE_SCHEMA_CHUNKS: &[(SchemaScope, &str)] = &[
    // memories
    (
        SchemaScope::Portable,
        r#"
        CREATE TABLE IF NOT EXISTS memories (
            id           TEXT PRIMARY KEY,
            path         TEXT NOT NULL DEFAULT '/',
            summary      TEXT NOT NULL DEFAULT '',
            text         TEXT NOT NULL DEFAULT '',
            importance   REAL NOT NULL DEFAULT 0.7,
            timestamp    TEXT NOT NULL,
            valid_from   TEXT NOT NULL DEFAULT '',
            valid_until  TEXT,
            category     TEXT NOT NULL DEFAULT 'fact',
            topic        TEXT NOT NULL DEFAULT '',
            keywords     TEXT NOT NULL DEFAULT '[]',
            entities     TEXT NOT NULL DEFAULT '[]',
            source       TEXT NOT NULL DEFAULT 'manual',
            scope        TEXT NOT NULL DEFAULT 'general',
            archived     INTEGER NOT NULL DEFAULT 0,
            created_at   TEXT NOT NULL DEFAULT '',
            updated_at   TEXT NOT NULL DEFAULT '',
                -- tachi#1459: these two observe the search path only; reads
                -- through path-listing routes do not increment or update them.
                -- Their sole production writer is `record_access_with_updates`,
                -- reached only from `search.rs`'s `hybrid_search`, so
                -- `access_count = 0` means "never surfaced by search", NOT
                -- "never retrieved" — a row served only by `list_by_path` /
                -- `list_by_path_recent` / `list_memories_by_path_prefix`
                -- (kanban, handoffs, briefings, cards mirror, GC scans) reads
                -- zero here no matter how often it is read.
                access_count    INTEGER NOT NULL DEFAULT 0,
                scored_count    INTEGER NOT NULL DEFAULT 0,
                last_access     TEXT,
                -- tachi#1446: exposure-free recency reference. Nothing writes
                -- it yet; `last_access` above is written for every row a search
                -- returns, which is why ranking needs a separate column to read.
                last_use_at     TEXT,
                revision        INTEGER NOT NULL DEFAULT 1,
                metadata        TEXT NOT NULL DEFAULT '{}',
                superseded_by   TEXT,
                idless_identity TEXT,
                -- tachi#1459: same blind spot as `access_count` above, and these
                -- two are the tier-promotion gate. `recall_count` moves only for
                -- the FTS-matched subset of a search's results; `query_diversity`
                -- is derived from `access_history` rows carrying a non-empty
                -- query hash, which only search writes. Use rows have empty
                -- query hashes and do not add query-diversity evidence. The gate
                -- therefore rests on a search-only view of query diversity.
                recall_count    INTEGER NOT NULL DEFAULT 0,
                query_diversity INTEGER NOT NULL DEFAULT 0,
                tier            TEXT NOT NULL DEFAULT 'raw'
            );

        CREATE INDEX IF NOT EXISTS idx_memories_path        ON memories(path);
        CREATE INDEX IF NOT EXISTS idx_memories_importance  ON memories(importance DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_timestamp   ON memories(timestamp DESC);
"#,
    ),
    // memories_fts
    (
        SchemaScope::Portable,
        r#"
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
"#,
    ),
    // memories_symbolic_fts
    (
        SchemaScope::Portable,
        r#"
        -- Trigram FTS5 index for symbolic candidate retrieval (#1331).
        -- Accelerates unanchored LIKE '%term%' across the same columns the
        -- symbolic channel matches, without changing LIKE eligibility or the
        -- relevance-first pre-cap ORDER BY from #1154. Stores raw memories
        -- column bytes (including JSON keywords/entities) so LIKE semantics
        -- match a table scan of `memories`. case_sensitive 0 mirrors SQLite's
        -- default ASCII-case-insensitive LIKE.
        CREATE VIRTUAL TABLE IF NOT EXISTS memories_symbolic_fts USING fts5(
            id,
            path,
            summary,
            text,
            keywords,
            entities,
            topic,
            tokenize = 'trigram case_sensitive 0'
        );
"#,
    ),
    // memory_edges
    (
        SchemaScope::Portable,
        r#"
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
"#,
    ),
    // edge_observations
    (
        SchemaScope::Portable,
        r#"
        -- Append-only observation ledger under the working graph (#774 Layer-2,
        -- sol audit cut ①). `memory_edges` is a mutable last-write-wins working
        -- projection: its PK (source_id, target_id, relation) + ON CONFLICT DO
        -- UPDATE collapses every re-observation of the same triple into ONE row
        -- (created_at/valid_from/valid_to included), which erases the evidence
        -- count Layer-2 induction needs. Each successful edge write appends
        -- exactly one immutable row here in the same transaction, so this
        -- ledger accumulates one row per observation while the graph keeps a
        -- single mutable projection row. `observed_at` is immutable;
        -- invalidation is a soft `invalidated_at` stamp, never a delete, so the
        -- history stays complete.
        --   Layer-2 counts observations (rows here), not graph rows
        --   (#774 sol audit ruling).
        -- Added as a pure additive CREATE TABLE IF NOT EXISTS with no
        -- schema-version bump — the same in-place-on-BASE_SCHEMA convention
        -- exec_envs / dispatch_outcomes / session_claims entered by.
        CREATE TABLE IF NOT EXISTS edge_observations (
            observation_id     TEXT PRIMARY KEY,
            source_id          TEXT NOT NULL,
            target_id          TEXT NOT NULL,
            relation           TEXT NOT NULL,
            capture_event_kind TEXT NOT NULL DEFAULT 'unknown',
            capture_event_id   TEXT NOT NULL DEFAULT '',
            actor              TEXT NOT NULL DEFAULT 'unknown',
            reason_code        TEXT NOT NULL DEFAULT '',
            observed_at        TEXT NOT NULL,
            evidence_hash      TEXT,
            invalidated_at     TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_edge_obs_edge
            ON edge_observations(source_id, target_id, relation);
        CREATE INDEX IF NOT EXISTS idx_edge_obs_observed_at
            ON edge_observations(observed_at);
"#,
    ),
    // hard_state
    (
        SchemaScope::Portable,
        r#"
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
"#,
    ),
    // access_history
    (
        SchemaScope::Portable,
        r#"
        -- Access history for ACT-R base-level activation.
        --
        -- tachi#1446 lever 5: `event_kind` is the provenance discriminator.
        -- `display` = the recall pipeline recorded that it SHOWED this row
        -- (`record_access_with_updates`, the sole production writer before
        -- this column existed, so `DEFAULT 'display'` is the honest value for
        -- every legacy row). `use` = a caller-initiated save cited this memory
        -- (`record_memory_use`). `get_use_access_times` reads only the latter,
        -- which is what keeps the system's own display action out of the
        -- ACT-R base-level-activation floor at `scorer.rs`'s
        -- `default_decay_score_actr_with_config`.
        CREATE TABLE IF NOT EXISTS access_history (
            memory_id  TEXT NOT NULL,
            accessed_at TEXT NOT NULL,
            query_hash  TEXT NOT NULL DEFAULT '',
            event_kind  TEXT NOT NULL DEFAULT 'display'
        );
        CREATE INDEX IF NOT EXISTS idx_access_hist_mem ON access_history(memory_id);
        CREATE INDEX IF NOT EXISTS idx_access_hist_time ON access_history(accessed_at DESC);
        CREATE INDEX IF NOT EXISTS idx_access_hist_mem_time ON access_history(memory_id, accessed_at DESC);
"#,
    ),
    // derived_items
    (
        SchemaScope::Portable,
        r#"        -- idx_access_hist_hash and idx_access_hist_mem_kind_time reference the
        -- evolutionary `query_hash` / `event_kind` columns and are created in
        -- MIGRATED_INDEXES_SQL, AFTER `ensure_column` adds them to legacy
        -- access_history tables (#1289). Creating them here would `no such
        -- column`-crash init_schema_inner on a DB that predates either column.

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
"#,
    ),
    // processed_events
    (
        SchemaScope::Portable,
        r#"
        CREATE TABLE IF NOT EXISTS processed_events (
            event_hash TEXT NOT NULL,
            event_id   TEXT NOT NULL DEFAULT '',
            worker     TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (event_hash, worker)
        );
        CREATE INDEX IF NOT EXISTS idx_processed_events_created_at ON processed_events(created_at DESC);
"#,
    ),
    // tachi_events
    (
        SchemaScope::Portable,
        r#"
        -- Tachi continuity event ledger. This is append-only source material for
        -- typed projectors; it does not directly mutate recall, prompts, routing,
        -- execution, or domain state.
        CREATE TABLE IF NOT EXISTS tachi_events (
            id               TEXT PRIMARY KEY,
            source_repo      TEXT NOT NULL DEFAULT '',
            adapter          TEXT NOT NULL DEFAULT '',
            project          TEXT NOT NULL DEFAULT '',
            domain           TEXT NOT NULL DEFAULT '',
            session_id       TEXT NOT NULL DEFAULT '',
            actor            TEXT NOT NULL DEFAULT '',
            event_type       TEXT NOT NULL,
            authority        TEXT NOT NULL DEFAULT 'collect_only',
            effects          TEXT NOT NULL DEFAULT '[]',
            projection_hints TEXT NOT NULL DEFAULT '[]',
            payload_json     TEXT NOT NULL DEFAULT '{}',
            provenance_json  TEXT NOT NULL DEFAULT '{}',
            created_at       TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_tachi_events_created_at ON tachi_events(created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_tachi_events_project_domain ON tachi_events(project, domain, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_tachi_events_type ON tachi_events(event_type, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_tachi_events_session ON tachi_events(session_id, created_at DESC);
"#,
    ),
    // hub_capabilities
    (
        SchemaScope::Product,
        r#"
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
"#,
    ),
    // hub_version_routes
    (
        SchemaScope::Product,
        r#"        -- review_status / health_status indexes are created after ensure_column migrations

        CREATE TABLE IF NOT EXISTS hub_version_routes (
            alias_id TEXT PRIMARY KEY,
            active_capability_id TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_hub_route_target ON hub_version_routes(active_capability_id);
"#,
    ),
    // virtual_capability_bindings
    (
        SchemaScope::Product,
        r#"
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
"#,
    ),
    // audit_log
    (
        SchemaScope::Product,
        r#"
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
"#,
    ),
    // llm_usage
    (
        SchemaScope::Product,
        r#"
        -- LLM usage ledger for provider spend attribution. This stores only
        -- provider/model/token metadata, never prompts, responses, or secrets.
        CREATE TABLE IF NOT EXISTS llm_usage (
            id                    INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp             TEXT NOT NULL,
            lane                  TEXT NOT NULL,
            model                 TEXT NOT NULL,
            provider_host         TEXT NOT NULL DEFAULT '',
            provider_logical_name TEXT NOT NULL DEFAULT '',
            provider_key_id       TEXT NOT NULL DEFAULT '',
            prompt_tokens         INTEGER,
            completion_tokens     INTEGER,
            total_tokens          INTEGER,
            max_tokens            INTEGER NOT NULL DEFAULT 0,
            request_chars         INTEGER NOT NULL DEFAULT 0,
            response_chars        INTEGER NOT NULL DEFAULT 0,
            duration_ms           INTEGER NOT NULL DEFAULT 0,
            success               INTEGER NOT NULL DEFAULT 1,
            created_at            TEXT NOT NULL DEFAULT (STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now'))
        );
        CREATE INDEX IF NOT EXISTS idx_llm_usage_timestamp ON llm_usage(timestamp DESC);
        CREATE INDEX IF NOT EXISTS idx_llm_usage_lane ON llm_usage(lane, timestamp DESC);
        CREATE INDEX IF NOT EXISTS idx_llm_usage_provider_key ON llm_usage(provider_logical_name, provider_key_id, timestamp DESC);
"#,
    ),
    // agent_known_state
    (
        SchemaScope::Product,
        r#"
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
"#,
    ),
    // sandbox_rules
    (
        SchemaScope::Product,
        r#"
        -- Sandbox rules for role-based memory isolation (Semantic Sandboxing)
        CREATE TABLE IF NOT EXISTS sandbox_rules (
            agent_role   TEXT NOT NULL,
            path_pattern TEXT NOT NULL,
            access_level TEXT NOT NULL DEFAULT 'read',
            created_at   TEXT NOT NULL,
            PRIMARY KEY (agent_role, path_pattern)
        );
        CREATE INDEX IF NOT EXISTS idx_sandbox_role ON sandbox_rules(agent_role);
"#,
    ),
    // sandbox_policies
    (
        SchemaScope::Product,
        r#"
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
"#,
    ),
    // sandbox_exec_audit
    (
        SchemaScope::Product,
        r#"
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
"#,
    ),
    // vault_config
    (
        SchemaScope::Product,
        r#"
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
"#,
    ),
    // vault_entries
    (
        SchemaScope::Product,
        r#"
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
"#,
    ),
    // vault_audit
    (
        SchemaScope::Product,
        r#"
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
"#,
    ),
    // vault_key_rotations
    (
        SchemaScope::Product,
        r#"
        -- Vault: key rotation state for multi-key secrets
        CREATE TABLE IF NOT EXISTS vault_key_rotations (
            prefix              TEXT PRIMARY KEY,
            current_index       INTEGER NOT NULL DEFAULT 1,
            total_keys          INTEGER NOT NULL DEFAULT 0,
            rotation_strategy   TEXT NOT NULL DEFAULT 'round_robin',
            created_at          TEXT NOT NULL DEFAULT '',
            updated_at          TEXT NOT NULL DEFAULT ''
        );
"#,
    ),
    // vault_key_health
    (
        SchemaScope::Product,
        r#"
        -- Vault provider key runtime health and selection metadata.
        -- key_id is a concrete key entry name for non-rotations,
        -- or a rotation member name for rotation pools (e.g., VOYAGE_API_KEY_1).
        CREATE TABLE IF NOT EXISTS vault_key_health (
            logical_name    TEXT NOT NULL,
            key_id          TEXT NOT NULL,
            status          TEXT NOT NULL DEFAULT 'ok',
            cooldown_until  TEXT,
            last_success    TEXT,
            last_attempt    TEXT,
            last_error      TEXT,
            error_count     INTEGER NOT NULL DEFAULT 0,
            auth_failed     INTEGER NOT NULL DEFAULT 0,
            disabled        INTEGER NOT NULL DEFAULT 0,
            metadata        TEXT NOT NULL DEFAULT '{}',
            updated_at      TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (logical_name, key_id)
        );
        CREATE INDEX IF NOT EXISTS idx_vault_key_health_status ON vault_key_health(status);
        CREATE INDEX IF NOT EXISTS idx_vault_key_health_logical ON vault_key_health(logical_name);
        CREATE INDEX IF NOT EXISTS idx_vault_key_health_cooldown ON vault_key_health(logical_name, cooldown_until);
"#,
    ),
    // provider_accounts
    (
        SchemaScope::Product,
        r#"
        -- Vault provider accounts (tachi#1680 D1): public-safe account
        -- metadata only. An account row says which vendor account a
        -- credential belongs to; it never says which Vault entry or rotation
        -- member physically holds the secret. That pointer lives in
        -- `account_custody` — a separate table precisely so no serialization
        -- of an account row can carry custody by accident. `auth_ref` is the
        -- opaque ("va1:...") handle joining the two, and is NULL for
        -- metadata-only auth modes that have no stored credential at all.
        CREATE TABLE IF NOT EXISTS provider_accounts (
            account_id            TEXT PRIMARY KEY,
            provider_kind         TEXT NOT NULL,
            auth_mode             TEXT NOT NULL CHECK (auth_mode IN (
                                      'api_key_pool', 'brokered_oauth', 'cloud_iam',
                                      'provider_owned_session', 'local_no_auth', 'unsupported'
                                  )),
            auth_ref              TEXT UNIQUE,
            account_fingerprint   TEXT NOT NULL,
            account_class         TEXT NOT NULL DEFAULT 'model_api',
            capabilities          TEXT NOT NULL DEFAULT '[]',
            credential_policy_ref TEXT,
            refresh_authority     TEXT NOT NULL DEFAULT 'none',
            status                TEXT NOT NULL DEFAULT 'active',
            revision              INTEGER NOT NULL DEFAULT 1,
            source_refs           TEXT NOT NULL DEFAULT '[]',
            created_at            TEXT NOT NULL,
            updated_at            TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_provider_accounts_kind ON provider_accounts(provider_kind);
        CREATE INDEX IF NOT EXISTS idx_provider_accounts_fingerprint ON provider_accounts(account_fingerprint);
"#,
    ),
    // provider_account_aliases
    (
        SchemaScope::Product,
        r#"
        -- Every env-var name an account has been observed under. Append and
        -- retire, never delete: two names holding one value collapse into a
        -- single account with two alias rows (#1680 D2 discrimination 1), and
        -- a name that stops being observed is marked `retired` so the record
        -- of what was once called what survives the rename.
        CREATE TABLE IF NOT EXISTS provider_account_aliases (
            account_id  TEXT NOT NULL,
            alias_name  TEXT NOT NULL,
            source_kind TEXT NOT NULL,
            first_seen  TEXT NOT NULL,
            last_seen   TEXT NOT NULL,
            retired     INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (account_id, alias_name)
        );
        CREATE INDEX IF NOT EXISTS idx_provider_account_aliases_name ON provider_account_aliases(alias_name);
"#,
    ),
    // provider_account_events
    (
        SchemaScope::Product,
        r#"
        -- Append-only audit of everything that ever changed an account:
        -- creation, alias observation, member rotation, re-fingerprint after a
        -- Vault master-key rekey, apply/no-op. `revision` is the account
        -- revision the event produced (which is how identity survives rotating
        -- key material, #1680 D2); `plan_digest` binds an event to the
        -- reconcile plan that caused it. Rows are never updated or deleted.
        CREATE TABLE IF NOT EXISTS provider_account_events (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            account_id  TEXT NOT NULL,
            revision    INTEGER NOT NULL,
            event_kind  TEXT NOT NULL,
            plan_digest TEXT,
            evidence    TEXT NOT NULL DEFAULT '{}',
            created_at  TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_provider_account_events_account ON provider_account_events(account_id, id);
"#,
    ),
    // account_custody
    (
        SchemaScope::Product,
        r#"
        -- The custody pointer for one account: which Vault rotation pool or
        -- which Vault entry actually holds its secret. Deliberately NOT columns
        -- on `provider_accounts` — a table boundary, not field discipline, is
        -- what keeps Vault layout (rotation prefixes, member names) off every
        -- account-shaped serialization surface (#1680 D1/D5). Restructuring a
        -- pool bumps `revision` here and leaves `auth_ref` — and therefore
        -- every upper-layer reference to the account — unchanged.
        CREATE TABLE IF NOT EXISTS account_custody (
            auth_ref       TEXT PRIMARY KEY,
            account_id     TEXT NOT NULL UNIQUE,
            custody_kind   TEXT NOT NULL CHECK (custody_kind IN ('vault_rotation_pool', 'vault_entry')),
            custody_target TEXT NOT NULL,
            revision       INTEGER NOT NULL,
            updated_at     TEXT NOT NULL
        );
"#,
    ),
    // foundry_jobs
    (
        SchemaScope::Product,
        r#"
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
"#,
    ),
    // foundry_config
    (
        SchemaScope::Product,
        r#"
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
"#,
    ),
    // recall_cache
    (
        SchemaScope::Portable,
        r#"
        -- Recall cache: rendered hybrid-search result rows keyed by a query
        -- context hash. Lives OUTSIDE `memories` on purpose — a prior design
        -- stored these as memory rows and they leaked into every long-lived
        -- DB. The foreground recall path writes through after computing
        -- results and short-circuits a full hybrid-search round trip on a
        -- fresh cache hit; the background rerank job upgrades entries with
        -- reranked orderings (reranked=1). Staleness is bounded by a TTL on
        -- `updated_at`, checked at read time.
        CREATE TABLE IF NOT EXISTS recall_cache (
            cache_id      TEXT PRIMARY KEY,
            generation_fingerprint TEXT NOT NULL DEFAULT '',
            query         TEXT NOT NULL DEFAULT '',
            rows_json     TEXT NOT NULL DEFAULT '[]',
            result_count  INTEGER NOT NULL DEFAULT 0,
            reranked      INTEGER NOT NULL DEFAULT 0,
            hit_count     INTEGER NOT NULL DEFAULT 0,
            created_at    TEXT NOT NULL DEFAULT '',
            updated_at    TEXT NOT NULL DEFAULT '',
            last_hit_at   TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_recall_cache_updated ON recall_cache(updated_at);
"#,
    ),
    // exec_envs
    (
        SchemaScope::Product,
        r#"
        -- Execution-environment leases (#894 S1). Daemon-owned rows are the
        -- single source of truth for a provisioned worktree/env: worktree
        -- markers (`.tachi-worktree.json`) and the global `worktrees.json`
        -- become read-only projections/backstops for offline tools (the
        -- sweep), never a second owner. `state` is the S1 lifecycle
        -- (`active` -> `reclaimed`); the reclaim transition is a transactional
        -- state flip written by exactly one reclaim function. `dispatch_id`
        -- links a lease to the dispatch that owns it.
        --
        -- `env_class` (#894 S2c) is the provisioning policy class — a closed
        -- vocabulary of `edit-only` (default) | `build-ticketed` |
        -- `build-private`. It decides ONE thing at provision time: whether a
        -- `build_target` resource is allocated and bound to this lease
        -- (`edit-only` gets none — that is the ~14MB tree). It is NOT a
        -- security boundary: a worker with a shell and the same UID can run
        -- cargo in an `edit-only` tree regardless of what this column says
        -- (owner-ratified 2026-07-13). Its value is disk (no target dir) and
        -- default routing (builds go to the broker's serialized executor seat
        -- instead of poisoning a shared target from N diverged trees).
        -- Retrofitted onto existing DBs by the v15 sentinel migration; legacy
        -- rows read back `edit-only` because they were never provisioned under
        -- a class at all and carry no resource bindings (S2a is newer).
        CREATE TABLE IF NOT EXISTS exec_envs (
            env_id         TEXT PRIMARY KEY,
            kind           TEXT NOT NULL DEFAULT 'worktree',
            path           TEXT NOT NULL,
            repo_root      TEXT NOT NULL DEFAULT '',
            branch         TEXT NOT NULL DEFAULT '',
            base_sha       TEXT NOT NULL DEFAULT '',
            dispatch_id    TEXT,
            agent_identity_id TEXT,
            claim_id       TEXT,
            env_class      TEXT NOT NULL DEFAULT 'edit-only',
            state          TEXT NOT NULL DEFAULT 'active',
            reclaim_reason TEXT,
            schema_version INTEGER NOT NULL DEFAULT 1,
            created_at     TEXT NOT NULL DEFAULT '',
            reclaimed_at   TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_exec_envs_state ON exec_envs(state);
        CREATE INDEX IF NOT EXISTS idx_exec_envs_path ON exec_envs(path);
        CREATE INDEX IF NOT EXISTS idx_exec_envs_dispatch ON exec_envs(dispatch_id);
"#,
    ),
    // exec_env_resources
    (
        SchemaScope::Product,
        r#"        -- idx_exec_envs_claim references the v21 `claim_id` column and is created
        -- in MIGRATED_INDEXES_SQL, AFTER `ensure_column` adds claim_id to legacy
        -- exec_envs tables (#1289). Creating it here would `no such column:
        -- claim_id`-crash init_schema_inner on a pre-v21 DB.

        -- Execution-environment RESOURCE ledger (#894 S2a). `exec_envs` tracks
        -- the *lease*; these two tables track the BYTES that lease owns —
        -- because a `reclaimed` lease row turned out to be SQLite-only
        -- bookkeeping (#1029): the row flipped, the disk stayed full. One row
        -- here = one physical resource (a worktree dir, a shared cargo target,
        -- a scratch dir, a project DB), and `state = 'reclaimed'` means the
        -- bytes are ACTUALLY gone. `reclaim_resource` stamps `reclaiming`, the
        -- CALLER deletes from the filesystem (memcore never touches the FS),
        -- then it records `reclaimed_bytes` and flips `reclaimed`. A crash in
        -- between leaves the row in `reclaiming`, which is re-enterable
        -- (at-least-once, idempotent — `reclaimed_bytes` is assigned, never
        -- accumulated). `reclaim_failed` is a failed delete (retryable);
        -- `quarantined` is "hands off, a human/broker decides" and is entered
        -- through `quarantine_resource`; S2c adds `release_quarantine` as the
        -- one way back out (quarantined -> active), gated on the caller
        -- verifying or clearing the bytes first.
        --
        -- `state` has exactly four writers, all in `db::exec_env_resources` and
        -- all taking an IMMEDIATE transaction so their read-then-write is atomic
        -- against each other: `reclaim_resource`, `quarantine_resource`,
        -- `release_quarantine`, and `insert_resource`'s re-registration path (a
        -- `reclaimed` (path, kind) is revived as `active` under a fresh
        -- resource_id — the reclaimer churns the same worktree dirs, so a path
        -- must be registerable more than once in the life of the DB). Nothing
        -- writes `state` with ad-hoc SQL.
        --
        -- Bindings are many-to-many on purpose: one shared `build_target`
        -- (CARGO_TARGET_DIR) is bound by every live lease at once. refcount =
        -- bindings with `released_at IS NULL`; a resource with refcount > 0 is
        -- NOT reclaimable (`reclaim_resource` returns a typed
        -- `BlockedByBinding`, never a silent skip), so a shared target is freed
        -- only when the LAST binding is released. `bind_resource` re-reads the
        -- resource state inside its own IMMEDIATE transaction, so a bind and a
        -- reclaim of the same resource can never both commit (which would leave a
        -- live binding pointing at deleted bytes). `UNIQUE (env_id, resource_id)`
        -- makes a re-bind of the same pair a re-activation of that one row, not
        -- a second binding, so refcount can't be inflated by a retry.
        --
        -- Added as pure additive CREATE TABLE IF NOT EXISTS with no
        -- schema-version bump — the same in-place-on-BASE_SCHEMA convention
        -- exec_envs / dispatch_outcomes / session_claims / edge_observations
        -- entered by.
        CREATE TABLE IF NOT EXISTS exec_env_resources (
            resource_id     TEXT PRIMARY KEY,
            -- closed vocabulary: worktree | build_target | scratch_dir | project_db
            kind            TEXT NOT NULL,
            -- canonicalized absolute path
            path            TEXT NOT NULL,
            -- most recent measurement (caller-measured; memcore only stores it)
            bytes           INTEGER,
            measured_at     TEXT,
            -- active | reclaiming | reclaimed | reclaim_failed | quarantined
            state           TEXT NOT NULL DEFAULT 'active',
            reclaim_reason  TEXT,
            reclaimed_at    TEXT,
            -- bytes actually freed from the filesystem, stamped after the delete
            reclaimed_bytes INTEGER,
            created_at      TEXT NOT NULL DEFAULT '',
            updated_at      TEXT NOT NULL DEFAULT '',
            UNIQUE (path, kind)
        );
        CREATE INDEX IF NOT EXISTS idx_exec_env_resources_state
            ON exec_env_resources(state);
        CREATE INDEX IF NOT EXISTS idx_exec_env_resources_kind
            ON exec_env_resources(kind);
"#,
    ),
    // exec_env_resource_bindings
    (
        SchemaScope::Product,
        r#"
        CREATE TABLE IF NOT EXISTS exec_env_resource_bindings (
            binding_id  TEXT PRIMARY KEY,
            env_id      TEXT NOT NULL,
            resource_id TEXT NOT NULL,
            created_at  TEXT NOT NULL DEFAULT '',
            released_at TEXT,
            UNIQUE (env_id, resource_id)
        );
        CREATE INDEX IF NOT EXISTS idx_exec_env_resource_bindings_env
            ON exec_env_resource_bindings(env_id);
        CREATE INDEX IF NOT EXISTS idx_exec_env_resource_bindings_resource
            ON exec_env_resource_bindings(resource_id);
"#,
    ),
    // dispatch_outcomes
    (
        SchemaScope::Product,
        r#"
        -- Canonical dispatch outcome ledger (#773 v4 sol carve). Append-only:
        -- one row per (dispatch_id, task_type) completion, written FIRST by
        -- the `tachi_complete` seam before any derived row (eval memory,
        -- signature evidence, precedent) — a derivation failure must never
        -- lose this row (see `complete_ops::dispatch_outcome`). This is the
        -- future router's real read surface; `/eval` stays the searchable
        -- narrative projection and cards remain a reviewed projection on
        -- top. Graph edges (seat->performed->dispatch, dispatch->produced->
        -- outcome, outcome->classified_as->signature, outcome->about->
        -- issue/PR) are deferred to the S4 seat; every ref this row carries
        -- (issue_ref/pr_ref/flow_id/dispatch_id/eval_memory_id) is present
        -- so those edges can be derived later without a re-migration.
        -- Adjudication evidence lives in the append-only dispatch_adjudications
        -- table (#1035); this table holds mutable execution facts only — a
         -- replayed complete may rewrite mutable execution columns here; the
         -- identity receipt is frozen on its first write.
        --
        -- Truthfulness (#773 Layer-2 ②): `execution_outcome` is the
        -- MACHINE-RESOLVED terminal verdict (after the #878-A completion
        -- predicate intercepts a false success, or the terminal state a
        -- non-`tachi_complete` path reached), NOT the raw self-report;
        -- `reported_outcome` keeps the agent's verbatim claim (NULL when a
        -- terminal path carried no self-report). New `reported_outcome` column
        -- is back-filled onto existing DBs by the v14 sentinel migration.
        CREATE TABLE IF NOT EXISTS dispatch_outcomes (
            outcome_id       TEXT PRIMARY KEY,
            dispatch_id      TEXT NOT NULL DEFAULT '',
            eval_memory_id   TEXT,
            model            TEXT,
            vendor           TEXT NOT NULL DEFAULT 'unknown',
            role             TEXT,
            seat             TEXT,
            task_type        TEXT,
            -- machine-resolved terminal verdict (completed/failed/aborted/partial)
            execution_outcome    TEXT NOT NULL,
            -- raw self-reported outcome, verbatim; NULL for no-self-report terminals
            reported_outcome     TEXT,
            -- retry_count: awaiting dispatch-context plumb (no source at write
            -- points yet) — stays 0 until the dispatch retry ledger is wired.
            retry_count      INTEGER NOT NULL DEFAULT 0,
            error_class      TEXT,
            issue_ref        TEXT,
            pr_ref           TEXT,
            flow_id          TEXT,
            cost_tokens      INTEGER,
            cost_usd         REAL,
            verification_present INTEGER NOT NULL DEFAULT 0,
             diff_present         INTEGER NOT NULL DEFAULT 0,
             evidence_refs    TEXT NOT NULL DEFAULT '[]',
             -- Immutable receipt copied from the accepted run ledger. NULL is
             -- a valid explicit legacy/unattributed state.
             identity_receipt TEXT,
             -- Evidentiary basis of the flat identity columns above
             -- (planned_unconfirmed | acknowledged_overlay | observed |
             -- fallback_unreceipted | unknown). Frozen with the receipt so a
             -- reader can always tell planned routing intent from
             -- carrier-observed execution fact (#1065 option D). Back-filled
             -- onto existing DBs by the v17 sentinel migration.
             identity_attribution_basis TEXT NOT NULL DEFAULT 'unknown',
             idempotency_key  TEXT NOT NULL,
            created_at       TEXT NOT NULL DEFAULT '',
            updated_at       TEXT NOT NULL DEFAULT '',
            UNIQUE (idempotency_key)
        );
        CREATE INDEX IF NOT EXISTS idx_dispatch_outcomes_vendor_ts
            ON dispatch_outcomes(vendor, created_at);
        CREATE INDEX IF NOT EXISTS idx_dispatch_outcomes_issue_ref
            ON dispatch_outcomes(issue_ref);
        CREATE INDEX IF NOT EXISTS idx_dispatch_outcomes_dispatch_id
            ON dispatch_outcomes(dispatch_id);
"#,
    ),
    // dispatch_adjudications
    (
        SchemaScope::Product,
        r#"
        -- #1035: machine execution facts remain in dispatch_outcomes. Terminal
        -- leader judgment is an append-only event linked by outcome_id.
        CREATE TABLE IF NOT EXISTS dispatch_adjudications (
            adjudication_id TEXT PRIMARY KEY,
            outcome_id TEXT NOT NULL,
            event_key TEXT NOT NULL UNIQUE,
            verdict TEXT,
            not_required_reason TEXT,
            actor TEXT NOT NULL CHECK (length(trim(actor)) > 0),
            evidence_ref TEXT NOT NULL CHECK (length(trim(evidence_ref)) > 0),
            created_at TEXT NOT NULL DEFAULT '',
            insertion_seq INTEGER NOT NULL,
            UNIQUE (outcome_id, insertion_seq),
            CHECK (
                (verdict IS NOT NULL AND length(trim(verdict)) > 0 AND not_required_reason IS NULL)
                OR
                (verdict IS NULL AND not_required_reason IS NOT NULL AND length(trim(not_required_reason)) > 0)
            )
        );
        CREATE INDEX IF NOT EXISTS idx_dispatch_adjudications_outcome
            ON dispatch_adjudications(outcome_id, created_at);
"#,
    ),
    // dispatch_adjudication_signatures
    (
        SchemaScope::Product,
        r#"
        -- One adjudication event may classify multiple canonical signatures.
        CREATE TABLE IF NOT EXISTS dispatch_adjudication_signatures (
            adjudication_id TEXT NOT NULL,
            signature_id TEXT NOT NULL,
            evidence_ref TEXT,
            resolved INTEGER NOT NULL DEFAULT 0 CHECK (resolved IN (0, 1)),
            PRIMARY KEY (adjudication_id, signature_id)
        );
        CREATE INDEX IF NOT EXISTS idx_dispatch_adjudication_signatures_signature
            ON dispatch_adjudication_signatures(signature_id);
"#,
    ),
    // mirror_eval_runs
    (
        SchemaScope::Product,
        r#"
        -- #1066: first-class mirror eval intake for harness-native
        -- subagents — a NEW ledger backing the SAME `tachi_agent_eval`
        -- facade's register/observe/adjudicate/get actions, mirroring the
        -- dispatch_outcomes/dispatch_adjudications split above for work
        -- Tachi did not dispatch (a host-native subagent Tachi only
        -- observes). See `memcore::db::mirror_eval` for the write contract.
        CREATE TABLE IF NOT EXISTS mirror_eval_runs (
            eval_run_id         TEXT PRIMARY KEY,
            register_key        TEXT NOT NULL UNIQUE,
            frozen_contract_ref TEXT NOT NULL CHECK (length(trim(frozen_contract_ref)) > 0),
            execution_origin    TEXT NOT NULL CHECK (length(trim(execution_origin)) > 0),
            lifecycle_owner     TEXT NOT NULL CHECK (length(trim(lifecycle_owner)) > 0),
            harness             TEXT,
            native_child_id     TEXT,
            requested_profile   TEXT,
            requested_model     TEXT,
            requested_agent     TEXT,
            created_at          TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_mirror_eval_runs_native_child
            ON mirror_eval_runs(native_child_id, created_at);
"#,
    ),
    // mirror_eval_observations
    (
        SchemaScope::Product,
        r#"
        -- At most one terminal observation per run: carrier-observed facts
        -- only, no judgment field exists on this table (#1066 AC-3).
        CREATE TABLE IF NOT EXISTS mirror_eval_observations (
            observation_id   TEXT PRIMARY KEY,
            eval_run_id      TEXT NOT NULL UNIQUE,
            terminal_outcome TEXT NOT NULL CHECK (length(trim(terminal_outcome)) > 0),
            duration_ms      INTEGER,
            cost_tokens      INTEGER,
            cost_usd         REAL,
            result_ref       TEXT,
            artifacts        TEXT NOT NULL DEFAULT '[]',
            effective_model    TEXT,
            effective_backend  TEXT,
            effective_harness  TEXT,
            created_at       TEXT NOT NULL DEFAULT ''
        );
"#,
    ),
    // mirror_eval_adjudications
    (
        SchemaScope::Product,
        r#"
        -- Append-only leader/independent-reviewer judgment events, one row
        -- per adjudication event, linked to a run by eval_run_id (#1035
        -- pattern reused verbatim: event_key idempotency + durable
        -- per-run insertion_seq for ordered corrections).
        CREATE TABLE IF NOT EXISTS mirror_eval_adjudications (
            adjudication_id       TEXT PRIMARY KEY,
            eval_run_id           TEXT NOT NULL,
            event_key             TEXT NOT NULL UNIQUE,
            actor                 TEXT NOT NULL CHECK (length(trim(actor)) > 0),
            verifier_model        TEXT,
            usefulness            TEXT NOT NULL CHECK (length(trim(usefulness)) > 0),
            failure_mode          TEXT,
            first_review_findings TEXT NOT NULL DEFAULT '[]',
            plan_delta            TEXT,
            next_prompt_delta     TEXT,
            evidence_usable       INTEGER NOT NULL CHECK (evidence_usable IN (0, 1)),
            used_in_final_claim   INTEGER NOT NULL DEFAULT 0 CHECK (used_in_final_claim IN (0, 1)),
            human_override        INTEGER NOT NULL DEFAULT 0 CHECK (human_override IN (0, 1)),
            evidence_ref          TEXT NOT NULL CHECK (length(trim(evidence_ref)) > 0),
            created_at            TEXT NOT NULL DEFAULT '',
            insertion_seq         INTEGER NOT NULL,
            UNIQUE (eval_run_id, insertion_seq)
        );
        CREATE INDEX IF NOT EXISTS idx_mirror_eval_adjudications_run
            ON mirror_eval_adjudications(eval_run_id, created_at);
"#,
    ),
    // route_recommendations
    (
        SchemaScope::Product,
        r#"
        -- tachi#1675 PR1: decision-fact ledger, NOT a terminal ledger — zero
        -- execution/terminal-state columns live here (design D1). Captures
        -- the ONLY moment a routing candidate set exists in memory: one row
        -- per `handle_dispatch_recommendation` call (Seam A), append-only,
        -- never deduplicated (every consult is a new fact, even a repeat
        -- consult with identical content). `candidates` is the full scored
        -- candidate array (profile/agent/score/reasons/...) verbatim;
        -- `policy_source_revision` is the content-bearing route-policy
        -- snapshot hash (`route_policy_source_revision`, reused not
        -- reinvented) so a later replay can tell whether the active policy
        -- has since changed underneath this recommendation.
        CREATE TABLE IF NOT EXISTS route_recommendations (
            recommendation_id      TEXT PRIMARY KEY,
            task_type               TEXT,
            risk                    TEXT NOT NULL DEFAULT 'unknown',
            candidates              TEXT NOT NULL DEFAULT '[]',
            recommended_profile     TEXT,
            policy_source_revision  TEXT,
            rows_considered         INTEGER NOT NULL DEFAULT 0,
            occurred_at             TEXT NOT NULL DEFAULT '',
            recorded_at             TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_route_recommendations_recorded_at
            ON route_recommendations(recorded_at);
"#,
    ),
    // route_decisions
    (
        SchemaScope::Product,
        r#"
        -- tachi#1675 PR1: acceptance-moment decision fact (Seam B), keyed
        -- UNIQUE on dispatch_id so a replayed acceptance is a zero-write
        -- idempotent no-op (design D2) — status.json is written FIRST (the
        -- canonical acceptance receipt) and this row follows in the same
        -- code path; a crash between the two leaves no row, and the
        -- projection's rule for a missing row is `assignment_mode :=
        -- 'unadvised'`, the honest floor, never fabricated advice. This is
        -- an acknowledged dual-write, not a transaction with status.json
        -- (status.json is a filesystem atomic write, not a DB write).
        -- `recommendation_id` is a nullable reference, not an enforced FK:
        -- `staff_start` does not always consult `recommend` first, so
        -- NULL-recommendation must stay legal (day-one reality, not an
        -- aspiration). `work_claim_id` stays nullable until #1239 wires v21
        -- claims through the dispatch lifecycle (spec correction 1).
        CREATE TABLE IF NOT EXISTS route_decisions (
            route_decision_id  TEXT PRIMARY KEY,
            dispatch_id         TEXT NOT NULL,
            recommendation_id   TEXT,
            selected_profile    TEXT,
            selected_model      TEXT,
            assignment_mode     TEXT NOT NULL CHECK (assignment_mode IN ('advised', 'unadvised', 'user_forced', 'experiment')),
            override_flag       INTEGER NOT NULL DEFAULT 0 CHECK (override_flag IN (0, 1)),
            contract_hash       TEXT,
            env_id              TEXT,
            host_profile        TEXT,
            work_claim_id       TEXT,
            occurred_at         TEXT NOT NULL DEFAULT '',
            recorded_at         TEXT NOT NULL DEFAULT '',
            UNIQUE (dispatch_id)
        );
        CREATE INDEX IF NOT EXISTS idx_route_decisions_recommendation
            ON route_decisions(recommendation_id);
"#,
    ),
    // eval_rubric_scores
    (
        SchemaScope::Product,
        r#"
        -- tachi#1675 PR1: structured adjudication companion (design D3),
        -- generic key (subject_kind, adjudication_id) serving BOTH the
        -- dispatch spine (dispatch_adjudications) and the mirror spine
        -- (mirror_eval_adjudications) without altering either table. A
        -- rubric row is an OPTIONAL companion to the free-text `verdict`
        -- that already exists on both adjudication tables — no rubric row
        -- means `excluded_reason='unstructured_verdict'` at the projection
        -- layer, not an error here. Six ordinal judged dimensions (closed
        -- sets, never floats — floats invite averaging into a forbidden
        -- one-dimensional reputation score). `independence_basis` is
        -- computed by the writer at write time (design D5); `identity_bound`
        -- is a reserved enum value never written in this phase. `rubric_hash`
        -- pins the exact rubric_v1 code-constant definition a row was judged
        -- against, so a later rubric revision cannot silently reinterpret an
        -- old row. Append-only: no UPDATE/DELETE accessor exists — a
        -- corrected judgment is a NEW adjudication event (new
        -- adjudication_id) carrying its own NEW rubric row, never an edit of
        -- an old one; UNIQUE(subject_kind, adjudication_id) enforces exactly
        -- one rubric row per adjudication event at the schema level.
        CREATE TABLE IF NOT EXISTS eval_rubric_scores (
            rubric_score_id          TEXT PRIMARY KEY,
            adjudication_id          TEXT NOT NULL,
            subject_kind             TEXT NOT NULL CHECK (subject_kind IN ('dispatch', 'mirror')),
            rubric_hash              TEXT NOT NULL,
            contract_correctness     TEXT NOT NULL CHECK (contract_correctness IN ('pass', 'concern', 'fail', 'not_assessed')),
            evidence_quality         TEXT NOT NULL CHECK (evidence_quality IN ('pass', 'concern', 'fail', 'not_assessed')),
            safety                   TEXT NOT NULL CHECK (safety IN ('pass', 'concern', 'fail', 'not_assessed')),
            scope_discipline         TEXT NOT NULL CHECK (scope_discipline IN ('pass', 'concern', 'fail', 'not_assessed')),
            intervention_burden      TEXT NOT NULL CHECK (intervention_burden IN ('pass', 'concern', 'fail', 'not_assessed')),
            completion_integrity     TEXT NOT NULL CHECK (completion_integrity IN ('pass', 'concern', 'fail', 'not_assessed')),
            adjudication_confidence  TEXT NOT NULL CHECK (adjudication_confidence IN ('low', 'medium', 'high')),
            adjudicator_actor        TEXT NOT NULL CHECK (length(trim(adjudicator_actor)) > 0),
            adjudicator_vendor       TEXT NOT NULL DEFAULT 'unknown',
            independence_basis       TEXT NOT NULL CHECK (independence_basis IN ('structural_cross_vendor', 'declared_only', 'self', 'identity_bound')),
            occurred_at              TEXT NOT NULL DEFAULT '',
            recorded_at              TEXT NOT NULL DEFAULT '',
            UNIQUE (subject_kind, adjudication_id)
        );
        CREATE INDEX IF NOT EXISTS idx_eval_rubric_scores_adjudication
            ON eval_rubric_scores(subject_kind, adjudication_id);
"#,
    ),
    // session_claims
    (
        SchemaScope::Product,
        r#"
        -- Cross-session presence claims (#1001). Advisory "who's working on
        -- what" lease rows — NOT a mutual-exclusion lock. A session/dispatch
        -- registers a claim on an issue/lane when it starts touching it and
        -- heartbeats it on every briefing read; a claim with a stale
        -- `heartbeat_at` (older than the TTL) is treated as expired by
        -- readers without a separate reaper process (lazy expiry, same spirit
        -- as `find_active_exec_env_by_path` filtering by state). `state` is
        -- the three-state lifecycle (`active` -> `orphaned` -> `released`).
        -- Lease expiry only orphans a claim; v21 ownership changes require an
        -- explicit caller-versioned release or handoff.
        CREATE TABLE IF NOT EXISTS session_claims (
            claim_id             TEXT PRIMARY KEY,
            session_client       TEXT,
            issue_ref            TEXT,
            flow_id              TEXT,
            dispatch_id          TEXT,
            branch               TEXT NOT NULL DEFAULT '',
            worktree_path        TEXT,
            declared_file_scope  TEXT,
            agent_identity_id    TEXT,
            role                 TEXT,
            mode                 TEXT,
            expected_head        TEXT,
            lease_expires_at     TEXT,
            transition_version   INTEGER NOT NULL DEFAULT 0,
            exec_env_id          TEXT,
            orphaned_at          TEXT,
            state                TEXT NOT NULL DEFAULT 'active',
            release_reason       TEXT,
            created_at           TEXT NOT NULL DEFAULT '',
            heartbeat_at         TEXT NOT NULL DEFAULT '',
            released_at          TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_session_claims_state ON session_claims(state);
        CREATE INDEX IF NOT EXISTS idx_session_claims_issue ON session_claims(issue_ref);
        CREATE INDEX IF NOT EXISTS idx_session_claims_flow ON session_claims(flow_id);
        -- Legacy #1001 identity-triple uniqueness. v21 WorkClaims carry an
        -- explicit mode and use transactional collision semantics instead;
        -- keeping this partial index to legacy rows preserves old upsert
        -- behavior without making same-issue v21 claims inherently exclusive.
        -- upsert_or_heartbeat_claim's read-then-write relies on at the
        -- application level. Partial (WHERE state='active' AND mode IS NULL)
        -- so only legacy presence rows participate and a released historical
        -- row never blocks a fresh active claim for the same identity;
        -- COALESCE(..., '') on each nullable column so two active
        -- rows that are both NULL in the same slot collide the same way the
        -- upsert's `IS ?` lookup already treats them as one identity (see
        -- `migrations/session_claims_identity.rs` for the full rationale and
        -- the migration that retrofits this onto pre-existing DBs).
        --
        -- The index's `WHERE ... AND mode IS NULL` predicate references the v21
        -- `mode` column, so it is created in MIGRATED_INDEXES_SQL AFTER
        -- `ensure_column` adds `mode` to legacy session_claims tables (#1289).
        -- Creating it here would `no such column: mode`-crash init_schema_inner
        -- on a pre-v21 DB.
"#,
    ),
    // agent_identities
    (
        SchemaScope::Product,
        r#"
        -- #1253 identity / WorkClaim spine. These tables and columns are
        -- deliberately additive: a pre-v21 claim has no identity rather than
        -- a made-up one.
        CREATE TABLE IF NOT EXISTS agent_identities (
            agent_identity_id TEXT PRIMARY KEY,
            display_name      TEXT,
            seat              TEXT,
            capability_json   TEXT,
            created_at        TEXT NOT NULL DEFAULT ''
        );
"#,
    ),
    // identity_admissions
    (
        SchemaScope::Product,
        r#"        CREATE TABLE IF NOT EXISTS identity_admissions (
            admission_id      TEXT PRIMARY KEY,
            agent_identity_id TEXT,
            connection_id     TEXT NOT NULL,
            state             TEXT NOT NULL CHECK (state IN ('self_asserted', 'verified', 'rejected', 'unavailable')),
            rejection_evidence TEXT,
            created_at        TEXT NOT NULL DEFAULT '',
            UNIQUE(agent_identity_id, connection_id)
        );
        CREATE INDEX IF NOT EXISTS idx_identity_admissions_connection ON identity_admissions(connection_id);
"#,
    ),
];

/// v28 recovery ledgers for Wiki REM source claims and exact-dedupe apply
/// lineage. These objects are installed only by the sentinel-gated migration
/// runner so an older stamped database cannot acquire newer write semantics
/// without migration authority and a matching `user_version` stamp.
pub(super) const WIKI_RECOVERY_LEDGERS_V28_SQL: &str = r#"
        -- Shared REM coordination ledger. The Wiki store is the one database
        -- every repo-local evolver can see, so source claims live here rather
        -- than in one caller's global/project source store. Claims are
        -- insert-once and retained after completion as replay evidence.
        CREATE TABLE IF NOT EXISTS rem_source_claims (
            source_key      TEXT PRIMARY KEY NOT NULL,
            source_identity TEXT NOT NULL,
            draft_id        TEXT NOT NULL,
            claimed_at      TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_rem_source_claims_draft
            ON rem_source_claims(draft_id);

        -- Transactional binding between an exact-dedupe apply receipt and
        -- every archived loser. Keeping this outside caller metadata preserves
        -- arbitrary valid JSON while preventing a legacy receipt downgrade
        -- from claiming a v2-applied row.
        CREATE TABLE IF NOT EXISTS exact_dedupe_apply_lineage (
            loser_id          TEXT PRIMARY KEY NOT NULL,
            apply_id          TEXT NOT NULL,
            plan_digest       TEXT NOT NULL,
            winner_id         TEXT NOT NULL,
            before_revision   INTEGER NOT NULL,
            archived_revision INTEGER NOT NULL,
            loser_valid_until_before TEXT,
            applied_at        TEXT NOT NULL
        );
"#;

/// v29 durable outbox for outbound memory mutations (tachi#1643, #1630 A1).
///
/// Installed only by the sentinel-gated migration runner, for the same reason
/// v28 is: a stamped-older database must not acquire a new write surface
/// through idempotent init DDL without migration authority and a matching
/// `user_version` stamp.
///
/// **Portable, not product** ([`SchemaScope::Portable`] semantics): the
/// migration that installs it takes no `StoreProfile` argument, so every
/// profile gets the table — #1630's whole premise is a host-owned sync loop
/// with no Tachi daemon dependency, so a `StoreProfile::PortableKernel`
/// database must carry it.
///
/// Column notes:
///
/// * `event_id` is caller-supplied and immutable. The PRIMARY KEY is the
///   uniqueness enforcement; immutability is enforced in Rust, because this
///   repo's persistent-trigger inventory
///   (`db::open::validate_persistent_trigger_inventory`) is a byte-exact
///   allowlist of two `memories` guards plus the search-generation triggers —
///   any additional persistent trigger makes the whole database refuse to
///   open. The only UPDATE seam in `db::outbox` touches
///   `state`/`last_error_class`/`state_changed_at` and nothing else.
/// * `source_revision` is never caller-supplied: `db::outbox` reads it from
///   the destination `memories` row inside the same transaction.
/// * `state` carries a CHECK against the six #1630 states, so a raw-SQL writer
///   (or a future migration that recreates the table) cannot introduce a state
///   the Rust enum cannot name. The typed enum
///   (`db::outbox::OutboxState`) is the transition authority; the CHECK is the
///   storage-level backstop, and `schema::validate_memory_outbox_schema`
///   refuses a table whose CHECK clause has drifted.
/// * `created_at` and `state_changed_at` are canonical UTC-ISO
///   (millisecond precision + `Z`, tachi#1432). The health read model takes
///   `MIN`/`MAX` over them **lexically**, which is only chronologically
///   correct because of that canonical shape.
pub(super) const MEMORY_OUTBOX_V29_SQL: &str = r#"
        CREATE TABLE IF NOT EXISTS memory_outbox_events (
            event_id         TEXT PRIMARY KEY NOT NULL,
            object_id        TEXT NOT NULL,
            object_class     TEXT NOT NULL,
            authority_class  TEXT NOT NULL,
            source_store     TEXT NOT NULL,
            source_partition TEXT NOT NULL,
            source_revision  INTEGER NOT NULL,
            payload_digest   TEXT NOT NULL,
            state            TEXT NOT NULL CHECK (state IN ('pending', 'in_flight', 'acknowledged', 'rejected', 'conflicted', 'quarantined')),
            last_error_class TEXT,
            created_at       TEXT NOT NULL,
            state_changed_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_memory_outbox_events_state_created
            ON memory_outbox_events(state, created_at);
        CREATE INDEX IF NOT EXISTS idx_memory_outbox_events_state_changed
            ON memory_outbox_events(state, state_changed_at);
        CREATE INDEX IF NOT EXISTS idx_memory_outbox_events_object
            ON memory_outbox_events(object_id, created_at);
"#;

/// The exact CHECK clause `MEMORY_OUTBOX_V29_SQL` installs on
/// `memory_outbox_events.state`, in whitespace-normalized form. Schema
/// validation compares against this so a table recreated without the
/// constraint — or with a different state vocabulary — is refused rather than
/// silently accepted (`pragma_table_info` cannot see CHECK constraints).
pub(super) const MEMORY_OUTBOX_STATE_CHECK_CLAUSE: &str =
    "CHECK (state IN ('pending', 'in_flight', 'acknowledged', 'rejected', 'conflicted', \
     'quarantined'))";

/// Durable destination-side readback receipts for the portable outbox apply
/// boundary (#1718, #1630 A4).  The ledger is deliberately separate from the
/// source-side `memory_outbox_events` table: a destination may receive an
/// envelope from a different store and must retain only the immutable binding
/// it actually read back after applying it.
pub(super) const MEMORY_OUTBOX_DESTINATION_APPLY_V30_SQL: &str = r#"
        CREATE TABLE IF NOT EXISTS memory_outbox_destination_apply_receipts (
            event_id                         TEXT PRIMARY KEY NOT NULL,
            object_id                        TEXT NOT NULL,
            source_store                     TEXT NOT NULL,
            source_partition                 TEXT NOT NULL,
            source_revision                  INTEGER NOT NULL,
            source_payload_digest            TEXT NOT NULL,
            destination_store                TEXT NOT NULL,
            destination_partition            TEXT NOT NULL,
            destination_object_revision      INTEGER NOT NULL,
            destination_payload_digest       TEXT NOT NULL,
            application                      TEXT NOT NULL CHECK (application = 'applied')
        );
        CREATE INDEX IF NOT EXISTS idx_memory_outbox_destination_apply_object
            ON memory_outbox_destination_apply_receipts(object_id, event_id);
"#;

/// The exact CHECK clause `MEMORY_OUTBOX_DESTINATION_APPLY_V30_SQL` installs
/// on the durable receipt ledger.  Schema validation compares this in
/// whitespace-normalized form so a recreated table cannot widen the closed
/// application vocabulary silently.
pub(super) const MEMORY_OUTBOX_DESTINATION_APPLY_APPLICATION_CHECK_CLAUSE: &str =
    "CHECK (application = 'applied')";

/// Product-only local advisory mailbox. Installed by the v31 migration so a
/// stamped v30 store cannot acquire a new write surface outside migration
/// authority. PortableKernel stores intentionally never execute this SQL.
pub(super) const A2A_KIND_CHECK_CLAUSE: &str = "CHECK (kind = 'turn_response/v1')";
pub(super) const A2A_ISSUER_ASSURANCE_CHECK_CLAUSE: &str =
    "CHECK (issuer_identity_assurance = 'self_asserted')";
pub(super) const A2A_RECIPIENT_ASSURANCE_CHECK_CLAUSE: &str =
    "CHECK (recipient_identity_assurance = 'self_asserted')";
pub(super) const A2A_BODY_DIGEST_CHECK_CLAUSE: &str = "CHECK (length(body_digest) = 64 AND body_digest = lower(body_digest) AND body_digest NOT GLOB '*[^0-9a-f]*')";
pub(super) const A2A_BODY_SIZE_CHECK_CLAUSE: &str = "CHECK (length(CAST(body AS BLOB)) <= 4096)";
pub(super) const A2A_ISSUER_TRUST_DOMAIN_CHECK_CLAUSE: &str =
    "CHECK (issuer_trust_domain = 'same_host')";
pub(super) const A2A_RECIPIENT_TRUST_DOMAIN_CHECK_CLAUSE: &str =
    "CHECK (recipient_trust_domain = 'same_host')";
pub(super) const A2A_ISSUER_TRUST_BASIS_CHECK_CLAUSE: &str =
    "CHECK (issuer_trust_basis = 'current_local_connection')";
pub(super) const A2A_RECIPIENT_TRUST_BASIS_CHECK_CLAUSE: &str =
    "CHECK (recipient_trust_basis = 'historical_local_admission')";
pub(super) const A2A_EXPIRY_CHECK_CLAUSE: &str = "CHECK (expires_at > created_at)";
pub(super) const A2A_IDEMPOTENCY_UNIQUE_CLAUSE: &str =
    "UNIQUE (issuer_agent_identity_id, idempotency_key)";
pub(super) const A2A_ISSUER_IDENTITY_FK_CLAUSE: &str =
    "FOREIGN KEY (issuer_agent_identity_id) REFERENCES agent_identities(agent_identity_id)";
pub(super) const A2A_RECIPIENT_IDENTITY_FK_CLAUSE: &str =
    "FOREIGN KEY (recipient_agent_identity_id) REFERENCES agent_identities(agent_identity_id)";
pub(super) const A2A_ISSUER_ADMISSION_FK_CLAUSE: &str =
    "FOREIGN KEY (issuer_admission_id) REFERENCES identity_admissions(admission_id)";
pub(super) const A2A_RECIPIENT_ADMISSION_FK_CLAUSE: &str =
    "FOREIGN KEY (recipient_admission_id) REFERENCES identity_admissions(admission_id)";
pub(super) const A2A_RECEIPT_ENVELOPE_FK_CLAUSE: &str =
    "FOREIGN KEY (envelope_id) REFERENCES a2a_envelopes(envelope_id)";
pub(super) const A2A_RECEIPT_ACTOR_IDENTITY_FK_CLAUSE: &str =
    "FOREIGN KEY (actor_agent_identity_id) REFERENCES agent_identities(agent_identity_id)";
pub(super) const A2A_RECEIPT_ACTOR_ADMISSION_FK_CLAUSE: &str =
    "FOREIGN KEY (actor_admission_id) REFERENCES identity_admissions(admission_id)";

pub(super) const A2A_MAILBOX_V31_SQL: &str = r#"
        CREATE TABLE IF NOT EXISTS a2a_envelopes (
            envelope_id                    TEXT PRIMARY KEY NOT NULL,
            kind                           TEXT NOT NULL CHECK (kind = 'turn_response/v1'),
            issuer_agent_identity_id       TEXT NOT NULL,
            issuer_admission_id             TEXT NOT NULL,
            recipient_agent_identity_id    TEXT NOT NULL,
            recipient_admission_id          TEXT NOT NULL,
            subject_ref                    TEXT NOT NULL,
            body                           TEXT NOT NULL,
            body_digest                    TEXT NOT NULL CHECK (length(body_digest) = 64 AND body_digest = lower(body_digest) AND body_digest NOT GLOB '*[^0-9a-f]*'),
            issuer_identity_assurance      TEXT NOT NULL CHECK (issuer_identity_assurance = 'self_asserted'),
            recipient_identity_assurance   TEXT NOT NULL CHECK (recipient_identity_assurance = 'self_asserted'),
            issuer_trust_domain            TEXT NOT NULL CHECK (issuer_trust_domain = 'same_host'),
            recipient_trust_domain         TEXT NOT NULL CHECK (recipient_trust_domain = 'same_host'),
            issuer_trust_basis             TEXT NOT NULL CHECK (issuer_trust_basis = 'current_local_connection'),
            recipient_trust_basis          TEXT NOT NULL CHECK (recipient_trust_basis = 'historical_local_admission'),
            idempotency_key                TEXT NOT NULL,
            created_at                     TEXT NOT NULL,
            expires_at                     TEXT NOT NULL,
            current_state                  TEXT NOT NULL CHECK (current_state IN ('received','accepted','consumed','expired')),
            state_version                  INTEGER NOT NULL CHECK (state_version > 0),
            UNIQUE (issuer_agent_identity_id, idempotency_key),
            CHECK (length(trim(subject_ref)) > 0),
            CHECK (length(body) > 0),
            CHECK (length(CAST(body AS BLOB)) <= 4096),
            CHECK (expires_at > created_at),
            FOREIGN KEY (issuer_agent_identity_id) REFERENCES agent_identities(agent_identity_id),
            FOREIGN KEY (issuer_admission_id) REFERENCES identity_admissions(admission_id),
            FOREIGN KEY (recipient_agent_identity_id) REFERENCES agent_identities(agent_identity_id),
            FOREIGN KEY (recipient_admission_id) REFERENCES identity_admissions(admission_id)
        );
        CREATE INDEX IF NOT EXISTS idx_a2a_envelopes_recipient_state
            ON a2a_envelopes(recipient_agent_identity_id, current_state, expires_at, created_at, envelope_id);
        CREATE INDEX IF NOT EXISTS idx_a2a_envelopes_issuer_created
            ON a2a_envelopes(issuer_agent_identity_id, created_at DESC, envelope_id DESC);

        CREATE TABLE IF NOT EXISTS a2a_delivery_receipts (
            receipt_id                     TEXT PRIMARY KEY NOT NULL,
            envelope_id                    TEXT NOT NULL,
            envelope_version               INTEGER NOT NULL CHECK (envelope_version > 0),
            state                          TEXT NOT NULL CHECK (state IN ('received','accepted','consumed','expired')),
            actor_agent_identity_id        TEXT NOT NULL,
            actor_admission_id              TEXT NOT NULL,
            identity_assurance             TEXT NOT NULL CHECK (identity_assurance = 'self_asserted'),
            trust_domain                   TEXT NOT NULL CHECK (trust_domain = 'same_host'),
            trust_basis                    TEXT NOT NULL CHECK (trust_basis IN ('current_local_connection','historical_local_admission')),
            occurred_at                    TEXT NOT NULL,
            UNIQUE (envelope_id, envelope_version),
            UNIQUE (envelope_id, state),
            FOREIGN KEY (envelope_id) REFERENCES a2a_envelopes(envelope_id),
            FOREIGN KEY (actor_agent_identity_id) REFERENCES agent_identities(agent_identity_id),
            FOREIGN KEY (actor_admission_id) REFERENCES identity_admissions(admission_id)
        );
        CREATE INDEX IF NOT EXISTS idx_a2a_receipts_envelope_version
            ON a2a_delivery_receipts(envelope_id, envelope_version);
"#;

pub(super) const A2A_ENVELOPE_STATE_CHECK_CLAUSE: &str =
    "CHECK (current_state IN ('received','accepted','consumed','expired'))";
pub(super) const A2A_RECEIPT_STATE_CHECK_CLAUSE: &str =
    "CHECK (state IN ('received','accepted','consumed','expired'))";

/// Historical v25 DDL for the sampled, content-free recall impression ledger
/// (tachi#1447). It exists only so the versioned migration sequence can build
/// the same v25 shape before v26 upgrades it; new databases end at
/// [`RECALL_IMPRESSION_LEDGER_V26_SQL`].
pub(super) const RECALL_IMPRESSION_LEDGER_V25_SQL: &str = r#"
        CREATE TABLE IF NOT EXISTS recall_impression_groups (
            group_id         TEXT PRIMARY KEY,
            created_at       TEXT NOT NULL,
            query_hash       TEXT NOT NULL,
            weights_profile  TEXT NOT NULL,
            semantic_weight  REAL NOT NULL,
            fts_weight       REAL NOT NULL,
            symbolic_weight  REAL NOT NULL,
            decay_weight     REAL NOT NULL,
            use_rrf          INTEGER NOT NULL CHECK (use_rrf IN (0, 1)),
            rrf_k            REAL NOT NULL,
            top_k            INTEGER NOT NULL,
            candidate_count  INTEGER NOT NULL,
            displayed_count  INTEGER NOT NULL,
            scored_returned_count INTEGER NOT NULL,
            replay_count     INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_recall_impression_groups_created
            ON recall_impression_groups(created_at DESC, group_id);
        CREATE INDEX IF NOT EXISTS idx_recall_impression_groups_query_hash
            ON recall_impression_groups(query_hash, created_at DESC);

        CREATE TABLE IF NOT EXISTS recall_impressions (
            group_id              TEXT NOT NULL,
            memory_id             TEXT NOT NULL,
            vector_score          REAL NOT NULL,
            fts_score             REAL NOT NULL,
            symbolic_score        REAL NOT NULL,
            decay_score           REAL NOT NULL,
            vec_rank              INTEGER,
            fts_rank              INTEGER,
            sym_rank              INTEGER,
            merge_adjustment      TEXT NOT NULL CHECK (merge_adjustment IN ('none', 'exact_id', 'superseded_scale', 'exact_id_superseded_scale')),
            pre_boost_score       REAL NOT NULL,
            pre_boost_rank        INTEGER NOT NULL,
            tie_break_epoch_millis INTEGER NOT NULL,
            final_score           REAL NOT NULL,
            final_rank            INTEGER NOT NULL,
            scored                INTEGER NOT NULL CHECK (scored IN (0, 1)),
            scored_returned       INTEGER NOT NULL CHECK (scored_returned IN (0, 1)),
            access_count_at_recall INTEGER NOT NULL,
            PRIMARY KEY (group_id, memory_id),
            FOREIGN KEY (group_id) REFERENCES recall_impression_groups(group_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_recall_impressions_memory
            ON recall_impressions(memory_id, group_id);
        CREATE INDEX IF NOT EXISTS idx_recall_impressions_group_final_rank
            ON recall_impressions(group_id, final_rank, memory_id);
"#;

/// Canonical current (v26) DDL for the sampled, content-free recall
/// impression ledger. `legacy_query_bucket` is v25's 32-bit FNV value after a
/// name-correcting migration: it is non-unique compatibility evidence only,
/// never query identity and never a join/cohort key. New groups leave it NULL
/// and persist `query_fingerprint` instead.
pub(super) const RECALL_IMPRESSION_LEDGER_V26_SQL: &str = r#"
        CREATE TABLE IF NOT EXISTS recall_impression_groups (
            group_id         TEXT PRIMARY KEY,
            created_at       TEXT NOT NULL,
            legacy_query_bucket TEXT,
            query_fingerprint TEXT,
            fusion_policy_version TEXT,
            pre_boost_adjustment_version TEXT,
            tie_break_policy_version TEXT,
            candidate_policy_version TEXT,
            schema_identity TEXT,
            weights_profile  TEXT NOT NULL,
            semantic_weight  REAL NOT NULL,
            fts_weight       REAL NOT NULL,
            symbolic_weight  REAL NOT NULL,
            decay_weight     REAL NOT NULL,
            use_rrf          INTEGER NOT NULL CHECK (use_rrf IN (0, 1)),
            rrf_k            REAL NOT NULL,
            top_k            INTEGER NOT NULL,
            candidate_count  INTEGER NOT NULL,
            displayed_count  INTEGER NOT NULL,
            scored_returned_count INTEGER NOT NULL,
            replay_count     INTEGER NOT NULL DEFAULT 0,
            CHECK (
                (query_fingerprint IS NULL
                 AND fusion_policy_version IS NULL
                 AND pre_boost_adjustment_version IS NULL
                 AND tie_break_policy_version IS NULL
                 AND candidate_policy_version IS NULL
                 AND schema_identity IS NULL)
                OR
                (query_fingerprint IS NOT NULL
                 AND length(query_fingerprint) = 64
                 AND query_fingerprint NOT GLOB '*[^0-9a-f]*'
                 AND fusion_policy_version IS NOT NULL
                 AND pre_boost_adjustment_version IS NOT NULL
                 AND tie_break_policy_version IS NOT NULL
                 AND candidate_policy_version IS NOT NULL
                 AND schema_identity IS NOT NULL)
            )
        );
        CREATE INDEX IF NOT EXISTS idx_recall_impression_groups_created
            ON recall_impression_groups(created_at DESC, group_id);
        CREATE INDEX IF NOT EXISTS idx_recall_impression_groups_fingerprint
            ON recall_impression_groups(query_fingerprint, created_at DESC)
            WHERE query_fingerprint IS NOT NULL;

        CREATE TABLE IF NOT EXISTS recall_impressions (
            group_id              TEXT NOT NULL,
            memory_id             TEXT NOT NULL,
            vector_score          REAL NOT NULL,
            fts_score             REAL NOT NULL,
            symbolic_score        REAL NOT NULL,
            decay_score           REAL NOT NULL,
            vec_rank              INTEGER,
            fts_rank              INTEGER,
            sym_rank              INTEGER,
            merge_adjustment      TEXT NOT NULL CHECK (merge_adjustment IN ('none', 'exact_id', 'superseded_scale', 'exact_id_superseded_scale')),
            pre_boost_score       REAL NOT NULL,
            pre_boost_rank        INTEGER NOT NULL,
            tie_break_epoch_millis INTEGER NOT NULL,
            final_score           REAL NOT NULL,
            final_rank            INTEGER NOT NULL,
            scored                INTEGER NOT NULL CHECK (scored IN (0, 1)),
            scored_returned       INTEGER NOT NULL CHECK (scored_returned IN (0, 1)),
            access_count_at_recall INTEGER NOT NULL,
            PRIMARY KEY (group_id, memory_id),
            FOREIGN KEY (group_id) REFERENCES recall_impression_groups(group_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_recall_impressions_memory
            ON recall_impressions(memory_id, group_id);
        CREATE INDEX IF NOT EXISTS idx_recall_impressions_group_final_rank
            ON recall_impressions(group_id, final_rank, memory_id);
"#;

/// Indexes on evolutionary columns, split by scope the same way
/// [`BASE_SCHEMA_CHUNKS`] is. These run AFTER `init_schema_inner`'s
/// `ensure_column` calls (see the inline comments carried verbatim below).
///
/// The `hub_capabilities`, `exec_envs` and `session_claims` index chunks are
/// product-scoped: on a `PortableKernel` store those tables do not exist, so
/// a `CREATE INDEX ... ON <missing table>` would fail the whole init batch.
pub(super) const MIGRATED_INDEXES_CHUNKS: &[(SchemaScope, &str)] = &[
    (
        SchemaScope::Portable,
        r#"
        CREATE INDEX IF NOT EXISTS idx_memories_archived    ON memories(archived);
        CREATE INDEX IF NOT EXISTS idx_memories_last_access ON memories(last_access DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_valid_time  ON memories(valid_from, valid_until);
        CREATE INDEX IF NOT EXISTS idx_derived_source       ON derived_items(source);
        CREATE INDEX IF NOT EXISTS idx_derived_path         ON derived_items(path);
        CREATE INDEX IF NOT EXISTS idx_derived_created_at   ON derived_items(created_at DESC);
"#,
    ),
    (
        SchemaScope::Product,
        r#"        CREATE INDEX IF NOT EXISTS idx_hub_cap_review_status ON hub_capabilities(review_status);
        CREATE INDEX IF NOT EXISTS idx_hub_cap_health_status ON hub_capabilities(health_status);
"#,
    ),
    (
        SchemaScope::Portable,
        r#"        CREATE INDEX IF NOT EXISTS idx_memories_retention_policy ON memories(retention_policy);
        CREATE INDEX IF NOT EXISTS idx_memories_domain ON memories(domain);
        CREATE INDEX IF NOT EXISTS idx_memories_superseded ON memories(superseded_by);
        CREATE INDEX IF NOT EXISTS idx_memories_tier ON memories(tier);
        CREATE INDEX IF NOT EXISTS idx_memories_recall ON memories(recall_count DESC);
"#,
    ),
    (
        SchemaScope::Portable,
        r#"
        -- Indexes on evolutionary columns of NON-memories tables. These MUST be
        -- created here (after init_schema_inner's `ensure_column` calls) rather
        -- than inline in BASE_SCHEMA_SQL: on a legacy DB the table already
        -- exists so `CREATE TABLE IF NOT EXISTS` is a no-op and does NOT add the
        -- column, and a bare `CREATE INDEX` referencing that column would crash
        -- init_schema_inner with `no such column` before any sentinel migration
        -- ran (#1289). The matching `ensure_column` for each column runs above.
        CREATE INDEX IF NOT EXISTS idx_access_hist_hash
            ON access_history(memory_id, query_hash) WHERE query_hash != '';
        -- tachi#1446 lever 5. `get_use_access_times` and the per-kind GC quota
        -- both partition by (memory_id, event_kind) and order by accessed_at
        -- DESC; without this they fall back to idx_access_hist_mem_time and
        -- re-filter every display row to find the rare use rows.
        CREATE INDEX IF NOT EXISTS idx_access_hist_mem_kind_time
            ON access_history(memory_id, event_kind, accessed_at DESC);
"#,
    ),
    (
        SchemaScope::Product,
        r#"        CREATE INDEX IF NOT EXISTS idx_exec_envs_claim ON exec_envs(claim_id);
"#,
    ),
    (
        SchemaScope::Product,
        r#"        CREATE UNIQUE INDEX IF NOT EXISTS idx_session_claims_identity_active
            ON session_claims(COALESCE(session_client, ''), COALESCE(issue_ref, ''), COALESCE(flow_id, ''))
            WHERE state = 'active' AND mode IS NULL;
"#,
    ),
    (
        SchemaScope::Portable,
        r#"        -- idx_hard_state_ns_updated is on always-present columns (no ordering
        -- crash), but it previously lived ONLY in the v13 sentinel migration, so
        -- the migration-free `init_schema` path lacked it — the same class of
        -- init-path/migration-path divergence as the three indexes above (owner
        -- ruling A: `init_schema`'s product IS the complete current schema). The
        -- v13 migration's own doc comment already anticipated base schema would
        -- carry it. `CREATE INDEX IF NOT EXISTS` keeps v13 an idempotent no-op.
        CREATE INDEX IF NOT EXISTS idx_hard_state_ns_updated
            ON hard_state(namespace, updated_at DESC);
"#,
    ),
];

/// Byte-equality golden: the exact `BASE_SCHEMA_SQL` text this module carried
/// at schema v28, before #1585 D3 cut it into [`BASE_SCHEMA_CHUNKS`]. Test-only
/// and deliberately a verbatim duplicate — a golden that is *derived* from the
/// thing it checks proves nothing.
///
/// **Amendment discipline.** The golden is not frozen against *additive* base
/// schema growth — `CREATE TABLE IF NOT EXISTS` chunks that reach existing
/// databases through `init_schema_inner` without a version bump — it is frozen
/// against *silent edits to bytes that already shipped*. So the only legitimate
/// way to touch this constant is to insert a new chunk's bytes verbatim, in the
/// same position it occupies in [`BASE_SCHEMA_CHUNKS`], in the same commit that
/// adds the chunk, changing not one byte of any existing statement. The one
/// amendment so far: tachi#1680 D1's four provider-account tables, appended to
/// the vault section (`provider_accounts`, `provider_account_aliases`,
/// `provider_account_events`, `account_custody`). Rewriting an existing
/// statement here to make a failing assertion pass is exactly the drift this
/// golden exists to catch.
#[cfg(test)]
pub(super) const BASE_SCHEMA_SQL_V28_GOLDEN: &str = r#"
        CREATE TABLE IF NOT EXISTS memories (
            id           TEXT PRIMARY KEY,
            path         TEXT NOT NULL DEFAULT '/',
            summary      TEXT NOT NULL DEFAULT '',
            text         TEXT NOT NULL DEFAULT '',
            importance   REAL NOT NULL DEFAULT 0.7,
            timestamp    TEXT NOT NULL,
            valid_from   TEXT NOT NULL DEFAULT '',
            valid_until  TEXT,
            category     TEXT NOT NULL DEFAULT 'fact',
            topic        TEXT NOT NULL DEFAULT '',
            keywords     TEXT NOT NULL DEFAULT '[]',
            entities     TEXT NOT NULL DEFAULT '[]',
            source       TEXT NOT NULL DEFAULT 'manual',
            scope        TEXT NOT NULL DEFAULT 'general',
            archived     INTEGER NOT NULL DEFAULT 0,
            created_at   TEXT NOT NULL DEFAULT '',
            updated_at   TEXT NOT NULL DEFAULT '',
                -- tachi#1459: these two observe the search path only; reads
                -- through path-listing routes do not increment or update them.
                -- Their sole production writer is `record_access_with_updates`,
                -- reached only from `search.rs`'s `hybrid_search`, so
                -- `access_count = 0` means "never surfaced by search", NOT
                -- "never retrieved" — a row served only by `list_by_path` /
                -- `list_by_path_recent` / `list_memories_by_path_prefix`
                -- (kanban, handoffs, briefings, cards mirror, GC scans) reads
                -- zero here no matter how often it is read.
                access_count    INTEGER NOT NULL DEFAULT 0,
                scored_count    INTEGER NOT NULL DEFAULT 0,
                last_access     TEXT,
                -- tachi#1446: exposure-free recency reference. Nothing writes
                -- it yet; `last_access` above is written for every row a search
                -- returns, which is why ranking needs a separate column to read.
                last_use_at     TEXT,
                revision        INTEGER NOT NULL DEFAULT 1,
                metadata        TEXT NOT NULL DEFAULT '{}',
                superseded_by   TEXT,
                idless_identity TEXT,
                -- tachi#1459: same blind spot as `access_count` above, and these
                -- two are the tier-promotion gate. `recall_count` moves only for
                -- the FTS-matched subset of a search's results; `query_diversity`
                -- is derived from `access_history` rows carrying a non-empty
                -- query hash, which only search writes. Use rows have empty
                -- query hashes and do not add query-diversity evidence. The gate
                -- therefore rests on a search-only view of query diversity.
                recall_count    INTEGER NOT NULL DEFAULT 0,
                query_diversity INTEGER NOT NULL DEFAULT 0,
                tier            TEXT NOT NULL DEFAULT 'raw'
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

        -- Trigram FTS5 index for symbolic candidate retrieval (#1331).
        -- Accelerates unanchored LIKE '%term%' across the same columns the
        -- symbolic channel matches, without changing LIKE eligibility or the
        -- relevance-first pre-cap ORDER BY from #1154. Stores raw memories
        -- column bytes (including JSON keywords/entities) so LIKE semantics
        -- match a table scan of `memories`. case_sensitive 0 mirrors SQLite's
        -- default ASCII-case-insensitive LIKE.
        CREATE VIRTUAL TABLE IF NOT EXISTS memories_symbolic_fts USING fts5(
            id,
            path,
            summary,
            text,
            keywords,
            entities,
            topic,
            tokenize = 'trigram case_sensitive 0'
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

        -- Append-only observation ledger under the working graph (#774 Layer-2,
        -- sol audit cut ①). `memory_edges` is a mutable last-write-wins working
        -- projection: its PK (source_id, target_id, relation) + ON CONFLICT DO
        -- UPDATE collapses every re-observation of the same triple into ONE row
        -- (created_at/valid_from/valid_to included), which erases the evidence
        -- count Layer-2 induction needs. Each successful edge write appends
        -- exactly one immutable row here in the same transaction, so this
        -- ledger accumulates one row per observation while the graph keeps a
        -- single mutable projection row. `observed_at` is immutable;
        -- invalidation is a soft `invalidated_at` stamp, never a delete, so the
        -- history stays complete.
        --   Layer-2 counts observations (rows here), not graph rows
        --   (#774 sol audit ruling).
        -- Added as a pure additive CREATE TABLE IF NOT EXISTS with no
        -- schema-version bump — the same in-place-on-BASE_SCHEMA convention
        -- exec_envs / dispatch_outcomes / session_claims entered by.
        CREATE TABLE IF NOT EXISTS edge_observations (
            observation_id     TEXT PRIMARY KEY,
            source_id          TEXT NOT NULL,
            target_id          TEXT NOT NULL,
            relation           TEXT NOT NULL,
            capture_event_kind TEXT NOT NULL DEFAULT 'unknown',
            capture_event_id   TEXT NOT NULL DEFAULT '',
            actor              TEXT NOT NULL DEFAULT 'unknown',
            reason_code        TEXT NOT NULL DEFAULT '',
            observed_at        TEXT NOT NULL,
            evidence_hash      TEXT,
            invalidated_at     TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_edge_obs_edge
            ON edge_observations(source_id, target_id, relation);
        CREATE INDEX IF NOT EXISTS idx_edge_obs_observed_at
            ON edge_observations(observed_at);

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

        -- Access history for ACT-R base-level activation.
        --
        -- tachi#1446 lever 5: `event_kind` is the provenance discriminator.
        -- `display` = the recall pipeline recorded that it SHOWED this row
        -- (`record_access_with_updates`, the sole production writer before
        -- this column existed, so `DEFAULT 'display'` is the honest value for
        -- every legacy row). `use` = a caller-initiated save cited this memory
        -- (`record_memory_use`). `get_use_access_times` reads only the latter,
        -- which is what keeps the system's own display action out of the
        -- ACT-R base-level-activation floor at `scorer.rs`'s
        -- `default_decay_score_actr_with_config`.
        CREATE TABLE IF NOT EXISTS access_history (
            memory_id  TEXT NOT NULL,
            accessed_at TEXT NOT NULL,
            query_hash  TEXT NOT NULL DEFAULT '',
            event_kind  TEXT NOT NULL DEFAULT 'display'
        );
        CREATE INDEX IF NOT EXISTS idx_access_hist_mem ON access_history(memory_id);
        CREATE INDEX IF NOT EXISTS idx_access_hist_time ON access_history(accessed_at DESC);
        CREATE INDEX IF NOT EXISTS idx_access_hist_mem_time ON access_history(memory_id, accessed_at DESC);
        -- idx_access_hist_hash and idx_access_hist_mem_kind_time reference the
        -- evolutionary `query_hash` / `event_kind` columns and are created in
        -- MIGRATED_INDEXES_SQL, AFTER `ensure_column` adds them to legacy
        -- access_history tables (#1289). Creating them here would `no such
        -- column`-crash init_schema_inner on a DB that predates either column.

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

        -- Tachi continuity event ledger. This is append-only source material for
        -- typed projectors; it does not directly mutate recall, prompts, routing,
        -- execution, or domain state.
        CREATE TABLE IF NOT EXISTS tachi_events (
            id               TEXT PRIMARY KEY,
            source_repo      TEXT NOT NULL DEFAULT '',
            adapter          TEXT NOT NULL DEFAULT '',
            project          TEXT NOT NULL DEFAULT '',
            domain           TEXT NOT NULL DEFAULT '',
            session_id       TEXT NOT NULL DEFAULT '',
            actor            TEXT NOT NULL DEFAULT '',
            event_type       TEXT NOT NULL,
            authority        TEXT NOT NULL DEFAULT 'collect_only',
            effects          TEXT NOT NULL DEFAULT '[]',
            projection_hints TEXT NOT NULL DEFAULT '[]',
            payload_json     TEXT NOT NULL DEFAULT '{}',
            provenance_json  TEXT NOT NULL DEFAULT '{}',
            created_at       TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_tachi_events_created_at ON tachi_events(created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_tachi_events_project_domain ON tachi_events(project, domain, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_tachi_events_type ON tachi_events(event_type, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_tachi_events_session ON tachi_events(session_id, created_at DESC);

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

        -- LLM usage ledger for provider spend attribution. This stores only
        -- provider/model/token metadata, never prompts, responses, or secrets.
        CREATE TABLE IF NOT EXISTS llm_usage (
            id                    INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp             TEXT NOT NULL,
            lane                  TEXT NOT NULL,
            model                 TEXT NOT NULL,
            provider_host         TEXT NOT NULL DEFAULT '',
            provider_logical_name TEXT NOT NULL DEFAULT '',
            provider_key_id       TEXT NOT NULL DEFAULT '',
            prompt_tokens         INTEGER,
            completion_tokens     INTEGER,
            total_tokens          INTEGER,
            max_tokens            INTEGER NOT NULL DEFAULT 0,
            request_chars         INTEGER NOT NULL DEFAULT 0,
            response_chars        INTEGER NOT NULL DEFAULT 0,
            duration_ms           INTEGER NOT NULL DEFAULT 0,
            success               INTEGER NOT NULL DEFAULT 1,
            created_at            TEXT NOT NULL DEFAULT (STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now'))
        );
        CREATE INDEX IF NOT EXISTS idx_llm_usage_timestamp ON llm_usage(timestamp DESC);
        CREATE INDEX IF NOT EXISTS idx_llm_usage_lane ON llm_usage(lane, timestamp DESC);
        CREATE INDEX IF NOT EXISTS idx_llm_usage_provider_key ON llm_usage(provider_logical_name, provider_key_id, timestamp DESC);

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

        -- Vault provider key runtime health and selection metadata.
        -- key_id is a concrete key entry name for non-rotations,
        -- or a rotation member name for rotation pools (e.g., VOYAGE_API_KEY_1).
        CREATE TABLE IF NOT EXISTS vault_key_health (
            logical_name    TEXT NOT NULL,
            key_id          TEXT NOT NULL,
            status          TEXT NOT NULL DEFAULT 'ok',
            cooldown_until  TEXT,
            last_success    TEXT,
            last_attempt    TEXT,
            last_error      TEXT,
            error_count     INTEGER NOT NULL DEFAULT 0,
            auth_failed     INTEGER NOT NULL DEFAULT 0,
            disabled        INTEGER NOT NULL DEFAULT 0,
            metadata        TEXT NOT NULL DEFAULT '{}',
            updated_at      TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (logical_name, key_id)
        );
        CREATE INDEX IF NOT EXISTS idx_vault_key_health_status ON vault_key_health(status);
        CREATE INDEX IF NOT EXISTS idx_vault_key_health_logical ON vault_key_health(logical_name);
        CREATE INDEX IF NOT EXISTS idx_vault_key_health_cooldown ON vault_key_health(logical_name, cooldown_until);

        -- Vault provider accounts (tachi#1680 D1): public-safe account
        -- metadata only. An account row says which vendor account a
        -- credential belongs to; it never says which Vault entry or rotation
        -- member physically holds the secret. That pointer lives in
        -- `account_custody` — a separate table precisely so no serialization
        -- of an account row can carry custody by accident. `auth_ref` is the
        -- opaque ("va1:...") handle joining the two, and is NULL for
        -- metadata-only auth modes that have no stored credential at all.
        CREATE TABLE IF NOT EXISTS provider_accounts (
            account_id            TEXT PRIMARY KEY,
            provider_kind         TEXT NOT NULL,
            auth_mode             TEXT NOT NULL CHECK (auth_mode IN (
                                      'api_key_pool', 'brokered_oauth', 'cloud_iam',
                                      'provider_owned_session', 'local_no_auth', 'unsupported'
                                  )),
            auth_ref              TEXT UNIQUE,
            account_fingerprint   TEXT NOT NULL,
            account_class         TEXT NOT NULL DEFAULT 'model_api',
            capabilities          TEXT NOT NULL DEFAULT '[]',
            credential_policy_ref TEXT,
            refresh_authority     TEXT NOT NULL DEFAULT 'none',
            status                TEXT NOT NULL DEFAULT 'active',
            revision              INTEGER NOT NULL DEFAULT 1,
            source_refs           TEXT NOT NULL DEFAULT '[]',
            created_at            TEXT NOT NULL,
            updated_at            TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_provider_accounts_kind ON provider_accounts(provider_kind);
        CREATE INDEX IF NOT EXISTS idx_provider_accounts_fingerprint ON provider_accounts(account_fingerprint);

        -- Every env-var name an account has been observed under. Append and
        -- retire, never delete: two names holding one value collapse into a
        -- single account with two alias rows (#1680 D2 discrimination 1), and
        -- a name that stops being observed is marked `retired` so the record
        -- of what was once called what survives the rename.
        CREATE TABLE IF NOT EXISTS provider_account_aliases (
            account_id  TEXT NOT NULL,
            alias_name  TEXT NOT NULL,
            source_kind TEXT NOT NULL,
            first_seen  TEXT NOT NULL,
            last_seen   TEXT NOT NULL,
            retired     INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (account_id, alias_name)
        );
        CREATE INDEX IF NOT EXISTS idx_provider_account_aliases_name ON provider_account_aliases(alias_name);

        -- Append-only audit of everything that ever changed an account:
        -- creation, alias observation, member rotation, re-fingerprint after a
        -- Vault master-key rekey, apply/no-op. `revision` is the account
        -- revision the event produced (which is how identity survives rotating
        -- key material, #1680 D2); `plan_digest` binds an event to the
        -- reconcile plan that caused it. Rows are never updated or deleted.
        CREATE TABLE IF NOT EXISTS provider_account_events (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            account_id  TEXT NOT NULL,
            revision    INTEGER NOT NULL,
            event_kind  TEXT NOT NULL,
            plan_digest TEXT,
            evidence    TEXT NOT NULL DEFAULT '{}',
            created_at  TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_provider_account_events_account ON provider_account_events(account_id, id);

        -- The custody pointer for one account: which Vault rotation pool or
        -- which Vault entry actually holds its secret. Deliberately NOT columns
        -- on `provider_accounts` — a table boundary, not field discipline, is
        -- what keeps Vault layout (rotation prefixes, member names) off every
        -- account-shaped serialization surface (#1680 D1/D5). Restructuring a
        -- pool bumps `revision` here and leaves `auth_ref` — and therefore
        -- every upper-layer reference to the account — unchanged.
        CREATE TABLE IF NOT EXISTS account_custody (
            auth_ref       TEXT PRIMARY KEY,
            account_id     TEXT NOT NULL UNIQUE,
            custody_kind   TEXT NOT NULL CHECK (custody_kind IN ('vault_rotation_pool', 'vault_entry')),
            custody_target TEXT NOT NULL,
            revision       INTEGER NOT NULL,
            updated_at     TEXT NOT NULL
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

        -- Recall cache: rendered hybrid-search result rows keyed by a query
        -- context hash. Lives OUTSIDE `memories` on purpose — a prior design
        -- stored these as memory rows and they leaked into every long-lived
        -- DB. The foreground recall path writes through after computing
        -- results and short-circuits a full hybrid-search round trip on a
        -- fresh cache hit; the background rerank job upgrades entries with
        -- reranked orderings (reranked=1). Staleness is bounded by a TTL on
        -- `updated_at`, checked at read time.
        CREATE TABLE IF NOT EXISTS recall_cache (
            cache_id      TEXT PRIMARY KEY,
            generation_fingerprint TEXT NOT NULL DEFAULT '',
            query         TEXT NOT NULL DEFAULT '',
            rows_json     TEXT NOT NULL DEFAULT '[]',
            result_count  INTEGER NOT NULL DEFAULT 0,
            reranked      INTEGER NOT NULL DEFAULT 0,
            hit_count     INTEGER NOT NULL DEFAULT 0,
            created_at    TEXT NOT NULL DEFAULT '',
            updated_at    TEXT NOT NULL DEFAULT '',
            last_hit_at   TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_recall_cache_updated ON recall_cache(updated_at);

        -- Execution-environment leases (#894 S1). Daemon-owned rows are the
        -- single source of truth for a provisioned worktree/env: worktree
        -- markers (`.tachi-worktree.json`) and the global `worktrees.json`
        -- become read-only projections/backstops for offline tools (the
        -- sweep), never a second owner. `state` is the S1 lifecycle
        -- (`active` -> `reclaimed`); the reclaim transition is a transactional
        -- state flip written by exactly one reclaim function. `dispatch_id`
        -- links a lease to the dispatch that owns it.
        --
        -- `env_class` (#894 S2c) is the provisioning policy class — a closed
        -- vocabulary of `edit-only` (default) | `build-ticketed` |
        -- `build-private`. It decides ONE thing at provision time: whether a
        -- `build_target` resource is allocated and bound to this lease
        -- (`edit-only` gets none — that is the ~14MB tree). It is NOT a
        -- security boundary: a worker with a shell and the same UID can run
        -- cargo in an `edit-only` tree regardless of what this column says
        -- (owner-ratified 2026-07-13). Its value is disk (no target dir) and
        -- default routing (builds go to the broker's serialized executor seat
        -- instead of poisoning a shared target from N diverged trees).
        -- Retrofitted onto existing DBs by the v15 sentinel migration; legacy
        -- rows read back `edit-only` because they were never provisioned under
        -- a class at all and carry no resource bindings (S2a is newer).
        CREATE TABLE IF NOT EXISTS exec_envs (
            env_id         TEXT PRIMARY KEY,
            kind           TEXT NOT NULL DEFAULT 'worktree',
            path           TEXT NOT NULL,
            repo_root      TEXT NOT NULL DEFAULT '',
            branch         TEXT NOT NULL DEFAULT '',
            base_sha       TEXT NOT NULL DEFAULT '',
            dispatch_id    TEXT,
            agent_identity_id TEXT,
            claim_id       TEXT,
            env_class      TEXT NOT NULL DEFAULT 'edit-only',
            state          TEXT NOT NULL DEFAULT 'active',
            reclaim_reason TEXT,
            schema_version INTEGER NOT NULL DEFAULT 1,
            created_at     TEXT NOT NULL DEFAULT '',
            reclaimed_at   TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_exec_envs_state ON exec_envs(state);
        CREATE INDEX IF NOT EXISTS idx_exec_envs_path ON exec_envs(path);
        CREATE INDEX IF NOT EXISTS idx_exec_envs_dispatch ON exec_envs(dispatch_id);
        -- idx_exec_envs_claim references the v21 `claim_id` column and is created
        -- in MIGRATED_INDEXES_SQL, AFTER `ensure_column` adds claim_id to legacy
        -- exec_envs tables (#1289). Creating it here would `no such column:
        -- claim_id`-crash init_schema_inner on a pre-v21 DB.

        -- Execution-environment RESOURCE ledger (#894 S2a). `exec_envs` tracks
        -- the *lease*; these two tables track the BYTES that lease owns —
        -- because a `reclaimed` lease row turned out to be SQLite-only
        -- bookkeeping (#1029): the row flipped, the disk stayed full. One row
        -- here = one physical resource (a worktree dir, a shared cargo target,
        -- a scratch dir, a project DB), and `state = 'reclaimed'` means the
        -- bytes are ACTUALLY gone. `reclaim_resource` stamps `reclaiming`, the
        -- CALLER deletes from the filesystem (memcore never touches the FS),
        -- then it records `reclaimed_bytes` and flips `reclaimed`. A crash in
        -- between leaves the row in `reclaiming`, which is re-enterable
        -- (at-least-once, idempotent — `reclaimed_bytes` is assigned, never
        -- accumulated). `reclaim_failed` is a failed delete (retryable);
        -- `quarantined` is "hands off, a human/broker decides" and is entered
        -- through `quarantine_resource`; S2c adds `release_quarantine` as the
        -- one way back out (quarantined -> active), gated on the caller
        -- verifying or clearing the bytes first.
        --
        -- `state` has exactly four writers, all in `db::exec_env_resources` and
        -- all taking an IMMEDIATE transaction so their read-then-write is atomic
        -- against each other: `reclaim_resource`, `quarantine_resource`,
        -- `release_quarantine`, and `insert_resource`'s re-registration path (a
        -- `reclaimed` (path, kind) is revived as `active` under a fresh
        -- resource_id — the reclaimer churns the same worktree dirs, so a path
        -- must be registerable more than once in the life of the DB). Nothing
        -- writes `state` with ad-hoc SQL.
        --
        -- Bindings are many-to-many on purpose: one shared `build_target`
        -- (CARGO_TARGET_DIR) is bound by every live lease at once. refcount =
        -- bindings with `released_at IS NULL`; a resource with refcount > 0 is
        -- NOT reclaimable (`reclaim_resource` returns a typed
        -- `BlockedByBinding`, never a silent skip), so a shared target is freed
        -- only when the LAST binding is released. `bind_resource` re-reads the
        -- resource state inside its own IMMEDIATE transaction, so a bind and a
        -- reclaim of the same resource can never both commit (which would leave a
        -- live binding pointing at deleted bytes). `UNIQUE (env_id, resource_id)`
        -- makes a re-bind of the same pair a re-activation of that one row, not
        -- a second binding, so refcount can't be inflated by a retry.
        --
        -- Added as pure additive CREATE TABLE IF NOT EXISTS with no
        -- schema-version bump — the same in-place-on-BASE_SCHEMA convention
        -- exec_envs / dispatch_outcomes / session_claims / edge_observations
        -- entered by.
        CREATE TABLE IF NOT EXISTS exec_env_resources (
            resource_id     TEXT PRIMARY KEY,
            -- closed vocabulary: worktree | build_target | scratch_dir | project_db
            kind            TEXT NOT NULL,
            -- canonicalized absolute path
            path            TEXT NOT NULL,
            -- most recent measurement (caller-measured; memcore only stores it)
            bytes           INTEGER,
            measured_at     TEXT,
            -- active | reclaiming | reclaimed | reclaim_failed | quarantined
            state           TEXT NOT NULL DEFAULT 'active',
            reclaim_reason  TEXT,
            reclaimed_at    TEXT,
            -- bytes actually freed from the filesystem, stamped after the delete
            reclaimed_bytes INTEGER,
            created_at      TEXT NOT NULL DEFAULT '',
            updated_at      TEXT NOT NULL DEFAULT '',
            UNIQUE (path, kind)
        );
        CREATE INDEX IF NOT EXISTS idx_exec_env_resources_state
            ON exec_env_resources(state);
        CREATE INDEX IF NOT EXISTS idx_exec_env_resources_kind
            ON exec_env_resources(kind);

        CREATE TABLE IF NOT EXISTS exec_env_resource_bindings (
            binding_id  TEXT PRIMARY KEY,
            env_id      TEXT NOT NULL,
            resource_id TEXT NOT NULL,
            created_at  TEXT NOT NULL DEFAULT '',
            released_at TEXT,
            UNIQUE (env_id, resource_id)
        );
        CREATE INDEX IF NOT EXISTS idx_exec_env_resource_bindings_env
            ON exec_env_resource_bindings(env_id);
        CREATE INDEX IF NOT EXISTS idx_exec_env_resource_bindings_resource
            ON exec_env_resource_bindings(resource_id);

        -- Canonical dispatch outcome ledger (#773 v4 sol carve). Append-only:
        -- one row per (dispatch_id, task_type) completion, written FIRST by
        -- the `tachi_complete` seam before any derived row (eval memory,
        -- signature evidence, precedent) — a derivation failure must never
        -- lose this row (see `complete_ops::dispatch_outcome`). This is the
        -- future router's real read surface; `/eval` stays the searchable
        -- narrative projection and cards remain a reviewed projection on
        -- top. Graph edges (seat->performed->dispatch, dispatch->produced->
        -- outcome, outcome->classified_as->signature, outcome->about->
        -- issue/PR) are deferred to the S4 seat; every ref this row carries
        -- (issue_ref/pr_ref/flow_id/dispatch_id/eval_memory_id) is present
        -- so those edges can be derived later without a re-migration.
        -- Adjudication evidence lives in the append-only dispatch_adjudications
        -- table (#1035); this table holds mutable execution facts only — a
         -- replayed complete may rewrite mutable execution columns here; the
         -- identity receipt is frozen on its first write.
        --
        -- Truthfulness (#773 Layer-2 ②): `execution_outcome` is the
        -- MACHINE-RESOLVED terminal verdict (after the #878-A completion
        -- predicate intercepts a false success, or the terminal state a
        -- non-`tachi_complete` path reached), NOT the raw self-report;
        -- `reported_outcome` keeps the agent's verbatim claim (NULL when a
        -- terminal path carried no self-report). New `reported_outcome` column
        -- is back-filled onto existing DBs by the v14 sentinel migration.
        CREATE TABLE IF NOT EXISTS dispatch_outcomes (
            outcome_id       TEXT PRIMARY KEY,
            dispatch_id      TEXT NOT NULL DEFAULT '',
            eval_memory_id   TEXT,
            model            TEXT,
            vendor           TEXT NOT NULL DEFAULT 'unknown',
            role             TEXT,
            seat             TEXT,
            task_type        TEXT,
            -- machine-resolved terminal verdict (completed/failed/aborted/partial)
            execution_outcome    TEXT NOT NULL,
            -- raw self-reported outcome, verbatim; NULL for no-self-report terminals
            reported_outcome     TEXT,
            -- retry_count: awaiting dispatch-context plumb (no source at write
            -- points yet) — stays 0 until the dispatch retry ledger is wired.
            retry_count      INTEGER NOT NULL DEFAULT 0,
            error_class      TEXT,
            issue_ref        TEXT,
            pr_ref           TEXT,
            flow_id          TEXT,
            cost_tokens      INTEGER,
            cost_usd         REAL,
            verification_present INTEGER NOT NULL DEFAULT 0,
             diff_present         INTEGER NOT NULL DEFAULT 0,
             evidence_refs    TEXT NOT NULL DEFAULT '[]',
             -- Immutable receipt copied from the accepted run ledger. NULL is
             -- a valid explicit legacy/unattributed state.
             identity_receipt TEXT,
             -- Evidentiary basis of the flat identity columns above
             -- (planned_unconfirmed | acknowledged_overlay | observed |
             -- fallback_unreceipted | unknown). Frozen with the receipt so a
             -- reader can always tell planned routing intent from
             -- carrier-observed execution fact (#1065 option D). Back-filled
             -- onto existing DBs by the v17 sentinel migration.
             identity_attribution_basis TEXT NOT NULL DEFAULT 'unknown',
             idempotency_key  TEXT NOT NULL,
            created_at       TEXT NOT NULL DEFAULT '',
            updated_at       TEXT NOT NULL DEFAULT '',
            UNIQUE (idempotency_key)
        );
        CREATE INDEX IF NOT EXISTS idx_dispatch_outcomes_vendor_ts
            ON dispatch_outcomes(vendor, created_at);
        CREATE INDEX IF NOT EXISTS idx_dispatch_outcomes_issue_ref
            ON dispatch_outcomes(issue_ref);
        CREATE INDEX IF NOT EXISTS idx_dispatch_outcomes_dispatch_id
            ON dispatch_outcomes(dispatch_id);

        -- #1035: machine execution facts remain in dispatch_outcomes. Terminal
        -- leader judgment is an append-only event linked by outcome_id.
        CREATE TABLE IF NOT EXISTS dispatch_adjudications (
            adjudication_id TEXT PRIMARY KEY,
            outcome_id TEXT NOT NULL,
            event_key TEXT NOT NULL UNIQUE,
            verdict TEXT,
            not_required_reason TEXT,
            actor TEXT NOT NULL CHECK (length(trim(actor)) > 0),
            evidence_ref TEXT NOT NULL CHECK (length(trim(evidence_ref)) > 0),
            created_at TEXT NOT NULL DEFAULT '',
            insertion_seq INTEGER NOT NULL,
            UNIQUE (outcome_id, insertion_seq),
            CHECK (
                (verdict IS NOT NULL AND length(trim(verdict)) > 0 AND not_required_reason IS NULL)
                OR
                (verdict IS NULL AND not_required_reason IS NOT NULL AND length(trim(not_required_reason)) > 0)
            )
        );
        CREATE INDEX IF NOT EXISTS idx_dispatch_adjudications_outcome
            ON dispatch_adjudications(outcome_id, created_at);

        -- One adjudication event may classify multiple canonical signatures.
        CREATE TABLE IF NOT EXISTS dispatch_adjudication_signatures (
            adjudication_id TEXT NOT NULL,
            signature_id TEXT NOT NULL,
            evidence_ref TEXT,
            resolved INTEGER NOT NULL DEFAULT 0 CHECK (resolved IN (0, 1)),
            PRIMARY KEY (adjudication_id, signature_id)
        );
        CREATE INDEX IF NOT EXISTS idx_dispatch_adjudication_signatures_signature
            ON dispatch_adjudication_signatures(signature_id);

        -- #1066: first-class mirror eval intake for harness-native
        -- subagents — a NEW ledger backing the SAME `tachi_agent_eval`
        -- facade's register/observe/adjudicate/get actions, mirroring the
        -- dispatch_outcomes/dispatch_adjudications split above for work
        -- Tachi did not dispatch (a host-native subagent Tachi only
        -- observes). See `memcore::db::mirror_eval` for the write contract.
        CREATE TABLE IF NOT EXISTS mirror_eval_runs (
            eval_run_id         TEXT PRIMARY KEY,
            register_key        TEXT NOT NULL UNIQUE,
            frozen_contract_ref TEXT NOT NULL CHECK (length(trim(frozen_contract_ref)) > 0),
            execution_origin    TEXT NOT NULL CHECK (length(trim(execution_origin)) > 0),
            lifecycle_owner     TEXT NOT NULL CHECK (length(trim(lifecycle_owner)) > 0),
            harness             TEXT,
            native_child_id     TEXT,
            requested_profile   TEXT,
            requested_model     TEXT,
            requested_agent     TEXT,
            created_at          TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_mirror_eval_runs_native_child
            ON mirror_eval_runs(native_child_id, created_at);

        -- At most one terminal observation per run: carrier-observed facts
        -- only, no judgment field exists on this table (#1066 AC-3).
        CREATE TABLE IF NOT EXISTS mirror_eval_observations (
            observation_id   TEXT PRIMARY KEY,
            eval_run_id      TEXT NOT NULL UNIQUE,
            terminal_outcome TEXT NOT NULL CHECK (length(trim(terminal_outcome)) > 0),
            duration_ms      INTEGER,
            cost_tokens      INTEGER,
            cost_usd         REAL,
            result_ref       TEXT,
            artifacts        TEXT NOT NULL DEFAULT '[]',
            effective_model    TEXT,
            effective_backend  TEXT,
            effective_harness  TEXT,
            created_at       TEXT NOT NULL DEFAULT ''
        );

        -- Append-only leader/independent-reviewer judgment events, one row
        -- per adjudication event, linked to a run by eval_run_id (#1035
        -- pattern reused verbatim: event_key idempotency + durable
        -- per-run insertion_seq for ordered corrections).
        CREATE TABLE IF NOT EXISTS mirror_eval_adjudications (
            adjudication_id       TEXT PRIMARY KEY,
            eval_run_id           TEXT NOT NULL,
            event_key             TEXT NOT NULL UNIQUE,
            actor                 TEXT NOT NULL CHECK (length(trim(actor)) > 0),
            verifier_model        TEXT,
            usefulness            TEXT NOT NULL CHECK (length(trim(usefulness)) > 0),
            failure_mode          TEXT,
            first_review_findings TEXT NOT NULL DEFAULT '[]',
            plan_delta            TEXT,
            next_prompt_delta     TEXT,
            evidence_usable       INTEGER NOT NULL CHECK (evidence_usable IN (0, 1)),
            used_in_final_claim   INTEGER NOT NULL DEFAULT 0 CHECK (used_in_final_claim IN (0, 1)),
            human_override        INTEGER NOT NULL DEFAULT 0 CHECK (human_override IN (0, 1)),
            evidence_ref          TEXT NOT NULL CHECK (length(trim(evidence_ref)) > 0),
            created_at            TEXT NOT NULL DEFAULT '',
            insertion_seq         INTEGER NOT NULL,
            UNIQUE (eval_run_id, insertion_seq)
        );
        CREATE INDEX IF NOT EXISTS idx_mirror_eval_adjudications_run
            ON mirror_eval_adjudications(eval_run_id, created_at);

        -- tachi#1675 PR1: decision-fact ledger, NOT a terminal ledger — zero
        -- execution/terminal-state columns live here (design D1). Captures
        -- the ONLY moment a routing candidate set exists in memory: one row
        -- per `handle_dispatch_recommendation` call (Seam A), append-only,
        -- never deduplicated (every consult is a new fact, even a repeat
        -- consult with identical content). `candidates` is the full scored
        -- candidate array (profile/agent/score/reasons/...) verbatim;
        -- `policy_source_revision` is the content-bearing route-policy
        -- snapshot hash (`route_policy_source_revision`, reused not
        -- reinvented) so a later replay can tell whether the active policy
        -- has since changed underneath this recommendation.
        CREATE TABLE IF NOT EXISTS route_recommendations (
            recommendation_id      TEXT PRIMARY KEY,
            task_type               TEXT,
            risk                    TEXT NOT NULL DEFAULT 'unknown',
            candidates              TEXT NOT NULL DEFAULT '[]',
            recommended_profile     TEXT,
            policy_source_revision  TEXT,
            rows_considered         INTEGER NOT NULL DEFAULT 0,
            occurred_at             TEXT NOT NULL DEFAULT '',
            recorded_at             TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_route_recommendations_recorded_at
            ON route_recommendations(recorded_at);

        -- tachi#1675 PR1: acceptance-moment decision fact (Seam B), keyed
        -- UNIQUE on dispatch_id so a replayed acceptance is a zero-write
        -- idempotent no-op (design D2) — status.json is written FIRST (the
        -- canonical acceptance receipt) and this row follows in the same
        -- code path; a crash between the two leaves no row, and the
        -- projection's rule for a missing row is `assignment_mode :=
        -- 'unadvised'`, the honest floor, never fabricated advice. This is
        -- an acknowledged dual-write, not a transaction with status.json
        -- (status.json is a filesystem atomic write, not a DB write).
        -- `recommendation_id` is a nullable reference, not an enforced FK:
        -- `staff_start` does not always consult `recommend` first, so
        -- NULL-recommendation must stay legal (day-one reality, not an
        -- aspiration). `work_claim_id` stays nullable until #1239 wires v21
        -- claims through the dispatch lifecycle (spec correction 1).
        CREATE TABLE IF NOT EXISTS route_decisions (
            route_decision_id  TEXT PRIMARY KEY,
            dispatch_id         TEXT NOT NULL,
            recommendation_id   TEXT,
            selected_profile    TEXT,
            selected_model      TEXT,
            assignment_mode     TEXT NOT NULL CHECK (assignment_mode IN ('advised', 'unadvised', 'user_forced', 'experiment')),
            override_flag       INTEGER NOT NULL DEFAULT 0 CHECK (override_flag IN (0, 1)),
            contract_hash       TEXT,
            env_id              TEXT,
            host_profile        TEXT,
            work_claim_id       TEXT,
            occurred_at         TEXT NOT NULL DEFAULT '',
            recorded_at         TEXT NOT NULL DEFAULT '',
            UNIQUE (dispatch_id)
        );
        CREATE INDEX IF NOT EXISTS idx_route_decisions_recommendation
            ON route_decisions(recommendation_id);

        -- tachi#1675 PR1: structured adjudication companion (design D3),
        -- generic key (subject_kind, adjudication_id) serving BOTH the
        -- dispatch spine (dispatch_adjudications) and the mirror spine
        -- (mirror_eval_adjudications) without altering either table. A
        -- rubric row is an OPTIONAL companion to the free-text `verdict`
        -- that already exists on both adjudication tables — no rubric row
        -- means `excluded_reason='unstructured_verdict'` at the projection
        -- layer, not an error here. Six ordinal judged dimensions (closed
        -- sets, never floats — floats invite averaging into a forbidden
        -- one-dimensional reputation score). `independence_basis` is
        -- computed by the writer at write time (design D5); `identity_bound`
        -- is a reserved enum value never written in this phase. `rubric_hash`
        -- pins the exact rubric_v1 code-constant definition a row was judged
        -- against, so a later rubric revision cannot silently reinterpret an
        -- old row. Append-only: no UPDATE/DELETE accessor exists — a
        -- corrected judgment is a NEW adjudication event (new
        -- adjudication_id) carrying its own NEW rubric row, never an edit of
        -- an old one; UNIQUE(subject_kind, adjudication_id) enforces exactly
        -- one rubric row per adjudication event at the schema level.
        CREATE TABLE IF NOT EXISTS eval_rubric_scores (
            rubric_score_id          TEXT PRIMARY KEY,
            adjudication_id          TEXT NOT NULL,
            subject_kind             TEXT NOT NULL CHECK (subject_kind IN ('dispatch', 'mirror')),
            rubric_hash              TEXT NOT NULL,
            contract_correctness     TEXT NOT NULL CHECK (contract_correctness IN ('pass', 'concern', 'fail', 'not_assessed')),
            evidence_quality         TEXT NOT NULL CHECK (evidence_quality IN ('pass', 'concern', 'fail', 'not_assessed')),
            safety                   TEXT NOT NULL CHECK (safety IN ('pass', 'concern', 'fail', 'not_assessed')),
            scope_discipline         TEXT NOT NULL CHECK (scope_discipline IN ('pass', 'concern', 'fail', 'not_assessed')),
            intervention_burden      TEXT NOT NULL CHECK (intervention_burden IN ('pass', 'concern', 'fail', 'not_assessed')),
            completion_integrity     TEXT NOT NULL CHECK (completion_integrity IN ('pass', 'concern', 'fail', 'not_assessed')),
            adjudication_confidence  TEXT NOT NULL CHECK (adjudication_confidence IN ('low', 'medium', 'high')),
            adjudicator_actor        TEXT NOT NULL CHECK (length(trim(adjudicator_actor)) > 0),
            adjudicator_vendor       TEXT NOT NULL DEFAULT 'unknown',
            independence_basis       TEXT NOT NULL CHECK (independence_basis IN ('structural_cross_vendor', 'declared_only', 'self', 'identity_bound')),
            occurred_at              TEXT NOT NULL DEFAULT '',
            recorded_at              TEXT NOT NULL DEFAULT '',
            UNIQUE (subject_kind, adjudication_id)
        );
        CREATE INDEX IF NOT EXISTS idx_eval_rubric_scores_adjudication
            ON eval_rubric_scores(subject_kind, adjudication_id);

        -- Cross-session presence claims (#1001). Advisory "who's working on
        -- what" lease rows — NOT a mutual-exclusion lock. A session/dispatch
        -- registers a claim on an issue/lane when it starts touching it and
        -- heartbeats it on every briefing read; a claim with a stale
        -- `heartbeat_at` (older than the TTL) is treated as expired by
        -- readers without a separate reaper process (lazy expiry, same spirit
        -- as `find_active_exec_env_by_path` filtering by state). `state` is
        -- the three-state lifecycle (`active` -> `orphaned` -> `released`).
        -- Lease expiry only orphans a claim; v21 ownership changes require an
        -- explicit caller-versioned release or handoff.
        CREATE TABLE IF NOT EXISTS session_claims (
            claim_id             TEXT PRIMARY KEY,
            session_client       TEXT,
            issue_ref            TEXT,
            flow_id              TEXT,
            dispatch_id          TEXT,
            branch               TEXT NOT NULL DEFAULT '',
            worktree_path        TEXT,
            declared_file_scope  TEXT,
            agent_identity_id    TEXT,
            role                 TEXT,
            mode                 TEXT,
            expected_head        TEXT,
            lease_expires_at     TEXT,
            transition_version   INTEGER NOT NULL DEFAULT 0,
            exec_env_id          TEXT,
            orphaned_at          TEXT,
            state                TEXT NOT NULL DEFAULT 'active',
            release_reason       TEXT,
            created_at           TEXT NOT NULL DEFAULT '',
            heartbeat_at         TEXT NOT NULL DEFAULT '',
            released_at          TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_session_claims_state ON session_claims(state);
        CREATE INDEX IF NOT EXISTS idx_session_claims_issue ON session_claims(issue_ref);
        CREATE INDEX IF NOT EXISTS idx_session_claims_flow ON session_claims(flow_id);
        -- Legacy #1001 identity-triple uniqueness. v21 WorkClaims carry an
        -- explicit mode and use transactional collision semantics instead;
        -- keeping this partial index to legacy rows preserves old upsert
        -- behavior without making same-issue v21 claims inherently exclusive.
        -- upsert_or_heartbeat_claim's read-then-write relies on at the
        -- application level. Partial (WHERE state='active' AND mode IS NULL)
        -- so only legacy presence rows participate and a released historical
        -- row never blocks a fresh active claim for the same identity;
        -- COALESCE(..., '') on each nullable column so two active
        -- rows that are both NULL in the same slot collide the same way the
        -- upsert's `IS ?` lookup already treats them as one identity (see
        -- `migrations/session_claims_identity.rs` for the full rationale and
        -- the migration that retrofits this onto pre-existing DBs).
        --
        -- The index's `WHERE ... AND mode IS NULL` predicate references the v21
        -- `mode` column, so it is created in MIGRATED_INDEXES_SQL AFTER
        -- `ensure_column` adds `mode` to legacy session_claims tables (#1289).
        -- Creating it here would `no such column: mode`-crash init_schema_inner
        -- on a pre-v21 DB.

        -- #1253 identity / WorkClaim spine. These tables and columns are
        -- deliberately additive: a pre-v21 claim has no identity rather than
        -- a made-up one.
        CREATE TABLE IF NOT EXISTS agent_identities (
            agent_identity_id TEXT PRIMARY KEY,
            display_name      TEXT,
            seat              TEXT,
            capability_json   TEXT,
            created_at        TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE IF NOT EXISTS identity_admissions (
            admission_id      TEXT PRIMARY KEY,
            agent_identity_id TEXT,
            connection_id     TEXT NOT NULL,
            state             TEXT NOT NULL CHECK (state IN ('self_asserted', 'verified', 'rejected', 'unavailable')),
            rejection_evidence TEXT,
            created_at        TEXT NOT NULL DEFAULT '',
            UNIQUE(agent_identity_id, connection_id)
        );
        CREATE INDEX IF NOT EXISTS idx_identity_admissions_connection ON identity_admissions(connection_id);
"#;

/// Byte-equality golden for the former `MIGRATED_INDEXES_SQL`; see
/// [`BASE_SCHEMA_SQL_V28_GOLDEN`].
#[cfg(test)]
pub(super) const MIGRATED_INDEXES_SQL_V28_GOLDEN: &str = r#"
        CREATE INDEX IF NOT EXISTS idx_memories_archived    ON memories(archived);
        CREATE INDEX IF NOT EXISTS idx_memories_last_access ON memories(last_access DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_valid_time  ON memories(valid_from, valid_until);
        CREATE INDEX IF NOT EXISTS idx_derived_source       ON derived_items(source);
        CREATE INDEX IF NOT EXISTS idx_derived_path         ON derived_items(path);
        CREATE INDEX IF NOT EXISTS idx_derived_created_at   ON derived_items(created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_hub_cap_review_status ON hub_capabilities(review_status);
        CREATE INDEX IF NOT EXISTS idx_hub_cap_health_status ON hub_capabilities(health_status);
        CREATE INDEX IF NOT EXISTS idx_memories_retention_policy ON memories(retention_policy);
        CREATE INDEX IF NOT EXISTS idx_memories_domain ON memories(domain);
        CREATE INDEX IF NOT EXISTS idx_memories_superseded ON memories(superseded_by);
        CREATE INDEX IF NOT EXISTS idx_memories_tier ON memories(tier);
        CREATE INDEX IF NOT EXISTS idx_memories_recall ON memories(recall_count DESC);

        -- Indexes on evolutionary columns of NON-memories tables. These MUST be
        -- created here (after init_schema_inner's `ensure_column` calls) rather
        -- than inline in BASE_SCHEMA_SQL: on a legacy DB the table already
        -- exists so `CREATE TABLE IF NOT EXISTS` is a no-op and does NOT add the
        -- column, and a bare `CREATE INDEX` referencing that column would crash
        -- init_schema_inner with `no such column` before any sentinel migration
        -- ran (#1289). The matching `ensure_column` for each column runs above.
        CREATE INDEX IF NOT EXISTS idx_access_hist_hash
            ON access_history(memory_id, query_hash) WHERE query_hash != '';
        -- tachi#1446 lever 5. `get_use_access_times` and the per-kind GC quota
        -- both partition by (memory_id, event_kind) and order by accessed_at
        -- DESC; without this they fall back to idx_access_hist_mem_time and
        -- re-filter every display row to find the rare use rows.
        CREATE INDEX IF NOT EXISTS idx_access_hist_mem_kind_time
            ON access_history(memory_id, event_kind, accessed_at DESC);
        CREATE INDEX IF NOT EXISTS idx_exec_envs_claim ON exec_envs(claim_id);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_session_claims_identity_active
            ON session_claims(COALESCE(session_client, ''), COALESCE(issue_ref, ''), COALESCE(flow_id, ''))
            WHERE state = 'active' AND mode IS NULL;
        -- idx_hard_state_ns_updated is on always-present columns (no ordering
        -- crash), but it previously lived ONLY in the v13 sentinel migration, so
        -- the migration-free `init_schema` path lacked it — the same class of
        -- init-path/migration-path divergence as the three indexes above (owner
        -- ruling A: `init_schema`'s product IS the complete current schema). The
        -- v13 migration's own doc comment already anticipated base schema would
        -- carry it. `CREATE INDEX IF NOT EXISTS` keeps v13 an idempotent no-op.
        CREATE INDEX IF NOT EXISTS idx_hard_state_ns_updated
            ON hard_state(namespace, updated_at DESC);
"#;

#[cfg(test)]
mod golden_tests {
    use super::*;

    /// The whole point of the #1585 D3 chunking: a `TachiFull` store must get
    /// byte-identical DDL to what it got before the split. Concatenating every
    /// chunk — Portable AND Product, in declaration order — must reproduce the
    /// v28 literal exactly.
    #[test]
    fn base_schema_chunks_concatenate_to_the_v28_golden() {
        let joined: String = BASE_SCHEMA_CHUNKS.iter().map(|(_, sql)| *sql).collect();
        assert_eq!(
            joined.as_bytes(),
            BASE_SCHEMA_SQL_V28_GOLDEN.as_bytes(),
            "BASE_SCHEMA_CHUNKS no longer reproduce the v28 base schema byte-for-byte"
        );
    }

    #[test]
    fn migrated_index_chunks_concatenate_to_the_v28_golden() {
        let joined: String = MIGRATED_INDEXES_CHUNKS
            .iter()
            .map(|(_, sql)| *sql)
            .collect();
        assert_eq!(
            joined.as_bytes(),
            MIGRATED_INDEXES_SQL_V28_GOLDEN.as_bytes(),
            "MIGRATED_INDEXES_CHUNKS no longer reproduce the v28 migrated indexes byte-for-byte"
        );
    }

    /// A chunk list where every chunk carried the same scope would pass the
    /// golden test and silently defeat the whole split, so pin that both
    /// scopes are actually present on both lists.
    #[test]
    fn both_scopes_are_represented_on_both_chunk_lists() {
        for (label, list) in [
            ("base", BASE_SCHEMA_CHUNKS),
            ("migrated indexes", MIGRATED_INDEXES_CHUNKS),
        ] {
            assert!(
                list.iter()
                    .any(|(scope, _)| *scope == SchemaScope::Portable),
                "{label} chunks carry no Portable chunk"
            );
            assert!(
                list.iter().any(|(scope, _)| *scope == SchemaScope::Product),
                "{label} chunks carry no Product chunk"
            );
        }
    }

    /// Every product table named by #1585 D4's classification (the four
    /// third-bucket tables plus the deny-list families) must be created by a
    /// Product chunk, and every portable table by a Portable chunk. This is
    /// the assertion that would have caught a misfiled chunk tag — the golden
    /// above cannot, because it is scope-blind by construction.
    #[test]
    fn chunk_scopes_match_the_frozen_table_classification() {
        for (table, expected) in [
            // Portable — the memory kernel.
            ("memories", SchemaScope::Portable),
            ("memories_fts", SchemaScope::Portable),
            ("memories_symbolic_fts", SchemaScope::Portable),
            ("memory_edges", SchemaScope::Portable),
            ("edge_observations", SchemaScope::Portable),
            ("hard_state", SchemaScope::Portable),
            ("access_history", SchemaScope::Portable),
            ("derived_items", SchemaScope::Portable),
            ("processed_events", SchemaScope::Portable),
            ("tachi_events", SchemaScope::Portable),
            ("recall_cache", SchemaScope::Portable),
            // Product — D4's four third-bucket tables.
            ("audit_log", SchemaScope::Product),
            ("agent_known_state", SchemaScope::Product),
            ("llm_usage", SchemaScope::Product),
            ("sandbox_rules", SchemaScope::Product),
            ("sandbox_policies", SchemaScope::Product),
            ("sandbox_exec_audit", SchemaScope::Product),
            // Product — the deny-list families.
            ("hub_capabilities", SchemaScope::Product),
            ("hub_version_routes", SchemaScope::Product),
            ("virtual_capability_bindings", SchemaScope::Product),
            ("vault_config", SchemaScope::Product),
            ("vault_entries", SchemaScope::Product),
            ("vault_audit", SchemaScope::Product),
            ("vault_key_rotations", SchemaScope::Product),
            ("vault_key_health", SchemaScope::Product),
            // tachi#1680 D1 — provider accounts. Product, for the same reason
            // every other vault table is: they only exist alongside a Vault.
            // A PortableKernel store must not carry them (the parity side of
            // this is `store::profile_identity_tests::PRODUCT_TABLES`).
            ("provider_accounts", SchemaScope::Product),
            ("provider_account_aliases", SchemaScope::Product),
            ("provider_account_events", SchemaScope::Product),
            ("account_custody", SchemaScope::Product),
            ("foundry_jobs", SchemaScope::Product),
            ("foundry_config", SchemaScope::Product),
            ("exec_envs", SchemaScope::Product),
            ("exec_env_resources", SchemaScope::Product),
            ("exec_env_resource_bindings", SchemaScope::Product),
            ("dispatch_outcomes", SchemaScope::Product),
            ("dispatch_adjudications", SchemaScope::Product),
            ("dispatch_adjudication_signatures", SchemaScope::Product),
            ("mirror_eval_runs", SchemaScope::Product),
            ("mirror_eval_observations", SchemaScope::Product),
            ("mirror_eval_adjudications", SchemaScope::Product),
            // Product — tachi#1675 PR1 decision-fact ledger + rubric companion.
            ("route_recommendations", SchemaScope::Product),
            ("route_decisions", SchemaScope::Product),
            ("eval_rubric_scores", SchemaScope::Product),
            ("session_claims", SchemaScope::Product),
            ("agent_identities", SchemaScope::Product),
            ("identity_admissions", SchemaScope::Product),
        ] {
            let create = format!("TABLE IF NOT EXISTS {table} ");
            let matches: Vec<SchemaScope> = BASE_SCHEMA_CHUNKS
                .iter()
                .filter(|(_, sql)| sql.contains(&create))
                .map(|(scope, _)| *scope)
                .collect();
            assert_eq!(
                matches.len(),
                1,
                "expected exactly one base chunk to create {table}, found {}",
                matches.len()
            );
            assert_eq!(matches[0], expected, "wrong schema scope for table {table}");
        }
    }

    /// Sibling to `chunk_scopes_match_the_frozen_table_classification`,
    /// covering the *second* SQL split point (#1585 review): that test only
    /// inspected [`BASE_SCHEMA_CHUNKS`], leaving [`MIGRATED_INDEXES_CHUNKS`]'s
    /// own Product/Portable tags unverified. A mistagged index chunk here has
    /// the same failure mode as a mistagged table chunk — an index on
    /// `exec_envs` filed as Portable would crash `CREATE INDEX ... ON
    /// <missing table>` on a `PortableKernel` store; filed the other way, a
    /// Portable index would be silently dropped from a `PortableKernel`
    /// store's schema with no failing assertion anywhere. Per
    /// [`MIGRATED_INDEXES_CHUNKS`]'s own doc comment, the `hub_capabilities`,
    /// `exec_envs`, and `session_claims` index chunks are Product; every
    /// other chunk in this list is Portable.
    #[test]
    fn migrated_index_chunk_scopes_match_the_frozen_table_classification() {
        const PRODUCT_INDEX_TABLES: &[&str] = &["hub_capabilities", "exec_envs", "session_claims"];

        for (i, (scope, sql)) in MIGRATED_INDEXES_CHUNKS.iter().enumerate() {
            let touches_product_table = PRODUCT_INDEX_TABLES.iter().any(|table| {
                sql.contains(&format!("ON {table}(")) || sql.contains(&format!("ON {table} ("))
            });
            let expected = if touches_product_table {
                SchemaScope::Product
            } else {
                SchemaScope::Portable
            };
            assert_eq!(
                *scope, expected,
                "MIGRATED_INDEXES_CHUNKS[{i}] is tagged {scope:?} but its SQL touches a table \
                 classified {expected:?}: {sql}"
            );
        }
    }
}
