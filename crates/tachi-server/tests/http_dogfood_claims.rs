#![cfg(unix)]

mod common;

#[test]
fn packaged_claims_http_dogfood() {
    common::assert_dogfood_scenario("two_identities_contend_for_a_scope");
}

#[test]
fn packaged_eval_receipts_http_dogfood() {
    common::assert_dogfood_scenario(
        "eval_receipts_preserve_requested_identity_and_canonical_replay",
    );
}

#[test]
fn packaged_health_cohort_http_dogfood() {
    common::assert_dogfood_scenario("enrichment_failures_use_the_vector_coverage_cohort");
}

#[test]
fn packaged_queue_evidence_http_dogfood() {
    common::assert_dogfood_scenario("missing_vectors_do_not_claim_enrichment_queue_depth");
}

#[test]
fn packaged_voyage_models_http_dogfood() {
    common::assert_dogfood_scenario("voyage_model_selection_reaches_memory_search_and_rerank");
}
