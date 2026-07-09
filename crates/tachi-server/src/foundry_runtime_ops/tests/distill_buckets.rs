use super::*;
fn bucket_entry(id: &str, path: &str, topic: &str, entities: Vec<String>) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: path.to_string(),
        summary: topic.to_string(),
        text: topic.to_string(),
        importance: 0.5,
        timestamp: "2026-04-23T00:00:00Z".to_string(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: topic.to_string(),
        keywords: vec![],
        persons: vec![],
        entities,
        location: "".to_string(),
        source: "manual".to_string(),
        scope: "project".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({}),
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

#[test]
fn coherence_bucket_key_prefers_topic_then_entity() {
    let topic_bucket = collect_coherent_distill_buckets(vec![
        bucket_entry(
            "topic-1",
            "/project/a/1",
            "strategy",
            vec!["alpha".to_string()],
        ),
        bucket_entry(
            "topic-2",
            "/project/a/2",
            "strategy",
            vec!["alpha".to_string()],
        ),
        bucket_entry(
            "topic-3",
            "/project/a/3",
            "strategy",
            vec!["alpha".to_string()],
        ),
    ]);
    assert_eq!(topic_bucket[0].coherence_key, "topic:strategy");

    let entity_bucket = collect_coherent_distill_buckets(vec![
        bucket_entry(
            "entity-1",
            "/project/a/1",
            "Architecture",
            vec!["alpha".to_string()],
        ),
        bucket_entry(
            "entity-2",
            "/project/a/2",
            "Architecture",
            vec!["alpha".to_string()],
        ),
        bucket_entry(
            "entity-3",
            "/project/a/3",
            "Architecture",
            vec!["alpha".to_string()],
        ),
    ]);
    assert_eq!(entity_bucket[0].coherence_key, "entity:alpha");

    assert!(collect_coherent_distill_buckets(vec![
        bucket_entry("none-1", "/project/a/1", "", vec![]),
        bucket_entry("none-2", "/project/a/2", "", vec![]),
        bucket_entry("none-3", "/project/a/3", "", vec![]),
    ])
    .is_empty());
}
#[test]
fn scheduled_distill_path_prefix_keeps_second_level_project_namespace() {
    let buckets = collect_coherent_distill_buckets(vec![
        bucket_entry("project-1", "/project/API_配额/m-1", "strategy", vec![]),
        bucket_entry("project-2", "/project/API_配额/m-2", "strategy", vec![]),
        bucket_entry("project-3", "/project/API_配额/m-3", "strategy", vec![]),
        bucket_entry("kanban-1", "/kanban/antigravity/codex/m-1", "risk", vec![]),
        bucket_entry("kanban-2", "/kanban/antigravity/codex/m-2", "risk", vec![]),
        bucket_entry("kanban-3", "/kanban/antigravity/codex/m-3", "risk", vec![]),
        bucket_entry("wiki-1", "/wiki/debug/tachi/hub-call", "lesson", vec![]),
        bucket_entry("wiki-2", "/wiki/debug/tachi/status", "lesson", vec![]),
        bucket_entry("wiki-3", "/wiki/debug/tachi/ask", "lesson", vec![]),
    ]);
    let prefixes = buckets
        .into_iter()
        .map(|bucket| bucket.path_prefix)
        .collect::<std::collections::HashSet<_>>();

    assert!(prefixes.contains("/project/API_配额"));
    assert!(prefixes.contains("/kanban/antigravity/codex"));
    assert!(prefixes.contains("/wiki/debug/tachi"));
}
#[test]
fn coherent_distill_buckets_keep_unrelated_topics_apart() {
    let mut entries = Vec::new();
    for (idx, topic) in [
        "launch-signal",
        "launch-signal",
        "launch-signal",
        "risk-signal",
        "risk-signal",
        "risk-signal",
    ]
    .into_iter()
    .enumerate()
    {
        entries.push(MemoryEntry {
            id: format!("m-{idx}"),
            path: format!("/hapi/{topic}/m-{idx}"),
            summary: topic.to_string(),
            text: topic.to_string(),
            importance: 0.5,
            timestamp: format!("2026-04-23T00:00:0{idx}Z"),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: topic.to_string(),
            keywords: vec![],
            persons: vec![],
            entities: vec![format!("entity-{topic}")],
            location: "".to_string(),
            source: "manual".to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        });
    }

    let mut buckets = collect_coherent_distill_buckets(entries);
    buckets.sort_by(|a, b| a.bucket_key.cmp(&b.bucket_key));

    assert_eq!(buckets.len(), 2);
    assert_eq!(buckets[0].bucket_key, "/hapi#topic:launch-signal");
    assert_eq!(buckets[0].entries.len(), 3);
    assert_eq!(buckets[1].bucket_key, "/hapi#topic:risk-signal");
    assert_eq!(buckets[1].entries.len(), 3);
}
#[test]
fn coherent_distill_buckets_drop_generic_topics_without_shared_entity() {
    let entries = [
        ("api", "/project/API_配额", "Architecture", "quota"),
        ("bug", "/project/Bug_Fix", "Architecture", "qwen"),
        ("dex", "/project/Dexter_Stability", "Architecture", "dexter"),
    ]
    .into_iter()
    .enumerate()
    .map(|(idx, (text, path, topic, entity))| MemoryEntry {
        id: format!("generic-{idx}"),
        path: path.to_string(),
        summary: text.to_string(),
        text: text.to_string(),
        importance: 0.5,
        timestamp: format!("2026-04-23T00:01:0{idx}Z"),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: topic.to_string(),
        keywords: vec![],
        persons: vec![],
        entities: vec![entity.to_string()],
        location: "".to_string(),
        source: "manual".to_string(),
        scope: "project".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({}),
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    })
    .collect::<Vec<_>>();

    assert!(collect_coherent_distill_buckets(entries).is_empty());
}
