use super::handlers::{
    build_bracket_self_evolution_id, classify_bracket_self_evolution,
    extract_bracket_self_evolution_notes, matches_agent_tag, resolve_capture_target,
};
use super::maintenance::{
    build_distill_edges, classify_distill_guide_type, coherence_bucket_key,
    coherent_distill_buckets, infer_memory_insight, memory_claim_signature,
    scheduled_distill_group_key, scheduled_distill_path_prefix,
};
use super::recall::{parse_compact_context_response, parse_session_capture_response};
use super::*;
use crate::manifest::{DbEntry, DbRole, Manifest};
use tempfile::tempdir;

fn tachi_home_test_lock() -> &'static std::sync::Mutex<()> {
    crate::utils::global_test_lock()
}

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

#[tokio::test]
async fn resolve_capture_target_prefers_explicit_project() {
    let tmp = tempdir().expect("tempdir");
    let server = crate::MemoryServer::new(tmp.path().join("global.db"), None).expect("server");

    let (target_db, named_project, db_path, warning) =
        resolve_capture_target(&server, "global", Some("wiki"), "main");

    assert_eq!(target_db, DbScope::Project);
    assert_eq!(named_project.as_deref(), Some("wiki"));
    assert!(db_path.is_none());
    assert!(warning.is_none());
}

#[tokio::test]
async fn resolve_capture_target_prefers_manifest_agent_db() {
    let _guard = tachi_home_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp_home = tempdir().expect("temp home");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("TACHI_HOME", temp_home.path());

    let manifest_path = temp_home.path().join("manifest.json");
    let agent_db = temp_home
        .path()
        .join(".openclaw/extensions/tachi/data/agents/main/memory.db");
    let manifest = Manifest {
        schema_version: 1,
        generated_at: chrono::Utc::now().to_rfc3339(),
        comment: String::new(),
        dbs: vec![DbEntry {
            path: agent_db.to_string_lossy().into_owned(),
            role: DbRole::Agent,
            owner: "openclaw-agent:main".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: chrono::Utc::now().to_rfc3339(),
            last_classification: "healthy".to_string(),
            scope_hint: "agent:main".to_string(),
            notes: String::new(),
        }],
    };
    manifest.save(&manifest_path).expect("save manifest");

    let server =
        crate::MemoryServer::new(temp_home.path().join("global.db"), None).expect("server");
    let (target_db, named_project, db_path, warning) =
        resolve_capture_target(&server, "global", None, "main");

    assert_eq!(target_db, DbScope::Project);
    assert!(named_project.is_none());
    assert_eq!(db_path.as_deref(), Some(agent_db.as_path()));
    assert_eq!(
        warning.as_deref(),
        Some("agent capture pinned to manifest DB for main")
    );

    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[tokio::test]
async fn resolve_capture_target_falls_back_to_server_scope_without_manifest_match() {
    let _guard = tachi_home_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp_home = tempdir().expect("temp home");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("TACHI_HOME", temp_home.path());

    let project_db = temp_home.path().join("project.db");
    let server = crate::MemoryServer::new(temp_home.path().join("global.db"), Some(project_db))
        .expect("server");
    let (target_db, named_project, db_path, warning) =
        resolve_capture_target(&server, "project", None, "missing-agent");

    assert_eq!(target_db, DbScope::Project);
    assert!(named_project.is_none());
    assert!(db_path.is_none());
    assert!(warning.is_none());

    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[test]
fn parse_session_capture_response_filters_empty_text() {
    let raw = r#"[{"text": ""}, {"text": "valid"}]"#;
    let drafts = parse_session_capture_response(raw).unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].text, "valid");
}

#[test]
fn matches_agent_tag_handles_hyphenated_agent_ids() {
    assert!(matches_agent_tag("jayne-main", "jayne"));
    assert!(matches_agent_tag("openclaw:jayne:main", "jayne"));
    assert!(!matches_agent_tag("jayneville-bot", "jayne"));
}

#[test]
fn matches_agent_tag_handles_user_memory_slugs_without_substring_false_positives() {
    assert!(matches_agent_tag("user-memory", "user-memory"));
    assert!(matches_agent_tag("user-memory-v3", "user-memory"));
    assert!(matches_agent_tag("agent/user-memory", "user-memory"));
    assert!(!matches_agent_tag("my-user-memory-analyzer", "user-memory"));
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
    let insight = infer_memory_insight(&entry, 0.35, 2, 1, FOUNDRY_RELATED_LIMIT);

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
    let insight = infer_memory_insight(&entry, 0.5, 0, 8, 1);

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

fn distill_source_entry(id: &str, text: &str, category: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: "/project/tachi/crates/memory-server/src/tools.rs".to_string(),
        summary: text.chars().take(40).collect(),
        text: text.to_string(),
        importance: 0.7,
        timestamp: "2026-01-01T00:00:00Z".to_string(),
        valid_from: String::new(),
        valid_until: None,
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
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
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
        valid_from: String::new(),
        valid_until: None,
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
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
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

struct SftEnvGuard {
    original_tachi_home: Option<std::ffi::OsString>,
    original_tachi_app_home: Option<std::ffi::OsString>,
    original_siliconflow_api_key: Option<std::ffi::OsString>,
    original_voyage_api_key: Option<std::ffi::OsString>,
    original_siliconflow_base: Option<std::ffi::OsString>,
    original_reasoning_base: Option<std::ffi::OsString>,
    original_claude_bin: Option<std::ffi::OsString>,
}

impl SftEnvGuard {
    fn new(temp_path: &std::path::Path, mock_url: &str) -> Self {
        let original_tachi_home = std::env::var_os("TACHI_HOME");
        let original_tachi_app_home = std::env::var_os("TACHI_APP_HOME");
        let original_siliconflow_api_key = std::env::var_os("SILICONFLOW_API_KEY");
        let original_voyage_api_key = std::env::var_os("VOYAGE_API_KEY");
        let original_siliconflow_base = std::env::var_os("SILICONFLOW_BASE_URL");
        let original_reasoning_base = std::env::var_os("REASONING_BASE_URL");
        let original_claude_bin = std::env::var_os("CLAUDE_BIN");

        std::env::set_var("TACHI_HOME", temp_path);
        std::env::set_var("TACHI_APP_HOME", temp_path);
        std::env::set_var("SILICONFLOW_BASE_URL", mock_url);
        std::env::set_var("REASONING_BASE_URL", mock_url);
        std::env::set_var("SILICONFLOW_API_KEY", "test-mock-key");
        std::env::set_var("VOYAGE_API_KEY", "test-mock-key");
        std::env::set_var("CLAUDE_BIN", "/nonexistent/fake/bin");

        Self {
            original_tachi_home,
            original_tachi_app_home,
            original_siliconflow_api_key,
            original_voyage_api_key,
            original_siliconflow_base,
            original_reasoning_base,
            original_claude_bin,
        }
    }
}

impl Drop for SftEnvGuard {
    fn drop(&mut self) {
        fn restore(name: &str, val: Option<&std::ffi::OsStr>) {
            if let Some(v) = val {
                std::env::set_var(name, v);
            } else {
                std::env::remove_var(name);
            }
        }
        restore("TACHI_HOME", self.original_tachi_home.as_deref());
        restore("TACHI_APP_HOME", self.original_tachi_app_home.as_deref());
        restore("SILICONFLOW_API_KEY", self.original_siliconflow_api_key.as_deref());
        restore("VOYAGE_API_KEY", self.original_voyage_api_key.as_deref());
        restore("SILICONFLOW_BASE_URL", self.original_siliconflow_base.as_deref());
        restore("REASONING_BASE_URL", self.original_reasoning_base.as_deref());
        restore("CLAUDE_BIN", self.original_claude_bin.as_deref());
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes TACHI_HOME across async mock LLM + distillation
async fn test_run_daily_sft_distillation() {
    let _guard = tachi_home_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    // 1. Mock HTTP LLM Server
    use axum::{routing::post, Json, Router};
    let app = Router::new().route(
        "/chat/completions",
        post(|Json(_body): Json<serde_json::Value>| async {
            Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": "{\n  \"user\": \"How to design Promotion Gates?\",\n  \"assistant\": \"Promotion Gates protect long-term memory by promoting raw tier to consolidated.\",\n  \"type\": \"architecture\"\n}"
                        },
                        "finish_reason": "stop"
                    }
                ],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 20,
                    "total_tokens": 30
                }
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let port = addr.port();

    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // 2. Set environment overrides inside temp home using RAII Guard
    let temp_home = tempdir().expect("temp home");
    let mock_url = format!("http://127.0.0.1:{port}/chat/completions");
    let _env_guard = SftEnvGuard::new(temp_home.path(), &mock_url);

    // 3. Create server and seed consolidated memory entry in Project DB
    let global_db = temp_home.path().join("global.db");
    let project_db = temp_home.path().join("project.db");
    let server = crate::MemoryServer::new(global_db, Some(project_db.clone())).expect("server");

    server.with_project_store(|store| {
        store.upsert(&MemoryEntry {
            id: "sft-test-1".to_string(),
            path: "/project/tachi/sft-1".to_string(),
            summary: "Implement Promotion Gates".to_string(),
            text: "This memory describes the Promotion Gates mechanism including REM and Deep sleep gates with access thresholds.".to_string(),
            importance: 0.85,
            timestamp: "2026-05-30T00:00:00Z".to_string(),
            category: "experience".to_string(),
            topic: "memory-lifecycle".to_string(),
            keywords: vec!["sft".to_string()],
            persons: vec![],
            entities: vec![],
            location: "".to_string(),
            source: "manual".to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 5,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            valid_from: String::new(),
            valid_until: None,
            recall_count: 5,
            query_diversity: 3,
            tier: "consolidated".to_string(),
        }).map_err(|e| e.to_string())
    }).expect("seed test memory");

    // 4. Invoke run_daily_sft_distillation
    let result = sft_factory::run_daily_sft_distillation(&server).await;
    assert!(result.is_ok(), "sft distillation failed: {:?}", result);

    // 5. Verify the files are produced and populated
    let sft_dir = temp_home.path().join("foundry-runs").join("sft");
    let v3_file = sft_dir.join("sft_v3.jsonl");
    let chat_file = sft_dir.join("sft_data_chat.jsonl");
    let hf_file = sft_dir.join("sft_data_hf.jsonl");

    assert!(v3_file.exists());
    assert!(chat_file.exists());
    assert!(hf_file.exists());

    let v3_content = std::fs::read_to_string(v3_file).unwrap();
    assert!(v3_content.contains("How to design Promotion Gates?"));
    assert!(v3_content.contains("Promotion Gates protect long-term memory"));
    let pending_dir = sft_dir.join("pending");
    let pending_batches = std::fs::read_dir(&pending_dir)
        .expect("pending SFT dir")
        .filter_map(Result::ok)
        .count();
    assert_eq!(pending_batches, 3, "expected one durable pending file per export format");

    // 6. Verify entry in DB has been updated to processed
    let (is_processed, batch_id) = server.with_project_store_read(|store| {
        let conn = store.connection();
        let row: (Option<i64>, Option<String>) = conn.query_row(
            "SELECT json_extract(metadata, '$.sft.processed'), json_extract(metadata, '$.sft.batch_id') FROM memories WHERE id = 'sft-test-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?))
        ).map_err(|e| e.to_string())?;
        Ok((row.0.unwrap_or(0) == 1, row.1))
    }).unwrap();
    assert!(is_processed, "entry was not marked as sft.processed");
    assert!(batch_id.is_some(), "SFT marker should include durable batch id");

    // 7. Cleanup server task
    server_task.abort();
}
