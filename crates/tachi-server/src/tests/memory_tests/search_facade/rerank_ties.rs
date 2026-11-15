use super::*;

// #2305 finding 2: the adaptive rerank gate must decide on score
// ambiguity, not on candidate count — a result set that already fits
// top_k still reranks when the top scores tie, because the order WITHIN
// top_k is caller-visible.

fn diagnostics_policy(rows: &[serde_json::Value]) -> String {
    rows.first()
        .and_then(|row| row.get("rerank_diagnostics"))
        .and_then(|diag| diag.get("policy"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn tie_params(query: &str) -> SearchMemoryParams {
    SearchMemoryParams {
        query: query.to_string(),
        query_vec: None,
        top_k: 3,
        path_prefix: None,
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
        enable_rerank: true,
        conversation_scope: None,
        auto_conversation_scope: false,
        multi_evidence_pack: false,
        as_of: None,
        include_superseded: false,
        include_test: false,
        include_metadata: true,
        format: Some("json".to_string()),
    }
}

#[tokio::test]
async fn tied_candidates_within_top_k_still_rerank() {
    // Three entries with the same shape cover the query equally, so their
    // hybrid scores tie and the set fits top_k exactly. The old gate
    // short-circuited this as not_needed; the tie must reach the provider
    // (which is unconfigured in tests, so the run fails open to fallback).
    let server = make_server();
    server
        .with_global_store(|store| {
            for (id, ordinal) in [
                ("tie-probe-one", "one"),
                ("tie-probe-two", "two"),
                ("tie-probe-three", "three"),
            ] {
                let mut entry = make_entry(id);
                entry.path = format!("/scratch/tachi/{id}");
                entry.summary = "Tie probe".to_string();
                entry.text = format!("tie probe shared sentinel {ordinal}");
                store.upsert(&entry).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed tie probe entries");

    let response = server
        .search_memory(Parameters(tie_params("tie probe shared sentinel")))
        .await
        .expect("tie probe search should succeed");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&response).expect("search JSON");

    assert_eq!(rows.len(), 3, "all tied candidates fit top_k");
    assert_eq!(
        diagnostics_policy(&rows),
        "fallback",
        "an exact tie inside top_k must reach the rerank provider (fail-open here), not be skipped as not_needed"
    );
}

#[tokio::test]
async fn clear_leader_within_top_k_keeps_the_gap_throttle() {
    // One entry covers the whole query, two cover a single token: the top-2
    // score gap is wide and rerank must stay throttled even though the set
    // fits top_k (score_gap_too_wide, preserving the cost/confidence gate).
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut leader = make_entry("dominance-probe-leader");
            leader.path = "/scratch/tachi/dominance-probe-leader".to_string();
            leader.summary = "Dominance probe".to_string();
            leader.text = "dominant sentinel tokens full coverage row".to_string();
            store.upsert(&leader).map_err(|e| e.to_string())?;
            for id in ["dominance-probe-weak-a", "dominance-probe-weak-b"] {
                let mut weak = make_entry(id);
                weak.path = format!("/scratch/tachi/{id}");
                weak.summary = "Weak probe".to_string();
                weak.text = format!("sentinel only filler row {id}");
                store.upsert(&weak).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed dominance probe entries");

    let response = server
        .search_memory(Parameters(tie_params("dominant sentinel tokens")))
        .await
        .expect("dominance probe search should succeed");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&response).expect("search JSON");

    assert_eq!(rows[0]["id"], json!("dominance-probe-leader"));
    assert_eq!(
        diagnostics_policy(&rows),
        "score_gap_too_wide",
        "a clear leader keeps the rerank throttle even when the candidate set fits top_k"
    );
}

#[tokio::test]
async fn two_tied_exact_token_candidates_release_the_skip() {
    // Both rows contain the id-like query and cover it at full symbolic
    // confidence: two tied high-confidence exact-token candidates. The old
    // gate skipped rerank on the first row alone; the model must now decide
    // between them (#2305 finding 2).
    let server = make_server();
    server
        .with_global_store(|store| {
            for (id, tail) in [
                ("exact-tie-alpha-20260615", "alpha tail words"),
                ("exact-tie-beta-20260615", "beta tail words"),
            ] {
                let mut entry = make_entry(id);
                entry.path = format!("/scratch/tachi/{id}");
                entry.summary = "Exact tie probe".to_string();
                entry.text = format!("RECALL_TIE_PROBE_20260615 shared marker {tail}");
                store.upsert(&entry).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed exact tie probe entries");

    let response = server
        .search_memory(Parameters(tie_params("RECALL_TIE_PROBE_20260615")))
        .await
        .expect("exact tie probe search should succeed");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&response).expect("search JSON");

    assert_eq!(rows.len(), 2, "both exact-token candidates return");
    assert!(
        rows[0].get("rerank_policy").is_none(),
        "no skipped_exact_token marker when exact-token candidates tie"
    );
    assert_eq!(
        diagnostics_policy(&rows),
        "fallback",
        "tied high-confidence exact-token candidates must reach the provider, not be skipped"
    );
}
