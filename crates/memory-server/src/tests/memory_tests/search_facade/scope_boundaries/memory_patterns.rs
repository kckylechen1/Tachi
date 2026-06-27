use super::*;

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
