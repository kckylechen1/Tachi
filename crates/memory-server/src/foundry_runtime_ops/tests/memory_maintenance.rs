use super::*;
#[test]
fn memory_claim_signature_changes_on_revision() {
    let entry = MemoryEntry {
        id: "test".to_string(),
        path: "/test".to_string(),
        summary: "test".to_string(),
        text: "test".to_string(),
        importance: 0.5,
        timestamp: "2026-01-01T00:00:00Z".to_string(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: "".to_string(),
        keywords: vec![],
        persons: vec![],
        entities: vec![],
        location: "".to_string(),
        source: "test".to_string(),
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
    };

    let before = memory_claim_signature(&entry);
    let mut entry2 = entry.clone();
    entry2.revision = 2;
    let after = memory_claim_signature(&entry2);
    assert_ne!(before, after);
}
#[test]
fn memory_claim_signature_changes_on_vector() {
    let mut entry = MemoryEntry {
        id: "test".to_string(),
        path: "/test".to_string(),
        summary: "test".to_string(),
        text: "test".to_string(),
        importance: 0.5,
        timestamp: "2026-01-01T00:00:00Z".to_string(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: "".to_string(),
        keywords: vec![],
        persons: vec![],
        entities: vec![],
        location: "".to_string(),
        source: "test".to_string(),
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
    };

    let before = memory_claim_signature(&entry);
    entry.vector = Some(vec![0.1, 0.2]);
    let after = memory_claim_signature(&entry);
    assert_ne!(before, after);
}
fn test_memory_entry(id: &str, topic: &str, importance: f64, access_count: i64) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: "/test".to_string(),
        summary: "test".to_string(),
        text: "test".to_string(),
        importance,
        timestamp: "2026-01-01T00:00:00Z".to_string(),
        category: "fact".to_string(),
        topic: topic.to_string(),
        keywords: vec![],
        persons: vec![],
        entities: vec![],
        location: "".to_string(),
        source: "test".to_string(),
        scope: "project".to_string(),
        archived: false,
        access_count,
        last_access: None,
        revision: 1,
        metadata: json!({}),
        vector: None,
        retention_policy: None,
        domain: None,
        valid_from: String::new(),
        valid_until: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}
#[test]
fn infer_memory_insight_marks_surprising_memory_high_priority() {
    let entry = test_memory_entry("insight-high", "rare-topic", 0.95, 0);
    let insight = infer_memory_insight(
        &entry,
        0.35,
        2,
        1,
        FOUNDRY_RELATED_LIMIT,
        FOUNDRY_RELATED_LIMIT,
    );

    assert_eq!(insight["kind"], json!("memory_insight"));
    assert_eq!(insight["priority"], json!("high"));
    assert!(insight["surprise"].as_f64().unwrap() >= 0.4);
    assert!(insight["reasons"]
        .as_array()
        .unwrap()
        .contains(&json!("contradiction")));
    assert!(insight["reasons"]
        .as_array()
        .unwrap()
        .contains(&json!("novel_topic")));
    assert!(insight["reasons"]
        .as_array()
        .unwrap()
        .contains(&json!("overlooked_high_importance")));
    assert!(insight["reasons"]
        .as_array()
        .unwrap()
        .contains(&json!("dense_neighborhood")));
}
#[test]
fn infer_memory_insight_keeps_routine_memory_low_priority() {
    let entry = test_memory_entry("insight-low", "common-topic", 0.5, 3);
    let insight = infer_memory_insight(&entry, 0.5, 0, 8, 1, FOUNDRY_RELATED_LIMIT);

    assert_eq!(insight["priority"], json!("low"));
    assert!(insight["surprise"].as_f64().unwrap() < 0.2);
    assert!(insight["reasons"].as_array().unwrap().is_empty());
}
#[test]
fn forget_sweep_keeps_newest_distill_entries() {
    let mut entries = [
        MemoryEntry {
            id: "old".to_string(),
            path: "/foundry/agents/main/distilled/20260402T000000".to_string(),
            summary: "old".to_string(),
            text: "old".to_string(),
            importance: 0.7,
            timestamp: "2026-04-02T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "other".to_string(),
            topic: "foundry_distill".to_string(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: "".to_string(),
            source: FOUNDRY_DISTILL_SOURCE.to_string(),
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
        },
        MemoryEntry {
            id: "new".to_string(),
            path: "/foundry/agents/main/distilled/20260402T010000".to_string(),
            summary: "new".to_string(),
            text: "new".to_string(),
            importance: 0.7,
            timestamp: "2026-04-02T01:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "other".to_string(),
            topic: "foundry_distill".to_string(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: "".to_string(),
            source: FOUNDRY_DISTILL_SOURCE.to_string(),
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
        },
    ];
    entries.sort_by(|a, b| {
        b.timestamp
            .cmp(&a.timestamp)
            .then_with(|| b.path.cmp(&a.path))
            .then_with(|| b.id.cmp(&a.id))
    });

    assert_eq!(entries[0].id, "new");
    assert_eq!(entries[1].id, "old");
}
