use super::*;
use crate::db::a2a::{
    consume_a2a_for_recipient, insert_a2a_coordination, insert_a2a_envelope, A2aInsertOutcome,
    A2aTransitionActor, NewA2aEnvelope,
};
use crate::db::migrations::{
    catalogue::test_support::with_prefix, run_data_migrations_with_profile,
};
use crate::db::schema::inventory::{query_rows, schema_inventory};
use crate::db::StoreProfile;

fn version_44(profile: StoreProfile) -> Connection {
    crate::db::enable_simple_auto_extension().unwrap();
    crate::db::register_sqlite_vec();
    let conn = Connection::open_in_memory().unwrap();
    crate::db::configure_connection(&conn).unwrap();
    with_prefix(44, || {
        crate::db::schema::init_schema_for_profile(&conn, profile)
    })
    .unwrap();
    conn
}

fn old_columns(conn: &Connection, table: &str) -> String {
    let mut statement = conn
        .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
        .unwrap();
    let columns = statement
        .query_map([table], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|column| format!("\"{}\"", column.unwrap()))
        .collect::<Vec<_>>();
    columns.join(",")
}

#[test]
fn v44_to_v45_preserves_local_rows_replay_keys_foreign_keys_and_portable_44() {
    let mut conn = version_44(StoreProfile::TachiFull);
    let project = "a".repeat(64);
    conn.execute(
        "INSERT INTO agent_identities(agent_identity_id,created_at) VALUES ('local','2026-01-01T00:00:00Z')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO identity_admissions(admission_id,agent_identity_id,connection_id,state,created_at,canonical_project)
         VALUES ('local-admission','local','local-connection','self_asserted','2026-01-01T00:00:00Z',?1)",
        [&project],
    ).unwrap();
    let request = NewA2aEnvelope {
        envelope_id: "legacy-reply".into(),
        issuer_agent_identity_id: "local".into(),
        issuer_connection_id: "local-connection".into(),
        recipient_agent_identity_id: "local".into(),
        subject_ref: "peer_publication:legacy".into(),
        body: "原有回执 / exact bytes\nremain local".into(),
        idempotency_key: "same-local-key".into(),
        created_at: "2026-01-02T00:00:00Z".into(),
        expires_at: "2026-01-03T00:00:00Z".into(),
    };
    insert_a2a_envelope(&mut conn, &request).unwrap();
    consume_a2a_for_recipient(
        &mut conn,
        &A2aTransitionActor {
            agent_identity_id: "local".into(),
            connection_id: "local-connection".into(),
        },
        10,
        "2026-01-02T12:00:00Z",
    )
    .unwrap();
    conn.execute(
        "UPDATE a2a_envelopes SET body=NULL WHERE envelope_id='legacy-reply'",
        [],
    )
    .unwrap();
    let mut coordination = request.clone();
    coordination.envelope_id = "legacy-coordination".into();
    coordination.subject_ref = "coordination:legacy".into();
    insert_a2a_coordination(&mut conn, &coordination, &project).unwrap();
    let envelope_columns = old_columns(&conn, "a2a_envelopes");
    let receipt_columns = old_columns(&conn, "a2a_delivery_receipts");
    let envelopes = query_rows(&conn, "SELECT * FROM a2a_envelopes");
    let receipts = query_rows(&conn, "SELECT * FROM a2a_delivery_receipts");
    let admissions = query_rows(&conn, "SELECT * FROM identity_admissions");
    run_data_migrations_with_profile(
        &mut conn,
        "global",
        std::path::Path::new(":memory:"),
        StoreProfile::TachiFull,
    )
    .unwrap();
    assert_eq!(
        query_rows(
            &conn,
            &format!("SELECT {envelope_columns} FROM a2a_envelopes")
        ),
        envelopes
    );
    assert_eq!(
        query_rows(
            &conn,
            &format!("SELECT {receipt_columns} FROM a2a_delivery_receipts")
        ),
        receipts
    );
    assert_eq!(
        query_rows(&conn, "SELECT * FROM identity_admissions"),
        admissions
    );
    assert!(query_rows(&conn, "PRAGMA foreign_key_check").is_empty());
    assert_eq!(
        crate::db::migrations::read_schema_version(&conn).unwrap(),
        45
    );
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM a2a_envelopes WHERE remote_nonce IS NOT NULL OR remote_metadata IS NOT NULL", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    assert!(matches!(
        insert_a2a_envelope(&mut conn, &request).unwrap(),
        A2aInsertOutcome::Replay { .. }
    ));
    assert!(matches!(
        insert_a2a_coordination(&mut conn, &coordination, &project).unwrap(),
        A2aInsertOutcome::Replay { .. }
    ));
    assert!(
        conn.execute(
            "UPDATE a2a_envelopes SET body=?1 WHERE envelope_id='legacy-coordination'",
            ["x".repeat(4097)]
        )
        .is_err(),
        "remote size allowance must not widen local rows"
    );
    assert!(conn.execute("UPDATE a2a_envelopes SET issuer_identity_assurance='verified' WHERE envelope_id='legacy-coordination'", []).is_err(), "local mailbox keeps its original assurance law");

    // Portable production opens use the projected D7 funnel. The legacy
    // unprojected migration runner above deliberately stamps the Product head.
    let portable_dir = tempfile::tempdir().unwrap();
    let portable_path = portable_dir.path().join("portable.db");
    with_prefix(44, || {
        drop(
            crate::MemoryStore::open_with_context(
                portable_path.to_str().unwrap(),
                &crate::db::DbOpenContext::create_fresh()
                    .with_profile(StoreProfile::PortableKernel),
            )
            .unwrap(),
        );
    });
    let portable = Connection::open(&portable_path).unwrap();
    let portable_before = schema_inventory(&portable);
    let portable_sentinels = query_rows(
        &portable,
        "SELECT * FROM hard_state WHERE namespace='migrations'",
    );
    drop(portable);
    drop(
        crate::MemoryStore::open_with_context(
            portable_path.to_str().unwrap(),
            &crate::db::DbOpenContext::open_existing_deny()
                .with_profile(StoreProfile::PortableKernel),
        )
        .unwrap(),
    );
    let portable = Connection::open(&portable_path).unwrap();
    assert_eq!(
        crate::db::migrations::read_schema_version(&portable).unwrap(),
        44
    );
    assert_eq!(schema_inventory(&portable), portable_before);
    assert_eq!(
        query_rows(
            &portable,
            "SELECT * FROM hard_state WHERE namespace='migrations'"
        ),
        portable_sentinels
    );
    assert_eq!(crate::db::migrations::PORTABLE_EXPECTED_SCHEMA_VERSION, 44);
}

#[test]
fn current_remote_schema_requires_both_replay_constraints_without_repair() {
    let mut conn = version_44(StoreProfile::TachiFull);
    run_data_migrations_with_profile(
        &mut conn,
        "global",
        std::path::Path::new(":memory:"),
        StoreProfile::TachiFull,
    )
    .unwrap();
    for index in ["idx_a2a_remote_sender_key", "idx_a2a_remote_sender_nonce"] {
        conn.execute_batch(&format!("DROP INDEX {index}")).unwrap();
        let before = schema_inventory(&conn);
        let error = run_data_migrations_with_profile(
            &mut conn,
            "global",
            std::path::Path::new(":memory:"),
            StoreProfile::TachiFull,
        )
        .unwrap_err();
        assert!(error.to_string().contains(index), "{error}");
        assert_eq!(
            schema_inventory(&conn),
            before,
            "current-store admission must not repair missing uniqueness"
        );
        let (_, _, sql) = OBJECTS.iter().find(|(_, name, _)| *name == index).unwrap();
        conn.execute_batch(sql).unwrap();
    }
}
