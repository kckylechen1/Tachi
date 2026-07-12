/// Connection-level PRAGMAs that must run OUTSIDE any transaction.
/// `journal_mode` in particular is a no-op (and on some SQLite builds an
/// error) when issued mid-transaction, so this is executed before any
/// `BEGIN` — see `init_schema_with_label_mut` (#984 F1 round 3).
pub(super) const CONNECTION_PRAGMA_SQL: &str = r#"
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;
        PRAGMA busy_timeout = 5000;
        PRAGMA cache_size = -16000;   -- 16 MB page cache
"#;

pub(super) const BASE_SCHEMA_SQL: &str = r#"
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
                access_count    INTEGER NOT NULL DEFAULT 0,
                last_access     TEXT,
                revision        INTEGER NOT NULL DEFAULT 1,
                metadata        TEXT NOT NULL DEFAULT '{}',
                superseded_by   TEXT,
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

        -- Access history for ACT-R base-level activation
        CREATE TABLE IF NOT EXISTS access_history (
            memory_id  TEXT NOT NULL,
            accessed_at TEXT NOT NULL,
            query_hash  TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_access_hist_mem ON access_history(memory_id);
        CREATE INDEX IF NOT EXISTS idx_access_hist_time ON access_history(accessed_at DESC);
        CREATE INDEX IF NOT EXISTS idx_access_hist_mem_time ON access_history(memory_id, accessed_at DESC);
        CREATE INDEX IF NOT EXISTS idx_access_hist_hash ON access_history(memory_id, query_hash) WHERE query_hash != '';

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
        CREATE TABLE IF NOT EXISTS exec_envs (
            env_id         TEXT PRIMARY KEY,
            kind           TEXT NOT NULL DEFAULT 'worktree',
            path           TEXT NOT NULL,
            repo_root      TEXT NOT NULL DEFAULT '',
            branch         TEXT NOT NULL DEFAULT '',
            base_sha       TEXT NOT NULL DEFAULT '',
            dispatch_id    TEXT,
            state          TEXT NOT NULL DEFAULT 'active',
            reclaim_reason TEXT,
            schema_version INTEGER NOT NULL DEFAULT 1,
            created_at     TEXT NOT NULL DEFAULT '',
            reclaimed_at   TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_exec_envs_state ON exec_envs(state);
        CREATE INDEX IF NOT EXISTS idx_exec_envs_path ON exec_envs(path);
        CREATE INDEX IF NOT EXISTS idx_exec_envs_dispatch ON exec_envs(dispatch_id);

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
        -- replayed complete may rewrite any column here.
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
        -- Cross-session presence claims (#1001). Advisory "who's working on
        -- what" lease rows — NOT a mutual-exclusion lock. A session/dispatch
        -- registers a claim on an issue/lane when it starts touching it and
        -- heartbeats it on every briefing read; a claim with a stale
        -- `heartbeat_at` (older than the TTL) is treated as expired by
        -- readers without a separate reaper process (lazy expiry, same spirit
        -- as `find_active_exec_env_by_path` filtering by state). `state` is
        -- the two-state lifecycle (`active` -> `released`), mirroring
        -- `exec_envs`; the release transition goes through exactly one
        -- function (`release_claim`), same discipline as
        -- `reclaim_exec_env`.
        CREATE TABLE IF NOT EXISTS session_claims (
            claim_id             TEXT PRIMARY KEY,
            session_client       TEXT,
            issue_ref            TEXT,
            flow_id              TEXT,
            dispatch_id          TEXT,
            branch               TEXT NOT NULL DEFAULT '',
            declared_file_scope  TEXT,
            state                TEXT NOT NULL DEFAULT 'active',
            release_reason       TEXT,
            created_at           TEXT NOT NULL DEFAULT '',
            heartbeat_at         TEXT NOT NULL DEFAULT '',
            released_at          TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_session_claims_state ON session_claims(state);
        CREATE INDEX IF NOT EXISTS idx_session_claims_issue ON session_claims(issue_ref);
        CREATE INDEX IF NOT EXISTS idx_session_claims_flow ON session_claims(flow_id);
        -- #1001 round 2 item 2: DB-level uniqueness for the identity triple
        -- upsert_or_heartbeat_claim's read-then-write relies on at the
        -- application level. Partial (WHERE state='active') so a released
        -- historical row never blocks a fresh active claim for the same
        -- identity; COALESCE(..., '') on each nullable column so two active
        -- rows that are both NULL in the same slot collide the same way the
        -- upsert's `IS ?` lookup already treats them as one identity (see
        -- `migrations/session_claims_identity.rs` for the full rationale and
        -- the migration that retrofits this onto pre-existing DBs).
        CREATE UNIQUE INDEX IF NOT EXISTS idx_session_claims_identity_active
            ON session_claims(COALESCE(session_client, ''), COALESCE(issue_ref, ''), COALESCE(flow_id, ''))
            WHERE state = 'active';
"#;

pub(super) const MIGRATED_INDEXES_SQL: &str = r#"
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
"#;
