use super::handlers::{
    build_bracket_self_evolution_id, classify_bracket_self_evolution,
    extract_bracket_self_evolution_notes,
};
use super::maintenance::{
    build_distill_edges, classify_distill_guide_type, coherence_bucket_key,
    coherent_distill_buckets, memory_claim_signature, scheduled_distill_group_key,
    scheduled_distill_path_prefix,
};
use super::recall::{parse_compact_context_response, parse_session_capture_response};
use super::*;

#[test]
fn parse_session_capture_response_accepts_json_array() {
    let raw = r#"[{"text": "hello"}, {"text": "world"}]"#;
    let drafts = parse_session_capture_response(raw).unwrap();
    assert_eq!(drafts.len(), 2);
    assert_eq!(drafts[0].text, "hello");
    assert_eq!(drafts[1].text, "world");
}

#[test]
fn parse_session_capture_response_strips_code_fence() {
    let raw = "```json\n[{\"text\": \"hello\"}]\n```";
    let drafts = parse_session_capture_response(raw).unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].text, "hello");
}

#[test]
fn parse_session_capture_response_ignores_reasoning_prefix() {
    let raw = "<think>reasoning that should not be parsed</think>\n[{\"text\": \"hello\"}]";
    let drafts = parse_session_capture_response(raw).unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].text, "hello");
}

#[test]
fn parse_compact_context_response_ignores_reasoning_prefix() {
    let raw =
        "<think>not json</think>\n{\"compacted_text\":\"summary\",\"salient_topics\":[\"tachi\"]}";
    let draft = parse_compact_context_response(raw).unwrap();
    assert_eq!(draft.compacted_text, "summary");
    assert_eq!(draft.salient_topics, vec!["tachi"]);
}

#[test]
fn capture_session_params_accepts_string_messages() {
    let params: CaptureSessionParams = serde_json::from_value(json!({
        "conversation_id": "c",
        "turn_id": "t",
        "agent_id": "codex-smoke",
        "messages": ["raw user message"]
    }))
    .unwrap();

    assert_eq!(params.messages.len(), 1);
    assert_eq!(params.messages[0].role, "user");
    assert_eq!(params.messages[0].content, "raw user message");
}

#[test]
fn compact_rollup_params_accepts_string_items() {
    let params: CompactRollupParams = serde_json::from_value(json!({
        "agent_id": "codex-smoke",
        "conversation_id": "c",
        "rollup_id": "r",
        "items": ["already compact text"]
    }))
    .unwrap();

    assert_eq!(params.items.len(), 1);
    assert_eq!(params.items[0].compacted_text, "already compact text");
}

#[test]
fn parse_session_capture_response_filters_empty_text() {
    let raw = r#"[{"text": ""}, {"text": "valid"}]"#;
    let drafts = parse_session_capture_response(raw).unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].text, "valid");
}

#[test]
fn extract_bracket_self_evolution_notes_filters_and_dedups() {
    let messages = vec![
        Message {
            role: "assistant".to_string(),
            content: "你好（普通感想）还有（原来他喜欢这种直球，记住了，下次我要先夸再问）"
                .to_string(),
        },
        Message {
            role: "assistant".to_string(),
            content: "(原来他喜欢这种直球，记住了，下次我要先夸再问)".to_string(),
        },
        Message {
            role: "user".to_string(),
            content: "（记住了，下次我要这样）".to_string(),
        },
    ];

    let notes = extract_bracket_self_evolution_notes("jayne-main", &messages);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].category, "decision");
    assert_eq!(
        notes[0].text,
        "原来他喜欢这种直球，记住了，下次我要先夸再问"
    );
}

#[test]
fn classify_bracket_self_evolution_respects_priority() {
    assert_eq!(
        classify_bracket_self_evolution("原来他不喜欢被追问，这是雷区"),
        "preference"
    );
    assert_eq!(
        classify_bracket_self_evolution("这样更有效，下次我会先顺着他"),
        "decision"
    );
    assert_eq!(
        classify_bracket_self_evolution("这种方式有用，但刚才策略失败了"),
        "experience"
    );
}

#[test]
fn build_bracket_self_evolution_id_is_stable() {
    let first = build_bracket_self_evolution_id("jayne-main", "记住了，下次我要先夸再问");
    let second = build_bracket_self_evolution_id("jayne-main", "记住了，下次我要先夸再问");
    let third = build_bracket_self_evolution_id("other-agent", "记住了，下次我要先夸再问");

    assert_eq!(first, second);
    assert_ne!(first, third);
}

#[test]
fn memory_claim_signature_changes_on_revision() {
    let entry = MemoryEntry {
        id: "test".to_string(),
        path: "/test".to_string(),
        summary: "test".to_string(),
        text: "test".to_string(),
        importance: 0.5,
        timestamp: "2026-01-01T00:00:00Z".to_string(),
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
    };

    let before = memory_claim_signature(&entry);
    entry.vector = Some(vec![0.1, 0.2]);
    let after = memory_claim_signature(&entry);
    assert_ne!(before, after);
}

#[test]
fn forget_sweep_keeps_newest_distill_entries() {
    let mut entries = vec![
        MemoryEntry {
            id: "old".to_string(),
            path: "/foundry/agents/main/distilled/20260402T000000".to_string(),
            summary: "old".to_string(),
            text: "old".to_string(),
            importance: 0.7,
            timestamp: "2026-04-02T00:00:00Z".to_string(),
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
        },
        MemoryEntry {
            id: "new".to_string(),
            path: "/foundry/agents/main/distilled/20260402T010000".to_string(),
            summary: "new".to_string(),
            text: "new".to_string(),
            importance: 0.7,
            timestamp: "2026-04-02T01:00:00Z".to_string(),
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
fn scheduled_distill_group_key_separates_topics_with_same_root() {
    assert_eq!(
        scheduled_distill_group_key("/hapi/changelog/entry-1", "topic:changelog"),
        "/hapi#topic:changelog"
    );
    assert_eq!(
        scheduled_distill_group_key("/project/API_配额/entry-1", "topic:quota"),
        "/project/API_配额#topic:quota"
    );
    assert_ne!(
        scheduled_distill_group_key("/hapi/changelog/entry-1", "topic:changelog"),
        scheduled_distill_group_key("/hapi/strategy/entry-1", "topic:strategy")
    );
    assert_ne!(
        scheduled_distill_group_key("/hapi/changelog/entry-1", "topic:changelog"),
        scheduled_distill_group_key("/hapi/changelog/entry-2", "topic:release")
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
    })
    .collect::<Vec<_>>();

    assert!(coherent_distill_buckets(entries).is_empty());
}

fn distill_source_entry(id: &str, text: &str, category: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: "/project/tachi/crates/memory-server/src/tools.rs".to_string(),
        summary: text.chars().take(40).collect(),
        text: text.to_string(),
        importance: 0.7,
        timestamp: "2026-01-01T00:00:00Z".to_string(),
        category: category.to_string(),
        topic: "guide-layer".to_string(),
        keywords: vec!["tachi".to_string()],
        persons: vec![],
        entities: vec!["Tachi".to_string()],
        location: "crates/memory-server/src/tools.rs".to_string(),
        source: "manual".to_string(),
        scope: "project".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({
            "file_path": "crates/memory-server/src/tools.rs"
        }),
        vector: None,
        retention_policy: None,
        domain: None,
    }
}

#[test]
fn distill_guide_classifier_emits_supported_guide_types() {
    let source = vec![distill_source_entry("src", "context", "fact")];
    assert_eq!(
        classify_distill_guide_type("Must not add a normal guide write tool.", &source),
        "constraint"
    );
    assert_eq!(
        classify_distill_guide_type("Fix linker error by rebuilding sqlite vec.", &source),
        "fix_pattern"
    );
    assert_eq!(
        classify_distill_guide_type(
            "Decision: choose metadata fields over schema changes.",
            &source
        ),
        "decision"
    );
    assert_eq!(
        classify_distill_guide_type("Runbook: 1. Inspect logs\n2. Re-run cargo test.", &source),
        "runbook"
    );
}

#[test]
fn distill_edges_include_causal_guide_relations() {
    let sources = vec![distill_source_entry(
        "source-1",
        "error: linker failed for sqlite vec",
        "fact",
    )];
    let guide = MemoryEntry {
        id: "guide-1".to_string(),
        path: "/guide/fix_pattern/codex/20260101T000000".to_string(),
        summary: "Fix linker errors".to_string(),
        text: "Fix linker error by rebuilding sqlite vec; avoid deleting migrations.".to_string(),
        importance: 0.75,
        timestamp: "2026-01-01T00:00:00Z".to_string(),
        category: "guide".to_string(),
        topic: "fix_pattern".to_string(),
        keywords: vec!["guide".to_string(), "fix_pattern".to_string()],
        persons: vec![],
        entities: vec!["Tachi".to_string()],
        location: "/project/tachi".to_string(),
        source: FOUNDRY_DISTILL_SOURCE.to_string(),
        scope: "project".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({
            "guide": true,
            "guide_type": "fix_pattern",
        }),
        vector: None,
        retention_policy: None,
        domain: None,
    };

    let relations = build_distill_edges(&guide, &sources, "fix_pattern", &guide.timestamp)
        .into_iter()
        .map(|edge| edge.relation)
        .collect::<std::collections::HashSet<_>>();
    assert!(relations.contains("distilled_from"));
    assert!(relations.contains("fixed_by"));
    assert!(relations.contains("rejected_because"));

    let constraint_relations =
        build_distill_edges(&guide, &sources, "constraint", &guide.timestamp)
            .into_iter()
            .map(|edge| edge.relation)
            .collect::<std::collections::HashSet<_>>();
    assert!(constraint_relations.contains("causes"));
}
