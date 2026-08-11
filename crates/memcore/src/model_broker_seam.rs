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
//! **nothing** from `tachi-server`, `tachi-llm`, or `tachi-dispatch`. The
//! operational resolver lives in memcore precisely because memcore is the only
//! node below both tachi-llm (which calls the resolver to route) and
//! tachi-server (which projects HTTP ↔ canonical then calls it) — see the
//! `resolve_auth_ref` placement rationale in `store::vault_accounts`. If the
//! resolver's *input* types were owned by any of those upper crates, the
//! dependency would invert. So the snapshot types the resolver consumes
//! ([`CatalogSnapshot`], [`HealthSnapshot`], [`AccountSnapshot`], the budget /
//! pin / retry contexts) are all defined here, as memcore-owned data.
//!
//! The `module_has_no_external_tachi_crate_imports` test structurally pins
//! this: the source text carries no `use tachi_…` line.
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
//!   real implementation exists — that is the entire point of a seam.

use serde::{Deserialize, Serialize};

/// The bound on a resolution's fallback order.
///
/// Matches the #1680 fallback cap-4 + label-collapse provenance discipline
/// (`types.rs:252-267`): a durable chain of alternatives is deliberately
/// bounded so a resolution can never carry an unbounded provider-derived list.
pub const FALLBACK_ORDER_CAP: usize = 4;

// ---------------------------------------------------------------------------
// 1. ModelRef
// ---------------------------------------------------------------------------

/// Errors from constructing or validating a seam value.
///
/// Local to the seam and memcore-native: the seam does not reach into
/// `crate::error::MemoryError`, so a downstream crate can depend on the seam
/// without pulling the full memcore error surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeamError {
    /// A required opaque reference string was empty or whitespace-only.
    EmptyReference,
    /// A required revision/digest string was empty.
    EmptyRevision,
    /// A fallback order exceeded [`FALLBACK_ORDER_CAP`].
    FallbackOrderTooLong { len: usize },
}

impl std::fmt::Display for SeamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyReference => write!(f, "model reference must not be empty"),
            Self::EmptyRevision => write!(f, "policy revision must not be empty"),
            Self::FallbackOrderTooLong { len } => {
                write!(f, "fallback order {len} exceeds cap {FALLBACK_ORDER_CAP}")
            }
        }
    }
}

impl std::error::Error for SeamError {}

/// An opaque reference to a model, resolved *against a specific alias-set policy
/// revision*.
///
/// It may name an alias (`memory.chat`) or a stable model reference — the
/// newtype is deliberately opaque, so callers cannot branch on "is this an
/// alias" at the type level; that classification is the resolver's job. The
/// paired `policy_revision` is the alias-set policy revision (a canonical-JSON
/// content digest per #1681 D2) the reference is meaningful under: a `ModelRef`
/// carries the revision it was minted against so a resolution can assert
/// `stamped == recomputed` rather than resolving against a drifted alias set.
///
/// Fields are private; a `ModelRef` can only be built through [`ModelRef::new`],
/// which rejects empty inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    reference: String,
    policy_revision: String,
}

impl ModelRef {
    /// Construct a validated `ModelRef`.
    ///
    /// # Errors
    ///
    /// [`SeamError::EmptyReference`] if `reference` is empty/whitespace,
    /// [`SeamError::EmptyRevision`] if `policy_revision` is empty.
    pub fn new(
        reference: impl Into<String>,
        policy_revision: impl Into<String>,
    ) -> Result<Self, SeamError> {
        let reference = reference.into();
        let policy_revision = policy_revision.into();
        if reference.trim().is_empty() {
            return Err(SeamError::EmptyReference);
        }
        if policy_revision.trim().is_empty() {
            return Err(SeamError::EmptyRevision);
        }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireDialect {
    /// OpenAI `/v1/chat/completions` grammar (SiliconFlow / DeepSeek / ZAI /
    /// OpenAI itself all speak it today).
    OpenAiCompat,
    /// Anthropic messages grammar (`content_block_delta` / `tool_use`).
    Anthropic,
    /// xAI grammar.
    Xai,
    /// OpenRouter grammar.
    OpenRouter,
    /// A generic OpenAI-compatible endpoint that is none of the named vendors.
    GenericCompat,
    /// Ollama `/api/generate` (NDJSON stream, not SSE).
    Ollama,
    /// Catalogued but the wire grammar is not operable by this build.
    Unknown,
}

impl WireDialect {
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentCapabilities {
    pub chat: bool,
    pub embeddings: bool,
    pub tools: bool,
    pub streaming: bool,
    pub structured_output: bool,
    pub media: bool,
}

/// A deployment's context / output / attachment bounds.
///
/// `embedding_dimensions` is the #1681 D3 dimension declaration: an embed
/// deployment carries the vector dimensionality it produces, so an override
/// whose dimension mismatches the stored index fails loudly instead of silently
/// corrupting comparability. All fields are optional — a chat deployment has no
/// embedding dimension, a deployment whose bounds are unknown carries `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentBounds {
    pub context_window: Option<u32>,
    pub max_output: Option<u32>,
    pub attachment_bytes: Option<u64>,
    pub embedding_dimensions: Option<u32>,
}

/// A single already-resolved deployment — what a resolver hands out for one
/// concrete route.
///
/// This is the *resolved projection*, not the `model_deployments` row: it
/// carries no health columns (health is a separate authority, #1681 D4), no
/// catalog bookkeeping (`fetched_at`/`expires_at`/`status`), and no secret —
/// `account_ref` is the opaque #1680 `auth_ref`, never a Vault member name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedDeployment {
    /// Catalog-controlled deployment identity (closed vocabulary; the value the
    /// receipt provenance chain records — not provider error text).
    pub deployment_id: String,
    pub wire_dialect: WireDialect,
    /// Opaque endpoint reference (an id/handle, resolved to a base_url by the
    /// executor — not the URL itself at this layer).
    pub endpoint_ref: String,
    /// The provider's own model id sent on the wire.
    pub provider_model_id: String,
    pub capabilities: DeploymentCapabilities,
    pub bounds: DeploymentBounds,
    /// Opaque #1680 auth/account reference. Never a credential member name.
    pub account_ref: String,
    /// Content-addressed pricing snapshot id (#1681 D1), or `None` when the
    /// catalog has no price for this deployment yet.
    pub pricing_snapshot_ref: Option<String>,
}

// ---------------------------------------------------------------------------
// 3. ResolutionOutcome (codex #1682 BUG-10 full vocabulary)
// ---------------------------------------------------------------------------

/// Why a candidate deployment was excluded from selection. One variant per
/// filter axis the #1681 D5 resolver applies; the closed set is exhaustively
/// exercised by `exclusion_reason_variants_are_exhaustively_constructible`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    /// The candidate's capabilities do not meet the request (e.g. no
    /// `structured_output`, no `tools`).
    CapabilityMismatch,
    /// The candidate's `region` is not permitted for this request.
    RegionBlocked,
    /// The candidate's `data_policy` is not permitted for this request.
    DataPolicyBlocked,
    /// Selecting the candidate would exceed the budget ceiling in context.
    BudgetExceeded,
    /// The candidate's deployment health is in cooldown (429/quota/timeout/5xx
    /// per #1681 D4).
    HealthCooldown,
    /// The candidate comes from a catalog snapshot past `expires_at` — stale and
    /// non-authoritative (#1681 D3/PR-B).
    StaleCatalog,
    /// The candidate was not in the admitted candidate set (a gate cut it, not
    /// the resolver — recorded so exclusion is visible, per #1675 PR2).
    NotAdmitted,
    /// The candidate's deployment `status` is not active (retired/disabled).
    DeploymentInactive,
}

impl ExclusionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CapabilityMismatch => "capability_mismatch",
            Self::RegionBlocked => "region_blocked",
            Self::DataPolicyBlocked => "data_policy_blocked",
            Self::BudgetExceeded => "budget_exceeded",
            Self::HealthCooldown => "health_cooldown",
            Self::StaleCatalog => "stale_catalog",
            Self::NotAdmitted => "not_admitted",
            Self::DeploymentInactive => "deployment_inactive",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "capability_mismatch" => Self::CapabilityMismatch,
            "region_blocked" => Self::RegionBlocked,
            "data_policy_blocked" => Self::DataPolicyBlocked,
            "budget_exceeded" => Self::BudgetExceeded,
            "health_cooldown" => Self::HealthCooldown,
            "stale_catalog" => Self::StaleCatalog,
            "not_admitted" => Self::NotAdmitted,
            "deployment_inactive" => Self::DeploymentInactive,
            _ => return None,
        })
    }

    /// Every filter axis, in declaration order. The exhaustiveness test asserts
    /// this slice covers the enum.
    pub const ALL: &'static [ExclusionReason] = &[
        Self::CapabilityMismatch,
        Self::RegionBlocked,
        Self::DataPolicyBlocked,
        Self::BudgetExceeded,
        Self::HealthCooldown,
        Self::StaleCatalog,
        Self::NotAdmitted,
        Self::DeploymentInactive,
    ];
}

/// One candidate's disposition in a resolution: its identity, and — if it was
/// dropped — the single axis that dropped it. `exclusion: None` means the
/// candidate was eligible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateEvaluation {
    pub deployment_id: String,
    pub exclusion: Option<ExclusionReason>,
}

impl CandidateEvaluation {
    pub fn eligible(deployment_id: impl Into<String>) -> Self {
        Self {
            deployment_id: deployment_id.into(),
            exclusion: None,
        }
    }

    pub fn excluded(deployment_id: impl Into<String>, reason: ExclusionReason) -> Self {
        Self {
            deployment_id: deployment_id.into(),
            exclusion: Some(reason),
        }
    }

    pub fn is_eligible(&self) -> bool {
        self.exclusion.is_none()
    }
}

/// The three revisions a resolution is stamped against, so a consumer can
/// assert the resolution was computed over the inputs it thinks it was (#1681
/// D5). All are captured from the frozen input snapshot, never re-read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionRevisions {
    /// The catalog snapshot revision the candidate metadata came from.
    pub catalog_revision: String,
    /// When the health snapshot used for cooldown admission was observed.
    pub health_observed_at: String,
    /// The alias-set policy revision the `ModelRef` was resolved under.
    pub policy_revision: String,
}

/// A pre-invocation budget estimate for the chosen deployment. Cost computation
/// abstains until #1681's pricing catalog lands (D5/D6): the estimate carries
/// the pricing snapshot ref opaquely and leaves `estimated_cost_usd = None`
/// until a real price sheet is joined.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetEstimate {
    pub estimated_prompt_tokens: Option<u32>,
    pub estimated_completion_tokens: Option<u32>,
    /// `None` until #1681 pricing is joined — self-reported vs computed cost
    /// stay distinguishable forever (D6).
    pub estimated_cost_usd: Option<f64>,
    pub pricing_snapshot_ref: Option<String>,
}

/// What the resolver selected: one deployment, or a typed abstain.
///
/// `Abstain` is an honest terminal — "nothing in the admitted set survived the
/// filters" — not an error. The `candidates` list on the outcome carries the
/// per-axis reasons that explain it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "deployment")]
pub enum Selection {
    Chosen(ResolvedDeployment),
    Abstain,
}

impl Selection {
    pub fn chosen(&self) -> Option<&ResolvedDeployment> {
        match self {
            Self::Chosen(d) => Some(d),
            Self::Abstain => None,
        }
    }

    pub fn is_abstain(&self) -> bool {
        matches!(self, Self::Abstain)
    }
}

/// The complete output of one resolution — the full BUG-10 vocabulary.
///
/// Every candidate the resolver considered is listed with its disposition (not
/// just the winner), so exclusion is visible rather than silent (#1675 PR2 /
/// `recommendation.rs` precedent). The `fallback_order` is bounded by
/// [`FALLBACK_ORDER_CAP`]; build outcomes through [`ResolutionOutcome::new`] to
/// have that enforced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolutionOutcome {
    /// Every candidate the resolver saw, eligible and excluded alike.
    pub candidates: Vec<CandidateEvaluation>,
    /// The chosen deployment or a typed abstain.
    pub selection: Selection,
    /// Catalog / health / policy revisions this resolution was computed against.
    pub revisions: ResolutionRevisions,
    /// The opaque #1680 account/credential ref of the chosen deployment;
    /// `None` on abstain.
    pub account_ref: Option<String>,
    /// A pre-invocation budget estimate for the chosen deployment.
    pub budget_estimate: BudgetEstimate,
    /// Deployment ids to try in order if the chosen one fails, bounded by
    /// [`FALLBACK_ORDER_CAP`]. #1682 executes this order and records the chain
    /// truthfully into the receipt; it does not author it.
    pub fallback_order: Vec<String>,
}

impl ResolutionOutcome {
    /// Build a bounded outcome.
    ///
    /// # Errors
    ///
    /// [`SeamError::FallbackOrderTooLong`] if `fallback_order` exceeds
    /// [`FALLBACK_ORDER_CAP`].
    pub fn new(
        candidates: Vec<CandidateEvaluation>,
        selection: Selection,
        revisions: ResolutionRevisions,
        account_ref: Option<String>,
        budget_estimate: BudgetEstimate,
        fallback_order: Vec<String>,
    ) -> Result<Self, SeamError> {
        if fallback_order.len() > FALLBACK_ORDER_CAP {
            return Err(SeamError::FallbackOrderTooLong {
                len: fallback_order.len(),
            });
        }
        Ok(Self {
            candidates,
            selection,
            revisions,
            account_ref,
            budget_estimate,
            fallback_order,
        })
    }
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
    pub catalog_revision: String,
    /// Deployment ids the catalog considers stale (past `expires_at`).
    pub stale_deployment_ids: Vec<String>,
}

/// A per-deployment cooldown as observed by the health authority (#1681 D4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentCooldown {
    pub deployment_id: String,
    /// ISO timestamp the cooldown lifts, if bounded.
    pub cooldown_until: Option<String>,
}

/// Deployment-health facts the resolver reads. `observed_at` is stamped into
/// the outcome's revisions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthSnapshot {
    pub observed_at: String,
    pub cooldowns: Vec<DeploymentCooldown>,
}

/// Account-availability facts the resolver reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountAvailability {
    pub account_ref: String,
    /// Whether the account is admitted to serve this request.
    pub admitted: bool,
}

/// Account-side facts (memcore-owned plain data).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSnapshot {
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
    pub pinned_deployment_id: Option<String>,
}

/// The typed retry-context input slot (#1681 D7 discrimination 7 boundary):
/// which attempt this is and what failures preceded it. #1681's real resolver
/// uses it for fallback/backoff; the fixture ignores it but the slot is frozen
/// so streaming-continuation retry has a home.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryContext {
    pub attempt: u32,
    pub prior_failures: Vec<HealthObservation>,
}

/// The complete input to a resolution: the already-admitted candidate set plus
/// the frozen snapshots and contexts. All memcore-owned data — there is no
/// server/llm/dispatch policy type reachable from here.
///
/// The one constructor takes an **already-admitted** candidate set: the
/// resolver cannot enumerate the catalog itself. "Healthy cheap ≠ semantically
/// eligible" is enforced by this type shape (#1681 D4), not by a runtime check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolverInput {
    pub model_ref: ModelRef,
    /// The admitted candidate deployments (a gate already cut the set).
    pub admitted_candidates: Vec<ResolvedDeployment>,
    pub catalog: CatalogSnapshot,
    pub health: HealthSnapshot,
    pub accounts: AccountSnapshot,
    pub budget: BudgetContext,
    pub pin: PinContext,
    pub retry: RetryContext,
}

/// The operational resolver seam.
///
/// A pure function from a frozen input snapshot to a [`ResolutionOutcome`].
/// Identical inputs must give identical outputs (deterministic ordering
/// `pin > health > price > deployment_id`, all against the frozen snapshot).
/// Abstain is a valid outcome, so there is no `Result` — a resolution never
/// "fails", it selects or abstains with visible reasons.
///
/// #1681 PR-D ships the real implementation; #1682 develops against
/// [`StaticFixtureResolver`] until then.
pub trait OperationalResolver {
    fn resolve(&self, input: &ResolverInput) -> ResolutionOutcome;
}

/// A deterministic fixture resolver so #1682 can build before #1681 PR-D lands.
///
/// It is a real (if simplified) pure function over the input, not a canned
/// constant, so #1682 gets realistic outcome shapes:
///
/// 1. Each admitted candidate is evaluated: stale (per catalog) →
///    `StaleCatalog`; on health cooldown → `HealthCooldown`; its account not
///    admitted → `NotAdmitted`; otherwise eligible.
/// 2. Eligible candidates are ordered `pin-first, then deployment_id
///    lexicographic`. (Price ordering is #1681's real resolver's job — the
///    fixture has no price numbers, only opaque refs — so it uses the
///    lexicographic tiebreak deterministically. Documented, not hidden.)
/// 3. The first eligible is chosen; the rest become the fallback order, capped
///    at [`FALLBACK_ORDER_CAP`]. No eligible → abstain.
///
/// Given identical input this always produces identical output.
#[derive(Debug, Clone, Copy, Default)]
pub struct StaticFixtureResolver;

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
            let reason = if stale_ids.contains(dep.deployment_id.as_str()) {
                Some(ExclusionReason::StaleCatalog)
            } else if cooldown_ids.contains(dep.deployment_id.as_str()) {
                Some(ExclusionReason::HealthCooldown)
            } else if unadmitted_accounts.contains(dep.account_ref.as_str()) {
                Some(ExclusionReason::NotAdmitted)
            } else {
                None
            };
            match reason {
                Some(r) => candidates.push(CandidateEvaluation::excluded(&dep.deployment_id, r)),
                None => {
                    candidates.push(CandidateEvaluation::eligible(&dep.deployment_id));
                    eligible.push(dep);
                }
            }
        }

        // Ordering: pin-first, then deployment_id lexicographic.
        let pinned = input.pin.pinned_deployment_id.as_deref();
        eligible.sort_by(|a, b| {
            let a_pin = pinned == Some(a.deployment_id.as_str());
            let b_pin = pinned == Some(b.deployment_id.as_str());
            // pinned sorts first: descending on the bool.
            b_pin
                .cmp(&a_pin)
                .then_with(|| a.deployment_id.cmp(&b.deployment_id))
        });

        let revisions = ResolutionRevisions {
            catalog_revision: input.catalog.catalog_revision.clone(),
            health_observed_at: input.health.observed_at.clone(),
            policy_revision: input.model_ref.policy_revision().to_string(),
        };

        match eligible.split_first() {
            Some((chosen, rest)) => {
                let fallback_order: Vec<String> = rest
                    .iter()
                    .take(FALLBACK_ORDER_CAP)
                    .map(|d| d.deployment_id.clone())
                    .collect();
                let budget_estimate = BudgetEstimate {
                    estimated_prompt_tokens: None,
                    estimated_completion_tokens: None,
                    estimated_cost_usd: None,
                    pricing_snapshot_ref: chosen.pricing_snapshot_ref.clone(),
                };
                ResolutionOutcome {
                    candidates,
                    selection: Selection::Chosen((*chosen).clone()),
                    revisions,
                    account_ref: Some(chosen.account_ref.clone()),
                    budget_estimate,
                    fallback_order,
                }
            }
            None => ResolutionOutcome {
                candidates,
                selection: Selection::Abstain,
                revisions,
                account_ref: None,
                budget_estimate: BudgetEstimate::default(),
                fallback_order: Vec::new(),
            },
        }
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
#[serde(rename_all = "snake_case")]
pub enum ObservationEvidence {
    /// Observed by a non-generating probe.
    Probed,
    /// Reported by a consumer of the deployment (the invocation path).
    SelfReported,
}

/// The class of an invocation error, aligned with the #1681 D4 attribution
/// rules. Closed set: the executor classifies a failure into exactly one of
/// these, and the D4 rule decides which health authority each touches
/// (`AuthInvalid` never touches deployment health; `RateLimited` dual-records;
/// the rest are deployment-only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationErrorClass {
    /// 401 / 403 — credential/account surfaces only, never deployment health.
    AuthInvalid,
    /// 429 / quota — dual-record (deployment cooldown + credential rate-limit).
    RateLimited,
    /// Request timed out.
    Timeout,
    /// 5xx server error.
    ServerError,
    /// Malformed / unparseable / protocol-violating response.
    Protocol,
}

impl InvocationErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthInvalid => "auth_invalid",
            Self::RateLimited => "rate_limited",
            Self::Timeout => "timeout",
            Self::ServerError => "server_error",
            Self::Protocol => "protocol",
        }
    }

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
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum RetryAfter {
    /// `Retry-After: 120` — delta seconds.
    Seconds(u64),
    /// `Retry-After: <HTTP-date>` — preserved as the received string.
    At(String),
}

/// What #1682 reports back to the health layer for one invocation outcome: the
/// precise account/deployment pair, the error class, and any `Retry-After`.
/// This is exactly #1681 discrimination 6's input type.
///
/// Note the pairing of `account_ref` and `deployment_id`: the D4 attribution
/// rule needs both, because a 401/403 must reach the account surface while
/// never touching deployment health, and a 429 must reach both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthObservation {
    /// Opaque #1680 account/credential ref the invocation used.
    pub account_ref: String,
    /// The deployment the invocation targeted.
    pub deployment_id: String,
    pub error_class: InvocationErrorClass,
    /// The wire `Retry-After`, if the response carried one.
    pub retry_after: Option<RetryAfter>,
    /// When the outcome was observed (ISO-8601).
    pub observed_at: String,
    pub evidence: ObservationEvidence,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_deployment(id: &str, account: &str) -> ResolvedDeployment {
        ResolvedDeployment {
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
            let mut dep = sample_deployment("dep-x", "acct-x");
            dep.wire_dialect = d;
            round_trip(&dep);
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
        assert!(abstain.selection.is_abstain());
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
        round_trip(&HealthObservation {
            account_ref: "acct-1".to_string(),
            deployment_id: "dep-a".to_string(),
            error_class: InvocationErrorClass::RateLimited,
            retry_after: Some(RetryAfter::Seconds(120)),
            observed_at: "2026-08-11T00:00:00Z".to_string(),
            evidence: ObservationEvidence::SelfReported,
        });
        // The HTTP-date Retry-After form round-trips too.
        round_trip(&HealthObservation {
            account_ref: "acct-1".to_string(),
            deployment_id: "dep-a".to_string(),
            error_class: InvocationErrorClass::ServerError,
            retry_after: Some(RetryAfter::At("Wed, 21 Oct 2026 07:28:00 GMT".to_string())),
            observed_at: "2026-08-11T00:00:00Z".to_string(),
            evidence: ObservationEvidence::Probed,
        });
    }

    // --- Discrimination: ExclusionReason exhaustive construction ---

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
                | ExclusionReason::RegionBlocked
                | ExclusionReason::DataPolicyBlocked
                | ExclusionReason::BudgetExceeded
                | ExclusionReason::HealthCooldown
                | ExclusionReason::StaleCatalog
                | ExclusionReason::NotAdmitted
                | ExclusionReason::DeploymentInactive => {}
            }
        }
        assert_eq!(ExclusionReason::ALL.len(), 8);
        for &r in ExclusionReason::ALL {
            assert_all_covered(r);
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
            outcome.selection.chosen().map(|d| d.deployment_id.as_str()),
            Some("dep-a")
        );
        assert!(outcome.fallback_order.is_empty());
        // Every candidate is represented with its disposition.
        let by_id = |id: &str| {
            outcome
                .candidates
                .iter()
                .find(|c| c.deployment_id == id)
                .unwrap()
                .exclusion
        };
        assert_eq!(by_id("dep-a"), None);
        assert_eq!(by_id("dep-b"), Some(ExclusionReason::HealthCooldown));
        assert_eq!(by_id("dep-c"), Some(ExclusionReason::StaleCatalog));
        // Revisions are stamped from the frozen snapshot.
        assert_eq!(outcome.revisions.catalog_revision, "cat-rev-1");
        assert_eq!(outcome.revisions.policy_revision, "policy-rev-1");
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
            outcome.selection.chosen().map(|d| d.deployment_id.as_str()),
            Some("dep-c"),
            "pin must win over lexicographic order"
        );
        // Remaining eligible become the (bounded) fallback order, lexicographic.
        assert_eq!(outcome.fallback_order, vec!["dep-a", "dep-b"]);
    }

    #[test]
    fn fallback_order_cap_is_enforced_by_constructor() {
        let over = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
            "e".to_string(),
        ];
        let err = ResolutionOutcome::new(
            vec![],
            Selection::Abstain,
            ResolutionRevisions {
                catalog_revision: "c".to_string(),
                health_observed_at: "t".to_string(),
                policy_revision: "p".to_string(),
            },
            None,
            BudgetEstimate::default(),
            over,
        )
        .unwrap_err();
        assert_eq!(err, SeamError::FallbackOrderTooLong { len: 5 });
    }

    #[test]
    fn model_ref_rejects_empty_inputs() {
        assert_eq!(ModelRef::new("  ", "rev"), Err(SeamError::EmptyReference));
        assert_eq!(ModelRef::new("m", "  "), Err(SeamError::EmptyRevision));
    }

    // --- Discrimination: dependency direction (structural self-check) ---

    #[test]
    fn module_has_no_external_tachi_crate_imports() {
        // The seam must be memcore-native: zero imports of tachi-server /
        // tachi-llm / tachi-dispatch (codex #1681 OK-BUT-8). The needle is
        // assembled at runtime so this test's own source does not match it.
        let src = include_str!("model_broker_seam.rs");
        let needle = format!("use {}{}", "tachi", "_");
        assert!(
            !src.contains(&needle),
            "seam module must not import external tachi_* crates"
        );
    }
}
