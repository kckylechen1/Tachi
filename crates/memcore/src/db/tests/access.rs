use super::*;

// ───────────────────────────────────────────────────────────────────────────
// Touch-algorithm regression tests
//
// These guard the contract that `record_access` is a read-side accounting
// bump and `vault_touch_entry` is race-truthful. See
// .tachi/runs/agent-prompts/tachi-shell-github-convoy-touchy-agent-prompt.md
// section 三 for the full reveal that motivated these.
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn record_access_bumps_count_and_last_access_only() {
    let mut conn = make_conn();
    let e = make_entry("touch-1", "first text");
    upsert(&mut conn, &e, false).unwrap();
    let fixed_updated_at = "2000-01-01T00:00:00Z";
    conn.execute(
        "UPDATE memories SET updated_at = ?1 WHERE id = ?2",
        params![fixed_updated_at, "touch-1"],
    )
    .unwrap();

    let (rev0, upd0, ac0): (i64, String, i64) = conn
        .query_row(
            "SELECT revision, updated_at, access_count FROM memories WHERE id = ?1",
            params!["touch-1"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        ac0, 0,
        "freshly upserted memory must start with access_count=0"
    );
    assert_eq!(upd0, fixed_updated_at);

    record_access(&conn, &["touch-1".to_string()], &[], None).unwrap();

    let (rev1, upd1, ac1, la1): (i64, String, i64, Option<String>) = conn
        .query_row(
            "SELECT revision, updated_at, access_count, last_access \
             FROM memories WHERE id = ?1",
            params!["touch-1"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();

    assert_eq!(ac1, 1, "record_access must increment access_count by 1");
    assert!(la1.is_some(), "record_access must set last_access");
    assert_eq!(
        rev1, rev0,
        "record_access must NOT bump revision (optimistic-concurrency invariant)"
    );
    assert_eq!(
        upd1, upd0,
        "record_access must NOT bump updated_at (downstream cache invariant)"
    );

    // access_history row recorded
    let ah_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            params!["touch-1"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        ah_count, 1,
        "record_access must insert exactly one access_history row"
    );
}

#[test]
fn record_access_with_updates_returns_post_write_access_fields() {
    let mut conn = make_conn();
    let mut e = make_entry("touch-return", "return updated access fields");
    e.access_count = 7;
    upsert(&mut conn, &e, false).unwrap();

    let updates = record_access_with_updates(&conn, &["touch-return".to_string()], &[], None)
        .expect("record access updates");
    let update = updates.get("touch-return").expect("updated row");
    let db_update: AccessUpdate = conn
        .query_row(
            "SELECT access_count, last_access FROM memories WHERE id = ?1",
            params!["touch-return"],
            |row| {
                Ok(AccessUpdate {
                    access_count: row.get(0)?,
                    last_access: row.get(1)?,
                })
            },
        )
        .unwrap();

    assert_eq!(update.access_count, 8);
    assert!(update.last_access.is_some());
    assert_eq!(update, &db_update);
}

#[test]
fn record_access_with_updates_ignores_missing_ids() {
    let mut conn = make_conn();
    let e = make_entry("touch-present", "present access row");
    upsert(&mut conn, &e, false).unwrap();

    let updates = record_access_with_updates(
        &conn,
        &["touch-present".to_string(), "touch-missing".to_string()],
        &[],
        None,
    )
    .expect("missing rows should not abort accounting");

    assert!(updates.contains_key("touch-present"));
    assert!(!updates.contains_key("touch-missing"));
    let missing_history: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            params!["touch-missing"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(missing_history, 0);
}

#[test]
fn record_access_repeats_accumulate_on_access_history() {
    let mut conn = make_conn();
    let e = make_entry("touch-2", "repeat target");
    upsert(&mut conn, &e, false).unwrap();

    for _ in 0..3 {
        record_access(&conn, &["touch-2".to_string()], &[], None).unwrap();
    }

    let ac: i64 = conn
        .query_row(
            "SELECT access_count FROM memories WHERE id = ?1",
            params!["touch-2"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ac, 3);

    let ah: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            params!["touch-2"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        ah, 3,
        "each record_access call should append to access_history"
    );
}

#[cfg(feature = "admin")]
#[test]
fn vault_touch_entry_returns_post_touch_count() {
    use crate::vault::VaultEntry;

    let conn = make_conn();
    let entry = VaultEntry {
        name: "TOUCH_KEY".into(),
        encrypted_value: "AQID".into(),
        nonce: "BAUG".into(),
        secret_type: "api_key".into(),
        description: "regression".into(),
        allowed_agents: None,
        created_at: String::new(),
        updated_at: String::new(),
        accessed_at: String::new(),
        access_count: 0,
    };
    vault_upsert_entry(&conn, &entry).unwrap();

    let c1 = vault_touch_entry(&conn, "TOUCH_KEY").unwrap();
    assert_eq!(c1, 1, "first touch must report 1, not the pre-touch 0");

    let c2 = vault_touch_entry(&conn, "TOUCH_KEY").unwrap();
    assert_eq!(c2, 2, "second touch must report 2, race-truthful");

    let c3 = vault_touch_entry(&conn, "TOUCH_KEY").unwrap();
    assert_eq!(c3, 3);

    // DB row must agree with the last returned value.
    let db_count: i64 = conn
        .query_row(
            "SELECT access_count FROM vault_entries WHERE name = ?1",
            params!["TOUCH_KEY"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(db_count, c3);
}
