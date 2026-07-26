use super::*;

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
fn quarantine_purge_uses_default_deny_v23_connection() {
    use crate::manifest::{DbEntry, DbRole, Manifest};

    let dir = TempDir::new().unwrap();
    let (db_path, conn) = fresh_db(&dir, "purge-guard.db");
    let schema_version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        schema_version,
        i64::from(memcore::db::migrations::EXPECTED_SCHEMA_VERSION),
        "fixture must retain the current canonical schema"
    );
    let metadata = serde_json::json!({
        "quarantine": {
            "reason": "cross_db_pollution",
            "original_path": "/scratch/purge-guard",
            "expected_db": db_path.display().to_string(),
            "actual_db": db_path.display().to_string(),
            "detected_at": "2020-01-01T00:00:00Z",
        }
    });
    insert_memory(
        &conn,
        "purge-guarded",
        "/_quarantine/cross-db/scratch/purge-guard",
        "purge guard row",
        &metadata.to_string(),
        None,
        None,
    );
    insert_memory(
        &conn,
        "protected-row",
        "/scratch/protected",
        "protected row",
        "{}",
        None,
        None,
    );
    conn.execute_batch(
        "CREATE TRIGGER quarantine_purge_requires_default_deny_guard
         BEFORE DELETE ON memories
         WHEN tachi_reserved_reference_write_enabled() != 0
         BEGIN
             SELECT RAISE(ABORT, 'quarantine purge requires default-deny guard');
         END;",
    )
    .unwrap();
    drop(conn);

    let manifest = Manifest {
        schema_version: 1,
        generated_at: chrono::Utc::now().to_rfc3339(),
        comment: String::new(),
        dbs: vec![DbEntry {
            path: db_path.display().to_string(),
            role: DbRole::Project,
            owner: "test".into(),
            schema_kind: "tachi".into(),
            vec_enabled: false,
            allow_write: true,
            last_doctor_at: chrono::Utc::now().to_rfc3339(),
            last_classification: "tachi".into(),
            scope_hint: "project:purge-guard".into(),
            notes: String::new(),
        }],
    };

    crate::repair::quarantine::cmd_purge(&manifest, 1, true, true)
        .expect("v23 guarded purge should delete the quarantined row");

    let conn = crate::repair::open_repair_connection(&db_path)
        .expect("common repair connection should reopen the current DB");
    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE id = 'purge-guarded'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0, "guarded purge must commit its DELETE");
    let error = conn
        .execute(
            "UPDATE memories
             SET metadata = json_set(metadata, '$.source_refs', json('[\"untyped\"]'))
             WHERE id = 'protected-row'",
            [],
        )
        .expect_err("common repair connection must not grant protected metadata writes");
    assert!(
        error
            .to_string()
            .contains("reserved memory reference metadata requires typed mutation"),
        "unexpected protected-write result: {error}"
    );
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
