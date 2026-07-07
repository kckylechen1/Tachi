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

    assert_eq!(ids, vec!["hybrid-5", "hybrid-0", "hybrid-1"]);
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
