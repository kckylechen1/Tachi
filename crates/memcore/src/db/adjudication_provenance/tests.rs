use super::*;

fn open_conn() -> Connection {
    crate::db::enable_simple_auto_extension().unwrap();
    crate::db::register_sqlite_vec();
    let conn = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    conn
}

fn receipt_json(channel: &str, actor: &str, model: Option<&str>) -> String {
    serde_json::json!({
        "subject": {"actor": actor, "model": model},
        "channel": channel,
        "binding": {"admission_id": "admission-1"},
        "captured_at": "2026-11-18T00:00:00Z",
    })
    .to_string()
}

fn insert_dispatch(
    conn: &Connection,
    id: &str,
    seq: i64,
    receipt: Option<&str>,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO dispatch_adjudications
         (adjudication_id, outcome_id, event_key, verdict, actor, evidence_ref, insertion_seq,
          adjudication_receipt)
         VALUES (?1, 'outcome', ?1, 'accepted', 'leader', 'evidence', ?2, ?3)",
        params![id, seq, receipt],
    )
}

fn insert_mirror(
    conn: &Connection,
    id: &str,
    seq: i64,
    receipt: Option<&str>,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO mirror_eval_adjudications
         (adjudication_id, eval_run_id, event_key, actor, verifier_model, usefulness,
          evidence_usable, evidence_ref, insertion_seq, adjudication_receipt)
         VALUES (?1, 'run', ?1, 'leader', 'openai/gpt-5', 'useful', 1, 'evidence', ?2, ?3)",
        params![id, seq, receipt],
    )
}

#[test]
fn operator_cli_is_rejected_by_vocabulary_parse_and_sql_check() {
    assert_eq!(AdjudicationChannel::parse("operator_cli"), None);
    let raw = receipt_json("operator_cli", "leader", None);
    assert!(serde_json::from_str::<AdjudicationReceipt>(&raw).is_err());
    assert_eq!(AdjudicationReceipt::parse_stored(&raw), None);
    assert_eq!(
        adjudication_basis_for_receipt(Some(&raw), ReceiptRowSubject::Actor("leader")),
        "unknown"
    );

    let conn = open_conn();
    let missing_channel = r#"{"subject":{"actor":"leader"},"captured_at":"t"}"#;
    let numeric_channel = r#"{"subject":{"actor":"leader"},"channel":7,"captured_at":"t"}"#;
    for (case, receipt) in [
        ("operator_cli", raw.as_str()),
        (
            "future channel",
            r#"{"channel":"cli_token","subject":{"actor":"leader"},"captured_at":"t"}"#,
        ),
        ("absent channel", missing_channel),
        ("non-text channel", numeric_channel),
        ("not JSON", "{\"channel\":\"verified_admission\""),
        ("JSON array", r#"["verified_admission"]"#),
        (
            "root array smuggling",
            r#"[["leader",null],"verified_admission",null,"t1"]"#,
        ),
        (
            "subject array smuggling",
            r#"{"subject":["leader",null],"channel":"verified_admission","captured_at":"t1"}"#,
        ),
    ] {
        for insert in [insert_dispatch, insert_mirror] {
            let error = insert(&conn, &format!("bad-{case}"), 1, Some(receipt)).expect_err(case);
            assert!(
                error.to_string().contains("CHECK") || error.to_string().contains("JSON"),
                "{case}: {error}"
            );
        }
    }
    for table in ["dispatch_adjudications", "mirror_eval_adjudications"] {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "{table}: rejected receipts write nothing");
    }
}

#[test]
fn basis_column_check_refuses_tokens_outside_the_five_level_table() {
    let conn = open_conn();
    let error = conn
        .execute(
            "INSERT INTO dispatch_adjudications
             (adjudication_id, outcome_id, event_key, verdict, actor, evidence_ref, insertion_seq,
              adjudication_attribution_basis)
             VALUES ('a', 'o', 'k', 'accepted', 'leader', 'e', 1, 'verified')",
            [],
        )
        .expect_err("non-table basis token");
    assert!(error.to_string().contains("CHECK"), "{error}");
    assert_eq!(normalize_adjudication_basis("verified"), "unknown");
    assert_eq!(normalize_adjudication_basis(""), "unknown");
    for token in ADJUDICATION_ATTRIBUTION_BASES {
        assert_eq!(normalize_adjudication_basis(token), token);
    }
}

#[test]
fn classifier_maps_channels_and_degrades_illegal_or_mismatched_receipts() {
    let actor = ReceiptRowSubject::Actor("leader");
    for (channel, basis) in [
        ("verified_admission", "acknowledged_overlay"),
        ("self_asserted_local", "planned_unconfirmed"),
        ("unverified_caller", "unknown"),
    ] {
        let raw = receipt_json(channel, "leader", None);
        assert_eq!(
            adjudication_basis_for_receipt(Some(&raw), actor),
            basis,
            "{channel}"
        );
    }
    assert_eq!(adjudication_basis_for_receipt(None, actor), "unknown");

    // Structural parse only: a corrupt document that merely contains the
    // verified token is never sniffed into a higher basis.
    let verified = receipt_json("verified_admission", "leader", None);
    let truncated = &verified[..verified.len() - 1];
    for (case, raw) in [
        ("truncated", truncated.to_string()),
        (
            "garbage",
            "not json \"channel\":\"verified_admission\"".to_string(),
        ),
        ("unknown field", verified.replacen('{', r#"{"extra":1,"#, 1)),
        ("empty actor", receipt_json("verified_admission", " ", None)),
        (
            "missing captured_at",
            r#"{"subject":{"actor":"leader"},"channel":"verified_admission"}"#.to_string(),
        ),
        (
            "empty model",
            receipt_json("verified_admission", "leader", Some("")),
        ),
        (
            "actor mismatch",
            receipt_json("verified_admission", "someone-else", None),
        ),
        (
            "root array smuggling",
            r#"[["leader",null],"verified_admission",null,"t1"]"#.to_string(),
        ),
        (
            "subject array smuggling",
            r#"{"subject":["leader",null],"channel":"verified_admission","captured_at":"t1"}"#
                .to_string(),
        ),
    ] {
        assert_eq!(
            adjudication_basis_for_receipt(Some(&raw), actor),
            "unknown",
            "{case}"
        );
    }

    let mirror_raw = receipt_json("verified_admission", "leader", Some("openai/gpt-5"));
    assert_eq!(
        adjudication_basis_for_receipt(
            Some(&mirror_raw),
            ReceiptRowSubject::ActorAndModel("leader", Some("openai/gpt-5"))
        ),
        "acknowledged_overlay"
    );
    assert_eq!(
        adjudication_basis_for_receipt(
            Some(&mirror_raw),
            ReceiptRowSubject::ActorAndModel("leader", Some("anthropic/claude"))
        ),
        "unknown",
        "a receipt that binds a different reviewer model attests nothing"
    );

    // A structurally legal but JSON-valid-shaped receipt stored raw lands
    // `unknown` through the same classifier the INSERT gate uses.
    let conn = open_conn();
    insert_dispatch(
        &conn,
        "structurally-illegal",
        1,
        Some(r#"{"subject":{"actor":"leader"},"channel":"verified_admission"}"#),
    )
    .unwrap();
    let basis: String = conn
        .query_row(
            "SELECT adjudication_attribution_basis FROM dispatch_adjudications",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(basis, "unknown");
}

fn assert_append_only(
    conn: &Connection,
    table: &str,
    insert: fn(&Connection, &str, i64, Option<&str>) -> rusqlite::Result<usize>,
) {
    insert(conn, "victim", 1, None).unwrap();
    let victim_rowid: i64 = conn
        .query_row(
            &format!("SELECT rowid FROM {table} WHERE adjudication_id='victim'"),
            [],
            |r| r.get(0),
        )
        .unwrap();
    let parent = if table == "dispatch_adjudications" {
        "outcome_id"
    } else {
        "eval_run_id"
    };
    let parent_value = if table == "dispatch_adjudications" {
        "outcome"
    } else {
        "run"
    };
    let columns = if table == "dispatch_adjudications" {
        "(rowid, adjudication_id, outcome_id, event_key, verdict, actor, evidence_ref, insertion_seq)"
    } else {
        "(rowid, adjudication_id, eval_run_id, event_key, actor, usefulness, evidence_usable, evidence_ref, insertion_seq)"
    };
    let values = |rowid: &str, id: &str, key: &str, seq: i64| {
        if table == "dispatch_adjudications" {
            format!(
                "({rowid}, '{id}', '{parent_value}', '{key}', 'rejected', 'attacker', 'e', {seq})"
            )
        } else {
            format!("({rowid}, '{id}', '{parent_value}', '{key}', 'attacker', 'useless', 0, 'e', {seq})")
        }
    };
    let receipt = receipt_json("verified_admission", "leader", None);
    for (label, sql) in [
        (
            "update basis",
            format!("UPDATE {table} SET adjudication_attribution_basis='acknowledged_overlay'"),
        ),
        (
            "update receipt",
            format!("UPDATE {table} SET adjudication_receipt='{receipt}'"),
        ),
        (
            "update created_at",
            format!("UPDATE {table} SET created_at='2027-01-01T00:00:00Z'"),
        ),
        (
            "update or replace rowid",
            format!("UPDATE OR REPLACE {table} SET rowid={victim_rowid} + 1"),
        ),
        ("delete", format!("DELETE FROM {table}")),
        (
            "replace adjudication_id",
            format!(
                "INSERT OR REPLACE INTO {table} {columns} VALUES {}",
                values("NULL", "victim", "fresh-key-1", 90)
            ),
        ),
        (
            "replace event_key",
            format!(
                "INSERT OR REPLACE INTO {table} {columns} VALUES {}",
                values("NULL", "fresh-id-2", "victim", 91)
            ),
        ),
        (
            "replace parent+seq",
            format!(
                "INSERT OR REPLACE INTO {table} {columns} VALUES {}",
                values("NULL", "fresh-id-3", "fresh-key-3", 1)
            ),
        ),
        (
            "replace explicit rowid",
            format!(
                "INSERT OR REPLACE INTO {table} {columns} VALUES {}",
                values(&victim_rowid.to_string(), "fresh-id-4", "fresh-key-4", 92)
            ),
        ),
        (
            "plain explicit rowid",
            format!(
                "INSERT INTO {table} {columns} VALUES {}",
                values(&victim_rowid.to_string(), "fresh-id-5", "fresh-key-5", 93)
            ),
        ),
        (
            "explicit sentinel rowid -1",
            format!(
                "INSERT INTO {table} {columns} VALUES {}",
                values("-1", "fresh-id-6", "fresh-key-6", 94)
            ),
        ),
    ] {
        let error = conn.execute(&sql, []).expect_err(label);
        assert!(
            error.to_string().contains("append-only"),
            "{table} {label}: {error}"
        );
    }
    let survivor: (i64, String, String, Option<String>, String) = conn
        .query_row(
            &format!("SELECT rowid, adjudication_id, {parent}, adjudication_receipt, adjudication_attribution_basis FROM {table}"),
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(
        survivor,
        (
            victim_rowid,
            "victim".into(),
            parent_value.into(),
            None,
            "unknown".into()
        ),
        "{table}: the original row survives every refused mutation"
    );
}

#[test]
fn trigger_triple_refuses_update_delete_and_every_replace_on_both_ledgers() {
    for recursive_triggers in [false, true] {
        let conn = open_conn();
        conn.pragma_update(None, "recursive_triggers", recursive_triggers)
            .unwrap();
        assert_append_only(&conn, "dispatch_adjudications", insert_dispatch);
        assert_append_only(&conn, "mirror_eval_adjudications", insert_mirror);
    }
}

#[test]
fn trigger_inventory_refuses_missing_or_weakened_provenance_triggers() {
    for (name, table, _) in TRIGGERS {
        let conn = open_conn();
        validate_adjudication_provenance_schema(&conn).unwrap();
        crate::db::validate_persistent_trigger_inventory(&conn, true).unwrap();

        conn.execute_batch(&format!("DROP TRIGGER {name}")).unwrap();
        let error = validate_adjudication_provenance_schema(&conn).expect_err(name);
        assert!(error.to_string().contains(name), "{error}");
        let error = crate::db::validate_persistent_trigger_inventory(&conn, true).expect_err(name);
        assert!(
            error.to_string().contains("required trigger"),
            "{name}: {error}"
        );

        conn.execute_batch(&format!(
            "CREATE TRIGGER {name} BEFORE DELETE ON {table} WHEN 0 BEGIN SELECT 1; END"
        ))
        .unwrap();
        let error = crate::db::validate_persistent_trigger_inventory(&conn, true).expect_err(name);
        assert!(
            error.to_string().contains("does not match"),
            "{name}: {error}"
        );
    }
}

#[test]
fn validator_refuses_widened_conflict_targets() {
    let conn = open_conn();
    conn.execute_batch(
        "CREATE UNIQUE INDEX idx_widened_target ON dispatch_adjudications(created_at)",
    )
    .unwrap();
    let error = validate_adjudication_provenance_schema(&conn).expect_err("extra unique target");
    assert!(
        error
            .to_string()
            .contains("unexpected unique conflict targets"),
        "{error}"
    );
}

fn ledger_rows(conn: &Connection, table: &str) -> Vec<(String, Option<String>, String)> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT adjudication_id, adjudication_receipt, adjudication_attribution_basis
             FROM {table} ORDER BY adjudication_id"
        ))
        .unwrap();
    let rows = statement
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    rows
}

#[test]
fn v43_backfills_pre_v43_actor_rows_and_replays_with_zero_changes() {
    crate::db::enable_simple_auto_extension().unwrap();
    crate::db::register_sqlite_vec();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    crate::db::migrations::catalogue::test_support::with_prefix(42, || {
        let conn = Connection::open(&path).unwrap();
        crate::db::init_schema(&conn).unwrap();
        assert!(!has_column(&conn, "dispatch_adjudications", "adjudication_receipt").unwrap());
        assert!(!object_exists(&conn, "trigger", "dispatch_adjudications_no_update").unwrap());
        conn.execute_batch(
            "INSERT INTO dispatch_adjudications
             (adjudication_id, outcome_id, event_key, verdict, actor, evidence_ref, insertion_seq)
             VALUES ('legacy-d1', 'o', 'k1', 'accepted', 'leader', 'e', 1),
                    ('legacy-d2', 'o', 'k2', 'rejected', 'claimed-gpt', 'e', 2);
             INSERT INTO mirror_eval_adjudications
             (adjudication_id, eval_run_id, event_key, actor, verifier_model, usefulness,
              evidence_usable, evidence_ref, insertion_seq)
             VALUES ('legacy-m1', 'r', 'mk1', 'leader', 'openai/gpt-5', 'useful', 1, 'e', 1);",
        )
        .unwrap();
    });

    drop(
        crate::MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &crate::db::DbOpenContext::open_existing_allow("test:v43-backfill"),
        )
        .expect("authorized v42 -> v43 migration"),
    );

    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        crate::db::migrations::read_schema_version(&conn).unwrap(),
        43
    );
    let expected_dispatch = vec![
        (
            "legacy-d1".to_string(),
            None,
            "fallback_unreceipted".to_string(),
        ),
        (
            "legacy-d2".to_string(),
            None,
            "fallback_unreceipted".to_string(),
        ),
    ];
    let expected_mirror = vec![(
        "legacy-m1".to_string(),
        None,
        "fallback_unreceipted".to_string(),
    )];
    assert_eq!(
        ledger_rows(&conn, "dispatch_adjudications"),
        expected_dispatch
    );
    assert_eq!(
        ledger_rows(&conn, "mirror_eval_adjudications"),
        expected_mirror
    );
    validate_adjudication_provenance_schema(&conn).unwrap();

    // Second run: zero row changes, no refused UPDATE, identical rows.
    let before = conn.total_changes();
    assert_eq!(
        migrate_v43_adjudication_provenance(&conn, StoreProfile::TachiFull).unwrap(),
        0
    );
    assert_eq!(conn.total_changes(), before, "replay must change zero rows");
    assert_eq!(
        ledger_rows(&conn, "dispatch_adjudications"),
        expected_dispatch
    );
    assert_eq!(
        ledger_rows(&conn, "mirror_eval_adjudications"),
        expected_mirror
    );

    // Backfilled history is frozen too.
    let error = conn
        .execute(
            "UPDATE dispatch_adjudications SET adjudication_attribution_basis='acknowledged_overlay'",
            [],
        )
        .expect_err("backfilled rows are immutable");
    assert!(error.to_string().contains("append-only"), "{error}");
}

#[test]
fn legacy_negative_one_rowid_allows_ordinary_appends_and_blocks_replace() {
    crate::db::enable_simple_auto_extension().unwrap();
    crate::db::register_sqlite_vec();
    for recursive_triggers in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("neg1.db");
        crate::db::migrations::catalogue::test_support::with_prefix(42, || {
            let conn = Connection::open(&path).unwrap();
            crate::db::init_schema(&conn).unwrap();
            conn.execute_batch(
                "INSERT INTO dispatch_adjudications
                 (rowid, adjudication_id, outcome_id, event_key, verdict, actor, evidence_ref, insertion_seq)
                 VALUES (-1, 'legacy-d-neg1', 'o', 'k-neg1', 'accepted', 'leader', 'e', 1);
                 INSERT INTO mirror_eval_adjudications
                 (rowid, adjudication_id, eval_run_id, event_key, actor, usefulness, evidence_usable, evidence_ref, insertion_seq)
                 VALUES (-1, 'legacy-m-neg1', 'r', 'mk-neg1', 'leader', 'useful', 1, 'e', 1);",
            )
            .unwrap();
        });

        drop(
            crate::MemoryStore::open_with_context(
                path.to_str().unwrap(),
                &crate::db::DbOpenContext::open_existing_allow("test:v43-neg1"),
            )
            .expect("v43 migration succeeds with legacy rowid=-1"),
        );

        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "recursive_triggers", recursive_triggers)
            .unwrap();

        // Ordinary inserts omitting rowid (where SQLite passes NEW.rowid = -1
        // to BEFORE INSERT) must still succeed on both ledgers.
        insert_dispatch(&conn, "normal-d", 2, None).unwrap();
        insert_mirror(&conn, "normal-m", 2, None).unwrap();

        // Explicit INSERT OR REPLACE targeting rowid = -1 must still abort and
        // preserve the pre-v43 rowid=-1 row.
        for sql in [
            "INSERT OR REPLACE INTO dispatch_adjudications
             (rowid, adjudication_id, outcome_id, event_key, verdict, actor, evidence_ref, insertion_seq)
             VALUES (-1, 'attacker-d', 'o', 'k-atk', 'rejected', 'attacker', 'e', 99)",
            "INSERT OR REPLACE INTO mirror_eval_adjudications
             (rowid, adjudication_id, eval_run_id, event_key, actor, usefulness, evidence_usable, evidence_ref, insertion_seq)
             VALUES (-1, 'attacker-m', 'r', 'mk-atk', 'attacker', 'useless', 0, 'e', 99)",
        ] {
            let error = conn.execute(sql, []).expect_err(sql);
            assert!(error.to_string().contains("append-only"), "{error}");
        }
        assert_eq!(
            conn.query_row(
                "SELECT adjudication_id, adjudication_attribution_basis FROM dispatch_adjudications WHERE rowid = -1",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .unwrap(),
            ("legacy-d-neg1".to_string(), "fallback_unreceipted".to_string())
        );
        assert_eq!(
            conn.query_row(
                "SELECT adjudication_id, adjudication_attribution_basis FROM mirror_eval_adjudications WHERE rowid = -1",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .unwrap(),
            ("legacy-m-neg1".to_string(), "fallback_unreceipted".to_string())
        );
    }
}

#[test]
fn v43_migration_classifies_preexisting_illegal_receipts_through_full_validation_and_rejects_missing_check(
) {
    crate::db::enable_simple_auto_extension().unwrap();
    crate::db::register_sqlite_vec();

    // 1. Canonical pre-existing receipt column holding structurally illegal or
    // mismatched JSON runs through FULL migrate_v43_adjudication_provenance
    // (including canonical schema + trigger validation).
    let conn = Connection::open_in_memory().unwrap();
    crate::db::migrations::catalogue::test_support::with_prefix(42, || {
        crate::db::init_schema(&conn).unwrap();
        conn.execute_batch(&format!(
            "ALTER TABLE dispatch_adjudications ADD COLUMN {RECEIPT_COLUMN_SQL};
             INSERT INTO dispatch_adjudications
             (adjudication_id, outcome_id, event_key, verdict, actor, evidence_ref, insertion_seq,
              adjudication_receipt)
             VALUES
               ('illegal', 'o', 'k-illegal', 'accepted', 'leader', 'e', 1, '{{\"subject\":{{\"actor\":\"leader\"}},\"channel\":\"verified_admission\"}}'),
               ('extra-field', 'o', 'k-extra', 'accepted', 'leader', 'e', 2, '{{\"subject\":{{\"actor\":\"leader\"}},\"channel\":\"verified_admission\",\"extra\":1,\"captured_at\":\"t1\"}}'),
               ('empty-actor', 'o', 'k-empty', 'accepted', 'leader', 'e', 3, '{{\"subject\":{{\"actor\":\" \"}},\"channel\":\"verified_admission\",\"captured_at\":\"t1\"}}'),
               ('mismatch', 'o', 'k-mismatch', 'accepted', 'leader', 'e', 4, '{{\"subject\":{{\"actor\":\"other\"}},\"channel\":\"verified_admission\",\"captured_at\":\"t1\"}}'),
               ('valid', 'o', 'k-valid', 'accepted', 'leader', 'e', 5, '{{\"subject\":{{\"actor\":\"leader\"}},\"channel\":\"verified_admission\",\"captured_at\":\"t1\"}}'),
               ('unreceipted', 'o', 'k-none', 'accepted', 'leader', 'e', 6, NULL);"
        ))
        .unwrap();
    });

    let backfilled = migrate_v43_adjudication_provenance(&conn, StoreProfile::TachiFull).unwrap();
    assert_eq!(
        backfilled, 2,
        "only valid + unreceipted rows backfill off unknown"
    );
    assert_eq!(
        ledger_rows(&conn, "dispatch_adjudications")
            .into_iter()
            .map(|(id, _, basis)| (id, basis))
            .collect::<Vec<_>>(),
        vec![
            ("empty-actor".to_string(), "unknown".to_string()),
            ("extra-field".to_string(), "unknown".to_string()),
            ("illegal".to_string(), "unknown".to_string()),
            ("mismatch".to_string(), "unknown".to_string()),
            (
                "unreceipted".to_string(),
                "fallback_unreceipted".to_string()
            ),
            ("valid".to_string(), "acknowledged_overlay".to_string()),
        ]
    );
    validate_adjudication_provenance_schema(&conn).unwrap();

    // 2. A non-canonical receipt column lacking the CHECK constraint is
    // refused by full v43 migration instead of silently passing validation.
    let unguarded = Connection::open_in_memory().unwrap();
    crate::db::migrations::catalogue::test_support::with_prefix(42, || {
        crate::db::init_schema(&unguarded).unwrap();
        unguarded
            .execute_batch(
                "ALTER TABLE dispatch_adjudications ADD COLUMN adjudication_receipt TEXT;",
            )
            .unwrap();
    });
    let error = migrate_v43_adjudication_provenance(&unguarded, StoreProfile::TachiFull)
        .expect_err("non-canonical pre-existing receipt column without CHECK must be refused");
    assert!(
        error
            .to_string()
            .contains("missing a canonical provenance column"),
        "{error}"
    );
}

#[test]
fn current_v43_store_refuses_dropped_trigger_or_weakened_column_check() {
    crate::db::enable_simple_auto_extension().unwrap();
    crate::db::register_sqlite_vec();
    for damage in [
        "DROP TRIGGER dispatch_adjudications_no_replace",
        "DROP TRIGGER mirror_eval_adjudications_no_delete",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("damaged.db");
        drop(
            crate::MemoryStore::open_with_context(
                path.to_str().unwrap(),
                &crate::db::DbOpenContext::create_fresh(),
            )
            .unwrap(),
        );
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(damage).unwrap();
        drop(conn);
        let err = crate::MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &crate::db::DbOpenContext::open_existing_allow("test:damaged-v43"),
        )
        .err()
        .unwrap_or_else(|| panic!("expected refusal after {damage}"));
        assert!(
            err.to_string().contains("required trigger"),
            "{damage}: {err}"
        );
    }
}

#[test]
fn v43_is_vacuous_on_portable_stores() {
    let conn = Connection::open_in_memory().unwrap();
    assert_eq!(
        migrate_v43_adjudication_provenance(&conn, StoreProfile::PortableKernel).unwrap(),
        0
    );
}
