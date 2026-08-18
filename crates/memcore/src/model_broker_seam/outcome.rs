use super::*;

// ---------------------------------------------------------------------------
// 3. ResolutionOutcome
// ---------------------------------------------------------------------------

/// Why a candidate deployment was excluded from selection. One variant per
/// filter axis the #1681 D5 resolver applies; the closed set is exhaustively
/// exercised by `exclusion_reason_variants_are_exhaustively_constructible`.
///
/// **Scope of this vocabulary.** It covers candidates the resolver *saw* — i.e.
/// the contents of [`ResolverInput::admitted_candidates`]. A deployment that a
/// pre-resolver semantic gate cut never appears here at all: gates cut the set,
/// they do not report beside it, and reporting on invisible
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
    /// non-authoritative.
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
///
/// **Each reason states where the resolution stopped, so each implies a
/// candidate-list shape**, and [`ResolutionOutcome`] enforces the pairing —
/// the reason and the list are two statements about one resolution, and an
/// outcome that lets them disagree is a lie whichever half you believe:
///
/// | Reason | Implied `candidates` |
/// |---|---|
/// | `no_eligible_candidate` | non-empty, and none eligible |
/// | `empty_candidate_set` | empty |
/// | `unknown_alias` / `ambiguous_alias` / `policy_revision_mismatch` | empty |
///
/// The three request-level reasons take the empty shape because all three are
/// decided *before* per-candidate evaluation: the resolver resolves the
/// reference and asserts `stamped == recomputed` first, and only then filters.
/// Listing candidates it never looked at would mean marking each one
/// `eligible` — a disposition it never computed — and there is deliberately no
/// exclusion axis meaning "never evaluated"; that is the gate's territory.
/// Note this leaves no legal way to report an ambiguity discovered
/// *after* filtering, which is intentional: the D5 ordering
/// (`pin > health > price > deployment_id`) ends in a total lexicographic
/// tiebreak, so a post-evaluation tie cannot occur. Ambiguity is necessarily an
/// alias-set fact.
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
/// reported without saying why. For
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

/// The complete typed output of one resolution.
///
/// Every candidate the resolver considered is listed with its disposition, not
/// just the winner, so exclusion is visible rather than silent.
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
/// - an abstaining outcome carries neither `account_ref` nor `fallback_order`;
/// - an abstaining outcome's `candidates` matches what its [`AbstainReason`]
///   says was evaluated — `no_eligible_candidate` needs a non-empty list with
///   nothing eligible in it, and every other reason needs an empty one (see
///   the table on [`AbstainReason`]).
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
    /// [`SeamError::AbstainCarriesAccountRef`],
    /// [`SeamError::AbstainCarriesFallbackOrder`],
    /// [`SeamError::AbstainCarriesUnevaluatedCandidates`],
    /// [`SeamError::AbstainNoEligibleWithoutCandidates`], or
    /// [`SeamError::AbstainNoEligibleWithEligibleCandidate`] — see the type docs
    /// for the consistency set each one guards.
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
    /// [`FALLBACK_ORDER_CAP`]. A caller executes this order and records the
    /// chain truthfully into its receipt; it does not author it.
    pub fn fallback_order(&self) -> &[String] {
        &self.fallback_order
    }
}

/// The consistency set every [`ResolutionOutcome`] construction path runs —
/// constructor and `Deserialize` alike. Factored out so it is impossible for
/// one path to hold a weaker rule set than the other.
///
/// Rule order is load-bearing where two rules can both apply: the
/// [`FALLBACK_ORDER_CAP`] bound is checked **first**, because it constrains the
/// `fallback_order` field itself regardless of what the selection is. An
/// abstaining outcome carrying five entries is over the cap *and* carrying a
/// fallback order it has no business carrying; reporting the cap keeps the
/// bound's verdict from being masked by the shape rule. Within the abstain arm
/// the field-level rules (`account_ref`, `fallback_order`) likewise precede the
/// reason-vs-candidates rules for the same reason.
fn validate_outcome_consistency(
    candidates: &[CandidateEvaluation],
    selection: &Selection,
    account_ref: Option<&str>,
    fallback_order: &[String],
) -> Result<(), SeamError> {
    if fallback_order.len() > FALLBACK_ORDER_CAP {
        return Err(SeamError::FallbackOrderTooLong {
            len: fallback_order.len(),
        });
    }

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
        Selection::Abstain(reason) => {
            if account_ref.is_some() {
                return Err(SeamError::AbstainCarriesAccountRef);
            }
            if !fallback_order.is_empty() {
                return Err(SeamError::AbstainCarriesFallbackOrder);
            }
            // The reason and the candidate list are two statements about the
            // same resolution; an outcome that lets them disagree is a lie
            // whichever half you believe. Matched exhaustively so a new
            // `AbstainReason` cannot be added without ruling on what candidate
            // state it implies.
            match reason {
                AbstainReason::NoEligibleCandidate => {
                    if candidates.is_empty() {
                        return Err(SeamError::AbstainNoEligibleWithoutCandidates);
                    }
                    if let Some(candidate) = candidates.iter().find(|c| c.is_eligible()) {
                        return Err(SeamError::AbstainNoEligibleWithEligibleCandidate {
                            deployment_id: candidate.deployment_id.clone(),
                        });
                    }
                }
                AbstainReason::EmptyCandidateSet
                | AbstainReason::UnknownAlias
                | AbstainReason::AmbiguousAlias
                | AbstainReason::PolicyRevisionMismatch => {
                    if !candidates.is_empty() {
                        return Err(SeamError::AbstainCarriesUnevaluatedCandidates {
                            reason: *reason,
                        });
                    }
                }
            }
        }
    }

    Ok(())
}
