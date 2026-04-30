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

    Ok(report)
}

// ─── v1: normalize paths ──────────────────────────────────────────────────────

fn migrate_v1_path_normalize(conn: &mut Connection) -> Result<usize, MemoryError> {
    let tx = conn.transaction()?;
    let rows: Vec<(String, String)> = {
        let mut stmt = tx.prepare("SELECT id, path FROM memories")?;
        let iter = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for r in iter {
            out.push(r?);
        }
        out
    };

    let mut count = 0usize;
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
    tx.commit()?;
    Ok(count)
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

    // Pull candidates: rows with metadata.provenance.db_path set to something
    // other than this DB's canonical path.
    let candidates: Vec<(String, String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, path, metadata,
                    COALESCE(json_extract(metadata, '$.provenance.db_path'), '')
             FROM memories
             WHERE metadata IS NOT NULL
               AND json_extract(metadata, '$.provenance.db_path') IS NOT NULL",
        )?;
        let iter = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in iter {
            out.push(r?);
        }
        out
    };

    // Filter to genuine mismatches (skip rows already under /_quarantine).
    let mut to_quarantine: Vec<(String, String, String, String)> = Vec::new();
    for (id, path, metadata, prov_db_path) in candidates {
        if path.starts_with("/_quarantine") {
            continue;
        }
        if prov_db_path.is_empty() {
            continue;
        }
        let prov_canonical = std::fs::canonicalize(&prov_db_path)
            .ok()
            .map(|p| p.display().to_string())
            .unwrap_or(prov_db_path.clone());
        if prov_canonical != canonical_self {
            to_quarantine.push((id, path, metadata, prov_canonical));
        }
    }

    let candidate_count = to_quarantine.len();
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
    for (id, original_path, metadata_str, expected_db) in to_quarantine {
        let mut meta: serde_json::Value = serde_json::from_str(&metadata_str)
            .unwrap_or_else(|_| serde_json::json!({}));
        if !meta.is_object() {
            meta = serde_json::json!({});
        }
        let new_path = format!(
            "/_quarantine/cross-db{}",
            if original_path.starts_with('/') {
                original_path.clone()
            } else {
                format!("/{original_path}")
            }
        );
        let q = serde_json::json!({
            "reason": "cross_db_pollution",
            "original_path": original_path,
            "detected_at": detected_at,
            // expected_db: where provenance says the row should live
            // actual_db:   the DB file we're currently migrating (where the
            //              polluted row was found)
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
        // Refresh FTS path for accuracy (best-effort).
        let _ = tx.execute(
            "UPDATE memories_fts SET path = ?1 WHERE id = ?2",
            params![&format!("/_quarantine/cross-db{}", original_path), id],
        );
        moved += 1;
    }
    tx.commit()?;
    Ok((moved, false))
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

    fn insert_row(
        conn: &Connection,
        id: &str,
        path: &str,
        scope: &str,
        metadata: &str,
    ) {
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
}
