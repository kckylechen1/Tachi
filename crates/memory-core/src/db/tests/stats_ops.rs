use super::*;

#[test]
fn stats_aggregation() {
    let mut conn = make_conn();

    let mut e1 = make_entry("s1", "fact entry");
    e1.scope = "general".into();
    e1.category = "fact".into();
    e1.path = "/project/alpha".into();
    upsert(&mut conn, &e1, false).unwrap();

    let mut e2 = make_entry("s2", "decision entry");
    e2.scope = "project".into();
    e2.category = "decision".into();
    e2.path = "/project/beta".into();
    upsert(&mut conn, &e2, false).unwrap();

    let mut e3 = make_entry("s3", "user preference");
    e3.scope = "user".into();
    e3.category = "preference".into();
    e3.path = "/user/settings".into();
    upsert(&mut conn, &e3, false).unwrap();

    let s = stats(&conn, false).unwrap();
    assert_eq!(s.total, 3);
    assert_eq!(s.by_scope.get("general"), Some(&1_u64));
    assert_eq!(s.by_scope.get("project"), Some(&1_u64));
    assert_eq!(s.by_scope.get("user"), Some(&1_u64));
    assert_eq!(s.by_category.get("fact"), Some(&1_u64));
    assert_eq!(s.by_category.get("decision"), Some(&1_u64));
    assert_eq!(s.by_root_path.get("/project"), Some(&2_u64));
    assert_eq!(s.by_root_path.get("/user"), Some(&1_u64));
}
