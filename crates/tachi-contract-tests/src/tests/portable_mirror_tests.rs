//! Typed behavioral parity & default-value contract for memory search
//! parameters across the `tachi-params` full/facade surface and the portable
//! search profile (`top_k=6`, `MAX_SEARCH_TOP_K=100`,
//! `candidates_per_channel=20`, `MAX_SEARCH_CANDIDATES_PER_CHANNEL=500`,
//! `mmr_threshold=Some(0.85)`, `graph_expand_hops=1`, and kernel default hybrid
//! weights `0.35 / 0.25 / 0.20 / 0.20` with `use_rrf=true`).
//!
//! `crates/portable-server` is a zero-`tachi-server` binary crate whose own
//! unit tests (`quant_search_controls_keep_defaults_null_semantics_and_candidate_floor`
//! and `normalized_top_k_clamps_range` in `crates/portable-server/src/service.rs`)
//! verify the portable binary's `SearchParams`/`MemoryActionParams` against
//! these exact defaults and bounds. This module verifies the full/facade side
//! via real typed deserialization and `SearchOptions` construction rather than
//! parsing `.rs` source files as text.

use serde_json::json;
use tachi_params::{
    clamp_facade_top_k, FindSimilarMemoryParams, HybridWeightsParam, SearchMemoryParams,
    TachiMemoryParams, TachiSearchParams, MAX_FACADE_TOP_K, MAX_SEARCH_CANDIDATES_PER_CHANNEL,
    MAX_SEARCH_TOP_K,
};

const PORTABLE_DEFAULT_TOP_K: usize = 6;
const PORTABLE_MAX_SEARCH_TOP_K: usize = 100;
const PORTABLE_DEFAULT_CANDIDATES_PER_CHANNEL: usize = 20;
const PORTABLE_MAX_CANDIDATES_PER_CHANNEL: usize = 500;
const PORTABLE_DEFAULT_MMR_THRESHOLD: Option<f64> = Some(0.85);
const PORTABLE_DEFAULT_GRAPH_EXPAND_HOPS: u32 = 1;

#[test]
fn search_memory_defaults_match_portable_and_kernel_search_options() {
    let params: SearchMemoryParams = serde_json::from_value(json!({ "query": "memory" }))
        .expect("deserialize SearchMemoryParams");

    assert_eq!(params.top_k, PORTABLE_DEFAULT_TOP_K);
    assert_eq!(
        params.candidates_per_channel,
        PORTABLE_DEFAULT_CANDIDATES_PER_CHANNEL
    );
    assert_eq!(params.mmr_threshold, PORTABLE_DEFAULT_MMR_THRESHOLD);
    assert_eq!(params.graph_expand_hops, PORTABLE_DEFAULT_GRAPH_EXPAND_HOPS);
    assert!(params.auto_conversation_scope);
    assert!(params.multi_evidence_pack);
    assert!(!params.include_archived);
    assert!(!params.include_training);
    assert!(!params.include_superseded);
    assert!(!params.include_test);
    assert!(!params.enable_rerank);

    let options = params.to_search_options(false);
    assert_eq!(options.top_k, PORTABLE_DEFAULT_TOP_K);
    assert_eq!(
        options.candidates_per_channel,
        PORTABLE_DEFAULT_CANDIDATES_PER_CHANNEL
    );
    assert_eq!(options.mmr_threshold, PORTABLE_DEFAULT_MMR_THRESHOLD);
    assert_eq!(
        options.graph_expand_hops,
        PORTABLE_DEFAULT_GRAPH_EXPAND_HOPS
    );
    assert!(!options.record_access);
}

#[test]
fn search_memory_mmr_threshold_supports_explicit_null_disable_and_string_coercion() {
    let disabled: SearchMemoryParams = serde_json::from_value(json!({
        "query": "memory",
        "mmr_threshold": null,
    }))
    .expect("explicit null mmr_threshold deserializes");
    assert_eq!(disabled.mmr_threshold, None);
    assert_eq!(disabled.to_search_options(false).mmr_threshold, None);

    let coerced: SearchMemoryParams = serde_json::from_value(json!({
        "query": "memory",
        "mmr_threshold": "0.85",
    }))
    .expect("numeric string mmr_threshold deserializes");
    assert_eq!(coerced.mmr_threshold, PORTABLE_DEFAULT_MMR_THRESHOLD);
}

#[test]
fn search_bounds_and_candidate_floor_match_portable_contract() {
    assert_eq!(MAX_SEARCH_TOP_K, PORTABLE_MAX_SEARCH_TOP_K);
    assert_eq!(MAX_FACADE_TOP_K, PORTABLE_MAX_SEARCH_TOP_K);
    assert_eq!(
        MAX_SEARCH_CANDIDATES_PER_CHANNEL,
        PORTABLE_MAX_CANDIDATES_PER_CHANNEL
    );
    assert_eq!(clamp_facade_top_k(0), 1);
    assert_eq!(clamp_facade_top_k(999_999), PORTABLE_MAX_SEARCH_TOP_K);

    let zero_bounds: SearchMemoryParams = serde_json::from_value(json!({
        "query": "memory",
        "top_k": 0,
        "candidates_per_channel": 0,
    }))
    .expect("zero bounds deserialize");
    assert_eq!(zero_bounds.normalized_top_k(), 1);
    assert_eq!(
        zero_bounds.normalized_candidates_per_channel(),
        PORTABLE_DEFAULT_CANDIDATES_PER_CHANNEL
    );

    let oversized: SearchMemoryParams = serde_json::from_value(json!({
        "query": "memory",
        "top_k": 999_999,
        "candidates_per_channel": 999_999,
    }))
    .expect("oversized bounds deserialize");
    let oversized_opts = oversized.to_search_options(false);
    assert_eq!(oversized_opts.top_k, PORTABLE_MAX_SEARCH_TOP_K);
    assert_eq!(
        oversized_opts.candidates_per_channel,
        PORTABLE_MAX_CANDIDATES_PER_CHANNEL
    );

    let floor_raised: SearchMemoryParams = serde_json::from_value(json!({
        "query": "memory",
        "top_k": 75,
        "candidates_per_channel": 1,
    }))
    .expect("candidate floor deserializes");
    let floor_opts = floor_raised.to_search_options(false);
    assert_eq!(floor_opts.top_k, 75);
    assert_eq!(floor_opts.candidates_per_channel, 75);
}

#[test]
fn search_memory_omitted_weights_use_kernel_rrf_defaults_while_explicit_override_uses_param_defaults(
) {
    let default_params: SearchMemoryParams =
        serde_json::from_value(json!({ "query": "memory" })).expect("deserialize default search");
    let kernel_weights = default_params.to_search_options(false).weights;
    assert_eq!(kernel_weights.semantic, 0.35);
    assert_eq!(kernel_weights.fts, 0.25);
    assert_eq!(kernel_weights.symbolic, 0.20);
    assert_eq!(kernel_weights.decay, 0.20);
    assert!(kernel_weights.use_rrf);

    let explicit_empty_weights: SearchMemoryParams =
        serde_json::from_value(json!({ "query": "memory", "weights": {} }))
            .expect("deserialize empty weights override");
    let override_weights = explicit_empty_weights.to_search_options(false).weights;
    let param_weights: HybridWeightsParam =
        serde_json::from_value(json!({})).expect("deserialize empty HybridWeightsParam");
    assert_eq!(param_weights.semantic, 0.40);
    assert_eq!(param_weights.fts, 0.30);
    assert_eq!(param_weights.symbolic, 0.20);
    assert_eq!(param_weights.decay, 0.10);
    assert!(!param_weights.use_rrf);
    assert_eq!(override_weights.semantic, param_weights.semantic);
    assert_eq!(override_weights.fts, param_weights.fts);
    assert_eq!(override_weights.symbolic, param_weights.symbolic);
    assert_eq!(override_weights.decay, param_weights.decay);
    assert_eq!(override_weights.use_rrf, param_weights.use_rrf);
}

#[test]
fn facade_and_similarity_search_defaults_match_search_memory_contract() {
    let facade_search: TachiSearchParams = serde_json::from_value(json!({ "query": "memory" }))
        .expect("deserialize TachiSearchParams");
    assert_eq!(facade_search.top_k, PORTABLE_DEFAULT_TOP_K);
    assert_eq!(facade_search.scope, "all");
    assert!(facade_search.auto_conversation_scope);
    assert!(facade_search.multi_evidence_pack);

    let memory_search: TachiMemoryParams = serde_json::from_value(json!({
        "action": "search",
        "query": "memory",
    }))
    .expect("deserialize TachiMemoryParams");
    assert_eq!(memory_search.top_k, PORTABLE_DEFAULT_TOP_K);
    assert!(memory_search.auto_conversation_scope);
    assert!(memory_search.multi_evidence_pack);

    let similar: FindSimilarMemoryParams = serde_json::from_value(json!({
        "query_vec": [0.1, 0.2],
    }))
    .expect("deserialize FindSimilarMemoryParams");
    assert_eq!(similar.top_k, 5);
    assert_eq!(
        similar.candidates_per_channel,
        PORTABLE_DEFAULT_CANDIDATES_PER_CHANNEL
    );

    let clamped_similar: FindSimilarMemoryParams = serde_json::from_value(json!({
        "query_vec": [0.1, 0.2],
        "top_k": 999_999,
        "candidates_per_channel": 999_999,
    }))
    .expect("deserialize oversized FindSimilarMemoryParams");
    assert_eq!(
        clamped_similar.normalized_top_k(),
        PORTABLE_MAX_SEARCH_TOP_K
    );
    assert_eq!(
        clamped_similar.normalized_candidates_per_channel(),
        PORTABLE_MAX_CANDIDATES_PER_CHANNEL
    );
}
