use super::*;

#[test]
fn superseded_path_gate_matches_reserved_prefixes_exactly() {
    assert!(!scoped_path_can_surface_superseded(Some("/wiki")));
    assert!(!scoped_path_can_surface_superseded(Some(
        "/wiki/agent/tachi"
    )));
    assert!(!scoped_path_can_surface_superseded(Some("/kanban")));
    assert!(!scoped_path_can_surface_superseded(Some(
        "/kanban/active/task"
    )));

    assert!(scoped_path_can_surface_superseded(Some(
        "/wiki_rules/agent/tachi"
    )));
    assert!(scoped_path_can_surface_superseded(Some(
        "/kanbanboard/active/task"
    )));
}

#[test]
fn hybrid_hides_superseded_by_default() {
    let mut conn = setup();
    insert(
        &mut conn,
        "old",
        "TrendLock protects trends using the stale rule",
        &["trendlock"],
    );
    insert(
        &mut conn,
        "new",
        "TrendLock protects trends using the canonical rule",
        &["trendlock"],
    );
    crate::db::supersede_memory(&conn, "old", "new").unwrap();

    let opts = SearchOptions {
        top_k: 5,
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "TrendLock", &opts).unwrap();
    let ids = results
        .into_iter()
        .map(|result| result.entry.id)
        .collect::<Vec<_>>();
    assert!(ids.contains(&"new".to_string()));
    assert!(!ids.contains(&"old".to_string()));
}

#[test]
fn hybrid_surfaces_superseded_when_explicitly_scoped_to_deep_path() {
    let mut conn = setup();
    let mut old = memory_entry(
        "old-path-memory",
        "clean-cli integration defaults to dry-run and requires --force",
        &["clean-cli", "dry-run"],
    );
    old.path = "/scratch/tachi/clean-cli-integration".to_string();
    insert_entry(&mut conn, old);
    let mut new = memory_entry(
        "new-release-memory",
        "release prep summary for tachi version bump",
        &["release-prep"],
    );
    new.path = "/scratch/tachi/v1.5-release-prep".to_string();
    insert_entry(&mut conn, new);
    crate::db::supersede_memory(&conn, "old-path-memory", "new-release-memory").unwrap();

    let opts = SearchOptions {
        top_k: 5,
        path_prefix: Some("/scratch/tachi/clean-cli-integration".to_string()),
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "dry-run", &opts).unwrap();
    assert_eq!(results[0].entry.id, "old-path-memory");
    assert!(results[0].score.final_score < 1.0);
}

#[test]
fn hybrid_search_respects_as_of_validity_window() {
    let mut conn = setup();
    let mut old = memory_entry("temporal-old", "TemporalHybridNeedle old memory", &[]);
    old.valid_from = "2026-01-01T00:00:00Z".to_string();
    old.valid_until = Some("2026-02-01T00:00:00Z".to_string());
    upsert(&mut conn, &old, false).unwrap();

    let mut new = memory_entry("temporal-new", "TemporalHybridNeedle new memory", &[]);
    new.valid_from = "2026-02-01T00:00:00Z".to_string();
    upsert(&mut conn, &new, false).unwrap();

    let january = hybrid_search(
        &conn,
        "TemporalHybridNeedle",
        &SearchOptions {
            top_k: 5,
            record_access: false,
            as_of: Some("2026-01-15T00:00:00.000Z".to_string()),
            ..Default::default()
        },
    )
    .unwrap()
    .into_iter()
    .map(|result| result.entry.id)
    .collect::<Vec<_>>();
    assert!(january.contains(&"temporal-old".to_string()));
    assert!(!january.contains(&"temporal-new".to_string()));

    let march = hybrid_search(
        &conn,
        "TemporalHybridNeedle",
        &SearchOptions {
            top_k: 5,
            record_access: false,
            as_of: Some("2026-03-01T00:00:00.000Z".to_string()),
            ..Default::default()
        },
    )
    .unwrap()
    .into_iter()
    .map(|result| result.entry.id)
    .collect::<Vec<_>>();
    assert!(!march.contains(&"temporal-old".to_string()));
    assert!(march.contains(&"temporal-new".to_string()));
}

#[test]
fn supersede_closes_open_valid_until_for_point_in_time_recall() {
    let mut conn = setup();
    let mut old = memory_entry("sup-old", "SupersedeNeedle old memory", &[]);
    old.valid_from = "2020-01-01T00:00:00Z".to_string();
    upsert(&mut conn, &old, false).unwrap();
    let new = memory_entry("sup-new", "SupersedeNeedle new memory", &[]);
    upsert(&mut conn, &new, false).unwrap();

    // Before supersession the row's validity window is open.
    let before: Option<String> = conn
        .query_row(
            "SELECT valid_until FROM memories WHERE id = 'sup-old'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(before.is_none(), "valid_until should start open");

    crate::db::supersede_memory(&conn, "sup-old", "sup-new").unwrap();

    // After supersession valid_until is closed, so `as_of` after that time
    // prefers the superseding row instead of returning the stale fact.
    let after: Option<String> = conn
        .query_row(
            "SELECT valid_until FROM memories WHERE id = 'sup-old'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        after.is_some(),
        "supersede_memory must close valid_until at supersession time"
    );
}

#[test]
fn supersede_preserves_explicit_valid_until() {
    let mut conn = setup();
    let mut old = memory_entry("sup-old2", "SupersedeNeedle2 old memory", &[]);
    old.valid_from = "2020-01-01T00:00:00Z".to_string();
    old.valid_until = Some("2021-06-01T00:00:00Z".to_string());
    upsert(&mut conn, &old, false).unwrap();
    let new = memory_entry("sup-new2", "SupersedeNeedle2 new memory", &[]);
    upsert(&mut conn, &new, false).unwrap();

    crate::db::supersede_memory(&conn, "sup-old2", "sup-new2").unwrap();

    // COALESCE keeps an explicitly-set window; supersession does not clobber it.
    let after: Option<String> = conn
        .query_row(
            "SELECT valid_until FROM memories WHERE id = 'sup-old2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        after
            .as_deref()
            .unwrap_or_default()
            .starts_with("2021-06-01"),
        "explicit valid_until must be preserved, got {after:?}"
    );
}

#[test]
fn supersede_edge_is_immutable_after_the_first_write() {
    let mut conn = setup();
    insert(&mut conn, "immutable-source", "immutable source body", &[]);
    insert(&mut conn, "immutable-target-b", "requested target B", &[]);
    insert(&mut conn, "immutable-target-c", "canonical target C", &[]);

    assert!(
        crate::db::supersede_memory(&conn, "immutable-source", "immutable-target-c").unwrap(),
        "the first edge must install"
    );
    let before: (Option<String>, Option<String>, i64) = conn
        .query_row(
            "SELECT superseded_by, valid_until, revision FROM memories WHERE id = ?1",
            ["immutable-source"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();

    assert!(
        !crate::db::supersede_memory(&conn, "immutable-source", "immutable-target-c").unwrap(),
        "replaying the same edge must not rewrite it"
    );
    assert!(
        !crate::db::supersede_memory(&conn, "immutable-source", "immutable-target-b").unwrap(),
        "a conflicting edge must not replace the first edge"
    );
    let after: (Option<String>, Option<String>, i64) = conn
        .query_row(
            "SELECT superseded_by, valid_until, revision FROM memories WHERE id = ?1",
            ["immutable-source"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        after, before,
        "same-edge replay and conflicting requests must leave the first edge byte-for-byte intact"
    );
    assert_eq!(after.0.as_deref(), Some("immutable-target-c"));
}
