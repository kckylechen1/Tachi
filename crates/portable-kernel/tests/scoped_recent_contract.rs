//! Exercise the public portable consumer seam, not a host-side SQL replacement.

use portable_kernel::store::scoped_read::{
    MetadataTextFilter, RecentMemoryQuery, TextMembership,
};
use portable_kernel::{MemoryEntry, MemoryStore};
use serde_json::{json, Value};

fn entry(id: &str, path: &str, timestamp: &str, metadata: Value) -> MemoryEntry {
    MemoryEntry {
        id: id.into(),
        path: path.into(),
        summary: format!("Summary for {id}"),
        text: format!("{id}: {}", "body ".repeat(410)),
        importance: 0.7,
        timestamp: timestamp.into(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".into(),
        topic: String::new(),
        keywords: vec![],
        persons: vec![],
        entities: vec![],
        location: String::new(),
        source: "manual".into(),
        scope: "general".into(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        vector: None,
        retention_policy: None,
        domain: None,
        metadata,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".into(),
    }
}

fn filter(field: &str, values: &[&str], missing: Option<&str>) -> MetadataTextFilter {
    MetadataTextFilter {
        field: field.into(),
        values: values.iter().map(|value| (*value).to_string()).collect(),
        membership: TextMembership::AnyOf,
        missing_value: missing.map(str::to_string),
    }
}

fn ids(entries: &[MemoryEntry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.id.as_str()).collect()
}

fn matches(entry: &MemoryEntry, filter: &MetadataTextFilter) -> bool {
    let value = entry
        .metadata
        .get(&filter.field)
        .and_then(Value::as_str)
        .or(filter.missing_value.as_deref());
    let member = value.is_some_and(|value| filter.values.iter().any(|item| item == value));
    match filter.membership {
        TextMembership::AnyOf => member,
        TextMembership::NoneOf => !member,
    }
}

#[test]
fn eligible_older_bodies_fill_limit_despite_newer_foreign_rows() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    let prefix = "/scratch/scoped_recent";
    for index in 0..96 {
        // Each population fails exactly one predicate, so omitting any filter
        // lets newer ineligible bodies consume the result budget.
        let metadata = match index % 3 {
            0 => json!({"agent": "foreign", "session": "wanted", "space": "public"}),
            1 => json!({"agent": "alpha", "session": "other", "space": "public"}),
            _ => json!({"agent": "beta", "session": "wanted", "space": "protected"}),
        };
        let id = format!("foreign-{index:03}");
        store
            .upsert(&entry(
                &id,
                &format!("{prefix}/{id}"),
                "2026-07-10T00:00:00Z",
                metadata,
            ))
            .unwrap();
    }
    for (id, agent) in [
        ("eligible-c", "alpha"),
        ("eligible-a", "beta"),
        ("eligible-b", "alpha"),
    ] {
        store
            .upsert(&entry(
                id,
                &format!("{prefix}/{id}"),
                "2026-07-09T00:00:00Z",
                json!({"agent": agent, "session": "wanted", "space": "public"}),
            ))
            .unwrap();
    }
    let mut query = RecentMemoryQuery::new(prefix, 2);
    query.filters = vec![
        filter("agent", &["alpha", "beta"], Some("default")),
        filter("session", &["wanted"], None),
        MetadataTextFilter {
            membership: TextMembership::NoneOf,
            ..filter("space", &["protected"], Some("default"))
        },
    ];
    let old_window = store.list_by_path_recent(prefix, 16, false).unwrap();
    assert_eq!(
        old_window
            .iter()
            .filter(|entry| query.filters.iter().all(|filter| matches(entry, filter)))
            .count(),
        0,
        "the old fixed over-fetch must discriminate the crowd-out regression"
    );
    let mut oracle = store.list_by_path_recent(prefix, 200, false).unwrap();
    oracle.retain(|entry| query.filters.iter().all(|filter| matches(entry, filter)));
    oracle.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| a.id.cmp(&b.id)));
    oracle.truncate(query.limit);
    let actual = store.list_recent_matching(&query).unwrap();
    assert_eq!(ids(&actual), ["eligible-a", "eligible-b"]);
    assert_eq!(
        serde_json::to_value(&actual).unwrap(),
        serde_json::to_value(&oracle).unwrap()
    );
}

#[test]
fn string_lookup_fallback_and_empty_sets_match_the_host_predicate() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    let prefix = "/scratch/scoped_metadata";
    for (id, metadata) in [
        ("absent", json!({})),
        ("null", json!({"agent": null})),
        ("number", json!({"agent": 42})),
        ("boolean", json!({"agent": true})),
        ("object", json!({"agent": {"name": "default"}})),
        ("default", json!({"agent": "default"})),
        ("empty", json!({"agent": ""})),
        ("other", json!({"agent": "other"})),
        ("case", json!({"agent": "DEFAULT"})),
    ] {
        store
            .upsert(&entry(
                id,
                &format!("{prefix}/{id}"),
                "2026-07-09T00:00:00Z",
                metadata,
            ))
            .unwrap();
    }
    let all = store.list_by_path_recent(prefix, 100, false).unwrap();
    for predicate in [
        filter("agent", &["default"], Some("default")),
        filter("agent", &["default"], None),
        filter("agent", &[], Some("default")),
        MetadataTextFilter {
            membership: TextMembership::NoneOf,
            ..filter("agent", &["default"], None)
        },
        MetadataTextFilter {
            membership: TextMembership::NoneOf,
            ..filter("agent", &[], Some("default"))
        },
    ] {
        let mut expected: Vec<_> = all
            .iter()
            .filter(|entry| matches(entry, &predicate))
            .collect();
        expected.sort_by(|a, b| a.id.cmp(&b.id));
        let mut query = RecentMemoryQuery::new(prefix, 100);
        query.filters.push(predicate);
        let actual = store.list_recent_matching(&query).unwrap();
        assert_eq!(
            ids(&actual),
            expected
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn path_subtree_and_metadata_values_are_literal_not_query_syntax() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    for (id, path, value) in [
        ("exact", "/scratch/agent_one", "x' OR 1=1 --"),
        ("child", "/scratch/agent_one/child", "x' OR 1=1 --"),
        ("wildcard-neighbor", "/scratch/agentxone/child", "x' OR 1=1 --"),
        ("sibling", "/scratch/agent_one_more/child", "x' OR 1=1 --"),
        ("foreign-value", "/scratch/agent_one/other", "other"),
    ] {
        store
            .upsert(&entry(
                id,
                path,
                "2026-07-09T00:00:00Z",
                json!({"key": value}),
            ))
            .unwrap();
    }
    let mut query = RecentMemoryQuery::new(" scratch/agent_one/ ", 10);
    query.filters.push(filter("key", &["x' OR 1=1 --"], None));
    let actual = store.list_recent_matching(&query).unwrap();
    assert_eq!(ids(&actual), ["child", "exact"]);
}

#[test]
fn inclusive_timestamp_bounds_ties_archive_and_zero_limit_are_explicit() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    let prefix = "/scratch/scoped_time";
    for (id, timestamp, archived) in [
        ("before", "2026-07-08T00:00:00Z", false),
        ("at-start", "2026-07-09T00:00:00Z", false),
        ("end-b", "2026-07-10T00:00:00Z", false),
        ("end-a", "2026-07-10T00:00:00Z", false),
        ("archived", "2026-07-10T00:00:00Z", true),
        ("after", "2026-07-11T00:00:00Z", false),
    ] {
        let mut row = entry(id, &format!("{prefix}/{id}"), timestamp, json!({}));
        row.archived = archived;
        store.upsert(&row).unwrap();
    }
    let mut query = RecentMemoryQuery::new(prefix, 100);
    query.since = Some("2026-07-09T00:00:00Z".into());
    query.until = Some("2026-07-10T00:00:00Z".into());
    assert_eq!(
        ids(&store.list_recent_matching(&query).unwrap()),
        ["end-a", "end-b", "at-start"]
    );
    query.limit = 1;
    assert_eq!(ids(&store.list_recent_matching(&query).unwrap()), ["end-a"]);
    query.limit = 0;
    assert!(store.list_recent_matching(&query).unwrap().is_empty());
    query.limit = 100;
    query.include_archived = true;
    assert_eq!(
        ids(&store.list_recent_matching(&query).unwrap()),
        ["archived", "end-a", "end-b", "at-start"]
    );
}

#[test]
fn recent_reads_leave_the_stored_record_and_access_counters_unchanged() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    store
        .upsert(&entry(
            "stable",
            "/scratch/scoped_readonly",
            "2026-07-09T00:00:00Z",
            json!({}),
        ))
        .unwrap();
    let before = serde_json::to_value(store.get("stable").unwrap()).unwrap();
    let query = RecentMemoryQuery::new("/scratch", 1);
    for _ in 0..3 {
        assert_eq!(ids(&store.list_recent_matching(&query).unwrap()), ["stable"]);
    }
    assert_eq!(
        serde_json::to_value(store.get("stable").unwrap()).unwrap(),
        before
    );
}

#[test]
fn invalid_or_excessive_filter_requests_are_refused_without_query_widening() {
    let store = MemoryStore::open_in_memory().unwrap();
    for field in ["nested.key", "x') OR 1=1 --", "", "quote\"key"] {
        let mut query = RecentMemoryQuery::new("/scratch", 10);
        query.filters.push(filter(field, &["one"], None));
        assert!(store.list_recent_matching(&query).is_err());
    }
    let mut query = RecentMemoryQuery::new("/scratch", 10);
    query.filters = (0..17)
        .map(|_| filter("agent", &["one"], None))
        .collect();
    assert!(store.list_recent_matching(&query).is_err());
    query.filters = vec![MetadataTextFilter {
        values: vec!["one".into(); 257],
        ..filter("agent", &[], None)
    }];
    assert!(store.list_recent_matching(&query).is_err());
}
