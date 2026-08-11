//! Model-broker seam — the five frozen types shared by #1681 (operational
//! resolver / catalog) and #1682 (provider wire adapters / gateway).
//!
//! # Why this module exists
//!
//! #1681 and #1682 are two Broker chains developed in parallel. #1682 must be
//! able to build and test a gateway/executor *before* #1681's real resolver
//! (#1681 PR-D) lands, and #1681 must know the exact shape #1682 will report
//! health back in. The only way both can proceed independently is if the
//! contract between them — the resolver's output vocabulary, the resolved
//! deployment shape, and the health report shape — is frozen first, in one
//! small module both leaves depend on. That is this file.
//!
//! # The one architectural invariant (codex #1681 review, OK-BUT-8)
//!
//! Every type here is **memcore-native plain data**. This module imports
//! **nothing** from tachi-server, tachi-llm, or tachi-dispatch. The operational
//! resolver lives in memcore precisely because memcore is the only node below
//! both tachi-llm (which calls the resolver to route) and tachi-server (which
//! projects HTTP ↔ canonical then calls it) — see the `resolve_auth_ref`
//! placement rationale in `store::vault_accounts`. If the resolver's *input*
//! types were owned by any of those upper crates, the dependency would invert.
//! So the snapshot types the resolver consumes ([`CatalogSnapshot`],
//! [`HealthSnapshot`], [`AccountSnapshot`], the budget / pin / retry contexts)
//! are all defined here, as memcore-owned data.
//!
//! The `module_has_no_external_tachi_crate_imports` test structurally pins
//! this: the source text carries neither an import of a tachi-prefixed crate nor
//! a fully-qualified path into one. Its needles are assembled at runtime so the
//! test's own source cannot match them — and for the same reason no prose in
//! this file may spell those paths out (a doc comment that did exactly that is
//! what tripped the check at `cba796ad`).
//!
//! # Three disciplines this module holds (codex PR #1739 rework)
//!
//! 1. **Validated construction is not bypassable.** Every type carrying an
//!    invariant ([`ModelRef`], [`ResolvedDeployment`], [`ResolutionRevisions`],
//!    [`ResolutionOutcome`], [`HealthObservation`]) has private fields, one
//!    fallible constructor, and read-only accessors — and its `Deserialize` is
//!    routed through that same constructor via a shadow type
//!    (`#[serde(try_from = ...)]`), so a JSON payload cannot mint a value the
//!    constructor would have refused. Types with no invariant (the snapshots,
//!    [`DeploymentCapabilities`], [`DeploymentBounds`], [`BudgetEstimate`],
//!    [`CandidateEvaluation`]) keep public fields and say so.
//! 2. **Frozen spellings are pinned by literal goldens.** Every enum here
//!    declares its wire spelling once, next to the variant
//!    (`#[serde(rename = ...)]`), and `as_str()` returns the same string; the
//!    `*_serde_spelling_*` tests assert serde's output equals `as_str()` *and*
//!    equals a literal golden, so neither side can drift alone. Round-trip
//!    tests alone would not have caught `OpenAiCompat` serializing as
//!    `open_ai_compat` while `as_str()` said `openai_compat` (codex BUG-6).
//! 3. **Failure is typed and loud.** An unknown or ambiguous alias, a drifted
//!    policy revision, or an empty admitted set each get their own
//!    [`AbstainReason`]; every filter axis — including the
//!    [`DeploymentBounds`] axes — gets its own [`ExclusionReason`]. Nothing
//!    degrades silently into "some default model".
//!
//! # What is deliberately *not* here
//!
//! - No SQL, no DDL, no table row types. This is the seam contract, not the
//!   #1681 catalog schema (that is #1681 PR-A). [`ResolvedDeployment`] is the
//!   *resolved projection* a resolver hands out, not the `model_deployments`
//!   row.
//! - No wire/HTTP types. Canonical request / stream-event / disposition / usage
//!   vocabularies are #1682's property (tachi-llm broker module).
//! - No real resolver. [`StaticFixtureResolver`] is a deterministic stand-in so
//!   #1682 can develop against a stable [`OperationalResolver`] before #1681's
//!   real implementation exists — that is the entire point of a seam. It is
//!   gated behind `feature = "broker-fixtures"` (default off) so it cannot be
//!   reached from a production build.

use serde::{Deserialize, Serialize};

/// The bound on a resolution's fallback order.
///
/// Matches the #1680 fallback cap-4 + label-collapse provenance discipline
/// (`types.rs:252-267`): a durable chain of alternatives is deliberately
/// bounded so a resolution can never carry an unbounded provider-derived list.
pub const FALLBACK_ORDER_CAP: usize = 4;

// ---------------------------------------------------------------------------
// 0. SeamError
// ---------------------------------------------------------------------------

/// Errors from constructing or validating a seam value.
///
/// Local to the seam and memcore-native: the seam does not reach into
/// `crate::error::MemoryError`, so a downstream crate can depend on the seam
/// without pulling the full memcore error surface.
///
/// Every variant is reachable from *both* a constructor call and a
/// `Deserialize` of the same type — that identity is the point of the shadow
/// types, and the `*_deserialize_rejects_*` tests assert it payload by payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeamError {
    /// A required opaque reference / revision / timestamp string was empty or
    /// whitespace-only. `field` names the offending field.
    EmptyField {
        /// The field that was blank, in its wire spelling.
        field: &'static str,
    },
    /// A fallback order exceeded [`FALLBACK_ORDER_CAP`].
    FallbackOrderTooLong {
        /// The rejected length.
        len: usize,
    },
    /// [`Selection::Chosen`] named a deployment that is not present in the
    /// outcome's candidate list as an *eligible* candidate. A resolution may
    /// only choose something it evaluated and admitted.
    ChosenNotEligible {
        /// The chosen deployment id that was absent or excluded.
        deployment_id: String,
    },
    /// `ResolutionOutcome::account_ref` disagreed with the chosen deployment's
    /// own `account_ref`. The outcome's copy exists for consumers that never
    /// look at the deployment; it must never diverge from it.
    AccountRefMismatch,
    /// An abstaining outcome carried an `account_ref`. Nothing was chosen, so
    /// no credential/account was selected.
    AbstainCarriesAccountRef,
    /// An abstaining outcome carried a fallback order. Nothing was chosen, so
    /// there is nothing to fall back *from*.
    AbstainCarriesFallbackOrder,
    /// A fallback entry named something other than an eligible, non-chosen
    /// candidate. The fallback chain becomes durable receipt provenance
    /// (#1682 discrimination 8), so it may only contain ids this resolution
    /// actually evaluated and admitted.
    FallbackEntryNotEligible {
        /// The offending fallback entry.
        deployment_id: String,
    },
}

impl std::fmt::Display for SeamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyField { field } => write!(f, "`{field}` must not be empty"),
            Self::FallbackOrderTooLong { len } => {
                write!(f, "fallback order {len} exceeds cap {FALLBACK_ORDER_CAP}")
            }
            Self::ChosenNotEligible { deployment_id } => write!(
                f,
                "chosen deployment `{deployment_id}` is not an eligible candidate of this resolution"
            ),
            Self::AccountRefMismatch => write!(
                f,
                "outcome account_ref must equal the chosen deployment's account_ref"
            ),
            Self::AbstainCarriesAccountRef => {
                write!(f, "an abstaining resolution must not carry an account_ref")
            }
            Self::AbstainCarriesFallbackOrder => write!(
                f,
                "an abstaining resolution must not carry a fallback order"
            ),
            Self::FallbackEntryNotEligible { deployment_id } => write!(
                f,
                "fallback entry `{deployment_id}` is not an eligible, non-chosen candidate"
            ),
        }
    }
}

impl std::error::Error for SeamError {}

/// Shared blank-string guard. Whitespace-only counts as empty: an opaque
/// reference made of spaces is a typo, not an identifier.
fn require_non_empty(value: &str, field: &'static str) -> Result<(), SeamError> {
    if value.trim().is_empty() {
        return Err(SeamError::EmptyField { field });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 1. ModelRef
// ---------------------------------------------------------------------------

/// An opaque reference to a model, resolved *against a specific alias-set policy
/// revision*.
///
/// It may name an alias (`memory.chat`) or a stable model reference — the
/// newtype is deliberately opaque, so callers cannot branch on "is this an
/// alias" at the type level; that classification is the resolver's job. The
/// paired `policy_revision` is the alias-set policy revision (a canonical-JSON
/// content digest per #1681 D2) the reference is meaningful under: a `ModelRef`
/// carries the revision it was minted against so a resolution can assert
/// `stamped == recomputed` rather than resolving against a drifted alias set
/// (a mismatch abstains with [`AbstainReason::PolicyRevisionMismatch`]).
///
/// Fields are private and `Deserialize` is routed through [`ModelRef::new`], so
/// neither a struct literal nor a JSON payload can produce a blank reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ModelRefWire")]
pub struct ModelRef {
    reference: String,
    policy_revision: String,
}

/// Deserialization shadow for [`ModelRef`] — same wire shape, no invariant;
/// the `TryFrom` below is the only way out of it.
#[derive(Deserialize)]
struct ModelRefWire {
    reference: String,
    policy_revision: String,
}

impl TryFrom<ModelRefWire> for ModelRef {
    type Error = SeamError;

    fn try_from(wire: ModelRefWire) -> Result<Self, Self::Error> {
        Self::new(wire.reference, wire.policy_revision)
    }
}

impl ModelRef {
    /// Construct a validated `ModelRef`.
    ///
    /// # Errors
    ///
    /// [`SeamError::EmptyField`] if `reference` or `policy_revision` is empty or
    /// whitespace-only.
    pub fn new(
        reference: impl Into<String>,
        policy_revision: impl Into<String>,
    ) -> Result<Self, SeamError> {
        let reference = reference.into();
        let policy_revision = policy_revision.into();
        require_non_empty(&reference, "reference")?;
        require_non_empty(&policy_revision, "policy_revision")?;
        Ok(Self {
            reference,
            policy_revision,
        })
    }

    /// The opaque reference string (alias name or stable model reference).
    pub fn reference(&self) -> &str {
        &self.reference
    }

    /// The alias-set policy revision this reference was minted against.
    pub fn policy_revision(&self) -> &str {
        &self.policy_revision
    }
}

// ---------------------------------------------------------------------------
// 2. ResolvedDeployment
// ---------------------------------------------------------------------------

/// The wire dialect a deployment speaks. Closed set: the six provider grammars
/// the #1682 census names, plus an explicit `Unknown` for a catalogued
/// deployment whose grammar this build cannot operate.
///
/// `Unknown` is deliberate rather than absent — "we catalogued it and cannot
/// speak to it" must be distinguishable from "we never looked", the same rule
/// as `AuthMode::Unsupported`.
///
/// Each variant carries an explicit `rename` rather than relying on
/// `rename_all = "snake_case"`: serde's snake_case of `OpenAiCompat` is
/// `open_ai_compat`, which silently disagreed with `as_str()`'s
/// `openai_compat` (codex #1739 BUG-6). The spelling is now declared once, next
/// to the variant, and `wire_dialect_serde_spelling_matches_as_str_and_golden`
/// pins it against a literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireDialect {
    /// OpenAI `/v1/chat/completions` grammar (SiliconFlow / DeepSeek / ZAI /
    /// OpenAI itself all speak it today).
    #[serde(rename = "openai_compat")]
    OpenAiCompat,
    /// Anthropic messages grammar (`content_block_delta` / `tool_use`).
    #[serde(rename = "anthropic")]
    Anthropic,
    /// xAI grammar.
    #[serde(rename = "xai")]
    Xai,
    /// OpenRouter grammar.
    #[serde(rename = "open_router")]
    OpenRouter,
    /// A generic OpenAI-compatible endpoint that is none of the named vendors.
    #[serde(rename = "generic_compat")]
    GenericCompat,
    /// Ollama `/api/generate` (NDJSON stream, not SSE).
    #[serde(rename = "ollama")]
    Ollama,
    /// Catalogued but the wire grammar is not operable by this build.
    #[serde(rename = "unknown")]
    Unknown,
}

impl WireDialect {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompat => "openai_compat",
            Self::Anthropic => "anthropic",
            Self::Xai => "xai",
            Self::OpenRouter => "open_router",
            Self::GenericCompat => "generic_compat",
            Self::Ollama => "ollama",
            Self::Unknown => "unknown",
        }
    }

    /// Parse a frozen wire spelling. `None` for anything unrecognised — an
    /// unknown *spelling* is not the same as the `Unknown` *dialect*, and this
    /// seam never collapses the two.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "openai_compat" => Self::OpenAiCompat,
            "anthropic" => Self::Anthropic,
            "xai" => Self::Xai,
            "open_router" => Self::OpenRouter,
            "generic_compat" => Self::GenericCompat,
            "ollama" => Self::Ollama,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [WireDialect] = &[
        Self::OpenAiCompat,
        Self::Anthropic,
        Self::Xai,
        Self::OpenRouter,
        Self::GenericCompat,
        Self::Ollama,
        Self::Unknown,
    ];
}

/// What a deployment can do. The capability axes #1681 D1 lists for the
/// `capabilities` JSON column, restated as a flat plain-data projection.
///
/// No invariant, so the fields stay public: every combination of booleans is a
/// legal deployment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentCapabilities {
    /// Chat/completions lane.
    pub chat: bool,
    /// Embeddings lane.
    pub embeddings: bool,
    /// Tool/function calling.
    pub tools: bool,
    /// Incremental (SSE / NDJSON) streaming.
    pub streaming: bool,
    /// Schema-constrained output.
    pub structured_output: bool,
    /// Non-text attachments.
    pub media: bool,
}

/// A deployment's context / output / attachment bounds.
///
/// `embedding_dimensions` is the #1681 D3 dimension declaration: an embed
/// deployment carries the vector dimensionality it produces, so an override
/// whose dimension mismatches the stored index fails loudly instead of silently
/// corrupting comparability. All fields are optional — a chat deployment has no
/// embedding dimension, a deployment whose bounds are unknown carries `None`.
///
/// Each axis has a matching [`ExclusionReason`], so "the request did not fit"
/// is always attributable to one bound rather than to a generic mismatch.
/// No invariant, so the fields stay public.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentBounds {
    /// Total context window in tokens.
    pub context_window: Option<u32>,
    /// Maximum generated tokens per response.
    pub max_output: Option<u32>,
    /// Maximum attachment payload in bytes.
    pub attachment_bytes: Option<u64>,
    /// Produced embedding dimensionality (#1681 D3 dimension declaration).
    pub embedding_dimensions: Option<u32>,
}

/// Constructor input for [`ResolvedDeployment`], and its deserialization shadow.
///
/// One type serves both roles deliberately: it *is* the wire shape, so
/// `ResolvedDeployment::new(parts)` and `serde_json::from_str::<ResolvedDeployment>`
/// run the identical validation, and there is no second field list to drift.
/// A parts struct rather than an eight-argument constructor keeps the call site
/// readable (and stays under clippy's argument threshold).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResolvedDeploymentParts {
    /// Catalog-controlled deployment identity.
    pub deployment_id: String,
    /// The grammar this deployment speaks.
    pub wire_dialect: WireDialect,
    /// Opaque endpoint reference.
    pub endpoint_ref: String,
    /// The provider's own model id sent on the wire.
    pub provider_model_id: String,
    /// What the deployment can do.
    pub capabilities: DeploymentCapabilities,
    /// Context / output / attachment bounds.
    pub bounds: DeploymentBounds,
    /// Opaque #1680 auth/account reference.
    pub account_ref: String,
    /// Content-addressed pricing snapshot id, or `None`.
    pub pricing_snapshot_ref: Option<String>,
}

/// A single already-resolved deployment — what a resolver hands out for one
/// concrete route.
///
/// This is the *resolved projection*, not the `model_deployments` row: it
/// carries no health columns (health is a separate authority, #1681 D4), no
/// catalog bookkeeping (`fetched_at`/`expires_at`/`status`), and no secret —
/// `account_ref` is the opaque #1680 `auth_ref`, never a Vault member name.
///
/// Fields are private; build one with [`ResolvedDeployment::new`] (or
/// deserialize, which runs the same validation) and read it back through the
/// accessors. [`ResolvedDeployment::into_parts`] gives a mutable copy of the
/// wire shape for consumers that need to vary one field — re-validated on the
/// way back in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ResolvedDeploymentParts")]
pub struct ResolvedDeployment {
    deployment_id: String,
    wire_dialect: WireDialect,
    endpoint_ref: String,
    provider_model_id: String,
    capabilities: DeploymentCapabilities,
    bounds: DeploymentBounds,
    account_ref: String,
    pricing_snapshot_ref: Option<String>,
}

impl TryFrom<ResolvedDeploymentParts> for ResolvedDeployment {
    type Error = SeamError;

    fn try_from(parts: ResolvedDeploymentParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}

impl ResolvedDeployment {
    /// Construct a validated `ResolvedDeployment`.
    ///
    /// # Errors
    ///
    /// [`SeamError::EmptyField`] if `deployment_id`, `endpoint_ref`,
    /// `provider_model_id`, `account_ref`, or a present `pricing_snapshot_ref`
    /// is blank. A *missing* price (`None`) is legal — the catalog may not have
    /// one yet; a blank one is a bug.
    pub fn new(parts: ResolvedDeploymentParts) -> Result<Self, SeamError> {
        require_non_empty(&parts.deployment_id, "deployment_id")?;
        require_non_empty(&parts.endpoint_ref, "endpoint_ref")?;
        require_non_empty(&parts.provider_model_id, "provider_model_id")?;
        require_non_empty(&parts.account_ref, "account_ref")?;
        if let Some(price) = parts.pricing_snapshot_ref.as_deref() {
            require_non_empty(price, "pricing_snapshot_ref")?;
        }
        Ok(Self {
            deployment_id: parts.deployment_id,
            wire_dialect: parts.wire_dialect,
            endpoint_ref: parts.endpoint_ref,
            provider_model_id: parts.provider_model_id,
            capabilities: parts.capabilities,
            bounds: parts.bounds,
            account_ref: parts.account_ref,
            pricing_snapshot_ref: parts.pricing_snapshot_ref,
        })
    }

    /// Catalog-controlled deployment identity (closed vocabulary; the value the
    /// receipt provenance chain records — not provider error text).
    pub fn deployment_id(&self) -> &str {
        &self.deployment_id
    }

    /// The grammar this deployment speaks.
    pub fn wire_dialect(&self) -> WireDialect {
        self.wire_dialect
    }

    /// Opaque endpoint reference (an id/handle, resolved to a base_url by the
    /// executor — not the URL itself at this layer).
    pub fn endpoint_ref(&self) -> &str {
        &self.endpoint_ref
    }

    /// The provider's own model id sent on the wire.
    pub fn provider_model_id(&self) -> &str {
        &self.provider_model_id
    }

    /// What the deployment can do.
    pub fn capabilities(&self) -> DeploymentCapabilities {
        self.capabilities
    }

    /// Context / output / attachment bounds.
    pub fn bounds(&self) -> DeploymentBounds {
        self.bounds
    }

    /// Opaque #1680 auth/account reference. Never a credential member name.
    pub fn account_ref(&self) -> &str {
        &self.account_ref
    }

    /// Content-addressed pricing snapshot id (#1681 D1), or `None` when the
    /// catalog has no price for this deployment yet.
    pub fn pricing_snapshot_ref(&self) -> Option<&str> {
        self.pricing_snapshot_ref.as_deref()
    }

    /// Decompose into the (unvalidated) wire shape, for consumers that need to
    /// vary one field and rebuild through [`ResolvedDeployment::new`].
    pub fn into_parts(self) -> ResolvedDeploymentParts {
        ResolvedDeploymentParts {
            deployment_id: self.deployment_id,
            wire_dialect: self.wire_dialect,
            endpoint_ref: self.endpoint_ref,
            provider_model_id: self.provider_model_id,
            capabilities: self.capabilities,
            bounds: self.bounds,
            account_ref: self.account_ref,
            pricing_snapshot_ref: self.pricing_snapshot_ref,
        }
    }
}

// ---------------------------------------------------------------------------
// 3. ResolutionOutcome (codex #1682 BUG-10 full vocabulary)
// ---------------------------------------------------------------------------

/// Why a candidate deployment was excluded from selection. One variant per
/// filter axis the #1681 D5 resolver applies; the closed set is exhaustively
/// exercised by `exclusion_reason_variants_are_exhaustively_constructible`.
///
/// **Scope of this vocabulary.** It covers candidates the resolver *saw* — i.e.
/// the contents of [`ResolverInput::admitted_candidates`]. A deployment that a
/// pre-resolver semantic gate cut never appears here at all: gates cut the set,
/// they do not report beside it (#1675 PR2), and reporting on invisible
/// candidates would be fabrication. That is why there is no "not admitted"
/// variant for the *semantic* gate — the only admission axis the resolver
/// evaluates itself is account availability
/// ([`ExclusionReason::AccountNotAdmitted`], read from [`AccountSnapshot`],
/// a separate #1680 authority from the semantic gate).
///
/// The four bounds axes mirror [`DeploymentBounds`] one-for-one so an
/// over-long prompt is never reported as a generic capability mismatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExclusionReason {
    /// The candidate's capabilities do not meet the request (e.g. no
    /// `structured_output`, no `tools`).
    #[serde(rename = "capability_mismatch")]
    CapabilityMismatch,
    /// The request does not fit the candidate's `context_window`.
    #[serde(rename = "context_window_exceeded")]
    ContextWindowExceeded,
    /// The requested output length exceeds the candidate's `max_output`.
    #[serde(rename = "max_output_exceeded")]
    MaxOutputExceeded,
    /// The request's attachments exceed the candidate's `attachment_bytes`.
    #[serde(rename = "attachment_bounds_exceeded")]
    AttachmentBoundsExceeded,
    /// The candidate's `embedding_dimensions` disagrees with the dimensionality
    /// the stored index requires (#1681 D3: a dimension mismatch must fail
    /// loudly rather than silently corrupt comparability).
    #[serde(rename = "embedding_dimension_mismatch")]
    EmbeddingDimensionMismatch,
    /// The candidate's `region` is not permitted for this request.
    #[serde(rename = "region_blocked")]
    RegionBlocked,
    /// The candidate's `data_policy` is not permitted for this request.
    #[serde(rename = "data_policy_blocked")]
    DataPolicyBlocked,
    /// Selecting the candidate would exceed the budget ceiling in context.
    #[serde(rename = "budget_exceeded")]
    BudgetExceeded,
    /// The candidate's deployment health is in cooldown (429/quota/timeout/5xx
    /// per #1681 D4).
    #[serde(rename = "health_cooldown")]
    HealthCooldown,
    /// The candidate comes from a catalog snapshot past `expires_at` — stale and
    /// non-authoritative (#1681 D3/PR-B).
    #[serde(rename = "stale_catalog")]
    StaleCatalog,
    /// The candidate's account is not admitted to serve this request (#1680
    /// account authority, read from [`AccountSnapshot`] — distinct from the
    /// semantic gate that produced the admitted candidate set).
    #[serde(rename = "account_not_admitted")]
    AccountNotAdmitted,
    /// The candidate's deployment `status` is not active (retired/disabled).
    #[serde(rename = "deployment_inactive")]
    DeploymentInactive,
}

impl ExclusionReason {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CapabilityMismatch => "capability_mismatch",
            Self::ContextWindowExceeded => "context_window_exceeded",
            Self::MaxOutputExceeded => "max_output_exceeded",
            Self::AttachmentBoundsExceeded => "attachment_bounds_exceeded",
            Self::EmbeddingDimensionMismatch => "embedding_dimension_mismatch",
            Self::RegionBlocked => "region_blocked",
            Self::DataPolicyBlocked => "data_policy_blocked",
            Self::BudgetExceeded => "budget_exceeded",
            Self::HealthCooldown => "health_cooldown",
            Self::StaleCatalog => "stale_catalog",
            Self::AccountNotAdmitted => "account_not_admitted",
            Self::DeploymentInactive => "deployment_inactive",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "capability_mismatch" => Self::CapabilityMismatch,
            "context_window_exceeded" => Self::ContextWindowExceeded,
            "max_output_exceeded" => Self::MaxOutputExceeded,
            "attachment_bounds_exceeded" => Self::AttachmentBoundsExceeded,
            "embedding_dimension_mismatch" => Self::EmbeddingDimensionMismatch,
            "region_blocked" => Self::RegionBlocked,
            "data_policy_blocked" => Self::DataPolicyBlocked,
            "budget_exceeded" => Self::BudgetExceeded,
            "health_cooldown" => Self::HealthCooldown,
            "stale_catalog" => Self::StaleCatalog,
            "account_not_admitted" => Self::AccountNotAdmitted,
            "deployment_inactive" => Self::DeploymentInactive,
            _ => return None,
        })
    }

    /// Every filter axis, in declaration order. The exhaustiveness test asserts
    /// this slice covers the enum.
    pub const ALL: &'static [ExclusionReason] = &[
        Self::CapabilityMismatch,
        Self::ContextWindowExceeded,
        Self::MaxOutputExceeded,
        Self::AttachmentBoundsExceeded,
        Self::EmbeddingDimensionMismatch,
        Self::RegionBlocked,
        Self::DataPolicyBlocked,
        Self::BudgetExceeded,
        Self::HealthCooldown,
        Self::StaleCatalog,
        Self::AccountNotAdmitted,
        Self::DeploymentInactive,
    ];
}

/// Why a resolution selected nothing.
///
/// Abstain is a terminal, not an error — but it is never *anonymous*. The
/// alias-side variants exist because the seam's governing rule is that an
/// unknown or ambiguous alias must fail loudly: the resolver may not quietly
/// pick "some default model" when the reference it was handed does not resolve
/// to exactly one binding. A consumer that receives
/// [`AbstainReason::UnknownAlias`] has a typed, reportable fact; a consumer that
/// received a silently substituted deployment would not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbstainReason {
    /// The admitted candidate set was empty: an upstream gate cut everything,
    /// so the resolver never had anything to evaluate. Distinct from
    /// [`AbstainReason::NoEligibleCandidate`], where the resolver did evaluate
    /// candidates and its own filters dropped them all.
    #[serde(rename = "empty_candidate_set")]
    EmptyCandidateSet,
    /// Candidates were evaluated and every one was excluded; the per-candidate
    /// [`ExclusionReason`]s on the outcome say which axis dropped each.
    #[serde(rename = "no_eligible_candidate")]
    NoEligibleCandidate,
    /// The [`ModelRef`] named an alias that the alias set does not bind. Loud
    /// by construction — never a fallback to a default deployment.
    #[serde(rename = "unknown_alias")]
    UnknownAlias,
    /// The alias resolves to more than one binding with no deterministic
    /// winner. Also loud: guessing would make routing non-reproducible.
    #[serde(rename = "ambiguous_alias")]
    AmbiguousAlias,
    /// The [`ModelRef`]'s stamped `policy_revision` does not match the
    /// alias-set policy revision of the snapshot being resolved against
    /// (#1681 D2 `stamped == recomputed`). Resolving anyway would route against
    /// a drifted alias set.
    #[serde(rename = "policy_revision_mismatch")]
    PolicyRevisionMismatch,
}

impl AbstainReason {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EmptyCandidateSet => "empty_candidate_set",
            Self::NoEligibleCandidate => "no_eligible_candidate",
            Self::UnknownAlias => "unknown_alias",
            Self::AmbiguousAlias => "ambiguous_alias",
            Self::PolicyRevisionMismatch => "policy_revision_mismatch",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "empty_candidate_set" => Self::EmptyCandidateSet,
            "no_eligible_candidate" => Self::NoEligibleCandidate,
            "unknown_alias" => Self::UnknownAlias,
            "ambiguous_alias" => Self::AmbiguousAlias,
            "policy_revision_mismatch" => Self::PolicyRevisionMismatch,
            _ => return None,
        })
    }

    /// Every abstain reason, in declaration order.
    pub const ALL: &'static [AbstainReason] = &[
        Self::EmptyCandidateSet,
        Self::NoEligibleCandidate,
        Self::UnknownAlias,
        Self::AmbiguousAlias,
        Self::PolicyRevisionMismatch,
    ];
}

/// One candidate's disposition in a resolution: its identity, and — if it was
/// dropped — the single axis that dropped it. `exclusion: None` means the
/// candidate was eligible.
///
/// Plain data with public fields: it carries no invariant of its own. The
/// invariants that *involve* it (non-blank ids, chosen/fallback membership) are
/// enforced where they become load-bearing, in [`ResolutionOutcome::new`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateEvaluation {
    /// The candidate deployment's id.
    pub deployment_id: String,
    /// The axis that dropped it, or `None` if it survived.
    pub exclusion: Option<ExclusionReason>,
}

impl CandidateEvaluation {
    /// A candidate that survived every filter.
    pub fn eligible(deployment_id: impl Into<String>) -> Self {
        Self {
            deployment_id: deployment_id.into(),
            exclusion: None,
        }
    }

    /// A candidate dropped by one axis.
    pub fn excluded(deployment_id: impl Into<String>, reason: ExclusionReason) -> Self {
        Self {
            deployment_id: deployment_id.into(),
            exclusion: Some(reason),
        }
    }

    /// Whether this candidate survived every filter.
    pub fn is_eligible(&self) -> bool {
        self.exclusion.is_none()
    }
}

/// The three revisions a resolution is stamped against, so a consumer can
/// assert the resolution was computed over the inputs it thinks it was (#1681
/// D5). All are captured from the frozen input snapshot, never re-read.
///
/// Fields are private and `Deserialize` runs [`ResolutionRevisions::new`]: a
/// blank stamp would defeat the entire point of stamping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ResolutionRevisionsWire")]
pub struct ResolutionRevisions {
    catalog_revision: String,
    health_observed_at: String,
    policy_revision: String,
}

/// Deserialization shadow for [`ResolutionRevisions`].
#[derive(Deserialize)]
struct ResolutionRevisionsWire {
    catalog_revision: String,
    health_observed_at: String,
    policy_revision: String,
}

impl TryFrom<ResolutionRevisionsWire> for ResolutionRevisions {
    type Error = SeamError;

    fn try_from(wire: ResolutionRevisionsWire) -> Result<Self, Self::Error> {
        Self::new(
            wire.catalog_revision,
            wire.health_observed_at,
            wire.policy_revision,
        )
    }
}

impl ResolutionRevisions {
    /// Construct a validated stamp.
    ///
    /// # Errors
    ///
    /// [`SeamError::EmptyField`] if any of the three is blank.
    pub fn new(
        catalog_revision: impl Into<String>,
        health_observed_at: impl Into<String>,
        policy_revision: impl Into<String>,
    ) -> Result<Self, SeamError> {
        let catalog_revision = catalog_revision.into();
        let health_observed_at = health_observed_at.into();
        let policy_revision = policy_revision.into();
        require_non_empty(&catalog_revision, "catalog_revision")?;
        require_non_empty(&health_observed_at, "health_observed_at")?;
        require_non_empty(&policy_revision, "policy_revision")?;
        Ok(Self {
            catalog_revision,
            health_observed_at,
            policy_revision,
        })
    }

    /// The catalog snapshot revision the candidate metadata came from.
    pub fn catalog_revision(&self) -> &str {
        &self.catalog_revision
    }

    /// When the health snapshot used for cooldown admission was observed.
    pub fn health_observed_at(&self) -> &str {
        &self.health_observed_at
    }

    /// The alias-set policy revision the [`ModelRef`] was resolved under.
    pub fn policy_revision(&self) -> &str {
        &self.policy_revision
    }
}

/// A pre-invocation budget estimate for the chosen deployment. Cost computation
/// abstains until #1681's pricing catalog lands (D5/D6): the estimate carries
/// the pricing snapshot ref opaquely and leaves `estimated_cost_usd = None`
/// until a real price sheet is joined.
///
/// Plain data with public fields — every combination of `None`s is legal, which
/// is precisely the "abstain until priced" posture.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetEstimate {
    /// Estimated prompt tokens, if the caller supplied enough to estimate.
    pub estimated_prompt_tokens: Option<u32>,
    /// Estimated completion tokens, if bounded by the request.
    pub estimated_completion_tokens: Option<u32>,
    /// `None` until #1681 pricing is joined — self-reported vs computed cost
    /// stay distinguishable forever (D6).
    pub estimated_cost_usd: Option<f64>,
    /// The content-addressed price sheet the estimate would be computed from.
    pub pricing_snapshot_ref: Option<String>,
}

/// What the resolver selected: one deployment, or a typed abstain.
///
/// `Abstain` carries an [`AbstainReason`] — "nothing was selected" is never
/// reported without saying why (codex #1739 BUG-1). For
/// [`AbstainReason::NoEligibleCandidate`] the per-candidate reasons on the
/// outcome carry the detail; the alias/policy variants are facts about the
/// request itself, which no per-candidate reason could express.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum Selection {
    /// One deployment was selected.
    #[serde(rename = "chosen")]
    Chosen(ResolvedDeployment),
    /// Nothing was selected, for this reason.
    #[serde(rename = "abstain")]
    Abstain(AbstainReason),
}

impl Selection {
    /// The chosen deployment, if any.
    pub fn chosen(&self) -> Option<&ResolvedDeployment> {
        match self {
            Self::Chosen(d) => Some(d),
            Self::Abstain(_) => None,
        }
    }

    /// Whether this selection abstained.
    pub fn is_abstain(&self) -> bool {
        matches!(self, Self::Abstain(_))
    }

    /// Why this selection abstained, if it did.
    pub fn abstain_reason(&self) -> Option<AbstainReason> {
        match self {
            Self::Chosen(_) => None,
            Self::Abstain(reason) => Some(*reason),
        }
    }
}

/// The complete output of one resolution — the full BUG-10 vocabulary.
///
/// Every candidate the resolver considered is listed with its disposition (not
/// just the winner), so exclusion is visible rather than silent (#1675 PR2 /
/// `recommendation.rs` precedent).
///
/// Fields are private and every construction path — [`ResolutionOutcome::new`]
/// and `Deserialize` alike — enforces the same consistency set:
///
/// - `fallback_order` is bounded by [`FALLBACK_ORDER_CAP`];
/// - candidate ids are non-blank;
/// - a `Chosen` deployment must be present in `candidates` *as eligible*;
/// - `account_ref` must equal the chosen deployment's own `account_ref`;
/// - every fallback entry must be an eligible, non-chosen candidate (the chain
///   becomes durable receipt provenance, so it may not name anything this
///   resolution did not evaluate and admit);
/// - an abstaining outcome carries neither `account_ref` nor `fallback_order`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ResolutionOutcomeWire")]
pub struct ResolutionOutcome {
    candidates: Vec<CandidateEvaluation>,
    selection: Selection,
    revisions: ResolutionRevisions,
    account_ref: Option<String>,
    budget_estimate: BudgetEstimate,
    fallback_order: Vec<String>,
}

/// Deserialization shadow for [`ResolutionOutcome`].
#[derive(Deserialize)]
struct ResolutionOutcomeWire {
    candidates: Vec<CandidateEvaluation>,
    selection: Selection,
    revisions: ResolutionRevisions,
    account_ref: Option<String>,
    budget_estimate: BudgetEstimate,
    fallback_order: Vec<String>,
}

impl TryFrom<ResolutionOutcomeWire> for ResolutionOutcome {
    type Error = SeamError;

    fn try_from(wire: ResolutionOutcomeWire) -> Result<Self, Self::Error> {
        Self::new(
            wire.candidates,
            wire.selection,
            wire.revisions,
            wire.account_ref,
            wire.budget_estimate,
            wire.fallback_order,
        )
    }
}

impl ResolutionOutcome {
    /// Build a consistent outcome.
    ///
    /// # Errors
    ///
    /// [`SeamError::FallbackOrderTooLong`], [`SeamError::EmptyField`],
    /// [`SeamError::ChosenNotEligible`], [`SeamError::AccountRefMismatch`],
    /// [`SeamError::FallbackEntryNotEligible`],
    /// [`SeamError::AbstainCarriesAccountRef`], or
    /// [`SeamError::AbstainCarriesFallbackOrder`] — see the type docs for the
    /// consistency set each one guards.
    pub fn new(
        candidates: Vec<CandidateEvaluation>,
        selection: Selection,
        revisions: ResolutionRevisions,
        account_ref: Option<String>,
        budget_estimate: BudgetEstimate,
        fallback_order: Vec<String>,
    ) -> Result<Self, SeamError> {
        // Borrowing validation runs in a separate function so every borrow of
        // `candidates` has ended before the fields are moved into `Self`.
        validate_outcome_consistency(
            &candidates,
            &selection,
            account_ref.as_deref(),
            &fallback_order,
        )?;

        Ok(Self {
            candidates,
            selection,
            revisions,
            account_ref,
            budget_estimate,
            fallback_order,
        })
    }

    /// Every candidate the resolver saw, eligible and excluded alike.
    pub fn candidates(&self) -> &[CandidateEvaluation] {
        &self.candidates
    }

    /// The chosen deployment or a typed abstain.
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// Catalog / health / policy revisions this resolution was computed against.
    pub fn revisions(&self) -> &ResolutionRevisions {
        &self.revisions
    }

    /// The opaque #1680 account/credential ref of the chosen deployment;
    /// `None` on abstain.
    pub fn account_ref(&self) -> Option<&str> {
        self.account_ref.as_deref()
    }

    /// A pre-invocation budget estimate for the chosen deployment.
    pub fn budget_estimate(&self) -> &BudgetEstimate {
        &self.budget_estimate
    }

    /// Deployment ids to try in order if the chosen one fails, bounded by
    /// [`FALLBACK_ORDER_CAP`]. #1682 executes this order and records the chain
    /// truthfully into the receipt; it does not author it.
    pub fn fallback_order(&self) -> &[String] {
        &self.fallback_order
    }
}

/// The consistency set every [`ResolutionOutcome`] construction path runs —
/// constructor and `Deserialize` alike. Factored out so it is impossible for
/// one path to hold a weaker rule set than the other.
fn validate_outcome_consistency(
    candidates: &[CandidateEvaluation],
    selection: &Selection,
    account_ref: Option<&str>,
    fallback_order: &[String],
) -> Result<(), SeamError> {
    let mut eligible_ids: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for candidate in candidates {
        require_non_empty(&candidate.deployment_id, "candidate deployment_id")?;
        if candidate.is_eligible() {
            eligible_ids.insert(candidate.deployment_id.as_str());
        }
    }

    match selection {
        Selection::Chosen(deployment) => {
            if !eligible_ids.contains(deployment.deployment_id()) {
                return Err(SeamError::ChosenNotEligible {
                    deployment_id: deployment.deployment_id().to_string(),
                });
            }
            if account_ref != Some(deployment.account_ref()) {
                return Err(SeamError::AccountRefMismatch);
            }
            for entry in fallback_order {
                if entry == deployment.deployment_id() || !eligible_ids.contains(entry.as_str()) {
                    return Err(SeamError::FallbackEntryNotEligible {
                        deployment_id: entry.clone(),
                    });
                }
            }
        }
        Selection::Abstain(_) => {
            if account_ref.is_some() {
                return Err(SeamError::AbstainCarriesAccountRef);
            }
            if !fallback_order.is_empty() {
                return Err(SeamError::AbstainCarriesFallbackOrder);
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// 4. OperationalResolver + snapshot inputs + StaticFixtureResolver
// ---------------------------------------------------------------------------

/// Catalog-side facts the resolver reads (memcore-owned plain data). The
/// resolver never enumerates the catalog itself — it works over the already
/// admitted candidate set on [`ResolverInput`] — but it needs the catalog
/// snapshot's revision and freshness to stamp and to apply the stale axis.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSnapshot {
    /// The snapshot's revision, stamped onto the outcome.
    pub catalog_revision: String,
    /// Deployment ids the catalog considers stale (past `expires_at`).
    pub stale_deployment_ids: Vec<String>,
}

/// A per-deployment cooldown as observed by the health authority (#1681 D4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentCooldown {
    /// The deployment in cooldown.
    pub deployment_id: String,
    /// ISO timestamp the cooldown lifts, if bounded.
    pub cooldown_until: Option<String>,
}

/// Deployment-health facts the resolver reads. `observed_at` is stamped into
/// the outcome's revisions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthSnapshot {
    /// When these facts were observed (ISO-8601).
    pub observed_at: String,
    /// Per-deployment cooldowns in force at `observed_at`.
    pub cooldowns: Vec<DeploymentCooldown>,
}

/// Account-availability facts the resolver reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountAvailability {
    /// The opaque #1680 account reference.
    pub account_ref: String,
    /// Whether the account is admitted to serve this request.
    pub admitted: bool,
}

/// Account-side facts (memcore-owned plain data).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSnapshot {
    /// Availability per account reference.
    pub accounts: Vec<AccountAvailability>,
}

/// Budget context for this request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetContext {
    /// Hard ceiling in USD for this request, if any.
    pub ceiling_usd: Option<f64>,
}

/// A pin request: a caller-forced deployment that wins ordering when eligible
/// (`pin > health > price > deployment_id`, #1681 D5).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinContext {
    /// The deployment the caller pinned, if any.
    pub pinned_deployment_id: Option<String>,
}

/// The typed retry-context input slot (#1681 D7 discrimination 7 boundary):
/// which attempt this is and what failures preceded it. #1681's real resolver
/// uses it for fallback/backoff; the fixture ignores it but the slot is frozen
/// so streaming-continuation retry has a home.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryContext {
    /// 0 for the first attempt.
    pub attempt: u32,
    /// The health observations this request already produced.
    pub prior_failures: Vec<HealthObservation>,
}

/// The complete input to a resolution: the already-admitted candidate set plus
/// the frozen snapshots and contexts. All memcore-owned data — there is no
/// server/llm/dispatch policy type reachable from here.
///
/// **What "admitted" means here** (codex #1739 BUG-1): `admitted_candidates`
/// holds the deployments a *semantic* admission gate already passed — the
/// resolver receives the cut set and has no catalog handle to enumerate more.
/// That is the structural enforcement of "healthy cheap ≠ semantically
/// eligible" (#1681 D4): not a runtime check inside the resolver, but the
/// absence of any way for it to widen its own input. Deployments the gate cut
/// appear neither here nor in the outcome — gates cut the set, they do not
/// report beside it (#1675 PR2), so reporting them would be fabricating
/// visibility the resolver does not have. Account-level admission is a separate
/// #1680 authority and *is* evaluated here, from [`ResolverInput::accounts`],
/// yielding [`ExclusionReason::AccountNotAdmitted`].
///
/// Public fields: this is the caller-assembled input snapshot, and it carries
/// no invariant the seam can check (its element types validate themselves).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolverInput {
    /// The reference being resolved, with the policy revision it was minted
    /// against.
    pub model_ref: ModelRef,
    /// The admitted candidate deployments (a semantic gate already cut the set).
    pub admitted_candidates: Vec<ResolvedDeployment>,
    /// Catalog facts (revision + staleness).
    pub catalog: CatalogSnapshot,
    /// Deployment-health facts (cooldowns + observed_at).
    pub health: HealthSnapshot,
    /// Account-availability facts.
    pub accounts: AccountSnapshot,
    /// Budget ceiling for this request.
    pub budget: BudgetContext,
    /// Caller pin, if any.
    pub pin: PinContext,
    /// Retry/attempt context.
    pub retry: RetryContext,
}

/// The operational resolver seam.
///
/// A pure function from a frozen input snapshot to a [`ResolutionOutcome`].
/// Identical inputs must give identical outputs (deterministic ordering
/// `pin > health > price > deployment_id`, all against the frozen snapshot).
/// Abstain is a valid outcome, so there is no `Result` — a resolution never
/// "fails", it selects or abstains with a typed [`AbstainReason`] plus visible
/// per-candidate reasons.
///
/// #1681 PR-D ships the real implementation; #1682 develops against
/// [`StaticFixtureResolver`] until then.
pub trait OperationalResolver {
    /// Resolve one request against a frozen input snapshot.
    fn resolve(&self, input: &ResolverInput) -> ResolutionOutcome;
}

/// A deterministic fixture resolver so #1682 can build before #1681 PR-D lands.
///
/// **Not a production resolver, and mechanically so** (codex #1739 CONCERN-5):
/// it is gated behind `feature = "broker-fixtures"`, which is **off by
/// default**, plus memcore's own `cfg(test)`. A production build of memcore
/// does not compile this type at all, and the re-export in `lib.rs` carries the
/// same gate — downstream test targets must opt in explicitly
/// (`memcore = { …, features = ["broker-fixtures"] }` under `[dev-dependencies]`).
/// It applies a deliberately simplified filter set and has no price data, so
/// enabling it in production would silently downgrade routing.
///
/// It is a real (if simplified) pure function over the input, not a canned
/// constant, so #1682 gets realistic outcome shapes:
///
/// 1. Each admitted candidate is evaluated: stale (per catalog) →
///    [`ExclusionReason::StaleCatalog`]; on health cooldown →
///    [`ExclusionReason::HealthCooldown`]; its account not admitted →
///    [`ExclusionReason::AccountNotAdmitted`]; otherwise eligible.
/// 2. Eligible candidates are ordered `pin-first, then deployment_id
///    lexicographic`. (Price ordering is #1681's real resolver's job — the
///    fixture has no price numbers, only opaque refs — so it uses the
///    lexicographic tiebreak deterministically. Documented, not hidden.)
/// 3. The first eligible is chosen; the rest become the fallback order, capped
///    at [`FALLBACK_ORDER_CAP`]. No candidates at all → abstain with
///    [`AbstainReason::EmptyCandidateSet`]; candidates but none eligible →
///    [`AbstainReason::NoEligibleCandidate`]. The alias/policy abstain reasons
///    are unreachable here by construction: the fixture holds no alias set to
///    disagree with, and inventing that verdict would be a lie about a check it
///    never ran — #1681 PR-D's real resolver owns them.
///
/// Given identical input this always produces identical output.
///
/// **Precondition**: the input snapshots must carry a non-blank
/// `catalog.catalog_revision` and `health.observed_at` — the fixture stamps
/// both into [`ResolutionRevisions`], which refuses a blank stamp. An unstamped
/// snapshot panics here rather than yielding a resolution nobody can verify the
/// inputs of. (`CatalogSnapshot::default()` / `HealthSnapshot::default()` are
/// therefore not usable as-is; fill the two strings.)
#[cfg(any(test, feature = "broker-fixtures"))]
#[derive(Debug, Clone, Copy, Default)]
pub struct StaticFixtureResolver;

#[cfg(any(test, feature = "broker-fixtures"))]
impl OperationalResolver for StaticFixtureResolver {
    fn resolve(&self, input: &ResolverInput) -> ResolutionOutcome {
        let cooldown_ids: std::collections::BTreeSet<&str> = input
            .health
            .cooldowns
            .iter()
            .map(|c| c.deployment_id.as_str())
            .collect();
        let stale_ids: std::collections::BTreeSet<&str> = input
            .catalog
            .stale_deployment_ids
            .iter()
            .map(|s| s.as_str())
            .collect();
        let unadmitted_accounts: std::collections::BTreeSet<&str> = input
            .accounts
            .accounts
            .iter()
            .filter(|a| !a.admitted)
            .map(|a| a.account_ref.as_str())
            .collect();

        // Evaluate every candidate, preserving input order for the candidates
        // list. Deterministic given the frozen input.
        let mut candidates = Vec::with_capacity(input.admitted_candidates.len());
        let mut eligible: Vec<&ResolvedDeployment> = Vec::new();
        for dep in &input.admitted_candidates {
            let reason = if stale_ids.contains(dep.deployment_id()) {
                Some(ExclusionReason::StaleCatalog)
            } else if cooldown_ids.contains(dep.deployment_id()) {
                Some(ExclusionReason::HealthCooldown)
            } else if unadmitted_accounts.contains(dep.account_ref()) {
                Some(ExclusionReason::AccountNotAdmitted)
            } else {
                None
            };
            match reason {
                Some(r) => candidates.push(CandidateEvaluation::excluded(dep.deployment_id(), r)),
                None => {
                    candidates.push(CandidateEvaluation::eligible(dep.deployment_id()));
                    eligible.push(dep);
                }
            }
        }

        // Ordering: pin-first, then deployment_id lexicographic.
        let pinned = input.pin.pinned_deployment_id.as_deref();
        eligible.sort_by(|a, b| {
            let a_pin = pinned == Some(a.deployment_id());
            let b_pin = pinned == Some(b.deployment_id());
            // pinned sorts first: descending on the bool.
            b_pin
                .cmp(&a_pin)
                .then_with(|| a.deployment_id().cmp(b.deployment_id()))
        });

        // Through the public constructor, not a struct literal: the fixture is
        // in-module and *could* write the private fields directly, which is
        // exactly the bypass this rework closed everywhere else.
        let revisions = ResolutionRevisions::new(
            input.catalog.catalog_revision.clone(),
            input.health.observed_at.clone(),
            input.model_ref.policy_revision().to_string(),
        )
        .expect(
            "fixture resolver requires a stamped input: non-blank catalog_revision and \
             health.observed_at",
        );

        let outcome = match eligible.split_first() {
            Some((chosen, rest)) => {
                let fallback_order: Vec<String> = rest
                    .iter()
                    // A duplicate of the chosen id would not be a distinct
                    // fallback target, and the outcome constructor refuses it.
                    .filter(|d| d.deployment_id() != chosen.deployment_id())
                    .take(FALLBACK_ORDER_CAP)
                    .map(|d| d.deployment_id().to_string())
                    .collect();
                let budget_estimate = BudgetEstimate {
                    estimated_prompt_tokens: None,
                    estimated_completion_tokens: None,
                    estimated_cost_usd: None,
                    pricing_snapshot_ref: chosen.pricing_snapshot_ref().map(str::to_string),
                };
                ResolutionOutcome::new(
                    candidates,
                    Selection::Chosen((*chosen).clone()),
                    revisions,
                    Some(chosen.account_ref().to_string()),
                    budget_estimate,
                    fallback_order,
                )
            }
            None => {
                let reason = if candidates.is_empty() {
                    AbstainReason::EmptyCandidateSet
                } else {
                    AbstainReason::NoEligibleCandidate
                };
                ResolutionOutcome::new(
                    candidates,
                    Selection::Abstain(reason),
                    revisions,
                    None,
                    BudgetEstimate::default(),
                    Vec::new(),
                )
            }
        };

        // The fixture builds its outcome from its own evaluation, so every
        // consistency rule above holds by construction; a failure here is a bug
        // in the fixture, not bad caller input, and must be loud.
        outcome.expect("fixture resolver must build a consistent outcome")
    }
}

// ---------------------------------------------------------------------------
// 5. HealthObservation
// ---------------------------------------------------------------------------

/// How a health observation was learned. The seam-local, serde-capable twin of
/// the #1680 `vault::health::EvidenceKind{Probed, SelfReported}` pattern —
/// redefined here (rather than reused) so the seam stays self-contained and
/// serde round-trips without coupling to the vault module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObservationEvidence {
    /// Observed by a non-generating probe.
    #[serde(rename = "probed")]
    Probed,
    /// Reported by a consumer of the deployment (the invocation path).
    #[serde(rename = "self_reported")]
    SelfReported,
}

impl ObservationEvidence {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Probed => "probed",
            Self::SelfReported => "self_reported",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "probed" => Self::Probed,
            "self_reported" => Self::SelfReported,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [ObservationEvidence] = &[Self::Probed, Self::SelfReported];
}

/// The class of an invocation error, aligned with the #1681 D4 attribution
/// rules. Closed set: the executor classifies a failure into exactly one of
/// these, and the D4 rule decides which health authority each touches
/// (`AuthInvalid` never touches deployment health; `RateLimited` dual-records;
/// the rest are deployment-only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvocationErrorClass {
    /// 401 / 403 — credential/account surfaces only, never deployment health.
    #[serde(rename = "auth_invalid")]
    AuthInvalid,
    /// 429 / quota — dual-record (deployment cooldown + credential rate-limit).
    #[serde(rename = "rate_limited")]
    RateLimited,
    /// Request timed out.
    #[serde(rename = "timeout")]
    Timeout,
    /// 5xx server error.
    #[serde(rename = "server_error")]
    ServerError,
    /// Malformed / unparseable / protocol-violating response.
    #[serde(rename = "protocol")]
    Protocol,
}

impl InvocationErrorClass {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthInvalid => "auth_invalid",
            Self::RateLimited => "rate_limited",
            Self::Timeout => "timeout",
            Self::ServerError => "server_error",
            Self::Protocol => "protocol",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "auth_invalid" => Self::AuthInvalid,
            "rate_limited" => Self::RateLimited,
            "timeout" => Self::Timeout,
            "server_error" => Self::ServerError,
            "protocol" => Self::Protocol,
            _ => return None,
        })
    }

    /// Whether the D4 attribution rule routes this class to deployment health.
    /// `AuthInvalid` is the sole class that never does.
    pub fn touches_deployment_health(self) -> bool {
        !matches!(self, Self::AuthInvalid)
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [InvocationErrorClass] = &[
        Self::AuthInvalid,
        Self::RateLimited,
        Self::Timeout,
        Self::ServerError,
        Self::Protocol,
    ];
}

/// A `Retry-After` directive as read off the wire (#1682 codex BUG-4: the
/// header must survive classification). HTTP allows either a delta-seconds or
/// an HTTP-date form; both are preserved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum RetryAfter {
    /// `Retry-After: 120` — delta seconds.
    #[serde(rename = "seconds")]
    Seconds(u64),
    /// `Retry-After: <HTTP-date>` — preserved as the received string.
    #[serde(rename = "at")]
    At(String),
}

/// What #1682 reports back to the health layer for one invocation outcome: the
/// precise account/deployment pair, the error class, and any `Retry-After`.
/// This is exactly #1681 discrimination 6's input type.
///
/// Note the pairing of `account_ref` and `deployment_id`: the D4 attribution
/// rule needs both, because a 401/403 must reach the account surface while
/// never touching deployment health, and a 429 must reach both. Blank refs
/// would make the attribution unroutable, so fields are private and both
/// construction paths validate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "HealthObservationWire")]
pub struct HealthObservation {
    account_ref: String,
    deployment_id: String,
    error_class: InvocationErrorClass,
    retry_after: Option<RetryAfter>,
    observed_at: String,
    evidence: ObservationEvidence,
}

/// Deserialization shadow for [`HealthObservation`].
#[derive(Deserialize)]
struct HealthObservationWire {
    account_ref: String,
    deployment_id: String,
    error_class: InvocationErrorClass,
    retry_after: Option<RetryAfter>,
    observed_at: String,
    evidence: ObservationEvidence,
}

impl TryFrom<HealthObservationWire> for HealthObservation {
    type Error = SeamError;

    fn try_from(wire: HealthObservationWire) -> Result<Self, Self::Error> {
        Self::new(
            wire.account_ref,
            wire.deployment_id,
            wire.error_class,
            wire.retry_after,
            wire.observed_at,
            wire.evidence,
        )
    }
}

impl HealthObservation {
    /// Construct a validated observation.
    ///
    /// # Errors
    ///
    /// [`SeamError::EmptyField`] if `account_ref`, `deployment_id`, or
    /// `observed_at` is blank.
    pub fn new(
        account_ref: impl Into<String>,
        deployment_id: impl Into<String>,
        error_class: InvocationErrorClass,
        retry_after: Option<RetryAfter>,
        observed_at: impl Into<String>,
        evidence: ObservationEvidence,
    ) -> Result<Self, SeamError> {
        let account_ref = account_ref.into();
        let deployment_id = deployment_id.into();
        let observed_at = observed_at.into();
        require_non_empty(&account_ref, "account_ref")?;
        require_non_empty(&deployment_id, "deployment_id")?;
        require_non_empty(&observed_at, "observed_at")?;
        Ok(Self {
            account_ref,
            deployment_id,
            error_class,
            retry_after,
            observed_at,
            evidence,
        })
    }

    /// Opaque #1680 account/credential ref the invocation used.
    pub fn account_ref(&self) -> &str {
        &self.account_ref
    }

    /// The deployment the invocation targeted.
    pub fn deployment_id(&self) -> &str {
        &self.deployment_id
    }

    /// The classified failure.
    pub fn error_class(&self) -> InvocationErrorClass {
        self.error_class
    }

    /// The wire `Retry-After`, if the response carried one.
    pub fn retry_after(&self) -> Option<&RetryAfter> {
        self.retry_after.as_ref()
    }

    /// When the outcome was observed (ISO-8601).
    pub fn observed_at(&self) -> &str {
        &self.observed_at
    }

    /// How the observation was learned.
    pub fn evidence(&self) -> ObservationEvidence {
        self.evidence
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Literal goldens: the frozen wire bytes, spelled out once. ---
    //
    // These are deliberately whole-payload literals rather than round-trips: a
    // round-trip is satisfied by any self-consistent spelling, including a
    // wrong one (that is exactly how `open_ai_compat` slipped through at
    // `3f4d152f`). Changing any of these strings is changing the frozen seam.

    const MODEL_REF_GOLDEN: &str =
        r#"{"reference":"memory.chat","policy_revision":"policy-rev-1"}"#;

    const RESOLVED_DEPLOYMENT_GOLDEN: &str = concat!(
        r#"{"deployment_id":"dep-a","wire_dialect":"openai_compat","#,
        r#""endpoint_ref":"endpoint::dep-a","provider_model_id":"provider/dep-a","#,
        r#""capabilities":{"chat":true,"embeddings":false,"tools":false,"#,
        r#""streaming":false,"structured_output":false,"media":false},"#,
        r#""bounds":{"context_window":128000,"max_output":8192,"#,
        r#""attachment_bytes":null,"embedding_dimensions":null},"#,
        r#""account_ref":"acct-1","pricing_snapshot_ref":"price::dep-a"}"#
    );

    const ABSTAIN_OUTCOME_GOLDEN: &str = concat!(
        r#"{"candidates":[],"selection":{"kind":"abstain","value":"empty_candidate_set"},"#,
        r#""revisions":{"catalog_revision":"cat-rev-1","#,
        r#""health_observed_at":"2026-08-11T00:00:00Z","policy_revision":"policy-rev-1"},"#,
        r#""account_ref":null,"budget_estimate":{"estimated_prompt_tokens":null,"#,
        r#""estimated_completion_tokens":null,"estimated_cost_usd":null,"#,
        r#""pricing_snapshot_ref":null},"fallback_order":[]}"#
    );

    const HEALTH_OBSERVATION_GOLDEN: &str = concat!(
        r#"{"account_ref":"acct-1","deployment_id":"dep-a","error_class":"rate_limited","#,
        r#""retry_after":{"kind":"seconds","value":120},"#,
        r#""observed_at":"2026-08-11T00:00:00Z","evidence":"self_reported"}"#
    );

    fn deployment_parts(id: &str, account: &str) -> ResolvedDeploymentParts {
        ResolvedDeploymentParts {
            deployment_id: id.to_string(),
            wire_dialect: WireDialect::OpenAiCompat,
            endpoint_ref: format!("endpoint::{id}"),
            provider_model_id: format!("provider/{id}"),
            capabilities: DeploymentCapabilities {
                chat: true,
                ..DeploymentCapabilities::default()
            },
            bounds: DeploymentBounds {
                context_window: Some(128_000),
                max_output: Some(8_192),
                attachment_bytes: None,
                embedding_dimensions: None,
            },
            account_ref: account.to_string(),
            pricing_snapshot_ref: Some(format!("price::{id}")),
        }
    }

    fn sample_deployment(id: &str, account: &str) -> ResolvedDeployment {
        ResolvedDeployment::new(deployment_parts(id, account)).expect("valid fixture deployment")
    }

    fn sample_revisions() -> ResolutionRevisions {
        ResolutionRevisions::new("cat-rev-1", "2026-08-11T00:00:00Z", "policy-rev-1")
            .expect("valid fixture revisions")
    }

    fn sample_input() -> ResolverInput {
        ResolverInput {
            model_ref: ModelRef::new("memory.chat", "policy-rev-1").unwrap(),
            admitted_candidates: vec![
                sample_deployment("dep-b", "acct-1"),
                sample_deployment("dep-a", "acct-1"),
                sample_deployment("dep-c", "acct-2"),
            ],
            catalog: CatalogSnapshot {
                catalog_revision: "cat-rev-1".to_string(),
                stale_deployment_ids: vec!["dep-c".to_string()],
            },
            health: HealthSnapshot {
                observed_at: "2026-08-11T00:00:00Z".to_string(),
                cooldowns: vec![DeploymentCooldown {
                    deployment_id: "dep-b".to_string(),
                    cooldown_until: Some("2026-08-11T01:00:00Z".to_string()),
                }],
            },
            accounts: AccountSnapshot {
                accounts: vec![
                    AccountAvailability {
                        account_ref: "acct-1".to_string(),
                        admitted: true,
                    },
                    AccountAvailability {
                        account_ref: "acct-2".to_string(),
                        admitted: true,
                    },
                ],
            },
            budget: BudgetContext {
                ceiling_usd: Some(1.0),
            },
            pin: PinContext::default(),
            retry: RetryContext::default(),
        }
    }

    fn sample_observation() -> HealthObservation {
        HealthObservation::new(
            "acct-1",
            "dep-a",
            InvocationErrorClass::RateLimited,
            Some(RetryAfter::Seconds(120)),
            "2026-08-11T00:00:00Z",
            ObservationEvidence::SelfReported,
        )
        .expect("valid fixture observation")
    }

    // --- Discrimination: serde round-trip for all five frozen types ---

    fn round_trip<T>(value: &T)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&back, value, "round-trip mismatch: {json}");
    }

    #[test]
    fn model_ref_round_trips() {
        round_trip(&ModelRef::new("memory.chat", "policy-rev-1").unwrap());
    }

    #[test]
    fn resolved_deployment_round_trips() {
        round_trip(&sample_deployment("dep-a", "acct-1"));
        // Every wire dialect survives serde, both directions.
        for &d in WireDialect::ALL {
            let mut parts = deployment_parts("dep-x", "acct-x");
            parts.wire_dialect = d;
            round_trip(&ResolvedDeployment::new(parts).unwrap());
            assert_eq!(WireDialect::parse(d.as_str()), Some(d));
        }
    }

    #[test]
    fn resolution_outcome_round_trips() {
        let outcome = StaticFixtureResolver.resolve(&sample_input());
        round_trip(&outcome);

        // Abstain shape round-trips too.
        let empty = ResolverInput {
            admitted_candidates: vec![],
            ..sample_input()
        };
        let abstain = StaticFixtureResolver.resolve(&empty);
        assert!(abstain.selection().is_abstain());
        round_trip(&abstain);
    }

    #[test]
    fn resolver_input_round_trips() {
        // The OperationalResolver seam is a trait; its frozen input snapshot is
        // the serde-bearing surface both leaves exchange.
        round_trip(&sample_input());
    }

    #[test]
    fn health_observation_round_trips() {
        round_trip(&sample_observation());
        // The HTTP-date Retry-After form round-trips too.
        round_trip(
            &HealthObservation::new(
                "acct-1",
                "dep-a",
                InvocationErrorClass::ServerError,
                Some(RetryAfter::At("Wed, 21 Oct 2026 07:28:00 GMT".to_string())),
                "2026-08-11T00:00:00Z",
                ObservationEvidence::Probed,
            )
            .unwrap(),
        );
    }

    // --- Discrimination (codex #1739 BUG-3/BUG-6): frozen spellings are
    // pinned by literal goldens, and serde == as_str for every variant. ---

    /// Assert one enum variant's serde spelling equals both its `as_str()` and
    /// a literal golden — the three-way tie that makes a one-sided rename fail.
    fn assert_spelling<T: Serialize>(value: &T, as_str: &str, golden: &str) {
        let json = serde_json::to_string(value).expect("serialize");
        assert_eq!(
            json,
            format!("\"{golden}\""),
            "frozen wire spelling changed: expected the literal golden"
        );
        assert_eq!(
            json,
            format!("\"{as_str}\""),
            "serde spelling and as_str() disagree — the BUG-6 divergence"
        );
    }

    #[test]
    fn wire_dialect_serde_spelling_matches_as_str_and_golden() {
        let goldens = [
            (WireDialect::OpenAiCompat, "openai_compat"),
            (WireDialect::Anthropic, "anthropic"),
            (WireDialect::Xai, "xai"),
            (WireDialect::OpenRouter, "open_router"),
            (WireDialect::GenericCompat, "generic_compat"),
            (WireDialect::Ollama, "ollama"),
            (WireDialect::Unknown, "unknown"),
        ];
        assert_eq!(
            goldens.len(),
            WireDialect::ALL.len(),
            "every dialect must carry a literal golden"
        );
        for (variant, golden) in goldens {
            assert_spelling(&variant, variant.as_str(), golden);
            assert_eq!(
                serde_json::from_str::<WireDialect>(&format!("\"{golden}\"")).unwrap(),
                variant
            );
            assert_eq!(WireDialect::parse(golden), Some(variant));
        }
    }

    #[test]
    fn exclusion_reason_serde_spelling_matches_as_str_and_golden() {
        let goldens = [
            (ExclusionReason::CapabilityMismatch, "capability_mismatch"),
            (
                ExclusionReason::ContextWindowExceeded,
                "context_window_exceeded",
            ),
            (ExclusionReason::MaxOutputExceeded, "max_output_exceeded"),
            (
                ExclusionReason::AttachmentBoundsExceeded,
                "attachment_bounds_exceeded",
            ),
            (
                ExclusionReason::EmbeddingDimensionMismatch,
                "embedding_dimension_mismatch",
            ),
            (ExclusionReason::RegionBlocked, "region_blocked"),
            (ExclusionReason::DataPolicyBlocked, "data_policy_blocked"),
            (ExclusionReason::BudgetExceeded, "budget_exceeded"),
            (ExclusionReason::HealthCooldown, "health_cooldown"),
            (ExclusionReason::StaleCatalog, "stale_catalog"),
            (ExclusionReason::AccountNotAdmitted, "account_not_admitted"),
            (ExclusionReason::DeploymentInactive, "deployment_inactive"),
        ];
        assert_eq!(
            goldens.len(),
            ExclusionReason::ALL.len(),
            "every exclusion axis must carry a literal golden"
        );
        for (variant, golden) in goldens {
            assert_spelling(&variant, variant.as_str(), golden);
            assert_eq!(ExclusionReason::parse(golden), Some(variant));
        }
    }

    #[test]
    fn abstain_reason_serde_spelling_matches_as_str_and_golden() {
        let goldens = [
            (AbstainReason::EmptyCandidateSet, "empty_candidate_set"),
            (AbstainReason::NoEligibleCandidate, "no_eligible_candidate"),
            (AbstainReason::UnknownAlias, "unknown_alias"),
            (AbstainReason::AmbiguousAlias, "ambiguous_alias"),
            (
                AbstainReason::PolicyRevisionMismatch,
                "policy_revision_mismatch",
            ),
        ];
        assert_eq!(
            goldens.len(),
            AbstainReason::ALL.len(),
            "every abstain reason must carry a literal golden"
        );
        for (variant, golden) in goldens {
            assert_spelling(&variant, variant.as_str(), golden);
            assert_eq!(AbstainReason::parse(golden), Some(variant));
        }
    }

    #[test]
    fn invocation_error_class_serde_spelling_matches_as_str_and_golden() {
        let goldens = [
            (InvocationErrorClass::AuthInvalid, "auth_invalid"),
            (InvocationErrorClass::RateLimited, "rate_limited"),
            (InvocationErrorClass::Timeout, "timeout"),
            (InvocationErrorClass::ServerError, "server_error"),
            (InvocationErrorClass::Protocol, "protocol"),
        ];
        assert_eq!(goldens.len(), InvocationErrorClass::ALL.len());
        for (variant, golden) in goldens {
            assert_spelling(&variant, variant.as_str(), golden);
            assert_eq!(InvocationErrorClass::parse(golden), Some(variant));
        }
    }

    #[test]
    fn observation_evidence_serde_spelling_matches_as_str_and_golden() {
        let goldens = [
            (ObservationEvidence::Probed, "probed"),
            (ObservationEvidence::SelfReported, "self_reported"),
        ];
        assert_eq!(goldens.len(), ObservationEvidence::ALL.len());
        for (variant, golden) in goldens {
            assert_spelling(&variant, variant.as_str(), golden);
            assert_eq!(ObservationEvidence::parse(golden), Some(variant));
        }
    }

    #[test]
    fn tagged_envelopes_match_their_literal_goldens() {
        // Adjacent tagging: the `kind`/`value` key names are as frozen as the
        // variant spellings.
        assert_eq!(
            serde_json::to_string(&RetryAfter::Seconds(30)).unwrap(),
            r#"{"kind":"seconds","value":30}"#
        );
        assert_eq!(
            serde_json::to_string(&RetryAfter::At("Wed, 21 Oct 2026 07:28:00 GMT".to_string()))
                .unwrap(),
            r#"{"kind":"at","value":"Wed, 21 Oct 2026 07:28:00 GMT"}"#
        );
        assert_eq!(
            serde_json::to_string(&Selection::Abstain(AbstainReason::UnknownAlias)).unwrap(),
            r#"{"kind":"abstain","value":"unknown_alias"}"#
        );
        assert_eq!(
            serde_json::to_string(&Selection::Chosen(sample_deployment("dep-a", "acct-1")))
                .unwrap(),
            format!(r#"{{"kind":"chosen","value":{RESOLVED_DEPLOYMENT_GOLDEN}}}"#)
        );
    }

    #[test]
    fn struct_field_names_match_their_literal_goldens() {
        assert_eq!(
            serde_json::to_string(&ModelRef::new("memory.chat", "policy-rev-1").unwrap()).unwrap(),
            MODEL_REF_GOLDEN
        );
        assert_eq!(
            serde_json::to_string(&sample_deployment("dep-a", "acct-1")).unwrap(),
            RESOLVED_DEPLOYMENT_GOLDEN
        );
        assert_eq!(
            serde_json::to_string(&sample_observation()).unwrap(),
            HEALTH_OBSERVATION_GOLDEN
        );
        let abstain = ResolutionOutcome::new(
            vec![],
            Selection::Abstain(AbstainReason::EmptyCandidateSet),
            sample_revisions(),
            None,
            BudgetEstimate::default(),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_string(&abstain).unwrap(),
            ABSTAIN_OUTCOME_GOLDEN
        );
        // The goldens are accepted back, so they pin the read side too.
        assert_eq!(
            serde_json::from_str::<ResolutionOutcome>(ABSTAIN_OUTCOME_GOLDEN).unwrap(),
            abstain
        );
        assert_eq!(
            serde_json::from_str::<HealthObservation>(HEALTH_OBSERVATION_GOLDEN).unwrap(),
            sample_observation()
        );
        assert_eq!(
            serde_json::from_str::<ModelRef>(MODEL_REF_GOLDEN).unwrap(),
            ModelRef::new("memory.chat", "policy-rev-1").unwrap()
        );
    }

    // --- Discrimination (codex #1739 BUG-4): the deserialize path cannot
    // bypass constructor validation. Each case is a JSON payload that the
    // matching constructor would refuse. ---

    #[test]
    fn model_ref_deserialize_rejects_blank_fields() {
        for payload in [
            r#"{"reference":"","policy_revision":"rev"}"#,
            r#"{"reference":"   ","policy_revision":"rev"}"#,
            r#"{"reference":"m","policy_revision":""}"#,
        ] {
            assert!(
                serde_json::from_str::<ModelRef>(payload).is_err(),
                "deserialize must not mint a ModelRef the constructor refuses: {payload}"
            );
        }
        // ... and the same inputs are refused by the constructor itself.
        assert_eq!(
            ModelRef::new("  ", "rev"),
            Err(SeamError::EmptyField { field: "reference" })
        );
        assert_eq!(
            ModelRef::new("m", "  "),
            Err(SeamError::EmptyField {
                field: "policy_revision"
            })
        );
    }

    #[test]
    fn resolved_deployment_deserialize_rejects_blank_refs() {
        for blank_field in [
            "deployment_id",
            "endpoint_ref",
            "provider_model_id",
            "account_ref",
            "pricing_snapshot_ref",
        ] {
            let mut value: serde_json::Value =
                serde_json::from_str(RESOLVED_DEPLOYMENT_GOLDEN).expect("golden parses");
            value[blank_field] = serde_json::Value::String(String::new());
            let payload = value.to_string();
            assert!(
                serde_json::from_str::<ResolvedDeployment>(&payload).is_err(),
                "blank `{blank_field}` must be refused on the deserialize path"
            );
        }
        // A *missing* price is legal; a blank one is not.
        let mut value: serde_json::Value =
            serde_json::from_str(RESOLVED_DEPLOYMENT_GOLDEN).expect("golden parses");
        value["pricing_snapshot_ref"] = serde_json::Value::Null;
        assert!(serde_json::from_str::<ResolvedDeployment>(&value.to_string()).is_ok());
    }

    #[test]
    fn resolution_revisions_deserialize_rejects_blank_stamps() {
        for blank_field in ["catalog_revision", "health_observed_at", "policy_revision"] {
            let mut value = serde_json::json!({
                "catalog_revision": "cat-rev-1",
                "health_observed_at": "2026-08-11T00:00:00Z",
                "policy_revision": "policy-rev-1",
            });
            value[blank_field] = serde_json::Value::String("  ".to_string());
            assert!(
                serde_json::from_str::<ResolutionRevisions>(&value.to_string()).is_err(),
                "blank `{blank_field}` must be refused on the deserialize path"
            );
        }
    }

    #[test]
    fn health_observation_deserialize_rejects_blank_refs() {
        for blank_field in ["account_ref", "deployment_id", "observed_at"] {
            let mut value: serde_json::Value =
                serde_json::from_str(HEALTH_OBSERVATION_GOLDEN).expect("golden parses");
            value[blank_field] = serde_json::Value::String(String::new());
            assert!(
                serde_json::from_str::<HealthObservation>(&value.to_string()).is_err(),
                "blank `{blank_field}` must be refused on the deserialize path"
            );
        }
    }

    /// A chosen-shaped outcome as JSON, so the bypass tests can mutate one
    /// field at a time.
    fn chosen_outcome_value() -> serde_json::Value {
        let outcome = ResolutionOutcome::new(
            vec![
                CandidateEvaluation::eligible("dep-a"),
                CandidateEvaluation::eligible("dep-b"),
                CandidateEvaluation::excluded("dep-c", ExclusionReason::StaleCatalog),
            ],
            Selection::Chosen(sample_deployment("dep-a", "acct-1")),
            sample_revisions(),
            Some("acct-1".to_string()),
            BudgetEstimate::default(),
            vec!["dep-b".to_string()],
        )
        .expect("consistent fixture outcome");
        serde_json::to_value(&outcome).expect("serialize")
    }

    #[test]
    fn resolution_outcome_deserialize_enforces_fallback_cap() {
        let mut value = chosen_outcome_value();
        value["fallback_order"] = serde_json::json!(["b", "c", "d", "e", "f"]);
        assert!(
            serde_json::from_str::<ResolutionOutcome>(&value.to_string()).is_err(),
            "an over-cap fallback order must not survive deserialization"
        );
        // Same rule from the constructor side.
        let err = ResolutionOutcome::new(
            vec![],
            Selection::Abstain(AbstainReason::EmptyCandidateSet),
            sample_revisions(),
            None,
            BudgetEstimate::default(),
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
                "e".to_string(),
            ],
        )
        .unwrap_err();
        assert_eq!(err, SeamError::FallbackOrderTooLong { len: 5 });
    }

    #[test]
    fn resolution_outcome_deserialize_enforces_account_ref_consistency() {
        let mut value = chosen_outcome_value();
        value["account_ref"] = serde_json::Value::String("acct-someone-else".to_string());
        assert!(
            serde_json::from_str::<ResolutionOutcome>(&value.to_string()).is_err(),
            "outcome account_ref must not diverge from the chosen deployment's"
        );

        let mut missing = chosen_outcome_value();
        missing["account_ref"] = serde_json::Value::Null;
        assert!(
            serde_json::from_str::<ResolutionOutcome>(&missing.to_string()).is_err(),
            "a chosen outcome must carry the chosen deployment's account_ref"
        );

        // Constructor side, same verdict.
        let err = ResolutionOutcome::new(
            vec![CandidateEvaluation::eligible("dep-a")],
            Selection::Chosen(sample_deployment("dep-a", "acct-1")),
            sample_revisions(),
            Some("acct-2".to_string()),
            BudgetEstimate::default(),
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(err, SeamError::AccountRefMismatch);
    }

    #[test]
    fn resolution_outcome_rejects_unevaluated_chosen_or_fallback() {
        // Chosen deployment absent from the candidate list.
        let err = ResolutionOutcome::new(
            vec![CandidateEvaluation::eligible("dep-z")],
            Selection::Chosen(sample_deployment("dep-a", "acct-1")),
            sample_revisions(),
            Some("acct-1".to_string()),
            BudgetEstimate::default(),
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(
            err,
            SeamError::ChosenNotEligible {
                deployment_id: "dep-a".to_string()
            }
        );

        // Chosen deployment present but excluded.
        let err = ResolutionOutcome::new(
            vec![CandidateEvaluation::excluded(
                "dep-a",
                ExclusionReason::HealthCooldown,
            )],
            Selection::Chosen(sample_deployment("dep-a", "acct-1")),
            sample_revisions(),
            Some("acct-1".to_string()),
            BudgetEstimate::default(),
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(
            err,
            SeamError::ChosenNotEligible {
                deployment_id: "dep-a".to_string()
            }
        );

        // Fallback entry that was never an eligible candidate — the chain
        // becomes durable receipt provenance, so it may not be invented.
        let mut value = chosen_outcome_value();
        value["fallback_order"] = serde_json::json!(["dep-c"]);
        assert!(
            serde_json::from_str::<ResolutionOutcome>(&value.to_string()).is_err(),
            "an excluded candidate must not appear in the fallback order"
        );
        let mut value = chosen_outcome_value();
        value["fallback_order"] = serde_json::json!(["dep-never-seen"]);
        assert!(
            serde_json::from_str::<ResolutionOutcome>(&value.to_string()).is_err(),
            "an unevaluated id must not appear in the fallback order"
        );
        let mut value = chosen_outcome_value();
        value["fallback_order"] = serde_json::json!(["dep-a"]);
        assert!(
            serde_json::from_str::<ResolutionOutcome>(&value.to_string()).is_err(),
            "the chosen deployment must not also be its own fallback"
        );
    }

    #[test]
    fn resolution_outcome_rejects_abstain_carrying_selection_state() {
        let mut value: serde_json::Value =
            serde_json::from_str(ABSTAIN_OUTCOME_GOLDEN).expect("golden parses");
        value["account_ref"] = serde_json::Value::String("acct-1".to_string());
        assert!(
            serde_json::from_str::<ResolutionOutcome>(&value.to_string()).is_err(),
            "an abstaining outcome must not carry an account_ref"
        );

        let err = ResolutionOutcome::new(
            vec![],
            Selection::Abstain(AbstainReason::EmptyCandidateSet),
            sample_revisions(),
            Some("acct-1".to_string()),
            BudgetEstimate::default(),
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(err, SeamError::AbstainCarriesAccountRef);

        let err = ResolutionOutcome::new(
            vec![],
            Selection::Abstain(AbstainReason::EmptyCandidateSet),
            sample_revisions(),
            None,
            BudgetEstimate::default(),
            vec!["dep-a".to_string()],
        )
        .unwrap_err();
        assert_eq!(err, SeamError::AbstainCarriesFallbackOrder);
    }

    #[test]
    fn resolution_outcome_rejects_blank_candidate_ids() {
        let err = ResolutionOutcome::new(
            vec![CandidateEvaluation::eligible("  ")],
            Selection::Abstain(AbstainReason::NoEligibleCandidate),
            sample_revisions(),
            None,
            BudgetEstimate::default(),
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(
            err,
            SeamError::EmptyField {
                field: "candidate deployment_id"
            }
        );
    }

    // --- Discrimination: closed vocabularies are exhaustive ---

    #[test]
    fn exclusion_reason_variants_are_exhaustively_constructible() {
        // Every variant in ALL must be individually constructible, string
        // round-trip, and serde round-trip. If a variant is added to the enum
        // without being added to ALL, the exhaustive `match` below fails to
        // compile — so ALL cannot silently drift from the enum.
        for &reason in ExclusionReason::ALL {
            assert_eq!(ExclusionReason::parse(reason.as_str()), Some(reason));
            round_trip(&CandidateEvaluation::excluded("dep", reason));
        }
        // Compile-time exhaustiveness guard: adding a variant forces this arm.
        fn assert_all_covered(r: ExclusionReason) {
            match r {
                ExclusionReason::CapabilityMismatch
                | ExclusionReason::ContextWindowExceeded
                | ExclusionReason::MaxOutputExceeded
                | ExclusionReason::AttachmentBoundsExceeded
                | ExclusionReason::EmbeddingDimensionMismatch
                | ExclusionReason::RegionBlocked
                | ExclusionReason::DataPolicyBlocked
                | ExclusionReason::BudgetExceeded
                | ExclusionReason::HealthCooldown
                | ExclusionReason::StaleCatalog
                | ExclusionReason::AccountNotAdmitted
                | ExclusionReason::DeploymentInactive => {}
            }
        }
        assert_eq!(ExclusionReason::ALL.len(), 12);
        for &r in ExclusionReason::ALL {
            assert_all_covered(r);
        }
        // The four bounds axes exist one-for-one with DeploymentBounds, so a
        // bounds failure is never reported as a generic capability mismatch.
        for axis in [
            ExclusionReason::ContextWindowExceeded,
            ExclusionReason::MaxOutputExceeded,
            ExclusionReason::AttachmentBoundsExceeded,
            ExclusionReason::EmbeddingDimensionMismatch,
        ] {
            assert!(ExclusionReason::ALL.contains(&axis));
        }
    }

    #[test]
    fn abstain_reason_variants_are_exhaustively_constructible() {
        for &reason in AbstainReason::ALL {
            assert_eq!(AbstainReason::parse(reason.as_str()), Some(reason));
            round_trip(&Selection::Abstain(reason));
        }
        // Compile-time exhaustiveness guard.
        fn assert_all_covered(r: AbstainReason) {
            match r {
                AbstainReason::EmptyCandidateSet
                | AbstainReason::NoEligibleCandidate
                | AbstainReason::UnknownAlias
                | AbstainReason::AmbiguousAlias
                | AbstainReason::PolicyRevisionMismatch => {}
            }
        }
        assert_eq!(AbstainReason::ALL.len(), 5);
        for &r in AbstainReason::ALL {
            assert_all_covered(r);
        }
        // The loud-failure vocabulary the seam's alias law requires.
        for required in [
            AbstainReason::UnknownAlias,
            AbstainReason::AmbiguousAlias,
            AbstainReason::PolicyRevisionMismatch,
        ] {
            assert!(AbstainReason::ALL.contains(&required));
        }
    }

    #[test]
    fn invocation_error_class_covers_d4_taxonomy() {
        for &c in InvocationErrorClass::ALL {
            assert_eq!(InvocationErrorClass::parse(c.as_str()), Some(c));
        }
        // D4 attribution: AuthInvalid is the sole class off deployment health.
        assert!(!InvocationErrorClass::AuthInvalid.touches_deployment_health());
        for &c in InvocationErrorClass::ALL {
            if c != InvocationErrorClass::AuthInvalid {
                assert!(c.touches_deployment_health());
            }
        }
    }

    // --- Discrimination: StaticFixtureResolver determinism ---

    #[test]
    fn static_fixture_resolver_is_deterministic() {
        let input = sample_input();
        let first = StaticFixtureResolver.resolve(&input);
        let second = StaticFixtureResolver.resolve(&input);
        assert_eq!(first, second, "same input must give same output");
    }

    #[test]
    fn static_fixture_resolver_applies_frozen_filters_and_order() {
        let outcome = StaticFixtureResolver.resolve(&sample_input());
        // dep-b is on cooldown, dep-c is stale → only dep-a is eligible.
        assert_eq!(
            outcome
                .selection()
                .chosen()
                .map(ResolvedDeployment::deployment_id),
            Some("dep-a")
        );
        assert!(outcome.fallback_order().is_empty());
        // Every candidate is represented with its disposition.
        let by_id = |id: &str| {
            outcome
                .candidates()
                .iter()
                .find(|c| c.deployment_id == id)
                .unwrap()
                .exclusion
        };
        assert_eq!(by_id("dep-a"), None);
        assert_eq!(by_id("dep-b"), Some(ExclusionReason::HealthCooldown));
        assert_eq!(by_id("dep-c"), Some(ExclusionReason::StaleCatalog));
        // Revisions are stamped from the frozen snapshot.
        assert_eq!(outcome.revisions().catalog_revision(), "cat-rev-1");
        assert_eq!(outcome.revisions().policy_revision(), "policy-rev-1");
        // The outcome's account_ref is the chosen deployment's.
        assert_eq!(outcome.account_ref(), Some("acct-1"));
    }

    #[test]
    fn static_fixture_resolver_excludes_unadmitted_accounts() {
        let mut input = sample_input();
        input.catalog.stale_deployment_ids.clear();
        input.health.cooldowns.clear();
        input.accounts.accounts[0].admitted = false; // acct-1 → dep-a, dep-b out
        let outcome = StaticFixtureResolver.resolve(&input);
        assert_eq!(
            outcome
                .selection()
                .chosen()
                .map(ResolvedDeployment::deployment_id),
            Some("dep-c")
        );
        let excluded: Vec<_> = outcome
            .candidates()
            .iter()
            .filter(|c| !c.is_eligible())
            .map(|c| (c.deployment_id.as_str(), c.exclusion.unwrap()))
            .collect();
        assert_eq!(
            excluded,
            vec![
                ("dep-b", ExclusionReason::AccountNotAdmitted),
                ("dep-a", ExclusionReason::AccountNotAdmitted),
            ]
        );
    }

    #[test]
    fn static_fixture_resolver_abstain_reasons_distinguish_empty_from_filtered() {
        // No candidates at all: the gate cut everything upstream.
        let empty = ResolverInput {
            admitted_candidates: vec![],
            ..sample_input()
        };
        assert_eq!(
            StaticFixtureResolver
                .resolve(&empty)
                .selection()
                .abstain_reason(),
            Some(AbstainReason::EmptyCandidateSet)
        );

        // Candidates present, every one filtered out by the resolver itself.
        let mut all_stale = sample_input();
        all_stale.catalog.stale_deployment_ids = vec![
            "dep-a".to_string(),
            "dep-b".to_string(),
            "dep-c".to_string(),
        ];
        let outcome = StaticFixtureResolver.resolve(&all_stale);
        assert_eq!(
            outcome.selection().abstain_reason(),
            Some(AbstainReason::NoEligibleCandidate)
        );
        assert_eq!(outcome.candidates().len(), 3);
        assert!(outcome.account_ref().is_none());
        assert!(outcome.fallback_order().is_empty());
    }

    #[test]
    fn static_fixture_resolver_pin_wins_ordering() {
        let mut input = sample_input();
        // Make all three eligible, pin dep-c.
        input.catalog.stale_deployment_ids.clear();
        input.health.cooldowns.clear();
        input.pin.pinned_deployment_id = Some("dep-c".to_string());
        let outcome = StaticFixtureResolver.resolve(&input);
        assert_eq!(
            outcome
                .selection()
                .chosen()
                .map(ResolvedDeployment::deployment_id),
            Some("dep-c"),
            "pin must win over lexicographic order"
        );
        // Remaining eligible become the (bounded) fallback order, lexicographic.
        assert_eq!(outcome.fallback_order(), &["dep-a", "dep-b"]);
    }

    // --- Discrimination: dependency direction (structural self-check) ---

    #[test]
    fn module_has_no_external_tachi_crate_imports() {
        // The seam must be memcore-native: zero imports of, and zero
        // fully-qualified paths into, the upper crates (codex #1681 OK-BUT-8 /
        // #1739 BUG-3). Every needle is assembled at runtime so this test's own
        // source — and any prose in this file — cannot match it.
        let src = include_str!("model_broker_seam.rs");
        let prefix = format!("{}{}", "tachi", "_");
        let needles = [
            format!("use {prefix}"),
            format!("extern crate {prefix}"),
            format!("{}{}", "tachi", "_server::"),
            format!("{}{}", "tachi", "_llm::"),
            format!("{}{}", "tachi", "_dispatch::"),
        ];
        for needle in needles {
            assert!(
                !src.contains(&needle),
                "seam module must not reach into an external tachi crate (needle {needle:?})"
            );
        }
    }
}
