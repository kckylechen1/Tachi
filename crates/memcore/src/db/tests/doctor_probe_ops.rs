use super::*;
use tempfile::tempdir;

#[test]
fn table_exists_true_for_present_table_false_for_missing() {
    let conn = make_conn();
    assert!(table_exists(&conn, "memories").unwrap());
    assert!(!table_exists(&conn, "definitely_not_a_real_table").unwrap());
}

/// #1041 B6 regression: a REAL `sqlite_master` query failure (simulated here
/// via a file-backed connection that can't even acquire a read lock because
/// another connection holds an EXCLUSIVE one) must surface as `Err`, not
/// collapse into the same `false` as "table genuinely doesn't exist". Before
/// the fix, `probe_keyword_suspects`/`classify` would have read this as
/// "nothing to scan" and reported a false-clean `Some(0)`.
#[test]
fn table_exists_propagates_a_real_query_error_not_false() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("locked.db");
    let seed = Connection::open(&path).unwrap();
    seed.execute_batch("CREATE TABLE memories (id TEXT);")
        .unwrap();
    drop(seed);

    let locker = open_raw(&path).unwrap();

    // Open the reader BEFORE taking the EXCLUSIVE lock: connection open runs
    // the auto-extension load, which itself needs a read of the database — if
    // the lock is already held, the open (not the query under test) fails
    // with DatabaseBusy / "automatic extension loading failed", and under
    // full-suite parallel load no bounded retry reliably outlives the
    // contention window. With both connections open first, only the QUERY
    // runs against the held lock, which is exactly the failure this test is
    // about.
    let reader = open_raw(&path).unwrap();
    reader
        .busy_timeout(std::time::Duration::from_millis(0))
        .unwrap();

    locker.execute_batch("BEGIN EXCLUSIVE;").unwrap();
    let result = table_exists(&reader, "memories");
    assert!(
        result.is_err(),
        "a real sqlite_master query failure (locked out) must be Err, not false"
    );

    locker.execute_batch("ROLLBACK;").unwrap();
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
    if !table_exists(&conn, "memories_vec").unwrap() {
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
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE t (id INTEGER);").unwrap();
    }

    let uri = format!("file:{}?mode=ro&immutable=1", path.display());
    let conn = open_immutable_readonly(&uri).expect("open immutable readonly");
    assert!(table_exists(&conn, "t").unwrap());
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
    let seed = Connection::open(&path).unwrap();
    seed.execute_batch(
        "CREATE TABLE memories (
             id TEXT PRIMARY KEY,
             metadata TEXT NOT NULL DEFAULT '{}'
         );
         INSERT INTO memories(id, metadata)
         VALUES ('protected-evidence', '{\"evidence_refs_v1\":[{\"ref\":\"#1\"}]}');",
    )
    .unwrap();
    drop(seed);

    let conn = open_for_wal_checkpoint(path.to_str().unwrap()).expect("open for checkpoint");
    assert!(
        conn.execute_batch("CREATE TABLE checkpoint_schema_escape(id INTEGER)")
            .is_err(),
        "checkpoint handle mutated schema"
    );
    assert!(
        conn.execute(
            "UPDATE memories SET metadata = '{}' WHERE id = 'protected-evidence'",
            [],
        )
        .is_err(),
        "checkpoint handle mutated protected evidence"
    );
    // Not in WAL mode here — PRAGMA wal_checkpoint is still a valid no-op.
    checkpoint_wal_truncate(&conn).expect("checkpoint should not error off WAL mode");
    let metadata: String = conn
        .query_row(
            "SELECT metadata FROM memories WHERE id = 'protected-evidence'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(metadata.contains("evidence_refs_v1"));
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

#[test]
fn probe_keyword_suspects_errors_when_even_text_only_fallback_fails() {
    // #1041 F7 regression: a `memories` table that exists and has `text`
    // (so the count query succeeds) but no `id` column at all — the sample
    // query (`select id from memories ... order by id`) fails even in the
    // narrowest text-only fallback. This must propagate as a real error,
    // not collapse to `Ok(KeywordSuspectProbe::default())`: silently
    // reporting 0 here is indistinguishable from "checked, and clean",
    // which is the false-clean diagnostic this fix closes.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE memories (text TEXT);
         INSERT INTO memories (text) VALUES ('持仓 300502.SZ');",
    )
    .unwrap();

    let result = probe_keyword_suspects(&conn, &["持仓"], 5);
    assert!(
        result.is_err(),
        "a schema that can't even run the narrowest fallback must error, not default"
    );
}
