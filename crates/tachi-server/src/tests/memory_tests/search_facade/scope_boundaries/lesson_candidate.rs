use super::*;

/// #1073 D2 containment (cross-vendor review finding 1): a pending,
/// model-authored `LessonCandidateV1` row must not influence ordinary
/// recall before establishment — same shape as the sft/recall-cache
/// boundary tests in this directory.
#[tokio::test]
async fn tachi_search_excludes_pending_lesson_candidate_rows_by_default() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut live = make_entry("live-memory-row");
            live.path = "/scratch/sigil/current".to_string();
            live.text = "UniqueLessonCandidateNeedle live memory row.".to_string();
            live.summary = "Live memory row".to_string();
            store.upsert(&live).map_err(|e| e.to_string())?;

            let mut candidate = make_entry("lesson-candidate-row");
            candidate.path = "/lesson_candidates/global/abc123".to_string();
            candidate.text = "UniqueLessonCandidateNeedle pending lesson candidate.".to_string();
            candidate.summary = "Pending lesson candidate".to_string();
            candidate.category = "decision".to_string();
            candidate.domain =
                Some(crate::lesson_forge_ops::storage::LESSON_CANDIDATE_DOMAIN.to_string());
            candidate.metadata = json!({"kind": "lesson_candidate", "candidate_status": "pending"});
            store.upsert(&candidate).map_err(|e| e.to_string())
        })
        .expect("seed lesson-candidate boundary entries");

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniqueLessonCandidateNeedle".to_string(),
            scope: "memory".to_string(),
            top_k: 5,
            path_prefix: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            context_symbols: Vec::new(),
            agent_role: None,
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("memory scoped search");

    assert!(response.contains("live-memory-row"));
    assert!(!response.contains("lesson-candidate-row"));
}

#[tokio::test]
async fn tachi_search_path_prefix_opts_into_lesson_candidate_rows() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut live = make_entry("live-memory-row-2");
            live.path = "/scratch/sigil/current-2".to_string();
            live.text = "UniqueLessonCandidateScopeNeedle live memory row.".to_string();
            live.summary = "Live memory row".to_string();
            store.upsert(&live).map_err(|e| e.to_string())?;

            let mut candidate = make_entry("lesson-candidate-row-2");
            candidate.path = "/lesson_candidates/global/def456".to_string();
            candidate.text =
                "UniqueLessonCandidateScopeNeedle pending lesson candidate.".to_string();
            candidate.summary = "Pending lesson candidate".to_string();
            candidate.category = "decision".to_string();
            candidate.domain =
                Some(crate::lesson_forge_ops::storage::LESSON_CANDIDATE_DOMAIN.to_string());
            candidate.metadata = json!({"kind": "lesson_candidate", "candidate_status": "pending"});
            store.upsert(&candidate).map_err(|e| e.to_string())
        })
        .expect("seed lesson-candidate scope entries");

    let response = server
        .search_memory(Parameters(SearchMemoryParams {
            query: "UniqueLessonCandidateScopeNeedle".to_string(),
            query_vec: None,
            top_k: 5,
            path_prefix: Some("/lesson_candidates".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            context_symbols: Vec::new(),
            agent_role: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
            // Assertion below is a substring `.contains()` check on the
            // response, unaffected by the search_memory markdown/JSON
            // default (tachi#1201 k3); left unset intentionally.
            format: None,
        }))
        .await
        .expect("lesson-candidate scoped search");

    assert!(response.contains("lesson-candidate-row-2"));
}
