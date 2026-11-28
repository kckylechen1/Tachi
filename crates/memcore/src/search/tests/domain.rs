use super::*;

fn scoped_entry(id: &str, text: &str, domain: &str, timestamp: &str) -> MemoryEntry {
    let mut entry = memory_entry(id, text, &["scoped"]);
    entry.domain = Some(domain.to_string());
    entry.timestamp = timestamp.to_string();
    entry.valid_from = timestamp.to_string();
    entry
}

#[test]
fn fts_budget_is_spent_only_on_matching_domain_rows() {
    let mut conn = setup();
    let target = scoped_entry(
        "domain-target",
        "needle in the requested domain",
        "wanted",
        "2026-01-01T00:00:00Z",
    );
    insert_entry(&mut conn, target);
    for index in 0..16 {
        insert_entry(
            &mut conn,
            scoped_entry(
                &format!("domain-distractor-{index}"),
                "needle needle needle needle in another domain",
                "other",
                "2026-02-01T00:00:00Z",
            ),
        );
    }

    let hits = crate::db::search_fts_with_normalized_as_of_and_domain(
        &conn,
        "needle",
        true,
        1,
        false,
        false,
        None,
        None,
        None,
        false,
        Some("wanted"),
    )
    .expect("domain-scoped FTS");
    assert_eq!(hits.keys().collect::<Vec<_>>(), ["domain-target"]);
}

#[test]
fn symbolic_budget_is_spent_only_on_matching_domain_rows() {
    let mut conn = setup();
    insert_entry(
        &mut conn,
        scoped_entry(
            "domain-target",
            "scopedneedle",
            "wanted",
            "2026-01-01T00:00:00Z",
        ),
    );
    for index in 0..16 {
        insert_entry(
            &mut conn,
            scoped_entry(
                &format!("domain-distractor-{index}"),
                "scopedneedle scopedneedle scopedneedle scopedneedle",
                "other",
                "2026-02-01T00:00:00Z",
            ),
        );
    }

    let hits = crate::db::search_symbolic_candidates_with_relevance_and_domain(
        &conn,
        "scopedneedle",
        "scopedneedle",
        1,
        false,
        false,
        None,
        None,
        None,
        false,
        true,
        Some("wanted"),
    )
    .expect("domain-scoped symbolic search");
    assert_eq!(hits, ["domain-target"]);
}

#[test]
fn vector_budget_uses_exact_distance_within_the_requested_domain() {
    let mut conn = setup();
    let mut target = scoped_entry(
        "domain-target",
        "vector target",
        "wanted",
        "2026-01-01T00:00:00Z",
    );
    let mut target_vector = vec![0.0_f32; 1024];
    target_vector[0] = 0.9;
    target_vector[1] = 0.1;
    target.vector = Some(target_vector);
    upsert(&mut conn, &target, true).unwrap();
    for index in 0..16 {
        let mut distractor = scoped_entry(
            &format!("domain-distractor-{index}"),
            "vector distractor",
            "other",
            "2026-02-01T00:00:00Z",
        );
        let mut vector = vec![0.0_f32; 1024];
        vector[0] = 1.0;
        distractor.vector = Some(vector);
        upsert(&mut conn, &distractor, true).unwrap();
    }
    let mut zero = scoped_entry(
        "domain-zero-vector",
        "zero vector",
        "wanted",
        "2026-01-02T00:00:00Z",
    );
    zero.vector = Some(vec![0.0_f32; 1024]);
    upsert(&mut conn, &zero, true).unwrap();
    let mut query = vec![0.0_f32; 1024];
    query[0] = 1.0;

    let hits = crate::db::search_vec_with_domain(
        &conn,
        &query,
        1,
        false,
        false,
        None,
        None,
        None,
        false,
        Some("wanted"),
    )
    .expect("domain-scoped exact vector search");
    assert_eq!(hits.keys().collect::<Vec<_>>(), ["domain-target"]);

    let empty = crate::db::search_vec_with_domain(
        &conn,
        &vec![0.0_f32; 1024],
        4,
        false,
        false,
        None,
        None,
        None,
        false,
        Some("wanted"),
    )
    .expect("zero query has defined L2 distances");
    assert!(empty.contains_key("domain-zero-vector"));
}

#[test]
fn domain_filter_does_not_change_vector_metric_or_winner() {
    let mut conn = setup();
    for (id, first, second) in [("a", 2.0_f32, 0.0_f32), ("b", 1.0_f32, 0.1_f32)] {
        let mut entry = scoped_entry(id, "same domain", "wanted", "2026-01-01T00:00:00Z");
        let mut vector = vec![0.0_f32; 1024];
        vector[0] = first;
        vector[1] = second;
        entry.vector = Some(vector);
        upsert(&mut conn, &entry, true).unwrap();
    }
    let mut query = vec![0.0_f32; 1024];
    query[0] = 1.0;
    let unscoped = crate::db::search_vec(&conn, &query, 1, false, false, None, None, None, false)
        .expect("unscoped L2");
    let scoped = crate::db::search_vec_with_domain(
        &conn,
        &query,
        1,
        false,
        false,
        None,
        None,
        None,
        false,
        Some("wanted"),
    )
    .expect("domain-scoped L2");
    assert_eq!(unscoped.keys().collect::<Vec<_>>(), ["b"]);
    assert_eq!(
        scoped.keys().collect::<Vec<_>>(),
        unscoped.keys().collect::<Vec<_>>()
    );
}
