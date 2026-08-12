//! Embedding escape-hatch discriminators (tachi#1681 D7 PR-B, item 4).
//!
//! The design's requirement, in one line: an override whose dimension does not
//! match the stored index **fails loudly**. These tests pin that it fails, that
//! it fails at resolution rather than after writing vectors, and that the
//! benign case (a same-width sibling model) still goes through — a gate that
//! refuses everything would satisfy "fails loudly" and be useless.
//!
//! Every test takes the process-wide env lock: `TACHI_EMBEDDING_*` are shared
//! names by construction (the whole point is the names the real resolver
//! reads), so they cannot be uniquified per test the way `tests.rs`'s note
//! prefers.

use super::EnvRestore;
use crate::llm::embedding_config::{
    EmbeddingConfig, EmbeddingModelSource, DEFAULT_EMBEDDING_DIMENSION, DEFAULT_EMBEDDING_MODEL,
    EMBEDDING_DIMENSION_ENV, EMBEDDING_MODEL_ENV, STORED_INDEX_DIMENSION,
};

fn cleared_env() -> (EnvRestore, EnvRestore) {
    (
        EnvRestore::unset(EMBEDDING_MODEL_ENV),
        EnvRestore::unset(EMBEDDING_DIMENSION_ENV),
    )
}

#[test]
fn unset_env_resolves_to_the_model_the_stored_index_was_built_with() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _cleared = cleared_env();

    let config = EmbeddingConfig::from_env().expect("the default must always resolve");
    assert_eq!(config.model(), DEFAULT_EMBEDDING_MODEL);
    assert_eq!(config.dimension(), DEFAULT_EMBEDDING_DIMENSION);
    assert_eq!(config.source(), EmbeddingModelSource::Default);
    assert_eq!(
        config.dimension(),
        STORED_INDEX_DIMENSION,
        "the default configuration must agree with the index by construction, or every process \
         refuses to start"
    );
}

#[test]
fn a_model_override_without_a_declared_dimension_is_refused() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _cleared = cleared_env();
    let _model = EnvRestore::set(EMBEDDING_MODEL_ENV, "voyage-3-lite");

    let err = EmbeddingConfig::from_env()
        .expect_err("naming a new embedding model without its width must not resolve");
    assert!(
        err.contains(EMBEDDING_DIMENSION_ENV),
        "the refusal must name the thing to set: {err}"
    );
    assert!(
        err.contains("comparability") || err.contains("vector width"),
        "and say why it matters, so an operator does not just set a number to make it stop: {err}"
    );
}

#[test]
fn an_override_declaring_a_different_width_than_the_index_is_refused() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _cleared = cleared_env();
    let _model = EnvRestore::set(EMBEDDING_MODEL_ENV, "some-2048-dim-model");
    let _dimension = EnvRestore::set(EMBEDDING_DIMENSION_ENV, "2048");

    let err = EmbeddingConfig::from_env()
        .expect_err("a width change without a reindex must fail loudly, not silently degrade");
    assert!(err.contains("2048"), "{err}");
    assert!(err.contains(&STORED_INDEX_DIMENSION.to_string()), "{err}");
    assert!(
        err.contains("not comparable") || err.contains("Reindex"),
        "the refusal must explain the corruption it is preventing: {err}"
    );
}

#[test]
fn a_same_width_sibling_model_is_exactly_what_the_hatch_allows() {
    // The benign case the hatch exists for. Without this, "refuse everything"
    // would pass every other test in this file.
    let _lock = crate::test_support::global_test_lock().lock();
    let _cleared = cleared_env();
    let _model = EnvRestore::set(EMBEDDING_MODEL_ENV, "voyage-3-large");
    let _dimension = EnvRestore::set(
        EMBEDDING_DIMENSION_ENV,
        STORED_INDEX_DIMENSION.to_string().as_str(),
    );

    let config = EmbeddingConfig::from_env().expect("a same-width swap must go through");
    assert_eq!(config.model(), "voyage-3-large");
    assert_eq!(config.dimension(), STORED_INDEX_DIMENSION);
    assert_eq!(
        config.source(),
        EmbeddingModelSource::EnvOverride,
        "status has to be able to distinguish an operator's deliberate swap from the default"
    );
}

#[test]
fn naming_the_default_model_explicitly_is_not_reported_as_an_override() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _cleared = cleared_env();
    let _model = EnvRestore::set(EMBEDDING_MODEL_ENV, DEFAULT_EMBEDDING_MODEL);

    let config = EmbeddingConfig::from_env().expect("naming the default resolves to the default");
    assert_eq!(config.model(), DEFAULT_EMBEDDING_MODEL);
    assert_eq!(
        config.source(),
        EmbeddingModelSource::Default,
        "reporting this as an override would make the status surface lie about operator intent"
    );
}

#[test]
fn a_non_numeric_or_zero_dimension_is_refused_rather_than_defaulted() {
    let _lock = crate::test_support::global_test_lock().lock();

    for bad in ["1024d", "-1", "0", "one thousand twenty four"] {
        let _cleared = cleared_env();
        let _model = EnvRestore::set(EMBEDDING_MODEL_ENV, "voyage-3-large");
        let _dimension = EnvRestore::set(EMBEDDING_DIMENSION_ENV, bad);

        match EmbeddingConfig::from_env() {
            // Resolving is the failure here: an unparseable declaration that
            // quietly becomes 1024 is worse than no declaration at all,
            // because it *looks* checked.
            Ok(config) => {
                panic!("'{bad}' must be refused, not defaulted — it resolved to {config:?}")
            }
            Err(err) => assert!(
                err.contains(EMBEDDING_DIMENSION_ENV),
                "'{bad}': the refusal must name the variable it rejected: {err}"
            ),
        }
    }
}

#[test]
fn a_declared_dimension_that_disagrees_with_the_default_model_is_refused() {
    // Setting only the width, with no model override, still has to be checked:
    // it is a claim about the default model, and a wrong claim is the same
    // corruption by another route.
    let _lock = crate::test_support::global_test_lock().lock();
    let _cleared = cleared_env();
    let _dimension = EnvRestore::set(EMBEDDING_DIMENSION_ENV, "512");

    let err = EmbeddingConfig::from_env().expect_err("a wrong width claim must be refused");
    assert!(err.contains("512"), "{err}");
}

// ─── the gate is reusable against an observed index, not just the constant ───

#[test]
fn validate_against_index_refuses_a_mismatch_and_accepts_a_match() {
    let config = EmbeddingConfig::default_voyage();
    assert!(config
        .validate_against_index(DEFAULT_EMBEDDING_DIMENSION)
        .is_ok());

    let err = config
        .validate_against_index(768)
        .expect_err("a database whose vectors are 768-wide is not comparable");
    assert!(err.contains("768"), "{err}");
    assert!(err.contains(DEFAULT_EMBEDDING_MODEL), "{err}");
}

#[test]
fn the_default_construction_seam_reads_no_environment() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _cleared = cleared_env();
    let _model = EnvRestore::set(EMBEDDING_MODEL_ENV, "__must-not-be-read");
    let _dimension = EnvRestore::set(EMBEDDING_DIMENSION_ENV, "4096");

    let config = EmbeddingConfig::default_voyage();
    assert_eq!(config.model(), DEFAULT_EMBEDDING_MODEL);
    assert_eq!(config.dimension(), DEFAULT_EMBEDDING_DIMENSION);
}

// ─── the declaration reaches the catalog (#1681 D3: "catalog bounds carry a
//     dimension declaration") ───────────────────────────────────────────────

#[test]
fn the_embedding_catalog_row_declares_the_dimension_the_gate_turns_on() {
    use memcore::catalog::{EmbeddingsCapability, ProtocolKind};

    let config = EmbeddingConfig::default_voyage();
    let row = crate::llm::catalog_import::env_embedding_deployment(
        &config,
        "https://api.voyageai.com/v1/embeddings",
        "2026-08-11T00:00:00.000Z",
    )
    .expect("a credential-free endpoint projects");

    assert_eq!(row.lane, crate::llm::catalog_import::ENV_EMBEDDING_LANE);
    assert_eq!(row.deployment.protocol_kind, ProtocolKind::VoyageEmbeddings);
    assert_eq!(row.deployment.provider_model_id, DEFAULT_EMBEDDING_MODEL);
    assert_eq!(
        row.deployment.capabilities.embeddings,
        Some(EmbeddingsCapability {
            dimension: DEFAULT_EMBEDDING_DIMENSION
        }),
        "the catalog has to be able to answer 'at what width', or the escape hatch has nothing \
         to compare an override against"
    );
    assert!(
        !row.deployment.capabilities.chat,
        "the embedding lane is not a chat deployment"
    );
    assert_eq!(
        row.deployment.provider_account_id, "env:api.voyageai.com",
        "the account handle is the endpoint authority, same rule as the chat lanes"
    );
}

#[test]
fn an_operator_swap_is_visible_in_the_catalog_row_and_its_provenance() {
    let swapped = EmbeddingConfig::declared("voyage-3-large", STORED_INDEX_DIMENSION)
        .expect("a same-width swap is a legal configuration");
    let row = crate::llm::catalog_import::env_embedding_deployment(
        &swapped,
        "https://api.voyageai.com/v1/embeddings",
        "2026-08-11T00:00:00.000Z",
    )
    .expect("a credential-free endpoint projects");

    assert_eq!(row.deployment.provider_model_id, "voyage-3-large");
    assert!(
        row.deployment
            .source_refs
            .iter()
            .any(|source_ref| source_ref == &format!("env_model:{EMBEDDING_MODEL_ENV}")),
        "a deliberate swap must record which variable carried it: {:?}",
        row.deployment.source_refs
    );
    for source_ref in &row.deployment.source_refs {
        assert!(
            !source_ref.contains("voyage-3-large"),
            "provenance carries variable names, never their values: {source_ref}"
        );
    }
}

#[test]
fn swapping_the_embedding_model_is_a_catalog_change_not_a_no_op() {
    let default_row = crate::llm::catalog_import::env_embedding_deployment(
        &EmbeddingConfig::default_voyage(),
        "https://api.voyageai.com/v1/embeddings",
        "2026-08-11T00:00:00.000Z",
    )
    .expect("a credential-free endpoint projects");
    let swapped_row = crate::llm::catalog_import::env_embedding_deployment(
        &EmbeddingConfig::declared("voyage-3-large", STORED_INDEX_DIMENSION)
            .expect("a same-width swap is a legal configuration"),
        "https://api.voyageai.com/v1/embeddings",
        "2026-08-11T00:00:00.000Z",
    )
    .expect("a credential-free endpoint projects");
    assert_ne!(
        default_row.deployment.content_digest(),
        swapped_row.deployment.content_digest(),
        "a re-import after a swap must advance the row, not report Unchanged"
    );
}

#[test]
fn an_embedding_endpoint_carrying_userinfo_is_refused_like_a_chat_lane() {
    // `VOYAGE_BASE_URL` is operator-supplied and reaches `endpoint_ref`
    // verbatim, so the embedding lane needs the same door as the chat lanes —
    // not a scrub, and not an exemption for being "just the embedding row".
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
    memcore::db::init_schema(&conn).expect("schema");

    let refusal = crate::llm::catalog_import::import_env_embedding_lane(
        &conn,
        &EmbeddingConfig::default_voyage(),
        "https://svc-account:sk-live-SECRET@voyage.proxy.internal/v1/embeddings",
        "2026-08-11T00:00:00.000Z",
    )
    .expect_err("a userinfo-bearing embeddings endpoint must not import");

    assert!(
        matches!(
            refusal,
            crate::llm::catalog_import::CatalogImportError::EndpointCarriesUserinfo {
                lane: "embedding"
            }
        ),
        "expected a typed embedding-lane refusal, got {refusal:?}"
    );
    for rendering in [refusal.to_string(), format!("{refusal:?}")] {
        assert!(
            !rendering.contains("sk-live-SECRET"),
            "the refusal repeated the credential: {rendering}"
        );
        assert!(
            !rendering.contains("voyage.proxy.internal"),
            "the refusal repeated the endpoint: {rendering}"
        );
    }

    let stored = memcore::db::model_catalog::list_model_deployments_by_source(
        &conn,
        memcore::catalog::CatalogSource::Env,
    )
    .expect("rows read");
    assert!(
        stored.is_empty(),
        "a refused embedding import must leave the catalog untouched"
    );
}

// ─── the type is sealed: no hand-assembled mismatch ─────────────────────────

#[test]
fn the_fallible_constructor_refuses_what_from_env_would_have_refused() {
    // The bypass this closes: with public fields, anyone could assemble a
    // configuration that never met `validate_against_index` — including the
    // catalog import, which would then have published a dimension declaration
    // the process never agreed to. `declared` is the only other way in, and it
    // runs the same gate.
    let err = EmbeddingConfig::declared("some-2048-dim-model", 2048)
        .expect_err("a width the stored index disagrees with must be refused");
    assert!(err.contains("2048"), "{err}");
    assert!(err.contains(&STORED_INDEX_DIMENSION.to_string()), "{err}");

    let ok = EmbeddingConfig::declared("voyage-3-large", STORED_INDEX_DIMENSION)
        .expect("a same-width sibling is exactly what the hatch allows");
    assert_eq!(ok.model(), "voyage-3-large");
    assert_eq!(ok.dimension(), STORED_INDEX_DIMENSION);
}

#[test]
fn the_constructor_derives_source_rather_than_letting_a_caller_declare_it() {
    // `source` is what status uses to tell an operator's deliberate swap from
    // the default. A caller that could *say* "not an override" about a
    // non-default model would make that surface lie, so the rule is derived
    // from the model name — the same rule `from_env` applies.
    let default_named = EmbeddingConfig::declared(DEFAULT_EMBEDDING_MODEL, STORED_INDEX_DIMENSION)
        .expect("naming the default is legal");
    assert_eq!(
        default_named.source(),
        EmbeddingModelSource::Default,
        "naming the default is not an override, whichever constructor was used"
    );

    let swapped = EmbeddingConfig::declared("voyage-3-large", STORED_INDEX_DIMENSION)
        .expect("a same-width swap is legal");
    assert_eq!(swapped.source(), EmbeddingModelSource::EnvOverride);
}

#[test]
fn from_env_and_declared_agree_on_the_same_inputs() {
    // Two constructors are two chances to disagree. They must produce the same
    // value for the same model/width, or the catalog row and the status
    // surface can describe the same configuration differently.
    let _lock = crate::test_support::global_test_lock().lock();
    let _cleared = cleared_env();
    let _model = EnvRestore::set(EMBEDDING_MODEL_ENV, "voyage-3-large");
    let _dimension = EnvRestore::set(
        EMBEDDING_DIMENSION_ENV,
        STORED_INDEX_DIMENSION.to_string().as_str(),
    );

    assert_eq!(
        EmbeddingConfig::from_env().expect("resolves"),
        EmbeddingConfig::declared("voyage-3-large", STORED_INDEX_DIMENSION).expect("declared"),
    );
}
