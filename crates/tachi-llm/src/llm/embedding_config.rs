//! The guarded escape hatch for the embedding model (tachi#1681 D3).
//!
//! `embedding.rs` hard-coded `"voyage-4"` in the request body and `1024` in
//! the response parser, with no way to change either. The census called that
//! a stray surface; the design's ruling is that a *bare* env override would be
//! worse than the hard-code, because changing the embedding model changes
//! vector dimensionality and silently corrupts comparability with every
//! vector already in the index. Nothing would fail — searches would just
//! quietly get worse.
//!
//! So the hatch carries a **dimension declaration**, and the declaration is
//! checked:
//!
//! 1. A model override without [`EMBEDDING_DIMENSION_ENV`] is refused. You
//!    cannot name a new embedding model without saying how wide it is.
//! 2. A declared dimension that does not match the width the stored index was
//!    built at is refused. This is the guard the design calls "fails loudly":
//!    it fires at config resolution, before a single mismatched vector is
//!    written.
//!
//! Same-width swaps — the case the hatch actually exists for, e.g. moving to
//! a sibling model that also emits 1024 dimensions — go through untouched.
//! A genuine width change additionally needs a reindex path, which does not
//! exist in this repo; [`STORED_INDEX_DIMENSION`] is the one place that would
//! move when it does.

/// Override the embedding model. Requires [`EMBEDDING_DIMENSION_ENV`].
pub const EMBEDDING_MODEL_ENV: &str = "TACHI_EMBEDDING_MODEL";
/// Declare the output width of the configured embedding model.
pub const EMBEDDING_DIMENSION_ENV: &str = "TACHI_EMBEDDING_DIM";

/// The model every stored vector in this workspace was produced by.
pub const DEFAULT_EMBEDDING_MODEL: &str = "voyage-4";
/// Its output width.
pub const DEFAULT_EMBEDDING_DIMENSION: u32 = 1024;

/// The width the stored vector index was built at.
///
/// Pinned equal to `tachi-server`'s `status_ops::EXPECTED_EMBEDDING_DIM` by a
/// test in that crate — the constant lives in both places because neither
/// crate may depend on the other's internals, and a silent divergence between
/// "what we embed at" and "what we expect stored" is exactly the corruption
/// this module exists to prevent.
pub const STORED_INDEX_DIMENSION: u32 = 1024;

/// Where the configured embedding model came from. Reported on status
/// surfaces so an operator can tell a default from a deliberate override
/// without reading the process environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingModelSource {
    /// No override set: the built-in default.
    Default,
    /// [`EMBEDDING_MODEL_ENV`] named a model.
    EnvOverride,
}

impl EmbeddingModelSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::EnvOverride => "env_override",
        }
    }
}

/// Resolved embedding configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingConfig {
    pub model: String,
    /// The width this model emits, as declared. Never inferred from a
    /// response: inferring it would mean discovering the mismatch *after*
    /// writing vectors at the wrong width.
    pub dimension: u32,
    pub source: EmbeddingModelSource,
}

impl EmbeddingConfig {
    /// The built-in default, with no env read. The construction seam for
    /// callers that want deterministic config (tests, programmatic builders),
    /// mirroring `LlmClient::new_with_config`'s rationale.
    pub fn default_voyage() -> Self {
        Self {
            model: DEFAULT_EMBEDDING_MODEL.to_string(),
            dimension: DEFAULT_EMBEDDING_DIMENSION,
            source: EmbeddingModelSource::Default,
        }
    }

    /// Resolve from env, **failing closed** on:
    ///
    /// - a model override with no declared dimension,
    /// - a dimension that is not a positive integer,
    /// - a declared dimension that disagrees with [`STORED_INDEX_DIMENSION`].
    ///
    /// The last one is the whole point: it is what turns "silently corrupt
    /// every future search" into "the process refuses to start".
    pub fn from_env() -> Result<Self, String> {
        let model_override = env_value(EMBEDDING_MODEL_ENV);
        let declared_dimension = match env_value(EMBEDDING_DIMENSION_ENV) {
            None => None,
            Some(raw) => Some(raw.parse::<u32>().ok().filter(|width| *width > 0).ok_or(
                format!("{EMBEDDING_DIMENSION_ENV} must be a positive integer, got '{raw}'"),
            )?),
        };

        let (model, source) = match model_override {
            Some(model) if model != DEFAULT_EMBEDDING_MODEL => {
                (model, EmbeddingModelSource::EnvOverride)
            }
            // Explicitly naming the default is not an override; it resolves to
            // the same configuration, which keeps `source` honest.
            Some(_) | None => (
                DEFAULT_EMBEDDING_MODEL.to_string(),
                EmbeddingModelSource::Default,
            ),
        };

        let dimension = match (source, declared_dimension) {
            (EmbeddingModelSource::EnvOverride, None) => {
                return Err(format!(
                    "{EMBEDDING_MODEL_ENV}='{model}' overrides the embedding model but does not \
                     declare its output dimension: set {EMBEDDING_DIMENSION_ENV}. Changing the \
                     embedding model changes vector width, which silently breaks comparability \
                     with every vector already stored."
                ))
            }
            (_, Some(declared)) => declared,
            (EmbeddingModelSource::Default, None) => DEFAULT_EMBEDDING_DIMENSION,
        };

        let config = Self {
            model,
            dimension,
            source,
        };
        config.validate_against_index(STORED_INDEX_DIMENSION)?;
        Ok(config)
    }

    /// Refuse a configuration whose declared width does not match an index.
    ///
    /// Separate from [`Self::from_env`] so a status surface can also check the
    /// *observed* dimension of a particular database, not just the compiled
    /// expectation.
    pub fn validate_against_index(&self, index_dimension: u32) -> Result<(), String> {
        if self.dimension == index_dimension {
            return Ok(());
        }
        Err(format!(
            "embedding model '{}' declares {} dimensions but the stored vector index is {} \
             dimensions. Vectors of different widths are not comparable, so this configuration \
             would silently degrade every recall instead of failing. Reindex before changing \
             embedding width.",
            self.model, self.dimension, index_dimension
        ))
    }
}

fn env_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
