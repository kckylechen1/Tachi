use super::*;

#[test]
fn r11_plan_c_split_brain_merges_alias_and_relinks_symlink() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var_os("TACHI_HOME");
    let dir = TempDir::new().unwrap();
    let tachi_home = dir.path().join("home");
    std::env::set_var("TACHI_HOME", &tachi_home);

    let repo = dir.path().join("Split Brain Repo");
    let local_db = repo.join(".tachi/memory.db");
    let local_conn = fresh_db_at(&local_db, "project:Split_Brain_Repo");
    insert_memory(
        &local_conn,
        "canonical-only",
        "/project/canonical",
        "canonical row stays authoritative",
        "{}",
        Some("durable"),
        None,
    );
    drop(local_conn);

    let alias_db = crate::path_utils::plan_c_global_db_path("Split_Brain_Repo");
    let alias_conn = fresh_db_at(&alias_db, "alias:Split_Brain_Repo");
    insert_memory(
        &alias_conn,
        "alias-only",
        "/project/alias",
        "alias row should be merged by id",
        "{}",
        Some("durable"),
        None,
    );
    drop(alias_conn);

    let mut ctx = open_ctx(&local_db, "project:Split_Brain_Repo");
    let dry = PlanCRepair { backup_alias: true }
        .dry_run(&mut ctx)
        .unwrap();
    assert!(
        dry.findings
            .iter()
            .any(|finding| finding.kind == "plan_c_split_brain"),
        "dry-run should surface split-brain: {dry:?}"
    );

    let applied = PlanCRepair { backup_alias: true }.apply(&mut ctx).unwrap();
    assert!(
        applied
            .findings
            .iter()
            .any(|finding| finding.kind == "plan_c_alias_relinked"),
        "apply should report relink: {applied:?}"
    );
    assert!(applied.applied > 0, "apply should mutate: {applied:?}");
    assert!(alias_db.is_symlink(), "alias should become a symlink");
    assert!(
        std::fs::read_link(&alias_db).is_ok_and(|target| target == local_db),
        "alias symlink should point at canonical local db"
    );

    let merged_count: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE id IN ('canonical-only', 'alias-only')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(merged_count, 2, "canonical DB should contain both rows");
    assert!(
        std::fs::read_dir(alias_db.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().contains(".bak.")),
        "alias backup should be written before relink"
    );
    assert!(matches!(
        crate::path_utils::inspect_plan_c_alias_for_local_db(&local_db),
        crate::path_utils::PlanCAliasInspection::MatchingSymlink
    ));

    if let Some(value) = saved {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[test]
fn plan_c_repair_opens_v23_guards_and_denies_raw_reference_writes() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("plan-c-v23.db");
    let conn = fresh_db_at(&db_path, "project:plan-c-v23");
    insert_memory(
        &conn,
        "guarded-row",
        "/project/guarded",
        "guarded row",
        "{}",
        None,
        None,
    );
    drop(conn);

    let ctx = open_ctx(&db_path, "project:plan-c-v23");
    let schema_version: i64 = ctx
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        schema_version,
        i64::from(memcore::db::migrations::EXPECTED_SCHEMA_VERSION),
        "fixture must retain the current canonical schema"
    );
    let reference_guard_count: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'trigger'
               AND name IN (
                   'memories_reserved_refs_insert_guard',
                   'memories_reserved_refs_update_guard'
               )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        reference_guard_count, 2,
        "v23 canonical reference-write triggers must be present"
    );

    let error = ctx
        .conn
        .execute(
            "UPDATE memories
             SET metadata = json_set(metadata, '$.source_refs', json('[\"untyped\"]'))
             WHERE id = 'guarded-row'",
            [],
        )
        .expect_err("raw repair connection must not bypass reserved reference guards");
    assert!(
        error
            .to_string()
            .contains("reserved memory reference metadata requires typed mutation"),
        "unexpected protected-write result: {error}"
    );
}

#[test]
#[cfg(unix)]
fn quarantine_subcommands_refuse_manifest_project_symlink_without_following_foreign_db() {
    use crate::manifest::{DbEntry, DbRole, Manifest};
    use std::os::unix::fs::MetadataExt;

    let dir = TempDir::new().unwrap();
    let foreign_db = dir.path().join("foreign.db");
    let conn = fresh_db_at(&foreign_db, "foreign");
    let metadata = serde_json::json!({
        "quarantine": {
            "reason": "cross_db_pollution",
            "original_path": "/restored/item",
            "expected_db": foreign_db.display().to_string(),
            "actual_db": foreign_db.display().to_string(),
            "detected_at": "2020-01-01T00:00:00Z"
        }
    });
    insert_memory(
        &conn,
        "linked-row",
        "/_quarantine/cross-db/item",
        "foreign row",
        &metadata.to_string(),
        None,
        None,
    );
    drop(conn);
    let project_link = dir.path().join("project.db");
    std::os::unix::fs::symlink(&foreign_db, &project_link).unwrap();
    let manifest = Manifest {
        schema_version: crate::manifest::MANIFEST_SCHEMA_VERSION,
        generated_at: chrono::Utc::now().to_rfc3339(),
        comment: String::new(),
        dbs: vec![DbEntry {
            path: project_link.to_string_lossy().into_owned(),
            role: DbRole::Project,
            owner: "tachi".into(),
            schema_kind: "tachi".into(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: String::new(),
            last_classification: "healthy".into(),
            scope_hint: "project:linked".into(),
            notes: String::new(),
        }],
    };
    let foreign_metadata = std::fs::symlink_metadata(&foreign_db).unwrap();
    let foreign_identity = (foreign_metadata.dev(), foreign_metadata.ino());
    let foreign_before = std::fs::read(&foreign_db).unwrap();
    let link_metadata = std::fs::symlink_metadata(&project_link).unwrap();
    let link_identity = (link_metadata.dev(), link_metadata.ino());

    let results = [
        crate::repair::quarantine::cmd_list(&manifest, true),
        crate::repair::quarantine::cmd_restore(&manifest, "linked-row", false, true),
        crate::repair::quarantine::cmd_restore_all(&manifest, "project:linked", false, true),
        crate::repair::quarantine::cmd_purge(&manifest, 1, false, true),
    ];

    for result in results {
        let error = result.expect_err("quarantine subcommand must reject project symlink");
        assert!(
            error.to_string().contains("canonical repo DB path")
                && error.to_string().contains("must not be a symlink"),
            "unexpected refusal: {error}"
        );
    }
    let link_after = std::fs::symlink_metadata(&project_link).unwrap();
    assert_eq!((link_after.dev(), link_after.ino()), link_identity);
    assert_eq!(std::fs::read_link(&project_link).unwrap(), foreign_db);
    let foreign_after = std::fs::symlink_metadata(&foreign_db).unwrap();
    assert_eq!((foreign_after.dev(), foreign_after.ino()), foreign_identity);
    assert_eq!(std::fs::read(&foreign_db).unwrap(), foreign_before);
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

    crate::repair::quarantine::cmd_restore_all(&manifest, "project:dst", true, true).unwrap();

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

    // #1335 oracle: restore must project into memories_symbolic_fts so trigram
    // MATCH can retrieve the row (path is a symbolic-indexed column).
    let dst = Connection::open(&dst_path).unwrap();
    let sym_path: String = dst
        .query_row(
            "SELECT path FROM memories_symbolic_fts WHERE id = 'qx'",
            [],
            |r| r.get(0),
        )
        .expect("restored row must exist in memories_symbolic_fts");
    assert_eq!(
        sym_path, "/restored/a",
        "symbolic FTS path must match restored memories.path"
    );
    let match_hits: i64 = dst
        .query_row(
            "SELECT COUNT(*) FROM memories_symbolic_fts \
             WHERE memories_symbolic_fts MATCH '\"restored\"' AND id = 'qx'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        match_hits, 1,
        "trigram MATCH on restored path must hit the moved row"
    );
}

/// Discrimination for #1335 repair-writer blocker: same-DB quarantine restore
/// must refresh `memories_symbolic_fts` after rewriting `path`.
#[test]
fn quarantine_restore_syncs_symbolic_fts_path() {
    use crate::manifest::{DbEntry, DbRole, Manifest};
    let dir = TempDir::new().unwrap();
    let (db_path, mut conn) = fresh_db(&dir, "same.db");

    let meta = serde_json::json!({
        "quarantine": {
            "reason": "cross_db_pollution",
            "original_path": "/scratch/restore-symbolic-path",
            "expected_db": db_path.display().to_string(),
            "actual_db": db_path.display().to_string(),
            "detected_at": "2026-01-01T00:00:00Z",
        }
    });
    insert_memory(
        &conn,
        "qs",
        "/_quarantine/cross-db/scratch/restore-symbolic-path",
        "unique restore symbolic token alphabetazebra",
        &meta.to_string(),
        None,
        None,
    );
    // Seed a stale symbolic projection (pre-restore path) so a no-op sync
    // cannot accidentally pass — restore must rewrite the indexed path.
    {
        let tx = conn.transaction().unwrap();
        memcore::db::sync_memories_symbolic_fts(&tx, "qs").unwrap();
        tx.commit().unwrap();
    }
    let stale_path: String = conn
        .query_row(
            "SELECT path FROM memories_symbolic_fts WHERE id = 'qs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        stale_path.contains("/_quarantine/"),
        "precondition: symbolic FTS still has quarantine path: {stale_path}"
    );
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
            scope_hint: "project:same".into(),
            notes: String::new(),
        }],
    };

    crate::repair::quarantine::cmd_restore(&manifest, "qs", true, true).unwrap();

    let conn = Connection::open(&db_path).unwrap();
    let (mem_path, sym_path): (String, String) = conn
        .query_row(
            "SELECT m.path, s.path FROM memories m \
             JOIN memories_symbolic_fts s ON s.id = m.id \
             WHERE m.id = 'qs'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("restored row must join memories ↔ memories_symbolic_fts");
    assert_eq!(mem_path, "/scratch/restore-symbolic-path");
    assert_eq!(
        sym_path, "/scratch/restore-symbolic-path",
        "symbolic FTS must track the restored path"
    );
    let match_hits: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memories_symbolic_fts \
             WHERE memories_symbolic_fts MATCH '\"restore-symbolic-path\"' AND id = 'qs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        match_hits, 1,
        "trigram MATCH must retrieve the path-restored row"
    );
}

/// Replace `memories_symbolic_fts` with a plain table + aborting DELETE
/// trigger so `sync_memories_symbolic_fts` / `delete_memories_symbolic_fts`
/// fail without weakening those helpers (#1335 oracle fault-path).
fn arm_symbolic_fts_delete_failure(conn: &Connection, seed_id: &str) {
    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS memories_symbolic_fts;
        CREATE TABLE memories_symbolic_fts (
            id TEXT PRIMARY KEY,
            path TEXT,
            summary TEXT,
            text TEXT,
            keywords TEXT,
            entities TEXT,
            topic TEXT
        );
        "#,
    )
    .unwrap();
    conn.execute(
        "INSERT INTO memories_symbolic_fts(id, path, summary, text, keywords, entities, topic)
         VALUES (?1, 'seed', '', '', '[]', '[]', '')",
        params![seed_id],
    )
    .unwrap();
    conn.execute_batch(
        r#"
        CREATE TRIGGER memories_symbolic_fts_fail_delete
        BEFORE DELETE ON memories_symbolic_fts
        BEGIN
            SELECT RAISE(ABORT, 'injected symbolic fts failure');
        END;
        "#,
    )
    .unwrap();
}

/// #1335 oracle: same-DB restore must roll back the memories UPDATE when
/// symbolic sync fails after the old projection was deleted.
#[test]
fn quarantine_restore_rolls_back_when_symbolic_sync_fails() {
    use crate::manifest::{DbEntry, DbRole, Manifest};
    let dir = TempDir::new().unwrap();
    let (db_path, conn) = fresh_db(&dir, "same-fail.db");

    let quarantine_path = "/_quarantine/cross-db/scratch/restore-fail";
    let meta = serde_json::json!({
        "quarantine": {
            "reason": "cross_db_pollution",
            "original_path": "/scratch/restore-fail",
            "expected_db": db_path.display().to_string(),
            "actual_db": db_path.display().to_string(),
            "detected_at": "2026-01-01T00:00:00Z",
        }
    });
    insert_memory(
        &conn,
        "qs-fail",
        quarantine_path,
        "restore fault path",
        &meta.to_string(),
        None,
        None,
    );
    arm_symbolic_fts_delete_failure(&conn, "qs-fail");
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
            scope_hint: "project:same-fail".into(),
            notes: String::new(),
        }],
    };

    let err = crate::repair::quarantine::cmd_restore(&manifest, "qs-fail", true, true)
        .expect_err("symbolic sync failure must abort restore");
    assert!(
        err.to_string().contains("injected symbolic fts failure")
            || err.to_string().contains("ABORT"),
        "error should surface injected failure, got: {err}"
    );

    let conn = Connection::open(&db_path).unwrap();
    let (path, meta): (String, String) = conn
        .query_row(
            "SELECT path, metadata FROM memories WHERE id = 'qs-fail'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("primary row must still exist after rolled-back restore");
    assert_eq!(
        path, quarantine_path,
        "memories.path must not commit on sync failure"
    );
    let v: serde_json::Value = serde_json::from_str(&meta).unwrap();
    assert!(
        v.get("quarantine").is_some(),
        "quarantine metadata must not be stripped when sync fails: {meta}"
    );
}

/// #1335 oracle: purge must not commit memories DELETE when symbolic delete fails.
#[test]
fn quarantine_purge_rolls_back_when_symbolic_delete_fails() {
    use crate::manifest::{DbEntry, DbRole, Manifest};
    let dir = TempDir::new().unwrap();
    let (db_path, conn) = fresh_db(&dir, "purge-fail.db");

    let meta = serde_json::json!({
        "quarantine": {
            "reason": "cross_db_pollution",
            "original_path": "/scratch/purge-fail",
            "expected_db": db_path.display().to_string(),
            "actual_db": db_path.display().to_string(),
            "detected_at": "2020-01-01T00:00:00Z",
        }
    });
    insert_memory(
        &conn,
        "qp-fail",
        "/_quarantine/cross-db/scratch/purge-fail",
        "purge fault path",
        &meta.to_string(),
        None,
        None,
    );
    arm_symbolic_fts_delete_failure(&conn, "qp-fail");
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
            scope_hint: "project:purge-fail".into(),
            notes: String::new(),
        }],
    };

    let err = crate::repair::quarantine::cmd_purge(&manifest, 1, true, true)
        .expect_err("symbolic delete failure must abort purge");
    assert!(
        err.to_string().contains("injected symbolic fts failure")
            || err.to_string().contains("ABORT"),
        "error should surface injected failure, got: {err}"
    );

    let n: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE id = 'qp-fail'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        n, 1,
        "memories row must not commit-delete when symbolic delete fails"
    );
}

/// #1335 oracle: cross-DB move source deletion must roll back when symbolic
/// delete fails (destination insert may have committed in its own tx).
#[test]
fn quarantine_restore_all_rolls_back_source_when_symbolic_delete_fails() {
    use crate::manifest::{DbEntry, DbRole, Manifest};
    let dir = TempDir::new().unwrap();
    let (src_path, src_conn) = fresh_db(&dir, "src-fail.db");
    let (dst_path, _dst_conn) = fresh_db(&dir, "dst-fail.db");

    let dst_canon = std::fs::canonicalize(&dst_path).unwrap();
    let meta = serde_json::json!({
        "quarantine": {
            "reason": "cross_db_pollution",
            "original_path": "/restored/fail",
            "expected_db": dst_canon.display().to_string(),
            "actual_db": src_path.display().to_string(),
            "detected_at": "2026-01-01T00:00:00Z",
        }
    });
    insert_memory(
        &src_conn,
        "qx-fail",
        "/_quarantine/cross-db/restored/fail",
        "cross-db fault path",
        &meta.to_string(),
        None,
        None,
    );
    arm_symbolic_fts_delete_failure(&src_conn, "qx-fail");
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
                scope_hint: "project:src-fail".into(),
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
                scope_hint: "project:dst-fail".into(),
                notes: String::new(),
            },
        ],
    };

    // restore-all aggregates per-row failures into RepairExit(2); the invariant
    // under test is that the source primary DELETE did not commit.
    crate::repair::quarantine::cmd_restore_all(&manifest, "project:dst-fail", true, true)
        .expect_err("symbolic delete failure on source must fail restore-all");

    let n_src: i64 = Connection::open(&src_path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE id = 'qx-fail'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        n_src, 1,
        "source memories DELETE must not commit when symbolic delete fails"
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
    use crate::repair::quarantine::rewrite_legacy_expected_db;

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
