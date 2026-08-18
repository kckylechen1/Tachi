use super::*;

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
/// **What "admitted" means here:** `admitted_candidates`
/// holds the deployments a *semantic* admission gate already passed — the
/// resolver receives the cut set and has no catalog handle to enumerate more.
/// That is the structural enforcement of "healthy cheap ≠ semantically
/// eligible" (#1681 D4): not a runtime check inside the resolver, but the
/// absence of any way for it to widen its own input. Deployments the gate cut
/// appear neither here nor in the outcome — gates cut the set, they do not
/// report beside it, so reporting them would be fabricating
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
pub trait OperationalResolver {
    /// Resolve one request against a frozen input snapshot.
    fn resolve(&self, input: &ResolverInput) -> ResolutionOutcome;
}

/// A deterministic fixture resolver for downstream tests.
///
/// **Not a production resolver, and mechanically so:** it is gated behind
/// `feature = "broker-fixtures"`, which is **off by default**, plus memcore's
/// own `cfg(test)`. A production build of memcore
/// does not compile this type at all, and the re-export in `lib.rs` carries the
/// same gate — downstream test targets must opt in explicitly
/// (`memcore = { …, features = ["broker-fixtures"] }` under `[dev-dependencies]`).
/// It applies a deliberately simplified filter set and has no price data, so
/// enabling it in production would silently downgrade routing.
///
/// It is a real (if simplified) pure function over the input, not a canned
/// constant, so tests exercise realistic outcome shapes:
///
/// 1. Each admitted candidate is evaluated: stale (per catalog) →
///    [`ExclusionReason::StaleCatalog`]; on health cooldown →
///    [`ExclusionReason::HealthCooldown`]; its account not admitted →
///    [`ExclusionReason::AccountNotAdmitted`]; otherwise eligible.
/// 2. Eligible candidates are ordered `pin-first, then deployment_id
///    lexicographic`. Price ordering belongs to the production resolver; the
///    fixture has no price numbers, only opaque refs, so it uses the
///    lexicographic tiebreak deterministically. Documented, not hidden.
/// 3. The first eligible is chosen; the rest become the fallback order, capped
///    at [`FALLBACK_ORDER_CAP`]. No candidates at all → abstain with
///    [`AbstainReason::EmptyCandidateSet`]; candidates but none eligible →
///    [`AbstainReason::NoEligibleCandidate`]. The alias/policy abstain reasons
///    are unreachable here by construction: the fixture holds no alias set to
///    disagree with, and inventing that verdict would be a lie about a check it
///    never ran. The production resolver owns those decisions.
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
