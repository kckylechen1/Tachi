use super::*;
use tempfile::tempdir;

#[test]
fn table_exists_true_for_present_table_false_for_missing() {
    let conn = make_conn();
    assert!(table_exists(&conn, "memories"));
    assert!(!table_exists(&conn, "definitely_not_a_real_table"));
}

#[test]
fn count_memories_rows_zero_then_nonzero() {
    let mut conn = make_conn();
    assert_eq!(count_memories_rows(&conn).unwrap(), 0);
    upsert(&mut conn, &make_entry("m1", "first"), false).unwrap();
    upsert(&mut conn, &make_entry("m2", "second"), false).unwrap();
    assert_eq!(count_memories_rows(&conn).unwrap(), 2);
}

#[test]
fn count_chunks_rows_reflects_legacy_table_contents() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE chunks (id TEXT PRIMARY KEY, text TEXT);
         INSERT INTO chunks VALUES ('c1', 'legacy chunk');",
    )
    .unwrap();
    assert_eq!(count_chunks_rows(&conn).unwrap(), 1);
}

#[test]
fn count_memories_vec_rows_errors_when_table_missing() {
    // A bare connection with only a `memories` table (no sqlite-vec virtual
    // table) — mirrors a legacy/foreign DB doctor may scan.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE memories (id TEXT PRIMARY KEY);")
        .unwrap();
    assert!(count_memories_vec_rows(&conn).is_err());
}

#[test]
fn count_memories_vec_rows_zero_when_table_present_and_empty() {
    let conn = make_conn();
    if !table_exists(&conn, "memories_vec") {
        // sqlite-vec extension unavailable on this platform/build — skip.
        return;
    }
    assert_eq!(count_memories_vec_rows(&conn).unwrap(), 0);
}

#[test]
fn count_memories_missing_domain_covers_null_and_empty_string_but_not_set() {
    let mut conn = make_conn();
    let mut null_domain = make_entry("null-domain", "no domain");
    null_domain.domain = None;
    upsert(&mut conn, &null_domain, false).unwrap();

    let mut empty_domain = make_entry("empty-domain", "empty domain");
    empty_domain.domain = Some(String::new());
    upsert(&mut conn, &empty_domain, false).unwrap();

    let mut set_domain = make_entry("set-domain", "has domain");
    set_domain.domain = Some("trading".to_string());
    upsert(&mut conn, &set_domain, false).unwrap();

    assert_eq!(count_memories_missing_domain(&conn).unwrap(), 2);
}

#[test]
fn foundry_job_status_counts_all_zero_when_table_missing() {
    let conn = make_conn();
    let counts = foundry_job_status_counts(&conn);
    assert_eq!(counts, FoundryJobStatusCounts::default());
}

#[test]
fn foundry_job_status_counts_tallies_by_status_case_insensitively() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE foundry_jobs (id TEXT, status TEXT);
         INSERT INTO foundry_jobs VALUES
             ('j1', 'completed'),
             ('j2', 'Completed'),
             ('j3', 'skipped'),
             ('j4', 'FAILED'),
             ('j5', 'pending'),
             ('j6', 'running');",
    )
    .unwrap();

    let counts = foundry_job_status_counts(&conn);
    assert_eq!(counts.total, 6);
    assert_eq!(counts.completed, 2, "case-insensitive match on status");
    assert_eq!(counts.skipped, 1);
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.pending, 1);
    // 'running' isn't one of the tallied buckets — total still counts it,
    // callers reconcile the "other" bucket themselves from total - known.
}

#[test]
fn schema_version_fails_on_garbage_bytes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("garbage.db");
    std::fs::write(&path, b"not a sqlite file at all, just junk bytes").unwrap();

    // The file is garbage, so every probe must fail. On SQLite >= 3.50 the
    // failure can surface at open time (when a registered auto-extension like
    // libsimple cannot initialise against the invalid header) or at query time
    // (`pragma schema_version` returns SQLITE_NOTADB). Both prove the file is
    // not a database; we assert the combined outcome rather than a single step.
    let result = open_raw(&path).and_then(|conn| schema_version(&conn));
    assert!(
        result.is_err(),
        "expected an error on a garbage file, got Ok"
    );
}

#[test]
fn schema_version_succeeds_on_a_real_database() {
    let conn = Connection::open_in_memory().unwrap();
    assert!(schema_version(&conn).is_ok());
}

#[test]
fn open_immutable_readonly_round_trips_an_existing_database() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("ro.db");
    {
        let conn = open_raw(&path).unwrap();
        conn.execute_batch("CREATE TABLE t (id INTEGER);").unwrap();
    }

    let uri = format!("file:{}?mode=ro&immutable=1", path.display());
    let conn = open_immutable_readonly(&uri).expect("open immutable readonly");
    assert!(table_exists(&conn, "t"));
}

#[test]
fn open_immutable_readonly_errors_on_missing_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("does-not-exist.db");
    let uri = format!("file:{}?mode=ro&immutable=1", path.display());
    assert!(open_immutable_readonly(&uri).is_err());
}

#[test]
fn checkpoint_wal_truncate_is_a_harmless_noop_off_wal_mode() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("chk.db");
    let conn = open_for_wal_checkpoint(path.to_str().unwrap()).expect("open for checkpoint");
    conn.execute_batch("CREATE TABLE t (id INTEGER);").unwrap();
    // Not in WAL mode here — PRAGMA wal_checkpoint is still a valid no-op.
    checkpoint_wal_truncate(&conn).expect("checkpoint should not error off WAL mode");
}

// ── #1041 S4: doctor cross-domain keyword suspect probe ─────────────────────

#[test]
fn probe_keyword_suspects_finds_matching_row_and_samples_its_id() {
    let mut conn = make_conn();
    let mut trading_flavored = make_entry("trading-in-eng-store", "positions: 持仓 300502.SZ");
    trading_flavored.domain = Some("engineering".to_string());
    upsert(&mut conn, &trading_flavored, false).unwrap();
    upsert(
        &mut conn,
        &make_entry("clean", "just a normal engineering note"),
        false,
    )
    .unwrap();

    let probe = probe_keyword_suspects(&conn, &["持仓", "止损"], 5).unwrap();
    assert_eq!(probe.count, 1);
    assert_eq!(probe.sample_ids, vec!["trading-in-eng-store".to_string()]);
}

#[test]
fn probe_keyword_suspects_zero_when_no_keyword_matches() {
    let mut conn = make_conn();
    upsert(
        &mut conn,
        &make_entry("clean", "just engineering notes"),
        false,
    )
    .unwrap();

    let probe = probe_keyword_suspects(&conn, &["持仓", "止损"], 5).unwrap();
    assert_eq!(probe, KeywordSuspectProbe::default());
}

#[test]
fn probe_keyword_suspects_respects_sample_limit() {
    let mut conn = make_conn();
    for i in 0..3 {
        let id = format!("hit-{i}");
        upsert(&mut conn, &make_entry(&id, "持仓 hit"), false).unwrap();
    }

    let probe = probe_keyword_suspects(&conn, &["持仓"], 2).unwrap();
    assert_eq!(probe.count, 3);
    assert_eq!(probe.sample_ids.len(), 2);
}

#[test]
fn probe_keyword_suspects_empty_keywords_is_all_zero() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("a", "持仓 whatever"), false).unwrap();

    let probe = probe_keyword_suspects(&conn, &[], 5).unwrap();
    assert_eq!(probe, KeywordSuspectProbe::default());
}

#[test]
fn probe_keyword_suspects_degrades_to_text_only_on_legacy_schema() {
    // Foreign/legacy `memories` table lacking `summary`/`path` — the probe
    // must fall back to a text-only scan instead of erroring.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE memories (id TEXT PRIMARY KEY, text TEXT);
         INSERT INTO memories (id, text) VALUES ('legacy-hit', '持仓 300502.SZ');",
    )
    .unwrap();

    let probe = probe_keyword_suspects(&conn, &["持仓"], 5).unwrap();
    assert_eq!(probe.count, 1);
    assert_eq!(probe.sample_ids, vec!["legacy-hit".to_string()]);
}

#[test]
fn probe_keyword_suspects_missing_memories_table_is_all_zero_not_error() {
    let conn = Connection::open_in_memory().unwrap();
    let probe = probe_keyword_suspects(&conn, &["持仓"], 5).unwrap();
    assert_eq!(probe, KeywordSuspectProbe::default());
}
