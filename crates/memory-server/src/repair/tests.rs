//! Unit tests for `tachi repair` rules.

use std::path::PathBuf;

use rusqlite::{params, Connection};
use tempfile::TempDir;

use super::edges::OrphanRefs;
use super::fts::FtsRebuild;
use super::integrity::IntegrityCheck;
use super::jobs::JobsPurge;
use super::quarantine::QuarantineSweep;
use super::retention::RetentionBackfill;
use super::{DbContext, RepairRule};

fn fresh_db(dir: &TempDir, name: &str) -> (PathBuf, Connection) {
    // FTS5 + the `simple` tokenizer is required by `init_schema`.
    libsimple::enable_auto_extension().ok();
    memory_core::db::register_sqlite_vec();
    let path = dir.path().join(name);
    let mut conn = Connection::open(&path).unwrap();
    memory_core::db::init_schema_with_label_mut(&mut conn, "test", &path).unwrap();
    (path, conn)
}

fn open_ctx(path: &PathBuf, label: &str) -> DbContext {
    DbContext {
        label: label.to_string(),
        path: path.clone(),
        schema_kind: "tachi".to_string(),
        conn: Connection::open(path).unwrap(),
    }
}

fn insert_memory(
    conn: &Connection,
    id: &str,
    path: &str,
    text: &str,
    metadata: &str,
    retention: Option<&str>,
    source: Option<&str>,
) {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO memories (
            id, path, summary, text, importance, timestamp, category, topic,
            keywords, persons, entities, location, source, scope, archived,
            created_at, updated_at, access_count, revision, metadata, retention_policy
         ) VALUES (?1, ?2, '', ?3, 0.5, ?4, 'fact', '',
                   '[]', '[]', '[]', '', ?5, 'project', 0,
                   ?4, ?4, 0, 1, ?6, ?7)",
        params![
            id,
            path,
            text,
            now,
            source.unwrap_or("manual"),
            metadata,
            retention,
        ],
    )
    .expect("insert");
}

#[test]
fn r1_fts_drift_detected_and_rebuilt() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "drift.db");
    insert_memory(&conn, "m1", "/x/a", "hello", "{}", None, None);
    insert_memory(&conn, "m2", "/x/b", "world", "{}", None, None);
    // Manually delete one row from FTS to simulate drift.
    conn.execute("DELETE FROM memories_fts WHERE id = 'm2'", []).unwrap();
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = FtsRebuild.dry_run(&mut ctx).unwrap();
    assert!(
        dry.findings.iter().any(|f| f.kind == "fts_drift"),
        "dry-run should detect fts_drift, got {dry:?}"
    );

    let app = FtsRebuild.apply(&mut ctx).unwrap();
    assert!(app.errors.is_empty(), "apply errors: {:?}", app.errors);
    assert_eq!(app.applied, 2, "expected 2 fts rows after rebuild");

    let dry2 = FtsRebuild.dry_run(&mut ctx).unwrap();
    assert!(dry2.findings.is_empty(), "post-rebuild should be clean: {dry2:?}");
}

#[test]
fn r1_missing_fts_table_rebuilt() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "no_fts.db");
    insert_memory(&conn, "m1", "/x/a", "hello", "{}", None, None);
    conn.execute_batch("DROP TABLE memories_fts;").unwrap();
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = FtsRebuild.dry_run(&mut ctx).unwrap();
    assert!(dry.findings.iter().any(|f| f.kind == "fts_table_missing"));

    let app = FtsRebuild.apply(&mut ctx).unwrap();
    assert_eq!(app.applied, 1);
}

#[test]
fn r2_retention_backfill() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "ret.db");
    insert_memory(&conn, "h1", "/handoff/a", "h", "{}", None, None);
    insert_memory(&conn, "k1", "/kanban/x", "k", "{}", None, None);
    insert_memory(&conn, "w1", "/wiki/y", "w", "{}", None, None);
    insert_memory(&conn, "d1", "/notes/z", "d", "{}", None, Some("foundry_distill"));
    insert_memory(&conn, "n1", "/notes/n", "n", "{}", Some("durable"), None);
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = RetentionBackfill.dry_run(&mut ctx).unwrap();
    let total: usize = dry.findings.iter().map(|f| f.count).sum();
    assert_eq!(total, 4, "expected 4 backfill candidates, got {dry:?}");

    let app = RetentionBackfill.apply(&mut ctx).unwrap();
    assert_eq!(app.applied, 4);

    let dry2 = RetentionBackfill.dry_run(&mut ctx).unwrap();
    assert!(dry2.findings.is_empty());
}

#[test]
fn r3_quarantine_sweep_reports_count() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "q.db");
    let meta = serde_json::json!({
        "quarantine": {
            "reason": "cross_db_pollution",
            "original_path": "/x/orig",
            "expected_db": "/some/other/db",
            "actual_db": path.display().to_string(),
            "detected_at": "2026-01-01T00:00:00Z",
        }
    });
    insert_memory(
        &conn,
        "q1",
        "/_quarantine/cross-db/x/orig",
        "blob",
        &meta.to_string(),
        None,
        None,
    );
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = QuarantineSweep.dry_run(&mut ctx).unwrap();
    assert!(dry
        .findings
        .iter()
        .any(|f| f.kind == "quarantined_rows" && f.count == 1));
}

#[test]
fn r4_jobs_purge_dead_letter() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "jobs.db");
    let old = (chrono::Utc::now() - chrono::Duration::days(60)).to_rfc3339();
    let recent = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO foundry_jobs (id, kind, lane, status, created_at, updated_at)
         VALUES ('j_old', 'distill', 'distill', 'dead_letter', ?1, ?1)",
        [&old],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO foundry_jobs (id, kind, lane, status, created_at, updated_at)
         VALUES ('j_new', 'distill', 'distill', 'dead_letter', ?1, ?1)",
        [&recent],
    )
    .unwrap();
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = JobsPurge::default().dry_run(&mut ctx).unwrap();
    assert!(dry
        .findings
        .iter()
        .any(|f| f.kind == "dead_letter_jobs" && f.count == 1));
    let app = JobsPurge::default().apply(&mut ctx).unwrap();
    assert_eq!(app.applied, 1, "should purge only the old dead_letter row");
}

#[test]
fn r5_integrity_passes_on_clean_db() {
    let dir = TempDir::new().unwrap();
    let (path, _conn) = fresh_db(&dir, "ok.db");
    let mut ctx = open_ctx(&path, "test");
    let r = IntegrityCheck.dry_run(&mut ctx).unwrap();
    assert!(r.findings.is_empty(), "clean DB should report no findings: {r:?}");
}

#[test]
fn r5_integrity_detects_corruption() {
    use std::io::{Seek, SeekFrom, Write};
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "corrupt.db");
    insert_memory(&conn, "m1", "/x", "data", "{}", None, None);
    drop(conn);
    // Force WAL checkpoint then truncate so corruption is visible to a fresh open.
    {
        let c = Connection::open(&path).unwrap();
        c.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
    }
    // Stomp on the middle of the file (avoid the SQLite header which would
    // make the DB unopenable rather than corrupt).
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let len = f.metadata().unwrap().len();
    if len > 200 {
        f.seek(SeekFrom::Start(len / 2)).unwrap();
        f.write_all(&[0xFFu8; 200]).unwrap();
    }
    drop(f);

    // open_ctx itself may fail (Connection::open returns NOTADB) on heavy
    // corruption — accept that as a positive signal too.
    let conn_res = Connection::open(&path);
    if conn_res.is_err() {
        return;
    }
    let mut ctx = DbContext {
        label: "test".to_string(),
        path: path.clone(),
        schema_kind: "tachi".to_string(),
        conn: conn_res.unwrap(),
    };
    // dry_run can either return Err (pragma fails) OR Ok with findings.
    match IntegrityCheck.dry_run(&mut ctx) {
        Err(_) => { /* corruption surfaced as Err — expected */ }
        Ok(r) => assert!(
            r.findings.iter().any(|f| f.kind == "integrity_fail")
                || !r.errors.is_empty(),
            "expected corruption to surface, got {r:?}"
        ),
    }
}

#[test]
fn r7_orphan_edges_detected_and_purged() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "edges.db");
    insert_memory(&conn, "m1", "/a", "x", "{}", None, None);
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO memory_edges (source_id, target_id, relation, weight, metadata, created_at)
         VALUES ('m1', 'ghost', 'rel', 1.0, '{}', ?1)",
        [&now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO memory_edges (source_id, target_id, relation, weight, metadata, created_at)
         VALUES ('also_ghost', 'm1', 'rel', 1.0, '{}', ?1)",
        [&now],
    )
    .unwrap();
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = OrphanRefs.dry_run(&mut ctx).unwrap();
    let total: usize = dry.findings.iter().map(|f| f.count).sum();
    assert!(total >= 2, "expected at least 2 orphans, got {dry:?}");

    let app = OrphanRefs.apply(&mut ctx).unwrap();
    assert!(app.applied >= 2);
    let dry2 = OrphanRefs.dry_run(&mut ctx).unwrap();
    assert!(dry2.findings.is_empty(), "post-purge should be clean: {dry2:?}");
}

#[test]
fn r3_cross_db_restore_all_moves_row() {
    use crate::manifest::{DbEntry, DbRole, Manifest};
    let dir = TempDir::new().unwrap();
    let (src_path, src_conn) = fresh_db(&dir, "src.db");
    let (dst_path, _dst_conn) = fresh_db(&dir, "dst.db");

    let dst_canon = std::fs::canonicalize(&dst_path).unwrap();
    let meta = serde_json::json!({
        "quarantine": {
            "reason": "cross_db_pollution",
            "original_path": "/restored/a",
            "expected_db": dst_canon.display().to_string(),
            "actual_db": src_path.display().to_string(),
            "detected_at": "2026-01-01T00:00:00Z",
        }
    });
    insert_memory(
        &src_conn,
        "qx",
        "/_quarantine/cross-db/restored/a",
        "blob",
        &meta.to_string(),
        None,
        None,
    );
    drop(src_conn);

    let manifest = Manifest {
        schema_version: 1,
        generated_at: chrono::Utc::now().to_rfc3339(),
        comment: String::new(),
        dbs: vec![
            DbEntry {
                path: src_path.display().to_string(),
                role: DbRole::Project,
                owner: "test".into(),
                schema_kind: "tachi".into(),
                vec_enabled: false,
                allow_write: true,
                last_doctor_at: chrono::Utc::now().to_rfc3339(),
                last_classification: "tachi".into(),
                scope_hint: "project:src".into(),
                notes: String::new(),
            },
            DbEntry {
                path: dst_path.display().to_string(),
                role: DbRole::Project,
                owner: "test".into(),
                schema_kind: "tachi".into(),
                vec_enabled: false,
                allow_write: true,
                last_doctor_at: chrono::Utc::now().to_rfc3339(),
                last_classification: "tachi".into(),
                scope_hint: "project:dst".into(),
                notes: String::new(),
            },
        ],
    };

    super::quarantine::cmd_restore_all(&manifest, "project:dst", true, true).unwrap();

    // src should no longer have the row; dst should.
    let n_src: i64 = Connection::open(&src_path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM memories WHERE id='qx'", [], |r| r.get(0))
        .unwrap();
    let n_dst: i64 = Connection::open(&dst_path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM memories WHERE id='qx'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n_src, 0, "source should no longer have the row");
    assert_eq!(n_dst, 1, "destination should have the row");

    // Verify path rewritten + quarantine block stripped.
    let (dst_path_col, dst_meta): (String, String) = Connection::open(&dst_path)
        .unwrap()
        .query_row(
            "SELECT path, metadata FROM memories WHERE id='qx'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(dst_path_col, "/restored/a");
    let v: serde_json::Value = serde_json::from_str(&dst_meta).unwrap();
    assert!(
        v.get("quarantine").is_none(),
        "destination metadata should not contain quarantine block: {dst_meta}"
    );
}
