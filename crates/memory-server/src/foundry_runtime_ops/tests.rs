use super::handlers::{
    build_bracket_self_evolution_id, classify_bracket_self_evolution,
    extract_bracket_self_evolution_notes, matches_agent_tag, resolve_capture_target,
};
use super::maintenance::memory_claim_signature;
use super::recall::{
    merge_rerank_order_with_hybrid_floor, parse_compact_context_response,
    parse_session_capture_response,
};
use super::{FOUNDRY_DISTILL_SOURCE, FOUNDRY_RELATED_LIMIT};
use crate::manifest::{DbEntry, DbRole, Manifest};
use crate::server_state::DbScope;
use crate::tool_params::{CaptureSessionParams, CompactRollupParams, Message};
use memory_core::MemoryEntry;
use serde_json::json;
use tachi_foundry::{
    collect_coherent_distill_buckets, infer_memory_insight, plan_distill_edges,
    plan_guide_distill_memory,
};
use tempfile::tempdir;

fn tachi_home_test_lock() -> &'static std::sync::Mutex<()> {
    crate::utils::global_test_lock()
}

mod bracket_evolution;
mod capture_target;
mod distill_buckets;
mod distill_guides;
mod memory_maintenance;
mod recall_parse_params;

#[test]
fn rerank_merge_blends_rerank_order_with_hybrid_rank() {
    let rows = (0..6)
        .map(|idx| {
            json!({
                "id": format!("hybrid-{idx}"),
                "text": format!("candidate {idx}"),
                "relevance": 1.0 - (idx as f64 * 0.01),
                "score": { "final": 1.0 - (idx as f64 * 0.01) }
            })
        })
        .collect::<Vec<_>>();

    let merged = merge_rerank_order_with_hybrid_floor(
        &rows,
        &[(5, 0.99), (4, 0.98), (2, 0.97), (1, 0.96)],
        3,
    );
    let ids = merged
        .iter()
        .map(|row| row["id"].as_str().expect("row id"))
        .collect::<Vec<_>>();

    // head_floor for top_k=3 is 2 -> hybrid-0 and hybrid-1 are guaranteed by
    // the seatbelt; the remaining slot promotes the best tail item (hybrid-5,
    // rerank_rank 1, blend 1.167). Final order is blend_score descending.
    assert_eq!(ids, vec!["hybrid-5", "hybrid-0", "hybrid-1"]);
    // relevance now equals the blend sort key (reciprocal-rank fusion), not the
    // raw provider signal, so [Score=X.XX] in agent context matches the rank.
    assert_eq!(merged[0]["relevance"], json!(1.167)); // 1/6 + 1/1
    assert_eq!(merged[1]["relevance"], json!(1.143)); // 1/1 + 1/7
    assert_eq!(merged[2]["relevance"], json!(0.75)); //  1/2 + 1/4
                                                     // rerank_score keeps the raw provider signal where the model scored the row.
    assert_eq!(merged[0]["rerank_score"], json!(0.99));
    assert!(merged[1].get("rerank_score").is_none());
    assert_eq!(merged[2]["rerank_score"], json!(0.96));
    assert!(
        ids.contains(&"hybrid-0"),
        "hybrid head should remain available as the no-evict seatbelt"
    );
}

#[test]
fn rerank_merge_allows_tail_promotion_without_dropping_the_hybrid_head() {
    let rows = (0..6)
        .map(|idx| {
            json!({
                "id": format!("hybrid-{idx}"),
                "text": format!("candidate {idx}"),
                "relevance": 1.0 - (idx as f64 * 0.01),
                "score": { "final": 1.0 - (idx as f64 * 0.01) }
            })
        })
        .collect::<Vec<_>>();

    let merged = merge_rerank_order_with_hybrid_floor(
        &rows,
        &[(5, 0.99), (4, 0.98), (2, 0.97), (1, 0.96)],
        3,
    );
    let ids = merged
        .iter()
        .map(|row| row["id"].as_str().expect("row id"))
        .collect::<Vec<_>>();

    assert!(
        ids.contains(&"hybrid-5"),
        "expanded rerank candidates must be able to promote into top_k: {ids:?}"
    );
    assert!(
        ids.contains(&"hybrid-0"),
        "the hybrid head is the no-evict seatbelt for prior top-k hits: {ids:?}"
    );
}

#[test]
fn rerank_merge_never_evicts_hybrid_head_beyond_head_floor() {
    let rows = (0..6)
        .map(|idx| {
            json!({
                "id": format!("hybrid-{idx}"),
                "text": format!("candidate {idx}"),
                "relevance": 1.0 - (idx as f64 * 0.01),
                "score": { "final": 1.0 - (idx as f64 * 0.01) }
            })
        })
        .collect::<Vec<_>>();
    // rerank strongly prefers the TAIL — model ranks 5,4,3,2,1,0
    let order = vec![
        (5, 0.99_f64),
        (4, 0.98),
        (3, 0.97),
        (2, 0.96),
        (1, 0.95),
        (0, 0.94),
    ];
    let merged = merge_rerank_order_with_hybrid_floor(&rows, &order, 3);
    let ids: Vec<&str> = merged.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(merged.len(), 3, "output must be exactly top_k: {ids:?}");
    // head_floor for top_k=3 is 2 → hybrid-0 and hybrid-1 MUST survive
    assert!(
        ids.contains(&"hybrid-0"),
        "hybrid rank-1 must survive (head floor): {ids:?}"
    );
    assert!(
        ids.contains(&"hybrid-1"),
        "hybrid rank-2 must survive (head floor): {ids:?}"
    );
}

#[test]
fn rerank_merge_relevance_matches_blend_sort_order() {
    let rows = (0..4)
        .map(|idx| {
            json!({
                "id": format!("hybrid-{idx}"),
                "text": format!("candidate {idx}"),
                "relevance": 1.0 - (idx as f64 * 0.1),
                "score": { "final": 1.0 - (idx as f64 * 0.1) }
            })
        })
        .collect::<Vec<_>>();
    let order = vec![(3, 0.99_f64)]; // only tail item 3 gets a rerank score
    let merged = merge_rerank_order_with_hybrid_floor(&rows, &order, 2);
    assert_eq!(merged.len(), 2);
    // relevance must be non-increasing down the output (matches sort order)
    let rels: Vec<f64> = merged
        .iter()
        .map(|r| r["relevance"].as_f64().unwrap())
        .collect();
    for w in rels.windows(2) {
        assert!(
            w[0] >= w[1] - 1e-9,
            "relevance must be non-increasing down the output: {rels:?}"
        );
    }
    // promoted tail item hybrid-3 keeps its raw provider score in rerank_score
    let promo = merged.iter().find(|r| r["id"].as_str() == Some("hybrid-3"));
    assert!(
        promo.is_some(),
        "hybrid-3 should promote: {:?}",
        merged.iter().map(|r| r["id"].as_str()).collect::<Vec<_>>()
    );
    if let Some(p) = promo {
        assert_eq!(p["rerank_score"].as_f64().unwrap(), 0.99);
    }
}
