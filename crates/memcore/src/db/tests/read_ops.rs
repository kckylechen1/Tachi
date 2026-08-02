use super::*;
use crate::db::{find_active_wiki_entry_by_path, insert_if_absent, list_user_facing_wiki_entries};

#[test]
fn fetch_by_ids_returns_entries() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("fid-1", "alpha memory"), false).unwrap();
    upsert(&mut conn, &make_entry("fid-2", "beta memory"), false).unwrap();
    upsert(&mut conn, &make_entry("fid-3", "gamma memory"), false).unwrap();

    let result = fetch_by_ids(&conn, &["fid-1".into(), "fid-3".into()], false).unwrap();
    assert_eq!(result.len(), 2);
    assert!(result.contains_key("fid-1"));
    assert!(result.contains_key("fid-3"));
    assert!(!result.contains_key("fid-2"));
}

#[test]
fn fetch_by_ids_hydrates_vectors_in_main_query() {
    let mut conn = make_conn();
    let has_vec: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'memories_vec'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if has_vec == 0 {
        return;
    }

    let mut entry = make_entry("fid-vec", "vector memory");
    entry.vector = Some(vec![0.25_f32; 1024]);
    upsert(&mut conn, &entry, true).unwrap();

    let result = fetch_by_ids(&conn, &["fid-vec".into()], false).unwrap();
    let fetched = result.get("fid-vec").expect("entry should be fetched");
    assert_eq!(
        fetched.vector.as_ref().map(Vec::len),
        Some(1024),
        "fetch_by_ids should hydrate the vector from the joined row"
    );
}

#[test]
fn fetch_by_ids_returns_vector_decode_errors() {
    let mut conn = make_conn();
    upsert(
        &mut conn,
        &make_entry("fid-bad-vec", "bad vector memory"),
        false,
    )
    .unwrap();
    conn.execute("DROP TABLE IF EXISTS memories_vec", [])
        .unwrap();
    conn.execute(
        "CREATE TABLE memories_vec(id TEXT PRIMARY KEY, embedding BLOB)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO memories_vec(id, embedding) VALUES (?1, ?2)",
        params!["fid-bad-vec", vec![1_u8, 2, 3]],
    )
    .unwrap();

    let err = fetch_by_ids(&conn, &["fid-bad-vec".into()], false)
        .expect_err("invalid vector blob length should not be silently ignored");
    assert!(
        err.to_string().contains("invalid vector blob length"),
        "{err}"
    );
}

#[test]
fn fetch_by_ids_empty_input() {
    let conn = make_conn();
    let result = fetch_by_ids(&conn, &[], false).unwrap();
    assert!(result.is_empty());
}

#[test]
fn get_all_returns_ordered_by_timestamp() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("ga-1", "first memory"), false).unwrap();
    upsert(&mut conn, &make_entry("ga-2", "second memory"), false).unwrap();

    let all = get_all(&conn, 10, false).unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn get_all_respects_limit() {
    let mut conn = make_conn();
    for i in 0..5 {
        upsert(
            &mut conn,
            &make_entry(&format!("lim-{i}"), &format!("entry {i}")),
            false,
        )
        .unwrap();
    }
    let limited = get_all(&conn, 2, false).unwrap();
    assert_eq!(limited.len(), 2);
}

#[test]
fn list_by_path_filters_prefix() {
    let mut conn = make_conn();
    let mut e1 = make_entry("lp-1", "under project");
    e1.path = "/project/alpha".into();
    upsert(&mut conn, &e1, false).unwrap();

    let mut e2 = make_entry("lp-2", "under docs");
    e2.path = "/docs/beta".into();
    upsert(&mut conn, &e2, false).unwrap();

    let project_entries = list_by_path(&conn, "/project", 10, false).unwrap();
    assert_eq!(project_entries.len(), 1);
    assert_eq!(project_entries[0].id, "lp-1");
}

#[test]
fn list_by_path_empty_prefix_returns_all() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("lbe-1", "any"), false).unwrap();
    upsert(&mut conn, &make_entry("lbe-2", "any other"), false).unwrap();
    let all = list_by_path(&conn, "/", 10, false).unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn list_by_path_active_unsuperseded_excludes_superseded_without_changing_generic_list() {
    let mut conn = make_conn();
    let mut active = make_entry("active-current", "current wiki row");
    active.path = "/wiki/engineering/current".to_string();
    upsert(&mut conn, &active, false).unwrap();

    let mut superseded = make_entry("active-superseded", "historical wiki row");
    superseded.path = "/wiki/engineering/superseded".to_string();
    upsert(&mut conn, &superseded, false).unwrap();
    conn.execute(
        "UPDATE memories SET superseded_by = 'active-current' WHERE id = 'active-superseded'",
        [],
    )
    .unwrap();

    let current = list_by_path_active_unsuperseded(&conn, "/wiki/engineering", 10).unwrap();
    let current_ids = current
        .into_iter()
        .map(|entry| entry.id)
        .collect::<std::collections::HashSet<_>>();
    assert!(current_ids.contains("active-current"));
    assert!(
        !current_ids.contains("active-superseded"),
        "active-unsuperseded path listing must not surface historical superseded rows"
    );

    let generic = list_by_path(&conn, "/wiki/engineering", 10, false).unwrap();
    let generic_ids = generic
        .into_iter()
        .map(|entry| entry.id)
        .collect::<std::collections::HashSet<_>>();
    assert!(
        generic_ids.contains("active-superseded"),
        "generic list_by_path remains an audit-capable archived-only listing"
    );
}

#[test]
fn list_wiki_duplicate_candidates_pushes_path_topic_and_parent_filter_to_sql() {
    let mut conn = make_conn();
    let mut same_path = make_entry("wiki-same-path", "same path");
    same_path.path = "/wiki/engineering/mcp".to_string();
    same_path.topic = "mcp".to_string();
    upsert(&mut conn, &same_path, false).unwrap();

    let mut same_topic_elsewhere = make_entry("wiki-same-topic", "same topic");
    same_topic_elsewhere.path = "/wiki/ops/mcp".to_string();
    same_topic_elsewhere.topic = "mcp".to_string();
    upsert(&mut conn, &same_topic_elsewhere, false).unwrap();

    let mut parent_sibling = make_entry("wiki-parent-sibling", "sibling text candidate");
    parent_sibling.path = "/wiki/engineering/other".to_string();
    parent_sibling.topic = "other".to_string();
    upsert(&mut conn, &parent_sibling, false).unwrap();

    let mut unrelated = make_entry("wiki-unrelated", "unrelated text");
    unrelated.path = "/wiki/product/roadmap".to_string();
    unrelated.topic = "roadmap".to_string();
    upsert(&mut conn, &unrelated, false).unwrap();

    let candidates = list_wiki_duplicate_candidates(
        &conn,
        "/wiki/engineering/mcp",
        "mcp",
        "/wiki/engineering",
        Some(10),
    )
    .unwrap();
    let ids = candidates
        .into_iter()
        .map(|entry| entry.id)
        .collect::<std::collections::HashSet<_>>();

    assert!(ids.contains("wiki-same-path"));
    assert!(ids.contains("wiki-same-topic"));
    assert!(ids.contains("wiki-parent-sibling"));
    assert!(!ids.contains("wiki-unrelated"));
}

#[test]
fn guide_projection_candidates_are_scanned_without_crossing_into_wiki() {
    let mut conn = make_conn();
    let mut guide = make_entry("guide-same-path", "older guide body");
    guide.path = "/guide/global/workflows/review".to_string();
    guide.topic = "review".to_string();
    guide.category = "guide".to_string();
    upsert(&mut conn, &guide, false).unwrap();

    let mut wiki = make_entry("wiki-same-topic", "same semantic body");
    wiki.path = "/wiki/global/workflows/review".to_string();
    wiki.topic = "review".to_string();
    upsert(&mut conn, &wiki, false).unwrap();

    let candidates = list_wiki_duplicate_candidates(
        &conn,
        "/guide/global/workflows/review",
        "review",
        "/guide/global/workflows",
        None,
    )
    .unwrap();
    let ids = candidates
        .into_iter()
        .map(|entry| entry.id)
        .collect::<std::collections::HashSet<_>>();
    assert!(ids.contains("guide-same-path"));
    assert!(!ids.contains("wiki-same-topic"));
}

#[test]
fn ordinary_wiki_projection_does_not_select_rem_or_draft_rows() {
    let mut conn = make_conn();
    let mut rem = make_entry("wiki-rem:reserved", "same topic rem draft");
    rem.path = "/wiki/drafts/rem".to_string();
    rem.topic = "shared-topic".to_string();
    insert_if_absent(&mut conn, &rem, false).unwrap();
    let mut ordinary_draft = make_entry("ordinary-draft", "same topic ordinary draft");
    ordinary_draft.path = "/wiki/drafts/ordinary".to_string();
    ordinary_draft.topic = "shared-topic".to_string();
    upsert(&mut conn, &ordinary_draft, false).unwrap();

    assert!(
        find_active_wiki_entry_by_path(&conn, "/wiki/general/shared-topic")
            .unwrap()
            .is_none()
    );
    assert!(list_wiki_duplicate_candidates(
        &conn,
        "/wiki/general/shared-topic",
        "shared-topic",
        "/wiki/general",
        None,
    )
    .unwrap()
    .is_empty());
}

#[test]
fn ordinary_wiki_projection_excludes_internal_log_and_recall_cache_rows() {
    let mut conn = make_conn();
    let mut log = make_entry("wiki-internal-log", "same internal text");
    log.path = "/wiki/general/internal-log".to_string();
    log.topic = "shared-topic".to_string();
    log.metadata = json!({"wiki_log": true});
    upsert(&mut conn, &log, false).unwrap();
    conn.execute(
        "UPDATE memories SET metadata = json_set(metadata, '$.wiki_log', 1) WHERE id = 'wiki-internal-log'",
        [],
    )
    .unwrap();

    let mut cache = make_entry("wiki-recall-cache", "same internal text");
    cache.path = "/wiki/recall-cache".to_string();
    cache.topic = "shared-topic".to_string();
    upsert(&mut conn, &cache, false).unwrap();

    let mut cache_descendant = make_entry("wiki-recall-cache-descendant", "same internal text");
    cache_descendant.path = "/wiki/recall-cache/shared-topic".to_string();
    cache_descendant.topic = "shared-topic".to_string();
    upsert(&mut conn, &cache_descendant, false).unwrap();

    let mut metadata_log = make_entry("wiki-metadata-log", "same internal text");
    metadata_log.path = "/wiki/general/metadata-log".to_string();
    metadata_log.topic = "shared-topic".to_string();
    metadata_log.metadata = json!({"wiki_log": 1});
    upsert(&mut conn, &metadata_log, false).unwrap();
    conn.execute(
        "UPDATE memories SET metadata = json_set(metadata, '$.wiki_log', 1) WHERE id = 'wiki-metadata-log'",
        [],
    )
    .unwrap();

    let mut source_cache = make_entry("wiki-source-cache", "same internal text");
    source_cache.path = "/wiki/general/source-cache".to_string();
    source_cache.topic = "shared-topic".to_string();
    source_cache.source = "foundry_recall_rerank_cache".to_string();
    upsert(&mut conn, &source_cache, false).unwrap();

    assert!(crate::db::is_reserved_wiki_internal_path(
        "/wiki/recall-cache"
    ));
    assert!(crate::db::is_reserved_wiki_internal_path(
        "/wiki/engineering/debugging/recall-cache/polluted"
    ));
    assert!(!crate::db::is_reserved_wiki_internal_path(
        "/wiki/engineering/recall-cacheable"
    ));
    assert!(
        !crate::db::is_user_facing_wiki_entry(&metadata_log),
        "Rust predicate must treat JSON numeric wiki_log=1 as internal"
    );
    assert!(find_active_wiki_entry_by_path(&conn, "/wiki/_log")
        .unwrap()
        .is_none());
    assert!(find_active_wiki_entry_by_path(&conn, "/wiki/recall-cache")
        .unwrap()
        .is_none());
    assert!(
        find_active_wiki_entry_by_path(&conn, "/wiki/general/metadata-log")
            .unwrap()
            .is_none()
    );
    assert!(list_wiki_duplicate_candidates(
        &conn,
        "/wiki/general/shared-topic",
        "shared-topic",
        "/wiki/general",
        None,
    )
    .unwrap()
    .is_empty());
}

#[test]
fn user_facing_wiki_list_filters_internal_rows_before_limit() {
    let mut conn = make_conn();
    for suffix in ["a", "b", "c"] {
        let mut log = make_entry(&format!("wiki-log-{suffix}"), "internal Wiki log");
        log.path = format!("/wiki/a-internal-{suffix}");
        upsert(&mut conn, &log, false).unwrap();
        conn.execute(
            "UPDATE memories SET metadata = json_set(metadata, '$.wiki_log', 1) WHERE id = ?1",
            [&log.id],
        )
        .unwrap();
    }
    let mut real = make_entry("wiki-real", "durable user-facing knowledge");
    real.path = "/wiki/agent/real".to_string();
    upsert(&mut conn, &real, false).unwrap();
    let listed = list_user_facing_wiki_entries(&conn, "/wiki", 1, false).unwrap();
    assert_eq!(listed[0].id, "wiki-real");
}

#[test]
fn user_facing_wiki_predicate_matches_recall_cache_and_log_variants() {
    let mut conn = make_conn();
    let mut topic_cache = make_entry("topic-cache", "cache");
    topic_cache.path = "/wiki/general/cache".to_string();
    topic_cache.topic = "recall_rerank_cache".to_string();
    assert!(!crate::db::is_user_facing_wiki_entry(&topic_cache));
    upsert(&mut conn, &topic_cache, false).unwrap();

    let mut id_cache = make_entry("foundry:recall-cache:query", "cache");
    id_cache.path = "/wiki/general/cache-by-id".to_string();
    assert!(!crate::db::is_user_facing_wiki_entry(&id_cache));
    upsert(&mut conn, &id_cache, false).unwrap();

    let mut topic_log = make_entry("topic-log", "log");
    topic_log.path = "/wiki/general/log".to_string();
    topic_log.topic = "WIKI_LOG".to_string();
    assert!(!crate::db::is_user_facing_wiki_entry(&topic_log));
    let mut stored_topic_log = topic_log.clone();
    stored_topic_log.topic = "ordinary".to_string();
    upsert(&mut conn, &stored_topic_log, false).unwrap();
    conn.execute(
        "UPDATE memories SET topic = 'WIKI_LOG' WHERE id = 'topic-log'",
        [],
    )
    .unwrap();

    let ids = list_user_facing_wiki_entries(&conn, "/wiki", 10, false)
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    assert!(ids.is_empty(), "SQL and Rust classifiers must agree");
}

#[test]
fn user_facing_wiki_projection_excludes_rem_operations_even_with_lifecycle_all() {
    let mut conn = make_conn();
    let mut rem = make_entry("wiki-rem:internal-operation", "internal REM operation");
    rem.path = "/wiki/drafts/internal-operation".to_string();
    rem.topic = "draft-topic".to_string();
    insert_if_absent(&mut conn, &rem, false).unwrap();

    assert!(!crate::db::is_user_facing_wiki_entry(&rem));
    let listed = list_user_facing_wiki_entries(&conn, "/wiki", 10, true).unwrap();
    assert!(
        listed.is_empty(),
        "lifecycle=all must not expose reserved REM operation rows"
    );
}

#[test]
fn ordinary_draft_projection_can_update_non_rem_drafts_only() {
    let mut conn = make_conn();
    let mut rem = make_entry("wiki-rem:reserved", "reserved rem draft");
    rem.path = "/wiki/drafts/reserved".to_string();
    rem.topic = "draft-topic".to_string();
    insert_if_absent(&mut conn, &rem, false).unwrap();
    let mut ordinary = make_entry("ordinary-draft", "ordinary draft");
    ordinary.path = "/wiki/drafts/ordinary".to_string();
    ordinary.topic = "draft-topic".to_string();
    upsert(&mut conn, &ordinary, false).unwrap();

    let winner = find_active_wiki_entry_by_path(&conn, "/wiki/drafts/ordinary")
        .unwrap()
        .expect("ordinary draft winner");
    assert_eq!(winner.id, "ordinary-draft");
}

#[test]
fn archive_memory_marks_archived() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("arch-1", "to be archived"), false).unwrap();

    let ok = archive_memory(&conn, "arch-1").unwrap();
    assert!(ok, "archive should return true for existing entry");

    let archived_flag: bool = conn
        .query_row(
            "SELECT archived FROM memories WHERE id = 'arch-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(archived_flag, "entry should be archived");

    let not_found = archive_memory(&conn, "nonexistent").unwrap();
    assert!(!not_found, "archive should return false for nonexistent");
}

#[test]
fn archive_and_restore_revision_cas() {
    let mut conn = make_conn();
    upsert(
        &mut conn,
        &make_entry("arch-cas", "unchanged specimen"),
        false,
    )
    .unwrap();
    let revision: i64 = conn
        .query_row(
            "SELECT revision FROM memories WHERE id='arch-cas'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!archive_memory_if_revision(&conn, "arch-cas", revision + 1).unwrap());
    assert!(archive_memory_if_revision(&conn, "arch-cas", revision).unwrap());
    assert!(!restore_archived_if_revision(&conn, "arch-cas", revision).unwrap());
    assert!(restore_archived_if_revision(&conn, "arch-cas", revision + 1).unwrap());
    let (archived, text, restored_revision): (bool, String, i64) = conn
        .query_row(
            "SELECT archived, text, revision FROM memories WHERE id='arch-cas'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert!(!archived);
    assert_eq!(text, "unchanged specimen");
    assert_eq!(restored_revision, revision + 2);
}

#[test]
fn archive_excluded_from_default_fetch() {
    let mut conn = make_conn();
    upsert(
        &mut conn,
        &make_entry("arch-fetch-1", "visible before archive"),
        false,
    )
    .unwrap();
    archive_memory(&conn, "arch-fetch-1").unwrap();

    let active = get_all(&conn, 10, false).unwrap();
    assert!(
        !active.iter().any(|e| e.id == "arch-fetch-1"),
        "archived entry should not appear in default get_all"
    );

    let with_archived = get_all(&conn, 10, true).unwrap();
    assert!(
        with_archived.iter().any(|e| e.id == "arch-fetch-1"),
        "archived entry should appear when include_archived=true"
    );
}
