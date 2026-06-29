use super::*;
use crate::{MemoryServer, TachiEventParams};

fn event_params(action: &str) -> TachiEventParams {
    TachiEventParams {
        action: action.to_string(),
        format: None,
        id: None,
        source_repo: None,
        adapter: None,
        project: None,
        domain: None,
        session_id: None,
        actor: None,
        event_type: None,
        authority: None,
        effects: Vec::new(),
        projection_hints: Vec::new(),
        payload: None,
        provenance: None,
        created_at: None,
        limit: 20,
        path_prefix: None,
        dry_run: false,
    }
}

async fn seed_projected_pattern(server: &MemoryServer, key: &str, text: &str) -> String {
    let mut emit = event_params("emit");
    emit.id = Some(format!("pattern-seed-{key}"));
    emit.source_repo = Some("sigil".to_string());
    emit.adapter = Some("pattern-feedback-test".to_string());
    emit.domain = Some("agent_os".to_string());
    emit.session_id = Some("session-pattern-feedback".to_string());
    emit.actor = Some("codex".to_string());
    emit.event_type = Some("pattern.candidate".to_string());
    emit.authority = Some("collect_only".to_string());
    emit.projection_hints = vec!["pattern".to_string()];
    emit.payload = Some(json!({
        "pattern_key": key,
        "summary": format!("Pattern {key}"),
        "text": text,
    }));
    crate::event_ops::handle_tachi_event(server, emit)
        .await
        .expect("emit pattern seed");

    let mut project = event_params("project");
    project.projection_hints = vec!["pattern".to_string()];
    let projected = crate::event_ops::handle_tachi_event(server, project)
        .await
        .expect("project pattern seed");
    let projected_json: serde_json::Value = serde_json::from_str(&projected).expect("project JSON");
    projected_json["projections"][0]["memory_id"]
        .as_str()
        .expect("pattern memory id")
        .to_string()
}

fn memory_params(action: &str) -> TachiMemoryParams {
    TachiMemoryParams {
        action: action.to_string(),
        format: Some("json".to_string()),
        query: None,
        scope: None,
        top_k: 6,
        path_prefix: None,
        file_context: None,
        error_context: None,
        category: None,
        include_archived: false,
        include_training: false,
        enable_rerank: false,
        as_of: None,
        synthesize: false,
        model: None,
        text: None,
        title: None,
        summary: None,
        topic: None,
        keywords: Vec::new(),
        entities: Vec::new(),
        importance: None,
        retention_policy: None,
        kind: None,
        path: None,
        id: None,
        force: false,
        source: None,
        valid_from: None,
        valid_until: None,
        metadata: None,
        emit_continuity: false,
        files: Vec::new(),
        flow_id: None,
        event: None,
        state: None,
        project: None,
        domain: None,
        compact: false,
        proposal_id: None,
        review_status: None,
        notes: None,
        confirm: false,
        state_filter: None,
    }
}

#[tokio::test]
async fn tachi_search_memory_scope_excludes_wiki_rows() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut memory = make_entry("plain-memory-row");
            memory.path = "/facts/plain".to_string();
            memory.text = "UniqueBoundaryNeedle belongs in plain memory.".to_string();
            memory.summary = "Plain memory row".to_string();
            store.upsert(&memory).map_err(|e| e.to_string())?;

            let mut wiki = make_entry("wiki-row-should-not-appear");
            wiki.path = "/wiki/general/boundary".to_string();
            wiki.text = "UniqueBoundaryNeedle belongs in wiki.".to_string();
            wiki.summary = "Wiki row".to_string();
            wiki.domain = Some("wiki".to_string());
            wiki.metadata = json!({"wiki": true});
            store.upsert(&wiki).map_err(|e| e.to_string())
        })
        .expect("seed boundary entries");

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniqueBoundaryNeedle".to_string(),
            scope: "memory".to_string(),
            top_k: 5,
            path_prefix: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            context_symbols: Vec::new(),
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("memory scoped search");

    assert!(response.contains("plain-memory-row"));
    assert!(!response.contains("wiki-row-should-not-appear"));
}

#[tokio::test]
async fn tachi_search_patterns_scope_records_seen_feedback() {
    let server = make_server();
    let memory_id = seed_projected_pattern(
        &server,
        "search-seen-pattern",
        "UniquePatternSeenNeedle should count as seen when searched in pattern scope.",
    )
    .await;

    let before = server
        .with_global_store_read(|store| store.get(&memory_id).map_err(|e| e.to_string()))
        .expect("read pattern before")
        .expect("pattern exists before");
    assert_eq!(before.metadata["counters"]["seen"], json!(1));

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniquePatternSeenNeedle".to_string(),
            scope: "patterns".to_string(),
            top_k: 5,
            path_prefix: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            context_symbols: Vec::new(),
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("patterns scoped search");

    assert!(response.contains(&memory_id));
    let after = server
        .with_global_store_read(|store| store.get(&memory_id).map_err(|e| e.to_string()))
        .expect("read pattern after")
        .expect("pattern exists after");
    assert_eq!(after.metadata["counters"]["seen"], json!(2));
    assert_eq!(after.metadata["counters"]["hit"], json!(0));
    assert_eq!(after.metadata["counters"]["miss"], json!(0));
}

#[tokio::test]
async fn tachi_memory_pattern_feedback_records_hit_without_skill_promotion() {
    let server = make_server();
    let memory_id = seed_projected_pattern(
        &server,
        "feedback-hit-pattern",
        "UniquePatternFeedbackNeedle is a pattern that can receive explicit feedback.",
    )
    .await;

    let response = server
        .tachi_memory(Parameters({
            let mut params = memory_params("pattern_feedback");
            params.id = Some(memory_id.clone());
            params.event = Some("hit".to_string());
            params.query = Some("UniquePatternFeedbackNeedle".to_string());
            params.summary =
                Some("Pattern helped choose the right project-cycle action.".to_string());
            params.metadata = Some(json!({"reviewer": "test"}));
            params
        }))
        .await
        .expect("pattern feedback");
    let response_json: serde_json::Value =
        serde_json::from_str(&response).expect("feedback JSON response");
    assert_eq!(response_json["status"], json!("saved"));
    assert_eq!(response_json["outcome"], json!("hit"));

    let entry = server
        .with_global_store_read(|store| store.get(&memory_id).map_err(|e| e.to_string()))
        .expect("read pattern after feedback")
        .expect("pattern exists after feedback");
    assert_eq!(entry.metadata["counters"]["seen"], json!(2));
    assert_eq!(entry.metadata["counters"]["hit"], json!(1));
    assert_eq!(entry.metadata["counters"]["miss"], json!(0));
    assert_eq!(entry.metadata["counters"]["confidence"], json!(0.5));
    assert_eq!(entry.tier, "raw");
}

#[tokio::test]
async fn tachi_search_patterns_scope_is_explicit() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut memory = make_entry("plain-pattern-word-memory");
            memory.path = "/scratch/patterns/plain".to_string();
            memory.text = "UniquePatternScopeNeedle belongs in plain memory.".to_string();
            memory.summary = "Plain pattern word memory".to_string();
            store.upsert(&memory).map_err(|e| e.to_string())?;

            let mut pattern = make_entry("projected-pattern-row");
            pattern.path = "/user/patterns/agent_os/continuity-first".to_string();
            pattern.text = "UniquePatternScopeNeedle belongs in projected patterns.".to_string();
            pattern.summary = "Projected continuity pattern".to_string();
            pattern.metadata = json!({
                "projection_kind": "pattern",
                "projection_key": "continuity-first",
            });
            store.upsert(&pattern).map_err(|e| e.to_string())
        })
        .expect("seed pattern boundary entries");

    let memory_response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniquePatternScopeNeedle".to_string(),
            scope: "memory".to_string(),
            top_k: 5,
            path_prefix: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            context_symbols: Vec::new(),
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("memory scoped search");

    assert!(memory_response.contains("plain-pattern-word-memory"));
    assert!(!memory_response.contains("projected-pattern-row"));

    let pattern_response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniquePatternScopeNeedle".to_string(),
            scope: "patterns".to_string(),
            top_k: 5,
            path_prefix: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            context_symbols: Vec::new(),
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("patterns scoped search");

    assert!(pattern_response.contains("projected-pattern-row"));
    assert!(!pattern_response.contains("plain-pattern-word-memory"));
}
