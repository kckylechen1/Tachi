//! The operational resolver (tachi#1681 D5, D7 row D) — a pure function from a
//! frozen snapshot to a [`ResolutionOutcome`].
//!
//! # What it decides, and what it may never decide
//!
//! It decides **operational** eligibility: can this deployment physically and
//! lawfully serve this request right now — capability, bounds, region, data
//! policy, account availability, health cooldown, staleness, budget — and, of
//! the survivors, which one. It never decides whether a deployment is *good
//! for the task*. Task-quality routing is #1675's property, and the structural
//! guarantee that this file cannot encroach on it is that
//! [`ResolverInput::admitted_candidates`] is the resolver's entire universe:
//! there is no catalog handle here to enumerate more. A deployment a semantic
//! gate cut is not merely rejected — it is invisible, and cannot be
//! reintroduced by being cheap or healthy (discrimination 2). Gates cut the
//! set; they do not report beside it (the #1675 PR2 rule).
//!
//! # Fail loud, never guess (discrimination 9)
//!
//! Every way this resolver can fail to pick something is a *typed* answer:
//!
//! - an unknown reference → [`AbstainReason::UnknownAlias`];
//! - a reference that reads as both an alias and a deployment id →
//!   [`AbstainReason::AmbiguousAlias`];
//! - a `ModelRef` minted under a different alias set →
//!   [`AbstainReason::PolicyRevisionMismatch`];
//! - nothing to evaluate → [`AbstainReason::EmptyCandidateSet`];
//! - everything evaluated and excluded → [`AbstainReason::NoEligibleCandidate`],
//!   with the axis that dropped each candidate on the outcome.
//!
//! There is no code path that substitutes a default deployment for a reference
//! that did not resolve. That is the whole point of the alias layer's design:
//! a silent fallback would make an unknown alias indistinguishable from a
//! working one, and routing would be unreproducible from the record.
//!
//! # Where the inputs live, and why they are split in two
//!
//! [`ResolverInput`] is the **frozen seam** (#1739): the per-request snapshot
//! #1681 and #1682 agreed on. It deliberately carries no alias set, no request
//! requirement axes, no price sheet and no per-deployment region/data-policy —
//! [`ResolvedDeployment`] is the resolved *projection*, not the catalog row.
//! Extending that frozen struct would fork the contract, so the catalog-side
//! facts the seam has no slot for live on the resolver value itself
//! ([`CatalogResolver`]), which the caller builds per request. Both halves are
//! memcore-owned plain data, so the D5 dependency-direction constraint (codex
//! #1681 OK-BUT-8) holds: nothing here can pull a policy type down from
//! tachi-server, tachi-llm or tachi-dispatch.
//!
//! # Two different postures toward "unknown", on purpose
//!
//! - A **deployment-declared limit** that is absent means *no declared limit*.
//!   A row with `context_window: None` is not excluded by a request that
//!   states a prompt size; the env-imported catalog declares no bounds at all
//!   (#1681 D7 PR-B), and excluding on absence would empty every candidate set
//!   in production.
//! - A **caller-imposed constraint** that cannot be proven satisfied excludes.
//!   A region allowlist with no placement fact for the candidate, or a budget
//!   ceiling with no price for it, yields [`ExclusionReason::RegionBlocked`] /
//!   [`ExclusionReason::BudgetExceeded`]. Compliance and spend are refusals by
//!   default; "we could not tell" is not permission.

use std::collections::{BTreeMap, BTreeSet};

use crate::model_broker_seam::{
    AbstainReason, BudgetEstimate, CandidateEvaluation, DeploymentCapabilities, ExclusionReason,
    OperationalResolver, ResolutionOutcome, ResolutionRevisions, ResolvedDeployment, ResolverInput,
    Selection, FALLBACK_ORDER_CAP,
};

use super::alias_policy::AliasSetSnapshot;

/// Micro-USD per USD, the unit prices and ceilings are compared in.
///
/// Money is compared as integers so ordering and the ceiling test are exact
/// and reproducible; `f64` appears only at the two edges (the caller's ceiling
/// and the reported estimate), never in a comparison that decides routing.
const MICROS_PER_USD: u128 = 1_000_000;

/// Tokens per unit of the price sheet's quote (prices are per million tokens).
const TOKENS_PER_PRICE_UNIT: u128 = 1_000_000;

// ─── Catalog-side facts the seam's ResolvedDeployment does not carry ─────────

/// Where a deployment physically sits and under what data policy — the two
/// `model_deployments` columns the resolved projection drops.
///
/// `active` is here for completeness of the exclusion vocabulary
/// ([`ExclusionReason::DeploymentInactive`]): a caller that assembles its
/// candidate set through [`crate::catalog::AuthoritativeDeployment`] has
/// already refused retired rows, but a caller that assembles it another way
/// must still be able to have that verdict reported rather than silently
/// dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentPlacement {
    pub deployment_id: String,
    pub region: Option<String>,
    pub data_policy: Option<String>,
    pub active: bool,
}

impl DeploymentPlacement {
    /// A placement that constrains nothing: active, no declared region, no
    /// declared data policy. What an env-imported row looks like today.
    pub fn unconstrained(deployment_id: impl Into<String>) -> Self {
        Self {
            deployment_id: deployment_id.into(),
            region: None,
            data_policy: None,
            active: true,
        }
    }

    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = Some(region.into());
        self
    }

    pub fn with_data_policy(mut self, data_policy: impl Into<String>) -> Self {
        self.data_policy = Some(data_policy.into());
        self
    }

    pub fn retired(mut self) -> Self {
        self.active = false;
        self
    }
}

/// One content-addressed price sheet's unit prices, in micro-USD per million
/// tokens.
///
/// Integers, not `f64`: this number decides an ordering and a ceiling test, and
/// two deployments whose float prices differ in the last bit must not swap
/// places depending on how the sheet was parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotPrice {
    /// The `pricing_snapshots.snapshot_id` these prices belong to.
    pub pricing_snapshot_ref: String,
    pub prompt_micros_per_mtok: u64,
    pub completion_micros_per_mtok: u64,
}

/// What the request needs, on the axes the resolver filters.
///
/// [`DeploymentCapabilities`] is reused as the *required* mask rather than
/// growing a parallel "requirement" type: `true` means "the deployment must
/// have it", `false` means "do not care". A second boolean vocabulary for the
/// same six axes is exactly how a capability gets checked on one side and
/// forgotten on the other.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestRequirements {
    pub required_capabilities: DeploymentCapabilities,
    /// Prompt size the caller declares, in tokens.
    pub prompt_tokens: Option<u32>,
    /// Output ceiling the caller asks for, in tokens.
    pub max_output_tokens: Option<u32>,
    /// Attachment payload the caller carries, in bytes.
    pub attachment_bytes: Option<u64>,
    /// The vector width the stored index was built at (#1681 D3). A candidate
    /// that does not declare exactly this width is refused loudly rather than
    /// silently corrupting comparability.
    pub required_embedding_dimensions: Option<u32>,
    /// Regions the request may run in. Empty = unconstrained.
    pub allowed_regions: Vec<String>,
    /// Data policies the request may run under. Empty = unconstrained.
    pub allowed_data_policies: Vec<String>,
}

// ─── Reference classification ───────────────────────────────────────────────

/// How the resolver reads a [`crate::model_broker_seam::ModelRef`]'s reference
/// string.
///
/// Exposed because the same classification is what an ingress reporting gate
/// needs (#1681 PR-D debt (a)): "did a caller hand us a string that only the
/// resolver could have turned into a deployment" is answerable with exactly
/// this function, and answering it the same way in both places is the point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceClass {
    /// Exactly one active alias binds this name.
    Alias,
    /// The name is a known deployment id and no alias claims it.
    Deployment,
    /// The name reads as both an alias and a deployment id. There is no
    /// deterministic winner, and inventing one would make routing depend on
    /// which reading a future refactor happened to try first.
    Ambiguous,
    /// Neither an alias nor a known deployment.
    Unknown,
}

// ─── The resolver ───────────────────────────────────────────────────────────

/// The #1681 D5 operational resolver.
///
/// Holds the catalog-side facts and request requirements the frozen
/// [`ResolverInput`] has no slot for; built per request (it is plain data, and
/// construction is a move) and then applied to the seam input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogResolver {
    aliases: AliasSetSnapshot,
    placements: Vec<DeploymentPlacement>,
    prices: Vec<SnapshotPrice>,
    requirements: RequestRequirements,
}

/// Why a resolution could not even be *reported*.
///
/// Distinct from an abstain: an abstain is a resolution that ran and selected
/// nothing, and it is stamped with the revisions it ran against. A refusal
/// means the input could not be stamped at all, so no outcome — not even an
/// abstaining one — could truthfully be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveRefusal {
    /// A snapshot arrived without the stamp every outcome must carry.
    UnstampableInput { field: &'static str },
}

impl std::fmt::Display for ResolveRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnstampableInput { field } => write!(
                f,
                "resolver input carries a blank `{field}`; a resolution nobody can verify the \
                 inputs of is not a resolution"
            ),
        }
    }
}

impl std::error::Error for ResolveRefusal {}

impl CatalogResolver {
    /// Build a resolver for one request.
    pub fn new(
        aliases: AliasSetSnapshot,
        placements: Vec<DeploymentPlacement>,
        prices: Vec<SnapshotPrice>,
        requirements: RequestRequirements,
    ) -> Self {
        Self {
            aliases,
            placements,
            prices,
            requirements,
        }
    }

    /// The alias set this resolver resolves against.
    pub fn aliases(&self) -> &AliasSetSnapshot {
        &self.aliases
    }

    /// How a reference string reads against this resolver's alias set and the
    /// deployment ids it knows about.
    ///
    /// `known_deployment_ids` is the union of the catalog placements this
    /// resolver holds and whatever ids the caller can see; a resolution passes
    /// its admitted candidates.
    pub fn classify_reference<'a>(
        &self,
        reference: &str,
        known_deployment_ids: impl IntoIterator<Item = &'a str>,
    ) -> ReferenceClass {
        let is_alias = self.aliases.get(reference).is_some();
        let is_deployment = self
            .placements
            .iter()
            .any(|placement| placement.deployment_id == reference)
            || known_deployment_ids
                .into_iter()
                .any(|candidate| candidate == reference);
        match (is_alias, is_deployment) {
            (true, true) => ReferenceClass::Ambiguous,
            (true, false) => ReferenceClass::Alias,
            (false, true) => ReferenceClass::Deployment,
            (false, false) => ReferenceClass::Unknown,
        }
    }

    /// Resolve one request against a frozen input snapshot.
    ///
    /// The fallible form, and the one production callers should use: the seam's
    /// [`OperationalResolver`] trait cannot report an unstampable input,
    /// because its return type has no room for one.
    ///
    /// # Errors
    ///
    /// [`ResolveRefusal::UnstampableInput`] when the input's catalog revision
    /// or health `observed_at` is blank.
    pub fn try_resolve(&self, input: &ResolverInput) -> Result<ResolutionOutcome, ResolveRefusal> {
        // Stamped with the input's own strings, not a normalized copy: the
        // consumer's check is `stamped == the snapshot I passed in`, and a
        // resolver that quietly trimmed would fail that check for a reason the
        // consumer cannot see.
        let revisions = ResolutionRevisions::new(
            input.catalog.catalog_revision.as_str(),
            input.health.observed_at.as_str(),
            input.model_ref.policy_revision(),
        )
        .map_err(|err| ResolveRefusal::UnstampableInput {
            field: match err {
                crate::model_broker_seam::SeamError::EmptyField { field } => field,
                // `ResolutionRevisions::new` raises nothing else; naming the
                // stamp keeps the refusal honest if it ever does.
                _ => "revisions",
            },
        })?;

        // Step 1 — the alias set the reference was minted under must be the
        // alias set being resolved against. Checked before anything is
        // evaluated: filtering against a drifted set would produce an outcome
        // that looks computed but answers a question nobody asked.
        if input.model_ref.policy_revision() != self.aliases.policy_revision() {
            return Ok(abstain(revisions, AbstainReason::PolicyRevisionMismatch));
        }

        // Step 2 — read the reference. Unknown and ambiguous are terminal and
        // loud; neither may fall through to "some deployment".
        let admitted_ids: Vec<&str> = input
            .admitted_candidates
            .iter()
            .map(ResolvedDeployment::deployment_id)
            .collect();
        let reference = input.model_ref.reference();
        let candidate_ids: BTreeSet<&str> = match self
            .classify_reference(reference, admitted_ids.iter().copied())
        {
            ReferenceClass::Unknown => return Ok(abstain(revisions, AbstainReason::UnknownAlias)),
            ReferenceClass::Ambiguous => {
                return Ok(abstain(revisions, AbstainReason::AmbiguousAlias))
            }
            ReferenceClass::Alias => self
                .aliases
                .get(reference)
                .expect("classified as an alias by this same set")
                .bindings
                .iter()
                .map(|binding| binding.deployment_id.as_str())
                .collect(),
            ReferenceClass::Deployment => BTreeSet::from([reference]),
        };

        // Step 3 — the reference's bindings, intersected with what the semantic
        // gate admitted. Input order is preserved so the candidate list reads
        // back the way the caller assembled it; a repeated id is the same
        // deployment twice, and two dispositions for one id would make the
        // list uninterpretable, so the first occurrence stands.
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let considered: Vec<&ResolvedDeployment> = input
            .admitted_candidates
            .iter()
            .filter(|deployment| candidate_ids.contains(deployment.deployment_id()))
            .filter(|deployment| seen.insert(deployment.deployment_id()))
            .collect();
        if considered.is_empty() {
            // Nothing was evaluated: either the gate admitted nothing, or it
            // admitted nothing this reference binds. Both are honestly "no
            // candidates", and the empty candidate list on the outcome says
            // exactly that.
            return Ok(abstain(revisions, AbstainReason::EmptyCandidateSet));
        }

        // Step 4 — one axis per candidate, evaluated in a fixed order so the
        // reported reason is a property of the input and not of iteration.
        let context = FilterContext::new(input, self);
        let mut candidates = Vec::with_capacity(considered.len());
        let mut eligible: Vec<&ResolvedDeployment> = Vec::new();
        for deployment in &considered {
            match context.exclusion_for(deployment) {
                Some(reason) => candidates.push(CandidateEvaluation::excluded(
                    deployment.deployment_id(),
                    reason,
                )),
                None => {
                    candidates.push(CandidateEvaluation::eligible(deployment.deployment_id()));
                    eligible.push(deployment);
                }
            }
        }

        // Step 5 — D5's deterministic order. Health admission is a filter
        // rather than a comparator here because the frozen `HealthSnapshot`
        // carries cooldowns and nothing else: a candidate is either cooling
        // (and excluded above) or it is not, so among the survivors the order
        // reads pin > snapshot price > deployment_id. The tiebreak is total,
        // which is what makes identical inputs give identical outputs
        // (discrimination 1).
        let pinned = input.pin.pinned_deployment_id.as_deref();
        eligible.sort_by(|left, right| {
            let left_pinned = pinned == Some(left.deployment_id());
            let right_pinned = pinned == Some(right.deployment_id());
            right_pinned
                .cmp(&left_pinned)
                .then_with(|| {
                    price_rank(self.price_of(left)).cmp(&price_rank(self.price_of(right)))
                })
                .then_with(|| left.deployment_id().cmp(right.deployment_id()))
        });

        let Some((chosen, rest)) = eligible.split_first() else {
            return Ok(ResolutionOutcome::new(
                candidates,
                Selection::Abstain(AbstainReason::NoEligibleCandidate),
                revisions,
                None,
                BudgetEstimate::default(),
                Vec::new(),
            )
            .expect("every candidate was excluded, which is what NoEligibleCandidate states"));
        };

        // Step 6 — the fallback order is durable receipt provenance (#1681
        // review finding 4), so it carries deployment ids this resolution
        // actually evaluated and admitted, in the same deterministic order,
        // truncated at the frozen cap rather than growing with the catalog.
        let fallback_order: Vec<String> = rest
            .iter()
            .map(|deployment| deployment.deployment_id().to_string())
            .take(FALLBACK_ORDER_CAP)
            .collect();

        Ok(ResolutionOutcome::new(
            candidates,
            Selection::Chosen((*chosen).clone()),
            revisions,
            Some(chosen.account_ref().to_string()),
            self.budget_estimate_for(chosen),
            fallback_order,
        )
        .expect("the chosen and fallback ids are this resolution's own eligible candidates"))
    }

    /// The price sheet entry for a deployment, if the catalog has one.
    fn price_of(&self, deployment: &ResolvedDeployment) -> Option<&SnapshotPrice> {
        let reference = deployment.pricing_snapshot_ref()?;
        self.prices
            .iter()
            .find(|price| price.pricing_snapshot_ref == reference)
    }

    /// Lower-bound cost of this request on this deployment, in micro-USD.
    /// `None` when the catalog has no price for it — no bound is computable in
    /// either direction, which is a different fact from "it is free".
    fn cost_micros(&self, deployment: &ResolvedDeployment) -> Option<u128> {
        let price = self.price_of(deployment)?;
        let prompt = u128::from(self.requirements.prompt_tokens.unwrap_or(0));
        let completion = u128::from(self.requirements.max_output_tokens.unwrap_or(0));
        let prompt_cost = prompt * u128::from(price.prompt_micros_per_mtok) / TOKENS_PER_PRICE_UNIT;
        let completion_cost =
            completion * u128::from(price.completion_micros_per_mtok) / TOKENS_PER_PRICE_UNIT;
        Some(prompt_cost + completion_cost)
    }

    fn budget_estimate_for(&self, deployment: &ResolvedDeployment) -> BudgetEstimate {
        BudgetEstimate {
            estimated_prompt_tokens: self.requirements.prompt_tokens,
            estimated_completion_tokens: self.requirements.max_output_tokens,
            estimated_cost_usd: self
                .cost_micros(deployment)
                .map(|micros| micros as f64 / MICROS_PER_USD as f64),
            pricing_snapshot_ref: deployment.pricing_snapshot_ref().map(str::to_string),
        }
    }
}

/// A resolution that evaluated nothing, stamped with the inputs it was decided
/// against. The seam refuses an abstain that carries candidates for any reason
/// but `NoEligibleCandidate`, so these four all take the empty list.
fn abstain(revisions: ResolutionRevisions, reason: AbstainReason) -> ResolutionOutcome {
    debug_assert!(
        !matches!(reason, AbstainReason::NoEligibleCandidate),
        "NoEligibleCandidate needs the evaluated candidate list, not this helper"
    );
    ResolutionOutcome::new(
        Vec::new(),
        Selection::Abstain(reason),
        revisions,
        None,
        BudgetEstimate::default(),
        Vec::new(),
    )
    .expect("an abstain with no candidates, no account and no fallback is consistent by shape")
}

/// Cheaper sorts first; a deployment with no price sorts after every priced
/// one. Unknown is not "free" — it is unranked, and putting it last keeps a
/// missing price from winning a comparison it never entered.
fn price_rank(price: Option<&SnapshotPrice>) -> (bool, u64, u64) {
    match price {
        Some(price) => (
            false,
            price.prompt_micros_per_mtok,
            price.completion_micros_per_mtok,
        ),
        None => (true, u64::MAX, u64::MAX),
    }
}

// ─── The filter axes ────────────────────────────────────────────────────────

/// The per-request indexes the axes read, built once per resolution so a
/// candidate list of any size costs one pass over each snapshot.
struct FilterContext<'a> {
    resolver: &'a CatalogResolver,
    stale: BTreeSet<&'a str>,
    cooling: BTreeSet<&'a str>,
    unadmitted_accounts: BTreeSet<&'a str>,
    placements: BTreeMap<&'a str, &'a DeploymentPlacement>,
    ceiling_micros: Option<u128>,
}

impl<'a> FilterContext<'a> {
    fn new(input: &'a ResolverInput, resolver: &'a CatalogResolver) -> Self {
        let cooling = input
            .health
            .cooldowns
            .iter()
            .filter(|cooldown| {
                cooldown_in_force(
                    cooldown.cooldown_until.as_deref(),
                    &input.health.observed_at,
                )
            })
            .map(|cooldown| cooldown.deployment_id.as_str())
            .collect();
        Self {
            resolver,
            stale: input
                .catalog
                .stale_deployment_ids
                .iter()
                .map(String::as_str)
                .collect(),
            cooling,
            unadmitted_accounts: input
                .accounts
                .accounts
                .iter()
                .filter(|account| !account.admitted)
                .map(|account| account.account_ref.as_str())
                .collect(),
            placements: resolver
                .placements
                .iter()
                .map(|placement| (placement.deployment_id.as_str(), placement))
                .collect(),
            ceiling_micros: input.budget.ceiling_usd.map(usd_to_micros),
        }
    }

    /// The single axis that drops this candidate, or `None` if it survives.
    ///
    /// The order is fixed and documented because it decides *which* reason a
    /// candidate that fails several axes reports. It runs identity and
    /// authority first (is this row even usable, may this account serve),
    /// then what the deployment intrinsically cannot do, then the caller's
    /// placement policy, then the transient health state, and last the budget —
    /// so a throttled deployment reports `health_cooldown` rather than a
    /// budget verdict computed from a price it will not be asked for.
    fn exclusion_for(&self, deployment: &ResolvedDeployment) -> Option<ExclusionReason> {
        let id = deployment.deployment_id();
        let placement = self.placements.get(id).copied();

        if placement.is_some_and(|placement| !placement.active) {
            return Some(ExclusionReason::DeploymentInactive);
        }
        if self.stale.contains(id) {
            return Some(ExclusionReason::StaleCatalog);
        }
        if self.unadmitted_accounts.contains(deployment.account_ref()) {
            return Some(ExclusionReason::AccountNotAdmitted);
        }

        let requirements = &self.resolver.requirements;
        if capability_shortfall(
            requirements.required_capabilities,
            deployment.capabilities(),
        ) {
            return Some(ExclusionReason::CapabilityMismatch);
        }
        if let Some(required) = requirements.required_embedding_dimensions {
            if deployment.bounds().embedding_dimensions != Some(required) {
                return Some(ExclusionReason::EmbeddingDimensionMismatch);
            }
        }
        if exceeds(
            requirements.prompt_tokens,
            deployment.bounds().context_window,
        ) {
            return Some(ExclusionReason::ContextWindowExceeded);
        }
        if exceeds(
            requirements.max_output_tokens,
            deployment.bounds().max_output,
        ) {
            return Some(ExclusionReason::MaxOutputExceeded);
        }
        if exceeds(
            requirements.attachment_bytes,
            deployment.bounds().attachment_bytes,
        ) {
            return Some(ExclusionReason::AttachmentBoundsExceeded);
        }

        if !allowed(
            &requirements.allowed_regions,
            placement.and_then(|placement| placement.region.as_deref()),
        ) {
            return Some(ExclusionReason::RegionBlocked);
        }
        if !allowed(
            &requirements.allowed_data_policies,
            placement.and_then(|placement| placement.data_policy.as_deref()),
        ) {
            return Some(ExclusionReason::DataPolicyBlocked);
        }

        if self.cooling.contains(id) {
            return Some(ExclusionReason::HealthCooldown);
        }

        if let Some(ceiling) = self.ceiling_micros {
            match self.resolver.cost_micros(deployment) {
                // No price: the ceiling cannot be shown to hold, and an
                // unprovable ceiling is a refusal, not a pass.
                None => return Some(ExclusionReason::BudgetExceeded),
                Some(cost) if cost > ceiling => return Some(ExclusionReason::BudgetExceeded),
                Some(_) => {}
            }
        }

        None
    }
}

/// Whether a required capability the deployment lacks exists. `true` on the
/// requirement side means "must have"; `false` means "do not care", so this is
/// implication, not equality.
fn capability_shortfall(required: DeploymentCapabilities, offered: DeploymentCapabilities) -> bool {
    let axes = [
        (required.chat, offered.chat),
        (required.embeddings, offered.embeddings),
        (required.tools, offered.tools),
        (required.streaming, offered.streaming),
        (required.structured_output, offered.structured_output),
        (required.media, offered.media),
    ];
    axes.iter()
        .any(|(required, offered)| *required && !*offered)
}

/// Whether a declared need exceeds a declared limit. Absence on either side is
/// not a failure: an undeclared need asks for nothing, and an undeclared limit
/// is not a limit (see the module note on the two postures toward "unknown").
fn exceeds<T: PartialOrd>(need: Option<T>, limit: Option<T>) -> bool {
    match (need, limit) {
        (Some(need), Some(limit)) => need > limit,
        _ => false,
    }
}

/// Whether a candidate's declared placement satisfies an allowlist. An empty
/// allowlist constrains nothing; a non-empty one refuses a candidate that
/// declares nothing, because an undeclared region cannot be shown to be one of
/// the permitted ones.
fn allowed(allowlist: &[String], declared: Option<&str>) -> bool {
    if allowlist.is_empty() {
        return true;
    }
    declared.is_some_and(|declared| allowlist.iter().any(|allowed| allowed == declared))
}

/// Whether a cooldown is still in force at the instant the health snapshot was
/// observed.
///
/// An unbounded cooldown (`None`) is in force — that is what "no lift time"
/// means. A bounded one is in force until its instant passes. A timestamp that
/// will not parse is treated as in force: the fail-safe reading of an
/// unreadable cooldown is that the deployment is still cooling, because the
/// alternative sends traffic at something the health authority just told us to
/// stop hitting.
fn cooldown_in_force(cooldown_until: Option<&str>, observed_at: &str) -> bool {
    let Some(cooldown_until) = cooldown_until else {
        return true;
    };
    let (Ok(until), Ok(observed)) = (
        chrono::DateTime::parse_from_rfc3339(cooldown_until.trim()),
        chrono::DateTime::parse_from_rfc3339(observed_at.trim()),
    ) else {
        return true;
    };
    until > observed
}

/// A USD ceiling as micro-USD, rounded **down** so the integer comparison never
/// admits a request the stated ceiling does not cover. A negative or
/// non-finite ceiling floors at zero: it permits nothing, which is the only
/// safe reading of a ceiling that is not a number.
fn usd_to_micros(usd: f64) -> u128 {
    if !usd.is_finite() || usd <= 0.0 {
        return 0;
    }
    (usd * MICROS_PER_USD as f64).floor() as u128
}

// ─── The seam adapter ───────────────────────────────────────────────────────

/// The frozen seam trait, so #1682 can swap
/// [`crate::model_broker_seam::StaticFixtureResolver`] for this without
/// touching a call site.
///
/// # Panics
///
/// On a blank `catalog.catalog_revision` or `health.observed_at` — the
/// [`ResolutionOutcome`] this returns has nowhere to report an unstampable
/// input, and stamping it with a placeholder would produce a resolution whose
/// inputs nobody can verify. This is the *same* documented precondition
/// `StaticFixtureResolver` carries, so replacing the fixture with the real
/// resolver introduces no failure mode a #1682 consumer has not already had to
/// satisfy. Production callers use [`CatalogResolver::try_resolve`], which
/// reports the refusal instead.
impl OperationalResolver for CatalogResolver {
    fn resolve(&self, input: &ResolverInput) -> ResolutionOutcome {
        self.try_resolve(input)
            .unwrap_or_else(|refusal| panic!("{refusal}"))
    }
}

#[cfg(test)]
mod tests;
