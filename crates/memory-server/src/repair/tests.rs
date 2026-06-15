//! Unit tests for `tachi repair` rules.

use std::path::PathBuf;

use rusqlite::{params, Connection};
use tempfile::TempDir;

use super::domain::DomainRepair;
use super::edges::OrphanRefs;
use super::enrichment::EnrichmentFailureReset;
use super::fts::FtsRebuild;
use super::integrity::IntegrityCheck;
use super::jobs::JobsPurge;
use super::junk::JunkCleanup;
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
            keywords, entities, source, scope, archived,
            created_at, updated_at, access_count, revision, metadata, retention_policy
         ) VALUES (?1, ?2, '', ?3, 0.5, ?4, 'fact', '',
                   '[]', '[]', ?5, 'project', 0,
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
    conn.execute("DELETE FROM memories_fts WHERE id = 'm2'", [])
        .unwrap();
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
    assert!(
        dry2.findings.is_empty(),
        "post-rebuild should be clean: {dry2:?}"
    );
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

/// Reproduces the `project:quant` failure mode: the parent virtual table
/// `memories_fts` is gone but the FTS5 shadow tables (`memories_fts_data` &c.)
/// were left behind. A naive `CREATE VIRTUAL TABLE memories_fts` errors with
/// "table memories_fts_data already exists". R1 must drop the orphan shadows
/// inside the same transaction before recreating.
///
/// In practice this state arises from a half-applied DROP / crash. We
/// reproduce it by dropping the virtual head with FTS5's internal hooks
/// disabled via direct sqlite_master manipulation: SQLite normally cascades
/// the shadow tables, but when DDL is interrupted (or when an old version
/// wrote to the schema directly), shadows can survive.
#[test]
fn r1_orphan_shadow_tables_are_cleaned_before_rebuild() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "orphan_shadows.db");
    insert_memory(&conn, "m1", "/x/a", "hello", "{}", None, None);
    insert_memory(&conn, "m2", "/x/b", "world", "{}", None, None);

    // Surgical reproduction of the production state: delete the virtual table
    // entry from sqlite_master directly via writable_schema, leaving the
    // shadow tables (memories_fts_data, _idx, _docsize, _config) behind. This
    // mimics what a partially-applied DROP / crash leaves on disk.
    conn.execute_batch(
        "PRAGMA writable_schema = ON;\n\
         DELETE FROM sqlite_master WHERE name = 'memories_fts';\n\
         PRAGMA writable_schema = OFF;",
    )
    .unwrap();
    drop(conn);

    // Reopen so SQLite re-reads the now-tampered schema.
    let conn = Connection::open(&path).unwrap();
    let shadow_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type='table' AND name LIKE 'memories_fts\\_%' ESCAPE '\\'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        shadow_count > 0,
        "test setup invariant: shadow tables should survive after schema-row deletion"
    );
    // And confirm the recreate would fail without the fix.
    let recreate_attempt: rusqlite::Result<()> = conn.execute_batch(
        "CREATE VIRTUAL TABLE memories_fts USING fts5(\
            id UNINDEXED, path, summary, text, keywords, entities, tokenize='simple');",
    );
    assert!(
        recreate_attempt.is_err(),
        "without the fix, naive CREATE VIRTUAL TABLE should fail on orphan shadows"
    );
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = FtsRebuild.dry_run(&mut ctx).unwrap();
    assert!(
        dry.findings
            .iter()
            .any(|f| f.kind == "fts_table_missing_with_orphan_shadows"),
        "dry-run should surface the orphan-shadow blocker, got {dry:?}"
    );

    let app = FtsRebuild.apply(&mut ctx).unwrap();
    assert!(
        app.errors.is_empty(),
        "apply must clean shadows + recreate without errors: {:?}",
        app.errors
    );
    assert_eq!(app.applied, 2, "expected both rows reindexed");

    let dry2 = FtsRebuild.dry_run(&mut ctx).unwrap();
    assert!(
        dry2.findings.is_empty(),
        "post-rebuild should be clean: {dry2:?}"
    );
}

#[test]
fn r2_retention_backfill() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "ret.db");
    insert_memory(&conn, "h1", "/handoff/a", "h", "{}", None, None);
    insert_memory(&conn, "k1", "/kanban/x", "k", "{}", None, None);
    insert_memory(&conn, "g1", "/ghost/x", "g", "{}", None, None);
    insert_memory(&conn, "w1", "/wiki/y", "w", "{}", None, None);
    insert_memory(
        &conn,
        "d1",
        "/notes/z",
        "d",
        "{}",
        None,
        Some("foundry_distill"),
    );
    conn.execute("UPDATE memories SET category='decision' WHERE id='n1'", [])
        .unwrap();
    insert_memory(
        &conn,
        "f1",
        "/notes/f",
        "fallback",
        "{}",
        None,
        Some("manual"),
    );
    insert_memory(&conn, "n1", "/notes/n", "n", "{}", Some("durable"), None);
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = RetentionBackfill.dry_run(&mut ctx).unwrap();
    let total: usize = dry.findings.iter().map(|f| f.count).sum();
    assert_eq!(total, 6, "expected 6 backfill candidates, got {dry:?}");

    let app = RetentionBackfill.apply(&mut ctx).unwrap();
    assert_eq!(app.applied, 6);

    let dry2 = RetentionBackfill.dry_run(&mut ctx).unwrap();
    assert!(dry2.findings.is_empty());
}

#[test]
fn r8_junk_cleanup_removes_duplicate_and_cache_rows() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "junk.db");
    let old = "2026-01-01T00:00:00Z";
    let new = "2026-01-02T00:00:00Z";
    conn.execute(
        "UPDATE memories SET timestamp = ?2 WHERE id = ?1",
        ["missing-row", old],
    )
    .ok();

    insert_memory(
        &conn,
        "dup-old",
        "/notes/a",
        "same body duplicated long enough for cleanup",
        "{}",
        Some("durable"),
        None,
    );
    insert_memory(
        &conn,
        "dup-new",
        "/notes/b",
        "same body duplicated long enough for cleanup",
        "{}",
        Some("durable"),
        None,
    );
    conn.execute(
        "UPDATE memories SET timestamp = ?2 WHERE id = ?1",
        ["dup-old", old],
    )
    .unwrap();
    conn.execute(
        "UPDATE memories SET timestamp = ?2 WHERE id = ?1",
        ["dup-new", new],
    )
    .unwrap();
    insert_memory(
        &conn,
        "cache-1",
        "/system/foundry_recall_rerank_cache",
        "cache body",
        r#"{"cache_key":"foundry_recall_rerank_cache"}"#,
        Some("durable"),
        None,
    );
    insert_memory(
        &conn,
        "cache-2",
        "/scratch/recall-cache/noisy",
        "cache body by recall-cache path",
        "{}",
        Some("durable"),
        None,
    );
    conn.execute(
        "UPDATE memories SET topic='recall_rerank_cache' WHERE id='cache-2'",
        [],
    )
    .unwrap();
    insert_memory(
        &conn,
        "empty-turn",
        "/hermes/turns/1",
        "{}",
        "{}",
        Some("durable"),
        None,
    );
    conn.execute(
        "UPDATE memories SET category='other', topic='hermes_turn' WHERE id='empty-turn'",
        [],
    )
    .unwrap();
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = JunkCleanup.dry_run(&mut ctx).unwrap();
    let total: usize = dry.findings.iter().map(|f| f.count).sum();
    assert_eq!(total, 3, "expected 3 junk candidates, got {dry:?}");

    let app = JunkCleanup.apply(&mut ctx).unwrap();
    assert_eq!(app.applied, 3);

    let remaining: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        remaining, 2,
        "same text under different paths should be preserved"
    );
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
    assert!(
        r.findings.is_empty(),
        "clean DB should report no findings: {r:?}"
    );
}

#[test]
fn r5_integrity_detects_corruption() {
    use std::io::{Seek, SeekFrom, Write};
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "corrupt.db");
    insert_memory(&conn, "m1", "/x", "data", "{}", None, None);

    // Pin page size and compact so the file layout is deterministic across
    // SQLite versions, schema migrations, and host page-size differences.
    // After this, the SQLite header is in page 1 (offset 0..4096) and page 2
    // (offset 4096..8192) is the first b-tree interior/leaf page — corrupting
    // it reliably surfaces via `PRAGMA integrity_check` while still allowing
    // `Connection::open` to succeed (so we exercise the rule itself, not the
    // open-failure short-circuit).
    conn.execute_batch(
        "PRAGMA journal_mode = DELETE;\n\
         PRAGMA wal_checkpoint(TRUNCATE);\n\
         PRAGMA page_size = 4096;\n\
         VACUUM;",
    )
    .unwrap();
    drop(conn);

    // Stomp the entirety of page 2 (the first b-tree page after the SQLite
    // header). This invalidates the page-type byte at offset 0 of the page
    // (valid values are 0x02, 0x05, 0x0A, 0x0D) and obliterates every cell
    // pointer and cell payload, so `PRAGMA integrity_check` must report at
    // least one error regardless of which schema object happens to live on
    // page 2 in this build.
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let len = f.metadata().unwrap().len();
    assert!(
        len >= 8192,
        "VACUUMed DB must be at least 2 pages (got {len} bytes)"
    );
    f.seek(SeekFrom::Start(4096)).unwrap();
    f.write_all(&[0xFFu8; 4096]).unwrap();
    f.sync_all().unwrap();
    drop(f);

    // The DB must still be openable — that's the whole point of pinning the
    // header — so we deliberately don't accept `Connection::open` failure as
    // success. If open fails we want the test to fail loudly: that means our
    // corruption strategy regressed.
    let mut ctx = DbContext {
        label: "test".to_string(),
        path: path.clone(),
        conn: Connection::open(&path).expect("DB must remain openable after page-2 corruption"),
    };
    let r = IntegrityCheck
        .dry_run(&mut ctx)
        .expect("integrity_check rule itself must not error");
    assert!(
        r.findings.iter().any(|f| f.kind == "integrity_fail"),
        "expected exactly one integrity_fail finding, got {r:?}"
    );
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
    assert!(
        dry2.findings.is_empty(),
        "post-purge should be clean: {dry2:?}"
    );
}

#[test]
fn r7_orphan_vectors_and_superseded_refs_detected_and_purged() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "vectors.db");
    memory_core::db::try_load_sqlite_vec(&conn);
    insert_memory(&conn, "m1", "/a", "x", "{}", None, None);
    conn.execute(
        "UPDATE memories SET superseded_by = 'ghost' WHERE id = 'm1'",
        [],
    )
    .unwrap();
    let embedding = memory_core::db::serialize_f32(&vec![0.0; 1024]);
    conn.execute(
        "INSERT INTO memories_vec(id, embedding) VALUES ('ghost-vector', ?1)",
        [embedding],
    )
    .unwrap();
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = OrphanRefs.dry_run(&mut ctx).unwrap();
    assert!(
        dry.findings.iter().any(|f| f.kind == "orphans_vectors"),
        "expected vector orphan finding, got {dry:?}"
    );
    assert!(
        dry.findings
            .iter()
            .any(|f| f.kind == "orphans_memories_superseded_by"),
        "expected broken superseded_by finding, got {dry:?}"
    );

    let app = OrphanRefs.apply(&mut ctx).unwrap();
    assert!(app.applied >= 2, "expected at least 2 fixes, got {app:?}");
    let dry2 = OrphanRefs.dry_run(&mut ctx).unwrap();
    assert!(
        dry2.findings.is_empty(),
        "post-purge should be clean: {dry2:?}"
    );
}

#[test]
fn r10_enrichment_failure_reset_clears_failed_markers_only() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "enrichment.db");
    insert_memory(
        &conn,
        "m1",
        "/a",
        "x",
        &serde_json::json!({
            "enrichment": {
                "status": "failed",
                "failed_stage": "embedding",
                "last_error": "Voyage batch API error: 401 Unauthorized",
                "last_failure_at": "2026-06-08T00:00:00Z",
                "attempts": 3
            }
        })
        .to_string(),
        None,
        None,
    );
    insert_memory(
        &conn,
        "m2",
        "/b",
        "y",
        &serde_json::json!({
            "enrichment": {
                "status": "failed",
                "failed_stage": "db_update",
                "last_error": "no such column: persons",
                "last_failure_at": "2026-06-08T00:00:00Z"
            }
        })
        .to_string(),
        None,
        None,
    );
    insert_memory(
        &conn,
        "m3",
        "/c",
        "z",
        &serde_json::json!({
            "enrichment": {
                "status": "complete",
                "attempts": 1
            }
        })
        .to_string(),
        None,
        None,
    );
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = EnrichmentFailureReset.dry_run(&mut ctx).unwrap();
    assert_eq!(
        dry.finding_total(),
        2,
        "expected two failed markers: {dry:?}"
    );
    assert!(dry
        .findings
        .iter()
        .any(|finding| finding.kind == "enrichment_failed_embedding"));
    assert!(dry
        .findings
        .iter()
        .any(|finding| finding.kind == "enrichment_failed_db_update"));

    let applied = EnrichmentFailureReset.apply(&mut ctx).unwrap();
    assert_eq!(applied.applied, 2);
    let remaining = EnrichmentFailureReset.dry_run(&mut ctx).unwrap();
    assert!(
        remaining.findings.is_empty(),
        "failed markers should be cleared: {remaining:?}"
    );

    let metadata: String = ctx
        .conn
        .query_row("SELECT metadata FROM memories WHERE id = 'm1'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(parsed["enrichment"]["attempts"], serde_json::json!(3));
    assert!(parsed["enrichment"].get("status").is_none());
    assert!(parsed["enrichment"].get("last_error").is_none());

    let complete_status: Option<String> = ctx
        .conn
        .query_row(
            "SELECT json_extract(metadata, '$.enrichment.status') FROM memories WHERE id = 'm3'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(complete_status.as_deref(), Some("complete"));
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
        .query_row("SELECT COUNT(*) FROM memories WHERE id='qx'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let n_dst: i64 = Connection::open(&dst_path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM memories WHERE id='qx'", [], |r| {
            r.get(0)
        })
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

#[test]
fn r9_domain_backfill_repairs_missing_and_path_like_values() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "domains.db");
    insert_memory(&conn, "m1", "/wiki/agent/tachi", "wiki", "{}", None, None);
    insert_memory(&conn, "m2", "/scratch/repro", "scratch", "{}", None, None);
    insert_memory(
        &conn,
        "m3",
        "/trading/equity/positions",
        "trade",
        "{}",
        None,
        None,
    );
    conn.execute(
        "UPDATE memories SET domain = '/scratch/sigil' WHERE id = 'm2'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE memories SET domain = 'Hyperion' WHERE id = 'm3'",
        [],
    )
    .unwrap();
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = DomainRepair.dry_run(&mut ctx).unwrap();
    assert!(
        dry.findings.iter().any(|f| f.kind == "domain_repaired"),
        "expected domain repair finding, got {dry:?}"
    );

    let app = DomainRepair.apply(&mut ctx).unwrap();
    assert_eq!(app.applied, 3);

    let conn = Connection::open(&path).unwrap();
    let domains: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare("SELECT id, domain FROM memories ORDER BY id")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(
        domains,
        vec![
            ("m1".to_string(), "wiki".to_string()),
            ("m2".to_string(), "scratch".to_string()),
            ("m3".to_string(), "hyperion".to_string()),
        ]
    );
}

/// B5: pin the historical legacy→current rewrite for stale `expected_db`
/// values so future refactors of `rewrite_legacy_expected_db` cannot
/// silently drop the only mapping that matters in the wild — the
/// `memory-hybrid-bridge` → `extensions/tachi` move.
///
/// The actual filter behavior is exercised end-to-end by
/// `r3_cross_db_restore_all_moves_row`; this test is a focused unit
/// guard on the string-substitution helper itself.
#[test]
fn r3_legacy_expected_db_is_rewritten_to_modern_path() {
    use super::quarantine::rewrite_legacy_expected_db;

    // The exact stale path observed in the field (380 quarantined rows
    // pointed here on `kckylechen`'s box).
    let stale = "/Users/kckylechen/.openclaw/local-plugins/extensions/memory-hybrid-bridge/data/agents/jayne/memory.db";
    let modern = "/Users/kckylechen/.openclaw/extensions/tachi/data/agents/jayne/memory.db";
    assert_eq!(rewrite_legacy_expected_db(stale), modern);

    // Different agent — same prefix substitution must apply.
    let stale_main = "/Users/kckylechen/.openclaw/local-plugins/extensions/memory-hybrid-bridge/data/agents/main/memory.db";
    let modern_main = "/Users/kckylechen/.openclaw/extensions/tachi/data/agents/main/memory.db";
    assert_eq!(rewrite_legacy_expected_db(stale_main), modern_main);

    // Modern paths and unrelated paths must pass through unchanged so we
    // never collapse two different DBs onto one canonical home.
    let already_modern = "/Users/kckylechen/.openclaw/extensions/tachi/data/agents/jayne/memory.db";
    assert_eq!(rewrite_legacy_expected_db(already_modern), already_modern);
    let unrelated = "/Users/kckylechen/.tachi/projects/quant/memory.db";
    assert_eq!(rewrite_legacy_expected_db(unrelated), unrelated);
    assert_eq!(rewrite_legacy_expected_db(""), "");
}
