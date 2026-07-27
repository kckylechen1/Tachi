use super::*;
use rusqlite::params;
use serde_json::json;

#[test]
fn list_eval_evidence_returns_empty_for_no_rows() {
    let conn = make_conn();
    let rows = list_eval_evidence(&conn, 7, 100, false).expect("list eval evidence");
    assert!(rows.is_empty());
}

#[test]
fn list_eval_evidence_filters_by_days_and_excludes_auto_synthesized() {
    let mut conn = make_conn();
    let mut entry = make_entry("eval-old", "old eval");
    entry.path = "/eval/run-1".into();
    upsert(&mut conn, &entry, false).unwrap();
    conn.execute(
        "UPDATE memories SET created_at = ?1 WHERE id = 'eval-old'",
        params!["2020-01-01T00:00:00Z"],
    )
    .unwrap();

    let mut recent = make_entry("eval-recent", "recent eval");
    recent.path = "/eval/run-2".into();
    upsert(&mut conn, &recent, false).unwrap();
    conn.execute(
        "UPDATE memories SET created_at = ?1 WHERE id = 'eval-recent'",
        params!["2099-01-01T00:00:00Z"],
    )
    .unwrap();

    let mut auto = make_entry("eval-auto", "auto eval");
    auto.path = "/eval/run-3".into();
    auto.metadata = json!({ "auto_synthesized": 1 });
    upsert(&mut conn, &auto, false).unwrap();
    conn.execute(
        "UPDATE memories SET created_at = ?1 WHERE id = 'eval-auto'",
        params!["2099-01-02T00:00:00Z"],
    )
    .unwrap();

    let rows = list_eval_evidence(&conn, 7, 100, false).expect("list without filter");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| row.id == "eval-recent"));
    assert!(rows.iter().any(|row| row.id == "eval-auto"));

    let filtered = list_eval_evidence(&conn, 7, 100, true).expect("list with filter");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, "eval-recent");
}

#[test]
fn collect_daily_health_snapshot_reports_zeroes_for_empty_db() {
    let conn = make_conn();
    let snapshot = collect_daily_health_snapshot(&conn).expect("health snapshot");
    assert_eq!(snapshot.total_entries, 0);
    assert_eq!(snapshot.new_today, 0);
    assert_eq!(snapshot.stale_days, 0);
    assert!(snapshot.groups.is_empty());
    assert!(snapshot.duplicate_summaries.is_empty());
}

#[test]
fn collect_daily_health_snapshot_aggregates_groups_and_duplicates() {
    let mut conn = make_conn();
    let mut first = make_entry("dup-a", "shared summary text");
    first.summary = "shared summary".into();
    first.category = "fact".into();
    first.source = "agent".into();
    upsert(&mut conn, &first, false).unwrap();

    let mut second = make_entry("dup-b", "shared summary text again");
    second.summary = "shared summary".into();
    second.category = "fact".into();
    second.source = "agent".into();
    upsert(&mut conn, &second, false).unwrap();

    let mut other = make_entry("other", "different summary");
    other.summary = "different".into();
    other.category = "decision".into();
    other.source = "user".into();
    upsert(&mut conn, &other, false).unwrap();

    let snapshot = collect_daily_health_snapshot(&conn).expect("health snapshot");
    assert_eq!(snapshot.total_entries, 3);
    assert_eq!(snapshot.groups.len(), 2);
    assert_eq!(snapshot.duplicate_summaries.len(), 1);
    assert_eq!(snapshot.duplicate_summaries[0].summary, "shared summary");
    assert_eq!(snapshot.duplicate_summaries[0].count, 2);
}

#[test]
fn promote_memory_to_durable_updates_retention_fields() {
    let mut conn = make_conn();
    let entry = make_entry("promote-me", "promote candidate");
    upsert(&mut conn, &entry, false).unwrap();

    promote_memory_to_durable(&conn, "promote-me").expect("promote");
    let (importance, retention): (f64, String) = conn
        .query_row(
            "SELECT importance, retention_policy FROM memories WHERE id = 'promote-me'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!((importance - 0.7).abs() < f64::EPSILON);
    assert_eq!(retention, "durable");
}

#[test]
fn count_distinct_access_days_returns_zero_for_missing_history() {
    let mut conn = make_conn();
    let entry = make_entry("no-access", "no access history");
    upsert(&mut conn, &entry, false).unwrap();
    let days = count_distinct_access_days(&conn, "no-access").expect("days");
    assert_eq!(days, 0);
}

#[test]
fn count_distinct_access_days_counts_unique_dates() {
    let mut conn = make_conn();
    let entry = make_entry("with-access", "has access history");
    upsert(&mut conn, &entry, false).unwrap();
    conn.execute(
        "INSERT INTO access_history (memory_id, accessed_at, query_hash) VALUES (?1, ?2, ?3)",
        params!["with-access", "2026-07-01T10:00:00Z", "q1"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO access_history (memory_id, accessed_at, query_hash) VALUES (?1, ?2, ?3)",
        params!["with-access", "2026-07-01T12:00:00Z", "q2"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO access_history (memory_id, accessed_at, query_hash) VALUES (?1, ?2, ?3)",
        params!["with-access", "2026-07-02T12:00:00Z", "q3"],
    )
    .unwrap();

    let days = count_distinct_access_days(&conn, "with-access").expect("days");
    assert_eq!(days, 2);
}

/// tachi#1446 lever 6, requirement "knob OFF ⇒ byte-identical".
///
/// The assertion is deliberately *not* against a hand-written expected number:
/// it runs the literal pre-#1446 unfiltered statement against the same
/// connection and requires the knob-OFF arm to return that value. A future edit
/// that filters the OFF query fails here even if someone updates the constant
/// in the test above.
#[test]
fn off_arm_equals_the_pre_1446_unfiltered_count_on_display_only_history() {
    let mut conn = make_conn();
    let entry = make_entry("legacy", "history written before event_kind existed");
    upsert(&mut conn, &entry, false).unwrap();
    for (day, hash) in [
        ("2026-07-01T10:00:00Z", "q1"),
        ("2026-07-01T18:00:00Z", "q2"),
        ("2026-07-02T09:00:00Z", "q3"),
        ("2026-07-05T09:00:00Z", "q4"),
    ] {
        conn.execute(
            "INSERT INTO access_history (memory_id, accessed_at, query_hash) VALUES (?1, ?2, ?3)",
            params!["legacy", day, hash],
        )
        .unwrap();
    }

    let unfiltered: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT date(accessed_at)) FROM access_history WHERE memory_id = ?1",
            params!["legacy"],
            |row| row.get(0),
        )
        .unwrap();
    let off = count_distinct_promotion_days(&conn, "legacy", &crate::RecallConfig::default())
        .expect("days");

    assert_eq!(
        off, unfiltered as usize,
        "the knob-OFF arm must return exactly the number the pre-#1446 \
         unfiltered query returned"
    );
    assert_eq!(off, 3);
}

/// tachi#1446 lever 6: exposure alone must contribute nothing on the ON arm,
/// while OFF remains exactly the pre-#1446 unfiltered count even after `use`
/// rows exist.
#[test]
fn use_arm_ignores_display_days_and_off_arm_preserves_mixed_history() {
    let mut conn = make_conn();
    let entry = make_entry("mixed", "display and use history");
    upsert(&mut conn, &entry, false).unwrap();

    // Three days of pure exposure: the recall pipeline showed this row.
    for (day, hash) in [
        ("2026-07-01T10:00:00Z", "q1"),
        ("2026-07-02T10:00:00Z", "q2"),
        ("2026-07-03T10:00:00Z", "q3"),
    ] {
        conn.execute(
            "INSERT INTO access_history (memory_id, accessed_at, query_hash, event_kind)
             VALUES (?1, ?2, ?3, 'display')",
            params!["mixed", day, hash],
        )
        .unwrap();
    }
    // One day on which a caller-initiated save cited this memory, and on which
    // it was never displayed.
    conn.execute(
        "INSERT INTO access_history (memory_id, accessed_at, query_hash, event_kind)
         VALUES (?1, ?2, '', 'use')",
        params!["mixed", "2026-07-09T10:00:00Z"],
    )
    .unwrap();

    let display =
        count_distinct_access_days_of_kind(&conn, "mixed", Some(AccessEventKind::Display))
            .expect("days");
    let used = count_distinct_access_days_of_kind(&conn, "mixed", Some(AccessEventKind::Use))
        .expect("days");
    let off = count_distinct_promotion_days(&conn, "mixed", &crate::RecallConfig::default())
        .expect("days");
    let unfiltered: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT date(accessed_at)) FROM access_history WHERE memory_id = ?1",
            params!["mixed"],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(display, 3, "the display-only diagnostic sees exposure days");
    assert_eq!(
        used, 1,
        "the ON arm sees only the day a caller actually cited this memory"
    );
    assert_eq!(
        off, unfiltered as usize,
        "knob OFF must remain exactly the pre-lever-6 unfiltered query even \
         after the always-on use writer has added mixed-provenance history"
    );
}
