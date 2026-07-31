use super::*;
use crate::db::find_active_wiki_entry_by_path_or_topic;

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
fn ordinary_wiki_projection_does_not_select_rem_or_draft_rows() {
    let mut conn = make_conn();
    let mut rem = make_entry("wiki-rem:reserved", "same topic rem draft");
    rem.path = "/wiki/drafts/rem".to_string();
    rem.topic = "shared-topic".to_string();
    upsert(&mut conn, &rem, false).unwrap();
    let mut ordinary_draft = make_entry("ordinary-draft", "same topic ordinary draft");
    ordinary_draft.path = "/wiki/drafts/ordinary".to_string();
    ordinary_draft.topic = "shared-topic".to_string();
    upsert(&mut conn, &ordinary_draft, false).unwrap();

    assert!(find_active_wiki_entry_by_path_or_topic(
        &conn,
        "/wiki/general/shared-topic",
        "shared-topic"
    )
    .unwrap()
    .is_none());
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
fn ordinary_draft_projection_can_update_non_rem_drafts_only() {
    let mut conn = make_conn();
    let mut rem = make_entry("wiki-rem:reserved", "reserved rem draft");
    rem.path = "/wiki/drafts/reserved".to_string();
    rem.topic = "draft-topic".to_string();
    upsert(&mut conn, &rem, false).unwrap();
    let mut ordinary = make_entry("ordinary-draft", "ordinary draft");
    ordinary.path = "/wiki/drafts/ordinary".to_string();
    ordinary.topic = "draft-topic".to_string();
    upsert(&mut conn, &ordinary, false).unwrap();

    let winner =
        find_active_wiki_entry_by_path_or_topic(&conn, "/wiki/drafts/new-path", "draft-topic")
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
