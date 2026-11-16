#![cfg(unix)]

mod common;

#[test]
fn packaged_rerank_ties_http_dogfood() {
    common::assert_dogfood_scenario("exact_ties_within_top_k_reach_the_reranker");
}
