//! One-shot, idempotent data migrations for legacy memory DBs.
//!
//! Each migration is gated by a sentinel row in `hard_state`
//! (namespace='migrations', key=<migration_id>). Once written, the migration
//! is skipped on subsequent runs.
//!
//! See docs/audit-2026-04-30.md (PR-3) for the bugs each migration fixes:
//! - v1: H1 path normalization
//! - v2: H4 scope normalization (defense-in-depth on top of PR-1's enum migration)
//! - v3: H6 handoff path standardization
//! - v4: B4/B11 cross-DB pollution quarantine
//! - v5: drop HyperTachi legacy columns (`indexed_tags`, `domain_key`) after bridge
//! - v6: fold non-empty `persons` JSON into `entities`, then clear `persons`
//! - v7: reconcile half-migrated DBs (re-bridge + drop `indexed_tags`/`domain_key`, ensure `persons`/`location`)

use std::path::Path;

use rusqlite::{params, Connection};

use crate::error::MemoryError;
use crate::path_router;

use super::common::now_utc_iso;

const MIGRATION_NS: &str = "migrations";
const SANITY_QUARANTINE_FRACTION: f64 = 0.5;

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct MigrationReport {
    pub paths_normalized: usize,
    pub scopes_fixed: usize,
    pub handoff_paths_standardized: usize,
    pub quarantined: usize,
    pub quarantine_skipped_sanity_guard: bool,
    pub hypertachi_legacy_columns_dropped: usize,
    pub persons_folded_into_entities: usize,
    pub legacy_columns_reconciled: usize,
}

/// Run all data-fix migrations in order. Idempotent.
///
/// `db_label` is the manifest role/project label for this DB ("global",
/// "wiki", a project name, or "unknown"). `current_db_path` is the canonical
/// filesystem path to this DB file (used by v4 to detect rows whose
/// `metadata.provenance.db_path` points elsewhere).
pub fn run_data_migrations(
    conn: &mut Connection,
    db_label: &str,
    current_db_path: &Path,
) -> Result<MigrationReport, MemoryError> {
    let mut report = MigrationReport::default();

    if !was_run(conn, "v1_path_normalize_legacy")? {
        report.paths_normalized = migrate_v1_path_normalize(conn)?;
        mark_run(conn, "v1_path_normalize_legacy")?;
    }

    if !was_run(conn, "v2_scope_self_normalize")? {
        report.scopes_fixed = migrate_v2_scope_normalize(conn)?;
        mark_run(conn, "v2_scope_self_normalize")?;
    }

    if !was_run(conn, "v3_handoff_path_standardize")? {
        report.handoff_paths_standardized = migrate_v3_handoff_standardize(conn)?;
        mark_run(conn, "v3_handoff_path_standardize")?;
    }

    if !was_run(conn, "v4_quarantine_cross_db_rows")? {
        let (quarantined, skipped) =
            migrate_v4_quarantine_cross_db(conn, db_label, current_db_path)?;
        report.quarantined = quarantined;
        report.quarantine_skipped_sanity_guard = skipped;
        // Even on sanity-guard skip, mark run so we don't loop on every startup.
        mark_run(conn, "v4_quarantine_cross_db_rows")?;
    }

    if !was_run(conn, "v5_drop_hypertachi_legacy_columns")? {
        report.hypertachi_legacy_columns_dropped = migrate_v5_drop_hypertachi_legacy_columns(conn)?;
        mark_run(conn, "v5_drop_hypertachi_legacy_columns")?;
    }

    if !was_run(conn, "v6_fold_persons_into_entities")? {
        report.persons_folded_into_entities = migrate_v6_fold_persons_into_entities(conn)?;
        mark_run(conn, "v6_fold_persons_into_entities")?;
    }

    if !was_run(conn, "v7_reconcile_legacy_memory_columns")? {
        report.legacy_columns_reconciled = migrate_v7_reconcile_legacy_memory_columns(conn)?;
        mark_run(conn, "v7_reconcile_legacy_memory_columns")?;
    }

    Ok(report)
}

// ─── v6: fold persons → entities ─────────────────────────────────────────────

fn migrate_v6_fold_persons_into_entities(conn: &Connection) -> Result<usize, MemoryError> {
    if !table_has_column(conn, "memories", "persons")?
        || !table_has_column(conn, "memories", "entities")?
    {
        return Ok(0);
    }

    let mut stmt = conn.prepare("SELECT id, persons, entities FROM memories")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut updates = Vec::new();
    for row in rows {
        let (id, persons_raw, entities_raw) = row?;
        let persons: Vec<String> = serde_json::from_str(&persons_raw).unwrap_or_default();
        if persons.is_empty() {
            continue;
        }
        let mut entities: Vec<String> = serde_json::from_str(&entities_raw).unwrap_or_default();
        crate::types::fold_person_names_into_entities(&mut entities, persons);
        updates.push((id, serde_json::to_string(&entities).unwrap_or_default()));
    }

    if updates.is_empty() {
        return Ok(0);
    }

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<(), MemoryError> {
        for (id, entities_json) in &updates {
            conn.execute(
                "UPDATE memories SET persons = '[]', entities = ?2 WHERE id = ?1",
                params![id, entities_json],
            )?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(updates.len())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

// ─── v7: reconcile legacy physical columns (idempotent even if v5 already ran) ─

fn migrate_v7_reconcile_legacy_memory_columns(conn: &Connection) -> Result<usize, MemoryError> {
    let mut actions = 0usize;

    if table_has_column(conn, "memories", "indexed_tags")? {
        conn.execute(
            "UPDATE memories
             SET keywords = indexed_tags
             WHERE (keywords IS NULL OR trim(keywords) IN ('', '[]'))
               AND indexed_tags IS NOT NULL
               AND trim(indexed_tags) NOT IN ('', '[]')",
            [],
        )?;
        actions += 1;
    }

    if table_has_column(conn, "memories", "domain_key")? {
        conn.execute(
            "UPDATE memories
             SET domain = domain_key
             WHERE (domain IS NULL OR trim(COALESCE(domain, '')) = '')
               AND domain_key IS NOT NULL
               AND trim(domain_key) <> ''",
            [],
        )?;
        actions += 1;
    }

    actions += migrate_v6_fold_persons_into_entities(conn)?;

    for (column, definition) in [
        ("persons", "TEXT NOT NULL DEFAULT '[]'"),
        ("location", "TEXT NOT NULL DEFAULT ''"),
    ] {
        if !table_has_column(conn, "memories", column)? {
            let sql = format!("ALTER TABLE memories ADD COLUMN {column} {definition}");
            conn.execute(&sql, [])?;
            actions += 1;
        }
    }

    if !table_has_column(conn, "memories", "domain")? {
        conn.execute("ALTER TABLE memories ADD COLUMN domain TEXT", [])?;
        actions += 1;
    }

    for column in ["indexed_tags", "domain_key"] {
        if table_has_column(conn, "memories", column)? {
            conn.execute(&format!("ALTER TABLE memories DROP COLUMN {column}"), [])?;
            actions += 1;
        }
    }

    Ok(actions)
}

// ─── v5: drop HyperTachi legacy columns ───────────────────────────────────────

fn migrate_v5_drop_hypertachi_legacy_columns(conn: &Connection) -> Result<usize, MemoryError> {
    let mut dropped = 0usize;
    for column in ["indexed_tags", "domain_key"] {
        if table_has_column(conn, "memories", column)? {
            conn.execute(&format!("ALTER TABLE memories DROP COLUMN {column}"), [])?;
            dropped += 1;
        }
    }
    Ok(dropped)
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, MemoryError> {
    let sql = format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1 LIMIT 1");
    let exists = conn.query_row(&sql, [column], |_| Ok(())).is_ok();
    Ok(exists)
}

// ─── v1: normalize paths ──────────────────────────────────────────────────────

fn migrate_v1_path_normalize(conn: &mut Connection) -> Result<usize, MemoryError> {
    let tx = conn.transaction()?;
    let mut count = 0usize;
    let mut after_id = String::new();
    loop {
        let rows = fetch_id_path_batch(&tx, &after_id, 500)?;
        if rows.is_empty() {
            break;
        }
        after_id = rows.last().map(|(id, _)| id.clone()).unwrap_or(after_id);
        for (id, path) in rows {
            let normalized = path_router::normalize_path(&path);
            if normalized != path {
                tx.execute(
                    "UPDATE memories SET path = ?1 WHERE id = ?2",
                    params![normalized, id],
                )?;
                count += 1;
            }
        }
    }
    tx.commit()?;
    Ok(count)
}

fn fetch_id_path_batch(
    conn: &Connection,
    after_id: &str,
    limit: usize,
) -> Result<Vec<(String, String)>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id, path FROM memories
         WHERE id > ?1
         ORDER BY id
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![after_id, limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ─── v2: normalize scope (defensive) ──────────────────────────────────────────

fn migrate_v2_scope_normalize(conn: &mut Connection) -> Result<usize, MemoryError> {
    // PR-1 already added a CHECK constraint that prevents non-canonical scope
    // values. Per-project DBs that pre-existed PR-1 should also have been
    // normalized by PR-1's migration when init_schema runs. This is a
    // defensive sweep: count rows that LOOK wrong (defensive) and normalize.
    let count = conn.execute(
        "UPDATE memories SET scope = 'general'
         WHERE scope IS NULL OR scope NOT IN ('user','project','general')",
        [],
    )?;
    Ok(count)
}

// ─── v3: standardize handoff paths ────────────────────────────────────────────

fn migrate_v3_handoff_standardize(conn: &mut Connection) -> Result<usize, MemoryError> {
    // Bare "/handoff" → "/handoff/unknown".
    let count = conn.execute(
        "UPDATE memories SET path = '/handoff/unknown' WHERE path = '/handoff'",
        [],
    )?;
    Ok(count)
}

// ─── v4: quarantine cross-DB pollution ────────────────────────────────────────

fn migrate_v4_quarantine_cross_db(
    conn: &mut Connection,
    db_label: &str,
    current_db_path: &Path,
) -> Result<(usize, bool), MemoryError> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))?;
    if total == 0 {
        return Ok((0, false));
    }

    let canonical_self = std::fs::canonicalize(current_db_path)
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| current_db_path.display().to_string());

    let candidate_count = count_cross_db_candidates(conn, &canonical_self)?;
    if candidate_count == 0 {
        return Ok((0, false));
    }

    // 50%-row sanity guard.
    let total_f = total as f64;
    if (candidate_count as f64) / total_f > SANITY_QUARANTINE_FRACTION {
        eprintln!(
            "warning: v4_quarantine_cross_db_rows: would move {} of {} rows (>50%) in db_label={} path={}; aborting migration",
            candidate_count, total, db_label, canonical_self
        );
        return Ok((0, true));
    }

    let detected_at = now_utc_iso();
    let tx = conn.transaction()?;
    let mut moved = 0usize;
    let mut after_id = String::new();
    loop {
        let rows = fetch_cross_db_candidate_batch(&tx, &after_id, 500)?;
        if rows.is_empty() {
            break;
        }
        after_id = rows
            .last()
            .map(|(id, _, _, _)| id.clone())
            .unwrap_or(after_id);
        for (id, original_path, metadata_str, prov_db_path) in rows {
            if let Some(expected_db) = mismatched_provenance_db(&prov_db_path, &canonical_self) {
                let mut meta: serde_json::Value =
                    serde_json::from_str(&metadata_str).unwrap_or_else(|_| serde_json::json!({}));
                if !meta.is_object() {
                    meta = serde_json::json!({});
                }
                let original_suffix = if original_path.starts_with('/') {
                    original_path.clone()
                } else {
                    format!("/{original_path}")
                };
                let new_path = format!("/_quarantine/cross-db{original_suffix}");
                let q = serde_json::json!({
                    "reason": "cross_db_pollution",
                    "original_path": original_path,
                    "detected_at": detected_at,
                    "expected_db": expected_db,
                    "actual_db": canonical_self,
                });
                if let Some(obj) = meta.as_object_mut() {
                    obj.insert("quarantine".into(), q);
                }
                let new_meta = serde_json::to_string(&meta)?;
                tx.execute(
                    "UPDATE memories SET path = ?1, metadata = ?2 WHERE id = ?3",
                    params![new_path, new_meta, id],
                )?;
                if let Err(e) = tx.execute(
                    "UPDATE memories_fts SET path = ?1 WHERE id = ?2",
                    params![&format!("/_quarantine/cross-db{original_suffix}"), id],
                ) {
                    eprintln!("warning: failed to update FTS for quarantined row {id}: {e}");
                }
                moved += 1;
            }
        }
    }
    tx.commit()?;
    Ok((moved, false))
}

fn count_cross_db_candidates(
    conn: &Connection,
    canonical_self: &str,
) -> Result<usize, MemoryError> {
    let mut count = 0usize;
    let mut after_id = String::new();
    loop {
        let rows = fetch_cross_db_candidate_batch(conn, &after_id, 500)?;
        if rows.is_empty() {
            break;
        }
        after_id = rows
            .last()
            .map(|(id, _, _, _)| id.clone())
            .unwrap_or(after_id);
        for (_, _, _, prov_db_path) in rows {
            if mismatched_provenance_db(&prov_db_path, canonical_self).is_some() {
                count += 1;
            }
        }
    }
    Ok(count)
}

fn fetch_cross_db_candidate_batch(
    conn: &Connection,
    after_id: &str,
    limit: usize,
) -> Result<Vec<(String, String, String, String)>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id, path, metadata,
                COALESCE(json_extract(metadata, '$.provenance.db_path'), '')
         FROM memories
         WHERE id > ?1
           AND path NOT LIKE '/_quarantine%'
           AND metadata IS NOT NULL
           AND json_extract(metadata, '$.provenance.db_path') IS NOT NULL
         ORDER BY id
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![after_id, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn mismatched_provenance_db(prov_db_path: &str, canonical_self: &str) -> Option<String> {
    if prov_db_path.is_empty() {
        return None;
    }
    let prov_canonical = std::fs::canonicalize(prov_db_path)
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| prov_db_path.to_string());
    if prov_canonical != canonical_self {
        Some(prov_canonical)
    } else {
        None
    }
}

// ─── sentinel helpers ─────────────────────────────────────────────────────────

pub(crate) fn was_run(conn: &Connection, key: &str) -> Result<bool, MemoryError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM hard_state WHERE namespace = ?1 AND key = ?2",
        params![MIGRATION_NS, key],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub(crate) fn mark_run(conn: &Connection, key: &str) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    let value_json = format!("{{\"ran_at\":\"{}\"}}", now);
    conn.execute(
        "INSERT INTO hard_state (namespace, key, value_json, version, created_at, updated_at)
         VALUES (?1, ?2, ?3, 1, ?4, ?4)
         ON CONFLICT(namespace, key) DO UPDATE SET
             value_json = excluded.value_json,
             updated_at = excluded.updated_at,
             version    = hard_state.version + 1",
        params![MIGRATION_NS, key, value_json, now],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_schema, register_sqlite_vec, try_load_sqlite_vec};
    use rusqlite::Connection;

    fn open_test_db() -> (Connection, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        register_sqlite_vec();
        let conn = Connection::open(tmp.path()).expect("open");
        let _ = try_load_sqlite_vec(&conn);
        init_schema(&conn).expect("init_schema");
        (conn, tmp)
    }

    fn insert_row(conn: &Connection, id: &str, path: &str, scope: &str, metadata: &str) {
        conn.execute(
            "INSERT INTO memories
              (id, path, summary, text, importance, timestamp, category, topic,
               keywords, persons, entities, location, source, scope, archived,
               created_at, updated_at, access_count, last_access, revision,
               metadata, retention_policy, domain)
             VALUES (?1, ?2, '', '', 0.5, '2026-01-01T00:00:00Z', 'fact', '',
                     '[]', '[]', '[]', '', 'manual', ?3, 0,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 0, NULL, 1,
                     ?4, NULL, NULL)",
            params![id, path, scope, metadata],
        )
        .unwrap();
    }

    #[test]
    fn was_run_mark_run_roundtrip() {
        let (conn, _tmp) = open_test_db();
        assert!(!was_run(&conn, "v1_test").unwrap());
        mark_run(&conn, "v1_test").unwrap();
        assert!(was_run(&conn, "v1_test").unwrap());
        // Idempotent re-mark.
        mark_run(&conn, "v1_test").unwrap();
        assert!(was_run(&conn, "v1_test").unwrap());
    }

    #[test]
    fn v1_normalizes_legacy_paths() {
        let (mut conn, tmp) = open_test_db();
        insert_row(&conn, "a", "/Wiki//foo//", "general", "{}");
        insert_row(&conn, "b", "wiki/Bar", "general", "{}");
        insert_row(&conn, "c", "/handoff/agent", "general", "{}");

        let report = run_data_migrations(&mut conn, "wiki", tmp.path()).unwrap();
        assert_eq!(report.paths_normalized, 2);

        let pa: String = conn
            .query_row("SELECT path FROM memories WHERE id='a'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pa, "/wiki/foo");
        let pb: String = conn
            .query_row("SELECT path FROM memories WHERE id='b'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pb, "/wiki/Bar");

        // Idempotent on second run.
        let report2 = run_data_migrations(&mut conn, "wiki", tmp.path()).unwrap();
        assert_eq!(report2.paths_normalized, 0);
    }

    #[test]
    fn v3_standardizes_bare_handoff() {
        let (mut conn, tmp) = open_test_db();
        // PR-1 migration runs on init_schema; it normalizes scope. We insert
        // POST-migration so we satisfy CHECK constraints.
        insert_row(&conn, "h1", "/handoff", "general", "{}");
        insert_row(&conn, "h2", "/handoff/agent-x", "general", "{}");
        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report.handoff_paths_standardized, 1);
        let p1: String = conn
            .query_row("SELECT path FROM memories WHERE id='h1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(p1, "/handoff/unknown");
    }

    #[test]
    fn v4_quarantines_cross_db_rows() {
        let (mut conn, tmp) = open_test_db();
        // Row whose provenance.db_path points elsewhere → quarantined.
        let other_meta = serde_json::json!({
            "provenance": { "db_path": "/some/other/db.sqlite" }
        })
        .to_string();
        insert_row(&conn, "x", "/hapi/foo", "general", &other_meta);
        // Row with provenance pointing at this DB → kept.
        let canonical = std::fs::canonicalize(tmp.path()).unwrap();
        let self_meta = serde_json::json!({
            "provenance": { "db_path": canonical.display().to_string() }
        })
        .to_string();
        insert_row(&conn, "y", "/hapi/bar", "general", &self_meta);
        // Pad with extra clean rows so the bad row is <50% of total.
        for i in 0..5 {
            insert_row(&conn, &format!("z{i}"), "/hapi/clean", "general", "{}");
        }

        let report = run_data_migrations(&mut conn, "hapi", tmp.path()).unwrap();
        assert_eq!(report.quarantined, 1);
        assert!(!report.quarantine_skipped_sanity_guard);

        let px: String = conn
            .query_row("SELECT path FROM memories WHERE id='x'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(px, "/_quarantine/cross-db/hapi/foo");
        let qmeta: String = conn
            .query_row("SELECT metadata FROM memories WHERE id='x'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&qmeta).unwrap();
        assert_eq!(v["quarantine"]["reason"], "cross_db_pollution");
        assert_eq!(v["quarantine"]["original_path"], "/hapi/foo");
    }

    #[test]
    fn v4_sanity_guard_aborts_when_majority_would_move() {
        let (mut conn, tmp) = open_test_db();
        let bad_meta = serde_json::json!({
            "provenance": { "db_path": "/elsewhere.db" }
        })
        .to_string();
        // 3 polluted rows, 1 clean → would move 75% → guard aborts.
        for i in 0..3 {
            insert_row(&conn, &format!("p{i}"), "/proj/foo", "general", &bad_meta);
        }
        insert_row(&conn, "ok", "/proj/clean", "general", "{}");
        let report = run_data_migrations(&mut conn, "proj", tmp.path()).unwrap();
        assert_eq!(report.quarantined, 0);
        assert!(report.quarantine_skipped_sanity_guard);
        // Migration is still marked run so we don't retry every startup.
        assert!(was_run(&conn, "v4_quarantine_cross_db_rows").unwrap());
    }

    #[test]
    fn v7_reconciles_indexed_tags_after_v5_sentinel() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                keywords TEXT NOT NULL DEFAULT '[]',
                indexed_tags TEXT NOT NULL DEFAULT '[]',
                entities TEXT NOT NULL DEFAULT '[]',
                domain TEXT,
                domain_key TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE hard_state (
                namespace TEXT NOT NULL,
                key TEXT NOT NULL,
                value_json TEXT NOT NULL,
                version INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (namespace, key)
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, keywords, indexed_tags, entities) VALUES ('m1', '[]', '[\"rust\"]', '[]')",
            [],
        )
        .unwrap();
        mark_run(&conn, "v5_drop_hypertachi_legacy_columns").unwrap();

        let actions = migrate_v7_reconcile_legacy_memory_columns(&conn).unwrap();
        assert!(actions > 0);
        assert!(!table_has_column(&conn, "memories", "indexed_tags").unwrap());
        assert!(table_has_column(&conn, "memories", "persons").unwrap());

        let keywords: String = conn
            .query_row("SELECT keywords FROM memories WHERE id='m1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(keywords.contains("rust"));
    }

    #[test]
    fn v6_folds_persons_into_entities() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                persons TEXT NOT NULL DEFAULT '[]',
                entities TEXT NOT NULL DEFAULT '[]'
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, persons, entities) VALUES (?1, ?2, ?3)",
            params!["m1", r#"["Kyle"]"#, r#"["Sigil"]"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, persons, entities) VALUES ('m2', '[]', '[]')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, persons, entities) VALUES (?1, ?2, ?3)",
            params!["m3", r#"["sigil"]"#, r#"["Sigil"]"#],
        )
        .unwrap();

        let folded = migrate_v6_fold_persons_into_entities(&conn).unwrap();
        assert_eq!(folded, 2);

        let (persons, entities): (String, String) = conn
            .query_row(
                "SELECT persons, entities FROM memories WHERE id='m1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(persons, "[]");
        let ents: Vec<String> = serde_json::from_str(&entities).unwrap();
        assert!(ents.iter().any(|e| e == "Kyle"));
        assert!(ents.iter().any(|e| e == "user"));
        assert!(ents.iter().any(|e| e == "Sigil"));

        let duplicate_persons: String = conn
            .query_row("SELECT persons FROM memories WHERE id='m3'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(duplicate_persons, "[]");
    }

    #[test]
    fn v5_drops_hypertachi_legacy_columns() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                indexed_tags TEXT NOT NULL DEFAULT '[]',
                domain_key TEXT NOT NULL DEFAULT ''
            );",
        )
        .unwrap();
        let dropped = migrate_v5_drop_hypertachi_legacy_columns(&conn).unwrap();
        assert_eq!(dropped, 2);
        assert!(!table_has_column(&conn, "memories", "indexed_tags").unwrap());
        assert!(!table_has_column(&conn, "memories", "domain_key").unwrap());
        assert_eq!(migrate_v5_drop_hypertachi_legacy_columns(&conn).unwrap(), 0);
    }
}
