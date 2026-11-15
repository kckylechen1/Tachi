use super::*;

/// #2305 finding 3: retired dispatch-ledger `/cards/*` mirrors and dated
/// `/agent/checkpoints/*` snapshots are excluded from default recall behind
/// a `path_prefix` opt-in, mirroring the `/sft` boundary tests.

#[tokio::test]
async fn tachi_search_excludes_ledger_snapshots_by_default() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut live = make_entry("live-memory-row");
            live.path = "/scratch/sigil/current-dispatch".to_string();
            live.text = "UniqueLedgerBoundaryNeedle live dispatch decision with cargo and main."
                .to_string();
            live.summary = "Live memory row".to_string();
            store.upsert(&live).map_err(|e| e.to_string())?;

            let mut card = make_entry("retired-lane-card");
            card.path = "/cards/oz-sonnet".to_string();
            card.text =
                "UniqueLedgerBoundaryNeedle retired lane card cargo main dispatch".to_string();
            card.summary = "Retired dispatch-ledger card".to_string();
            card.importance = 0.9;
            store.upsert(&card).map_err(|e| e.to_string())?;

            let mut checkpoint = make_entry("stale-agent-checkpoint");
            checkpoint.path = "/agent/checkpoints/2026-07-09".to_string();
            checkpoint.text =
                "UniqueLedgerBoundaryNeedle CRITICAL main RED cargo checkpoint note".to_string();
            checkpoint.summary = "Stale agent checkpoint".to_string();
            checkpoint.importance = 0.8;
            store.upsert(&checkpoint).map_err(|e| e.to_string())
        })
        .expect("seed ledger boundary entries");

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            conversation_scope: None,
            auto_conversation_scope: true,
            multi_evidence_pack: true,
            query: "UniqueLedgerBoundaryNeedle".to_string(),
            scope: "memory".to_string(),
            top_k: 5,
            path_prefix: None,
            project: None,
            project_explicit: true,
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
            include_superseded: false,
            include_test: false,
        }))
        .await
        .expect("memory scoped search");

    assert!(response.contains("live-memory-row"));
    assert!(!response.contains("retired-lane-card"));
    assert!(!response.contains("stale-agent-checkpoint"));
}

#[tokio::test]
async fn tachi_search_path_prefix_opts_into_ledger_snapshots() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut live = make_entry("live-memory-row");
            live.path = "/scratch/sigil/current-dispatch".to_string();
            live.text = "UniqueLedgerScopeNeedle live dispatch decision.".to_string();
            live.summary = "Live memory row".to_string();
            store.upsert(&live).map_err(|e| e.to_string())?;

            let mut card = make_entry("retired-lane-card");
            card.path = "/cards/oz-sonnet".to_string();
            card.text = "UniqueLedgerScopeNeedle retired lane card".to_string();
            card.summary = "Retired dispatch-ledger card".to_string();
            store.upsert(&card).map_err(|e| e.to_string())
        })
        .expect("seed ledger scope entries");

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            conversation_scope: None,
            auto_conversation_scope: true,
            multi_evidence_pack: true,
            query: "UniqueLedgerScopeNeedle".to_string(),
            scope: "memory".to_string(),
            top_k: 5,
            path_prefix: Some("/cards".to_string()),
            project: None,
            project_explicit: true,
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
            include_superseded: false,
            include_test: false,
        }))
        .await
        .expect("cards scoped search");

    assert!(
        response.contains("retired-lane-card"),
        "path_prefix=/cards opts into the ledger snapshots: {response}"
    );
    assert!(!response.contains("live-memory-row"));
}
