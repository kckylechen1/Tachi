use super::*;

#[test]
fn list_memories_by_path_prefix_returns_empty_for_no_rows() {
    let conn = make_conn();
    let rows = list_memories_by_path_prefix(&conn, "/kanban/%").expect("list by path prefix");
    assert!(rows.is_empty());
}

#[test]
fn list_memories_by_path_prefix_filters_by_like_pattern() {
    let mut conn = make_conn();
    let mut matching = make_entry("kanban-a", "kanban card");
    matching.path = "/kanban/cards/a".into();
    upsert(&mut conn, &matching, false).unwrap();

    let mut other = make_entry("other-a", "unrelated memory");
    other.path = "/wiki/other".into();
    upsert(&mut conn, &other, false).unwrap();

    let rows = list_memories_by_path_prefix(&conn, "/kanban/%").expect("list by path prefix");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "kanban-a");
    assert_eq!(rows[0].path, "/kanban/cards/a");
    // metadata comes back as raw JSON text, not pre-parsed — the caller owns
    // parsing (and its error message) exactly as before the SQL moved here.
    assert!(serde_json::from_str::<serde_json::Value>(&rows[0].metadata).is_ok());
}

#[test]
fn list_memories_by_category_and_path_prefix_returns_empty_for_no_rows() {
    let conn = make_conn();
    let rows = list_memories_by_category_and_path_prefix(&conn, "handoff", "/handoff/%")
        .expect("list by category+path prefix");
    assert!(rows.is_empty());
}

#[test]
fn list_memories_by_category_and_path_prefix_requires_both_predicates() {
    let mut conn = make_conn();

    let mut right_category_wrong_path = make_entry("h1", "handoff wrong path");
    right_category_wrong_path.category = "handoff".into();
    right_category_wrong_path.path = "/wiki/not-handoff".into();
    upsert(&mut conn, &right_category_wrong_path, false).unwrap();

    let mut right_path_wrong_category = make_entry("h2", "not handoff category");
    right_path_wrong_category.category = "fact".into();
    right_path_wrong_category.path = "/handoff/h2".into();
    upsert(&mut conn, &right_path_wrong_category, false).unwrap();

    let mut matches_both = make_entry("h3", "real handoff memo");
    matches_both.category = "handoff".into();
    matches_both.path = "/handoff/h3".into();
    matches_both.archived = true;
    upsert(&mut conn, &matches_both, false).unwrap();

    let rows = list_memories_by_category_and_path_prefix(&conn, "handoff", "/handoff/%")
        .expect("list by category+path prefix");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "h3");
    assert!(rows[0].archived, "archived column must round-trip as true");
}
