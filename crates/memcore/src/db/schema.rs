use rusqlite::{params, Connection, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::MemoryError;

use super::common::normalize_utc_iso;

mod ddl;

pub fn init_schema(conn: &Connection) -> Result<(), MemoryError> {
    apply_connection_pragmas(conn)?;
    init_schema_inner(conn)
}

/// Initialize schema and run data migrations with a known DB label and path.
/// Backs up the DB file before migrating when the schema fingerprint has
/// changed since the last successful init (see `maybe_backup_before_migration`).
///
/// Hard-fails before touching the DB (#984) if it is stamped with a schema
/// version newer than this kernel's `EXPECTED_SCHEMA_VERSION` supports — see
/// `crate::db::migrations::check_schema_version_gate`.
///
/// Also enforces the #1119 typed migration gate
/// (`crate::db::migrations::check_db_open_context_gate`) using the caller's
/// [`crate::db::DbOpenContext`]: an `OpenExisting` open of a *stamped older*
/// DB (`1 ≤ stored < EXPECTED`) without `MigrationAuthority::Allow` refuses
/// with a typed `SchemaMigrationOptInRequired` instead of silently migrating
/// in place. Fresh (`user_version == 0`) files build with no authority.
///
/// ## Compatibility transaction boundary (#984 F1 round 3)
///
/// Everything that mutates schema or data in a way another kernel's
/// [`crate::db::migrations::check_schema_version_gate`] would care about —
/// `init_schema_inner`'s DDL/legacy-column work (including the v6/v8/v9
/// standalone migrations previously committed outside any outer transaction)
/// AND `run_data_migrations`'s sentinel-gated migrations AND the final
/// `user_version` stamp — now run inside ONE `BEGIN IMMEDIATE` opened here
/// and committed only after all of it succeeds. Only work that literally
/// cannot run inside a transaction (the `journal_mode`/`foreign_keys`/
/// `busy_timeout`/`cache_size` connection PRAGMAs) and the pre-migration file
/// backup (a live-connection `rusqlite::backup::Backup`, not a DB write)
/// happen outside the transaction, before it opens.
pub fn init_schema_with_label_mut(
    conn: &mut Connection,
    db_label: &str,
    current_db_path: &Path,
    ctx: &crate::db::DbOpenContext,
) -> Result<crate::db::migrations::MigrationReport, MemoryError> {
    crate::db::migrations::check_schema_version_gate(conn)?;
    // #1119: typed migration gate. Runs BEFORE any backup/DDL/migration/stamp
    // mutates the DB — an unauthorized `OpenExisting + Deny` open of a
    // stamped older DB must refuse before `init_schema_inner`'s idempotent
    // DDL or the final `write_schema_version_stamp` touches the file.
    crate::db::migrations::check_db_open_context_gate(conn, current_db_path, ctx)?;
    maybe_backup_before_migration(conn, current_db_path)?;
    apply_connection_pragmas(conn)?;

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    init_schema_inner(&tx)?;
    #[cfg(test)]
    test_hooks::fail_after_legacy_work_before_stamp()?;
    let report = crate::db::migrations::run_data_migrations_in_tx(&tx, db_label, current_db_path)?;
    crate::db::migrations::write_schema_version_stamp(&tx)?;
    tx.commit()?;

    remember_migration_fingerprint(conn, current_db_path)?;
    Ok(report)
}

/// Test-only fault-injection hook for proving the compatibility transaction
/// (#984 F1 round 3) actually rolls `init_schema_inner`'s legacy work back on
/// failure, not just the sentinel migrations. Real callers never touch this;
/// production builds don't compile it.
#[cfg(test)]
pub(crate) mod test_hooks {
    use crate::error::MemoryError;
    use std::cell::Cell;

    thread_local! {
        static FAIL_AFTER_LEGACY_WORK: Cell<bool> = const { Cell::new(false) };
    }

    /// Arm the injection: the NEXT call to `init_schema_with_label_mut` on
    /// this thread will fail right after `init_schema_inner` (DDL + legacy
    /// v6/v8/v9 work) completes but before `run_data_migrations_in_tx` and
    /// the version stamp run. Auto-disarms after firing once.
    pub(crate) fn arm_fail_after_legacy_work() {
        FAIL_AFTER_LEGACY_WORK.with(|flag| flag.set(true));
    }

    pub(super) fn fail_after_legacy_work_before_stamp() -> Result<(), MemoryError> {
        let armed = FAIL_AFTER_LEGACY_WORK.with(|flag| flag.replace(false));
        if armed {
            return Err(MemoryError::InvalidArg(
                "test_hooks: injected failure after init_schema_inner, before stamp".to_string(),
            ));
        }
        Ok(())
    }
}

/// Connection-level PRAGMAs (`journal_mode`, `foreign_keys`, `busy_timeout`,
/// `cache_size`). These MUST run outside any transaction — `journal_mode` in
/// particular is a no-op/error mid-transaction — so callers apply this before
/// opening the compatibility transaction, not inside `init_schema_inner`.
fn apply_connection_pragmas(conn: &Connection) -> Result<(), MemoryError> {
    execute_batch_retry(conn, ddl::CONNECTION_PRAGMA_SQL)
}

fn init_schema_inner(conn: &Connection) -> Result<(), MemoryError> {
    execute_batch_retry(conn, ddl::BASE_SCHEMA_SQL)?;

    // Forward-compatible migrations for existing DB files created before
    // archived/created_at/updated_at columns existed.
    ensure_column(conn, "memories", "archived", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(conn, "memories", "created_at", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "memories", "updated_at", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "memories", "revision", "INTEGER NOT NULL DEFAULT 1")?;
    ensure_column(conn, "memories", "valid_from", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "memories", "valid_until", "TEXT")?;

    // Retention policy and domain columns for Issue #38 and #32
    ensure_column(conn, "memories", "retention_policy", "TEXT")?;
    ensure_column(conn, "memories", "domain", "TEXT")?;
    ensure_column(conn, "memories", "superseded_by", "TEXT")?;

    // Memory lifecycle columns for tier-based decay and historical training flags.
    ensure_column(
        conn,
        "memories",
        "recall_count",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "memories",
        "query_diversity",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "memories", "tier", "TEXT NOT NULL DEFAULT 'raw'")?;
    ensure_column(conn, "access_history", "query_hash", "TEXT")?;

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
    execute_batch_retry(conn, ddl::MIGRATED_INDEXES_SQL)?;

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
    normalize_memory_validity_columns(conn)?;

    bridge_hypertachi_memory_columns(conn)?;
    crate::db::migrations::fold_and_drop_legacy_persons_column(conn)?;
    // Relocate legacy location before enum rebuild copies rows without that column.
    let _ = crate::db::migrations::migrate_v9_relocate_and_drop_location(conn)?;

    ensure_fts_backfilled(conn)?;

    migrate_enum_constraints(conn)?;

    ensure_optimization_indexes(conn);

    // NOTE: sqlite-vec virtual table (memories_vec) is created separately after
    // the extension is loaded by the caller via register_sqlite_vec().
    Ok(())
}

fn ensure_optimization_indexes(conn: &Connection) {
    if has_column(conn, "memories", "superseded_by").unwrap_or(false) {
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memories_path_active_ts \
             ON memories(path, timestamp DESC) \
             WHERE archived = 0 AND superseded_by IS NULL",
            [],
        );
    }
}

fn execute_batch_retry(conn: &Connection, sql: &str) -> Result<(), MemoryError> {
    retry_locked(|| conn.execute_batch(sql).map(|_| ()))
}

fn execute_retry(conn: &Connection, sql: &str) -> Result<usize, MemoryError> {
    retry_locked(|| conn.execute(sql, []))
}

fn retry_locked<T>(
    mut operation: impl FnMut() -> Result<T, rusqlite::Error>,
) -> Result<T, MemoryError> {
    let mut backoff = Duration::from_millis(10);
    let max_backoff = Duration::from_millis(250);
    let attempts = 24;

    for attempt in 1..=attempts {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) if is_locked_error(&error) && attempt < attempts => {
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(max_backoff);
            }
            Err(error) => return Err(error.into()),
        }
    }

    unreachable!("retry loop returns on every final attempt")
}

fn is_locked_error(error: &rusqlite::Error) -> bool {
    super::sqlite_error_is_locked(error)
}

fn is_duplicate_column_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(_, Some(message))
            if message.contains("duplicate column name")
    )
}

/// Align HyperTachi-shaped legacy DBs (`indexed_tags`, `domain_key`) with Sigil's
/// canonical columns before enum/CHECK migrations run.
fn bridge_hypertachi_memory_columns(conn: &Connection) -> Result<(), MemoryError> {
    ensure_column(conn, "memories", "domain", "TEXT")?;

    if has_column(conn, "memories", "indexed_tags")? {
        conn.execute(
            "UPDATE memories
             SET keywords = indexed_tags
             WHERE (keywords IS NULL OR trim(keywords) IN ('', '[]'))
               AND indexed_tags IS NOT NULL
               AND trim(indexed_tags) NOT IN ('', '[]')",
            [],
        )?;
    }

    if has_column(conn, "memories", "domain_key")? {
        conn.execute(
            "UPDATE memories
             SET domain = domain_key
             WHERE (domain IS NULL OR trim(COALESCE(domain, '')) = '')
               AND domain_key IS NOT NULL
               AND trim(domain_key) <> ''",
            [],
        )?;
    }

    // Undo mistaken v1 bridge that copied domain_key into location (pre-v9 DBs only).
    if has_column(conn, "memories", "location")? {
        conn.execute(
            "UPDATE memories
         SET domain = location, location = ''
         WHERE (domain IS NULL OR trim(COALESCE(domain, '')) = '')
           AND trim(location) <> ''
           AND location NOT GLOB '/*'
           AND location NOT LIKE '%/%'
           AND location NOT LIKE '% %'",
            [],
        )?;
    }

    Ok(())
}

/// Called from inside `init_schema_inner`, which itself runs either
/// standalone (via [`init_schema`], no enclosing transaction) or nested
/// inside `init_schema_with_label_mut`'s outer `BEGIN IMMEDIATE` (#984 F1
/// round 3) — so this uses a `SAVEPOINT` rather than a raw `BEGIN`, which
/// nests cleanly in either context (SQLite forbids nested top-level `BEGIN`
/// but savepoints nest, and a savepoint with no enclosing transaction
/// behaves like one).
fn normalize_memory_validity_columns(conn: &Connection) -> Result<(), MemoryError> {
    let rows = {
        let mut stmt = conn.prepare(
            "SELECT id, timestamp, valid_from, valid_until FROM memories \
             WHERE valid_from = '' OR valid_from IS NULL",
        )?;
        let mapped = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row?);
        }
        rows
    };

    conn.execute_batch("SAVEPOINT normalize_memory_validity_columns")?;
    let result = (|| -> Result<(), MemoryError> {
        for (id, timestamp, valid_from, valid_until) in rows {
            let valid_from_raw = valid_from
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(timestamp.trim());
            let normalized_from =
                normalize_utc_iso(valid_from_raw).unwrap_or_else(|_| valid_from_raw.to_string());
            let normalized_until = valid_until
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| normalize_utc_iso(value).unwrap_or_else(|_| value.to_string()));
            if valid_from.as_deref() == Some(normalized_from.as_str())
                && valid_until == normalized_until
            {
                continue;
            }
            conn.execute(
                "UPDATE memories SET valid_from = ?2, valid_until = ?3 WHERE id = ?1",
                params![id, normalized_from, normalized_until],
            )?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("RELEASE normalize_memory_validity_columns")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK TO normalize_memory_validity_columns");
            let _ = conn.execute_batch("RELEASE normalize_memory_validity_columns");
            Err(e)
        }
    }
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
///
/// Called from inside `init_schema_inner`, which itself runs either
/// standalone (via [`init_schema`], no enclosing transaction) or nested
/// inside `init_schema_with_label_mut`'s outer `BEGIN IMMEDIATE` (#984 F1
/// round 3) — so this uses a `SAVEPOINT` rather than a raw `BEGIN`, which
/// nests cleanly in either context.
fn migrate_enum_constraints(conn: &Connection) -> Result<(), MemoryError> {
    let existing_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='memories'",
            [],
            |row| row.get(0),
        )
        .ok();
    if let Some(sql) = existing_sql.as_deref() {
        let has_source_check = sql.contains("CHECK (source") || sql.contains("CHECK(source");
        // #964: 'sticky' is the newest category value: check it (not 'eval')
        // so DBs stamped before the sticky category was added re-run this
        // rebuild once, same idempotent-sentinel pattern as every prior
        // category/source addition here.
        let has_latest_category_values = sql.contains("'sticky'");
        let has_latest_source_values = sql.contains("'foundry_recall_rerank_cache'");
        if has_source_check && has_latest_category_values && has_latest_source_values {
            return Ok(());
        }
    }

    conn.execute_batch("SAVEPOINT migrate_enum_constraints")?;
    let migration_result = (|| -> Result<(), MemoryError> {
        normalize_source(conn)?;
        normalize_category(conn)?;
        normalize_scope(conn)?;
        normalize_retention_policy(conn)?;
        backfill_retention_defaults(conn)?;
        rebuild_memories_with_check_constraints(conn)?;
        Ok(())
    })();

    match migration_result {
        Ok(()) => {
            conn.execute_batch("RELEASE migrate_enum_constraints")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK TO migrate_enum_constraints");
            let _ = conn.execute_batch("RELEASE migrate_enum_constraints");
            Err(e)
        }
    }
}

const CANONICAL_SOURCES: &[&str] = &[
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

fn normalize_source(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute(
        "UPDATE memories SET source = 'manual'
         WHERE source IS NULL OR trim(source) = ''",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET source = 'manual'
         WHERE lower(source) IN ('test', 'unit_test')",
        [],
    )?;

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
                Err(_) => None,
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

    conn.execute(
        "UPDATE memories SET source = lower(source)
         WHERE source IN ('Manual','Extraction','Migration','Auto','FoundryDistill','Handoff','Kanban','Wiki','Ghost','IngestEvent')
            OR source GLOB '*[A-Z]*'",
        [],
    )?;

    loop {
        let rows = fetch_noncanonical_source_batch(conn, 500)?;
        if rows.is_empty() {
            break;
        }
        let mut changed = 0usize;
        for (id, source) in rows {
            if CANONICAL_SOURCES.contains(&source.as_str()) {
                continue;
            }
            if let Some(suffix) = source.strip_prefix("external:") {
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

    Ok(())
}

fn normalize_category(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute(
        "UPDATE memories SET category = lower(category) WHERE category IS NOT NULL",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET category = 'other'
         WHERE category IS NULL OR category = ''
            OR category NOT IN ('fact','decision','experience','preference','entity','other','kanban','handoff','ghost','wiki','guide','eval','sticky')",
        [],
    )?;
    Ok(())
}

fn normalize_scope(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute(
        "UPDATE memories SET scope = lower(scope) WHERE scope IS NOT NULL",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET scope = 'general'
         WHERE scope IS NULL OR scope NOT IN ('user','project','general')",
        [],
    )?;
    Ok(())
}

fn normalize_retention_policy(conn: &Connection) -> Result<(), MemoryError> {
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
    Ok(())
}

fn backfill_retention_defaults(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute(
        "UPDATE memories SET retention_policy = 'pinned'
         WHERE retention_policy IS NULL
           AND (path LIKE '/handoff%' OR path LIKE '/kanban%')",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET retention_policy = 'permanent'
         WHERE retention_policy IS NULL AND (path LIKE '/wiki%' OR path LIKE '/guide%')",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET retention_policy = 'permanent'
         WHERE retention_policy IS NULL AND source = 'foundry_distill'",
        [],
    )?;
    Ok(())
}

fn rebuild_memories_with_check_constraints(conn: &Connection) -> Result<(), MemoryError> {
    let recall_count_expr = if has_column(conn, "memories", "recall_count")? {
        "COALESCE(recall_count, 0)"
    } else {
        "0"
    };
    let query_diversity_expr = if has_column(conn, "memories", "query_diversity")? {
        "COALESCE(query_diversity, 0)"
    } else {
        "0"
    };
    let tier_expr = if has_column(conn, "memories", "tier")? {
        "COALESCE(tier, 'raw')"
    } else {
        "'raw'"
    };
    let rebuild_sql = r#"
        CREATE TABLE memories_new (
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
            access_count INTEGER NOT NULL DEFAULT 0,
            last_access  TEXT,
            revision     INTEGER NOT NULL DEFAULT 1,
            metadata     TEXT NOT NULL DEFAULT '{}',
             retention_policy TEXT,
             domain       TEXT,
             superseded_by TEXT,
             recall_count    INTEGER NOT NULL DEFAULT 0,
             query_diversity INTEGER NOT NULL DEFAULT 0,
             tier            TEXT NOT NULL DEFAULT 'raw',
             CHECK (category IN ('fact','decision','experience','preference','entity','other','kanban','handoff','ghost','wiki','guide','eval','sticky')),
            CHECK (scope IN ('user','project','general')),
            CHECK (retention_policy IS NULL OR retention_policy IN ('ephemeral','durable','permanent','pinned')),
            CHECK (
                source IN ('manual','extraction','migration','auto','foundry_distill','foundry_recall_rerank_cache','handoff','kanban','wiki','ghost','ingest_event')
                OR source LIKE 'external:%'
            )
        );

        INSERT INTO memories_new
            (id, path, summary, text, importance, timestamp, valid_from, valid_until,
             category, topic, keywords, entities, source, scope, archived,
             created_at, updated_at, access_count, last_access, revision,
             metadata, retention_policy, domain, superseded_by,
             recall_count, query_diversity, tier)
        SELECT
             id, path, summary, text, importance, timestamp,
             COALESCE(NULLIF(valid_from, ''), timestamp), NULLIF(valid_until, ''),
             category, topic, keywords, entities, source, scope, archived,
             created_at, updated_at, access_count, last_access, revision,
             metadata, retention_policy, domain, superseded_by,
              __RECALL_COUNT_EXPR__, __QUERY_DIVERSITY_EXPR__, __TIER_EXPR__
        FROM memories;

        DROP TABLE memories;
        ALTER TABLE memories_new RENAME TO memories;

        CREATE INDEX IF NOT EXISTS idx_memories_path        ON memories(path);
        CREATE INDEX IF NOT EXISTS idx_memories_importance  ON memories(importance DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_timestamp   ON memories(timestamp DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_archived    ON memories(archived);
        CREATE INDEX IF NOT EXISTS idx_memories_last_access ON memories(last_access DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_valid_time  ON memories(valid_from, valid_until);
        CREATE INDEX IF NOT EXISTS idx_memories_retention_policy ON memories(retention_policy);
        CREATE INDEX IF NOT EXISTS idx_memories_domain ON memories(domain);
        CREATE INDEX IF NOT EXISTS idx_memories_superseded ON memories(superseded_by);
        CREATE INDEX IF NOT EXISTS idx_memories_tier ON memories(tier);
        CREATE INDEX IF NOT EXISTS idx_memories_recall ON memories(recall_count DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_path_active_ts ON memories(path, timestamp DESC) WHERE archived = 0 AND superseded_by IS NULL;

        "#
        .replace("__RECALL_COUNT_EXPR__", recall_count_expr)
        .replace("__QUERY_DIVERSITY_EXPR__", query_diversity_expr)
        .replace("__TIER_EXPR__", tier_expr);
    conn.execute_batch(&rebuild_sql)?;
    Ok(())
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

    let table_name = table.to_string();
    let column_name = column.to_string();
    let table = quote_sql_identifier(table)?;
    let column = quote_sql_identifier(column)?;
    let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {definition}");
    match execute_retry(conn, &sql) {
        Ok(_) => {}
        Err(MemoryError::Sqlite(error))
            if is_duplicate_column_error(&error)
                && has_column(conn, &table_name, &column_name)? => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, MemoryError> {
    let table = quote_sql_identifier(table)?;
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

fn quote_sql_identifier(identifier: &str) -> Result<String, MemoryError> {
    if identifier.is_empty()
        || !identifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(MemoryError::InvalidArg(format!(
            "invalid SQL identifier: {identifier}"
        )));
    }
    Ok(format!("\"{identifier}\""))
}

fn ensure_fts_backfilled(conn: &Connection) -> Result<(), MemoryError> {
    ensure_column(conn, "vault_entries", "allowed_agents", "TEXT")?;

    let memories_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
    if memories_count == 0 {
        return Ok(());
    }

    conn.execute(
        "DELETE FROM memories_fts WHERE id NOT IN (SELECT id FROM memories)",
        [],
    )?;

    conn.execute(
        r#"INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
           SELECT
             m.id,
             m.path,
             m.summary,
             m.text,
             trim(replace(replace(replace(m.keywords, '[', ' '), ']', ' '), '"', ' ')),
             trim(replace(replace(replace(m.entities, '[', ' '), ']', ' '), '"', ' '))
           FROM memories m
           WHERE NOT EXISTS (SELECT 1 FROM memories_fts f WHERE f.id = m.id)"#,
        [],
    )?;

    Ok(())
}

fn migration_backup_retain_count() -> usize {
    std::env::var("TACHI_MIGRATION_BACKUP_RETAIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
        .max(1)
}

fn migration_marker_path(db_path: &Path) -> PathBuf {
    let mut s = db_path.as_os_str().to_owned();
    s.push(".migration-marker");
    PathBuf::from(s)
}

fn migration_schema_fingerprint(conn: &Connection) -> Result<String, MemoryError> {
    let sv: i64 = conn.query_row("PRAGMA schema_version", [], |r| r.get(0))?;
    Ok(format!("{}:{}", env!("CARGO_PKG_VERSION"), sv))
}

fn maybe_backup_before_migration(
    conn: &Connection,
    db_path: &Path,
) -> Result<Option<PathBuf>, MemoryError> {
    let current_fp = migration_schema_fingerprint(conn)?;
    // schema_version == 0: the file was just created and has no schema yet.
    // There is nothing to back up.
    if current_fp.ends_with(":0") {
        return Ok(None);
    }

    let marker = migration_marker_path(db_path);
    if std::fs::read_to_string(&marker).ok().as_deref() == Some(current_fp.as_str()) {
        return Ok(None);
    }

    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let backup_path = sibling_with_suffix(db_path, &format!("migration-bak.{ts}"));

    {
        let mut dst = Connection::open(&backup_path)?;
        let backup = rusqlite::backup::Backup::new(conn, &mut dst)?;
        backup.run_to_completion(128, Duration::from_millis(100), None)?;
    }

    retain_recent_migration_backups(db_path);

    Ok(Some(backup_path))
}

fn remember_migration_fingerprint(conn: &Connection, db_path: &Path) -> Result<(), MemoryError> {
    let fp = migration_schema_fingerprint(conn)?;
    if let Err(e) = std::fs::write(migration_marker_path(db_path), &fp) {
        eprintln!(
            "[migration] warning: failed to write migration marker: {e}; \
             next startup will back up again"
        );
    }
    Ok(())
}

fn retain_recent_migration_backups(db_path: &Path) {
    let Some(dir) = db_path.parent() else {
        return;
    };
    let Some(name) = db_path.file_name() else {
        return;
    };
    let prefix = format!("{}.migration-bak.", name.to_string_lossy());

    let mut backups: Vec<_> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .collect(),
        Err(_) => return,
    };

    // Lexicographic descending = newest first (ISO 8601 timestamp in filename).
    backups.sort_by_key(|b| std::cmp::Reverse(b.file_name()));

    for old in backups.into_iter().skip(migration_backup_retain_count()) {
        let _ = std::fs::remove_file(old.path());
    }
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".");
    s.push(suffix);
    PathBuf::from(s)
}

#[cfg(test)]
mod migration_tests;

#[cfg(test)]
mod migration_backup_tests;
