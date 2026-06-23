use super::*;
#[test]
fn coherence_bucket_key_prefers_topic_then_entity() {
    assert_eq!(
        coherence_bucket_key("strategy", &["alpha".to_string()]),
        Some("topic:strategy".to_string())
    );
    assert_eq!(
        coherence_bucket_key("Architecture", &["alpha".to_string()]),
        Some("entity:alpha".to_string())
    );
    assert_eq!(
        coherence_bucket_key("", &["alpha".to_string(), "beta".to_string()]),
        Some("entity:alpha".to_string())
    );
    assert_eq!(coherence_bucket_key("", &[]), None);
}
#[test]
fn scheduled_distill_path_prefix_keeps_second_level_project_namespace() {
    assert_eq!(
        scheduled_distill_path_prefix("/project/API_配额/m-1"),
        "/project/API_配额"
    );
    assert_eq!(
        scheduled_distill_path_prefix("/kanban/antigravity/codex"),
        "/kanban/antigravity/codex"
    );
    assert_eq!(
        scheduled_distill_path_prefix("/wiki/debug/tachi/hub-call"),
        "/wiki/debug/tachi"
    );
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

    let mut buckets = coherent_distill_buckets(entries);
    buckets.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(buckets.len(), 2);
    assert_eq!(buckets[0].0, "/hapi#topic:launch-signal");
    assert_eq!(buckets[0].1.len(), 3);
    assert_eq!(buckets[1].0, "/hapi#topic:risk-signal");
    assert_eq!(buckets[1].1.len(), 3);
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

    assert!(coherent_distill_buckets(entries).is_empty());
}
