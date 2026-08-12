//! The cross-crate embedding-width pin (tachi#1681 D3, PR-B item 4).
//!
//! `tachi-llm` refuses an embedding configuration whose declared width does
//! not match `tachi_llm::STORED_INDEX_DIMENSION`; `tachi-server` warns, probes
//! and reports against `status_ops::EXPECTED_EMBEDDING_DIM`. Neither crate may
//! depend on the other's internals, so the width is a constant in both — and
//! two constants that are supposed to be the same number are a drift waiting
//! to happen.
//!
//! This is the test that makes the duplication safe: if somebody moves one, the
//! build goes red instead of the gate quietly checking against a width nothing
//! is stored at.

use crate::status_ops::EXPECTED_EMBEDDING_DIM;

#[test]
fn the_embedding_gate_and_the_status_expectation_are_the_same_width() {
    assert_eq!(
        EXPECTED_EMBEDDING_DIM as u32,
        tachi_llm::STORED_INDEX_DIMENSION,
        "tachi-llm refuses embedding configs against STORED_INDEX_DIMENSION while tachi-server \
         reports and warns against EXPECTED_EMBEDDING_DIM. If they disagree, the escape-hatch \
         gate is checking against a width nothing is actually stored at — which is exactly the \
         silent corruption the gate exists to prevent."
    );
}

#[test]
fn the_default_embedding_model_declares_the_stored_width() {
    let config = tachi_llm::EmbeddingConfig::default_voyage();
    assert_eq!(config.dimension as usize, EXPECTED_EMBEDDING_DIM);
    assert_eq!(config.model, tachi_llm::DEFAULT_EMBEDDING_MODEL);
}
