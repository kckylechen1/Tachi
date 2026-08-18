//! Model-broker control-plane seam.
//!
//! # Why this module exists
//!
//! Catalog, resolution, and health code share a stable vocabulary for model
//! references, resolved deployments, and operational outcomes. Those values
//! live below their consumers so neither the catalog nor a caller-facing crate
//! owns the other's input types.
//!
//! # Architectural invariant
//!
//! Every type here is **memcore-native plain data**. This module imports
//! **nothing** from upper-layer crates. The operational resolver lives here so
//! its input types do not invert the dependency graph. The snapshot types the
//! resolver consumes ([`CatalogSnapshot`],
//! [`HealthSnapshot`], [`AccountSnapshot`], the budget / pin / retry contexts)
//! are all defined here, as memcore-owned data.
//!
//! The `module_has_no_external_tachi_crate_imports` test structurally pins
//! this: the source text carries neither an import of a tachi-prefixed crate nor
//! a fully-qualified path into one. Its needles are assembled at runtime so the
//! test's own source cannot match them. For the same reason, prose in this
//! module must not spell those paths out either.
//!
//! # Three disciplines this module holds
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
//!    tests alone cannot prove that either spelling matches the external
//!    contract.
//! 3. **Failure is typed and loud.** An unknown or ambiguous alias, a drifted
//!    policy revision, or an empty admitted set each get their own
//!    [`AbstainReason`]; every filter axis — including the
//!    [`DeploymentBounds`] axes — gets its own [`ExclusionReason`]. Nothing
//!    degrades silently into "some default model".
//!
//! # What is deliberately *not* here
//!
//! - No SQL, no DDL, no table row types. This is the seam contract, not the
//!   catalog schema. [`ResolvedDeployment`] is the *resolved projection* a
//!   resolver hands out, not the `model_deployments` row.
//! - No provider wire/HTTP execution types. This module describes control-plane
//!   resolution and health only.
//! - No second production resolver. [`StaticFixtureResolver`] is a deterministic
//!   test fixture behind `feature = "broker-fixtures"` (default off), so it
//!   cannot be reached from a production build.

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
    /// An abstaining outcome carried candidate evaluations that its
    /// [`AbstainReason`] says were never evaluated — an empty admitted set, or
    /// a request-level alias/policy failure that halts before candidates are
    /// looked at. Listing them would claim a disposition the resolver never
    /// reached.
    AbstainCarriesUnevaluatedCandidates {
        /// The reason whose semantics the candidate list contradicts.
        reason: AbstainReason,
    },
    /// [`AbstainReason::NoEligibleCandidate`] on an empty candidate list. That
    /// reason means "candidates were evaluated and every one was excluded"; an
    /// empty admitted set is [`AbstainReason::EmptyCandidateSet`].
    AbstainNoEligibleWithoutCandidates,
    /// [`AbstainReason::NoEligibleCandidate`] alongside a candidate the same
    /// outcome marks eligible. The reason and the list flatly disagree about
    /// whether anything survived the filters.
    AbstainNoEligibleWithEligibleCandidate {
        /// A candidate the outcome marks eligible despite claiming none is.
        deployment_id: String,
    },
    /// A fallback entry named something other than an eligible, non-chosen
    /// candidate. The fallback chain becomes durable receipt provenance, so it
    /// may only contain ids this resolution actually evaluated and admitted.
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
            Self::AbstainCarriesUnevaluatedCandidates { reason } => write!(
                f,
                "abstain reason `{}` means no candidate was evaluated, so the outcome must not \
                 list any",
                reason.as_str()
            ),
            Self::AbstainNoEligibleWithoutCandidates => write!(
                f,
                "abstain reason `{}` requires evaluated candidates; an empty set is `{}`",
                AbstainReason::NoEligibleCandidate.as_str(),
                AbstainReason::EmptyCandidateSet.as_str()
            ),
            Self::AbstainNoEligibleWithEligibleCandidate { deployment_id } => write!(
                f,
                "candidate `{deployment_id}` is marked eligible by an outcome that claims none is"
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

mod deployment;
mod health;
mod model_ref;
mod outcome;
mod resolver;

pub use deployment::{
    DeploymentBounds, DeploymentCapabilities, ResolvedDeployment, ResolvedDeploymentParts,
    WireDialect,
};
pub use health::{HealthObservation, InvocationErrorClass, ObservationEvidence, RetryAfter};
pub use model_ref::ModelRef;
pub use outcome::{
    AbstainReason, BudgetEstimate, CandidateEvaluation, ExclusionReason, ResolutionOutcome,
    ResolutionRevisions, Selection,
};
#[cfg(any(test, feature = "broker-fixtures"))]
pub use resolver::StaticFixtureResolver;
pub use resolver::{
    AccountAvailability, AccountSnapshot, BudgetContext, CatalogSnapshot, DeploymentCooldown,
    HealthSnapshot, OperationalResolver, PinContext, ResolverInput, RetryContext,
};

#[cfg(test)]
mod tests {
    use super::*;

    // --- Literal goldens: the frozen wire bytes, spelled out once. ---
    //
    // These are deliberately whole-payload literals rather than round-trips: a
    // round-trip is satisfied by any self-consistent spelling, including a
    // wrong one. Changing any of these strings is changing the frozen seam.

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

    /// The other legal abstain shape: candidates were evaluated and every one
    /// was excluded. Pins `CandidateEvaluation`'s field names too.
    const NO_ELIGIBLE_OUTCOME_GOLDEN: &str = concat!(
        r#"{"candidates":[{"deployment_id":"dep-a","exclusion":"stale_catalog"}],"#,
        r#""selection":{"kind":"abstain","value":"no_eligible_candidate"},"#,
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

    // Frozen spellings are pinned by literal goldens, and serde == as_str for
    // every variant.

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
            "serde spelling and as_str() disagree"
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

    // The deserialize path cannot bypass constructor validation. Each case is
    // a JSON payload that the matching constructor would refuse.

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
        // Every fallback entry here is an eligible, non-chosen candidate, so
        // the cap is the *only* rule that can reject this outcome. A payload
        // that also trips another rule would let this test pass green while the
        // cap itself was unenforced — which is exactly what happened when the
        // cap check went missing from the constructor.
        let over_cap: Vec<String> = ["dep-b", "dep-c", "dep-d", "dep-e", "dep-f"]
            .iter()
            .map(|id| id.to_string())
            .collect();
        assert_eq!(over_cap.len(), FALLBACK_ORDER_CAP + 1);
        let mut candidates = vec![CandidateEvaluation::eligible("dep-a")];
        candidates.extend(
            over_cap
                .iter()
                .map(|id| CandidateEvaluation::eligible(id.as_str())),
        );

        let err = ResolutionOutcome::new(
            candidates.clone(),
            Selection::Chosen(sample_deployment("dep-a", "acct-1")),
            sample_revisions(),
            Some("acct-1".to_string()),
            BudgetEstimate::default(),
            over_cap.clone(),
        )
        .unwrap_err();
        assert_eq!(err, SeamError::FallbackOrderTooLong { len: 5 });

        // The deserialize path refuses the same payload. Built from a legal
        // four-entry outcome and pushed over the cap in JSON, so the object is
        // otherwise entirely consistent.
        let legal = ResolutionOutcome::new(
            candidates,
            Selection::Chosen(sample_deployment("dep-a", "acct-1")),
            sample_revisions(),
            Some("acct-1".to_string()),
            BudgetEstimate::default(),
            over_cap[..FALLBACK_ORDER_CAP].to_vec(),
        )
        .expect("four eligible fallback entries are within the cap");
        let mut value = serde_json::to_value(&legal).expect("serialize");
        value["fallback_order"] = serde_json::to_value(&over_cap).expect("serialize");
        assert!(
            serde_json::from_str::<ResolutionOutcome>(&value.to_string()).is_err(),
            "an over-cap fallback order must not survive deserialization"
        );

        // Precedence, pinned: the cap bounds the field itself, so it is
        // reported ahead of the selection-shaped rules rather than being
        // masked by them.
        let err = ResolutionOutcome::new(
            vec![],
            Selection::Abstain(AbstainReason::EmptyCandidateSet),
            sample_revisions(),
            None,
            BudgetEstimate::default(),
            over_cap,
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

    /// Build an abstaining outcome with an arbitrary reason/candidate pairing,
    /// so the legal and illegal combinations are stated the same way.
    fn abstain_outcome(
        reason: AbstainReason,
        candidates: Vec<CandidateEvaluation>,
    ) -> Result<ResolutionOutcome, SeamError> {
        ResolutionOutcome::new(
            candidates,
            Selection::Abstain(reason),
            sample_revisions(),
            None,
            BudgetEstimate::default(),
            Vec::new(),
        )
    }

    #[test]
    fn abstain_reason_and_candidate_list_must_agree() {
        let excluded = || {
            vec![CandidateEvaluation::excluded(
                "dep-a",
                ExclusionReason::StaleCatalog,
            )]
        };

        // --- The two legal shapes, from the two golden payloads. ---
        let empty_set = abstain_outcome(AbstainReason::EmptyCandidateSet, vec![])
            .expect("an empty admitted set is the empty_candidate_set shape");
        assert_eq!(
            serde_json::to_string(&empty_set).unwrap(),
            ABSTAIN_OUTCOME_GOLDEN
        );
        let no_eligible = abstain_outcome(AbstainReason::NoEligibleCandidate, excluded())
            .expect("evaluated-and-all-excluded is the no_eligible_candidate shape");
        assert_eq!(
            serde_json::to_string(&no_eligible).unwrap(),
            NO_ELIGIBLE_OUTCOME_GOLDEN
        );
        assert_eq!(
            serde_json::from_str::<ResolutionOutcome>(NO_ELIGIBLE_OUTCOME_GOLDEN).unwrap(),
            no_eligible
        );

        // --- `no_eligible_candidate` cannot be claimed over an empty list ---
        assert_eq!(
            abstain_outcome(AbstainReason::NoEligibleCandidate, vec![]).unwrap_err(),
            SeamError::AbstainNoEligibleWithoutCandidates
        );
        let mut value: serde_json::Value =
            serde_json::from_str(NO_ELIGIBLE_OUTCOME_GOLDEN).expect("golden parses");
        value["candidates"] = serde_json::json!([]);
        assert!(
            serde_json::from_str::<ResolutionOutcome>(&value.to_string()).is_err(),
            "deserialize must not mint `no_eligible_candidate` over an empty candidate list"
        );

        // --- ...nor while marking a candidate eligible ---
        let mixed = vec![
            CandidateEvaluation::excluded("dep-a", ExclusionReason::StaleCatalog),
            CandidateEvaluation::eligible("dep-b"),
        ];
        assert_eq!(
            abstain_outcome(AbstainReason::NoEligibleCandidate, mixed).unwrap_err(),
            SeamError::AbstainNoEligibleWithEligibleCandidate {
                deployment_id: "dep-b".to_string()
            }
        );
        let mut value: serde_json::Value =
            serde_json::from_str(NO_ELIGIBLE_OUTCOME_GOLDEN).expect("golden parses");
        value["candidates"] = serde_json::json!([{"deployment_id": "dep-a", "exclusion": null}]);
        assert!(
            serde_json::from_str::<ResolutionOutcome>(&value.to_string()).is_err(),
            "deserialize must not mint `no_eligible_candidate` beside an eligible candidate"
        );

        // --- every other reason halts before evaluation, so it may list none ---
        for reason in [
            AbstainReason::EmptyCandidateSet,
            AbstainReason::UnknownAlias,
            AbstainReason::AmbiguousAlias,
            AbstainReason::PolicyRevisionMismatch,
        ] {
            assert!(
                abstain_outcome(reason, vec![]).is_ok(),
                "{} with no candidates is the legal shape",
                reason.as_str()
            );
            assert_eq!(
                abstain_outcome(reason, excluded()).unwrap_err(),
                SeamError::AbstainCarriesUnevaluatedCandidates { reason },
                "{} must not carry candidates it never evaluated",
                reason.as_str()
            );

            let mut value: serde_json::Value =
                serde_json::from_str(NO_ELIGIBLE_OUTCOME_GOLDEN).expect("golden parses");
            value["selection"]["value"] = serde_json::Value::String(reason.as_str().to_string());
            assert!(
                serde_json::from_str::<ResolutionOutcome>(&value.to_string()).is_err(),
                "deserialize must not mint `{}` beside an evaluated candidate list",
                reason.as_str()
            );
        }
    }

    #[test]
    fn resolution_outcome_rejects_blank_candidate_ids() {
        // Excluded, not eligible: the blank id is then the *only* thing wrong
        // with this outcome, so the assertion cannot pass on another rule.
        let err = abstain_outcome(
            AbstainReason::NoEligibleCandidate,
            vec![CandidateEvaluation::excluded(
                "  ",
                ExclusionReason::StaleCatalog,
            )],
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
        // fully-qualified paths into, upper crates. Every needle is assembled
        // at runtime so this test's own source — and any prose in these files —
        // cannot match it.
        let src = [
            include_str!("model_broker_seam.rs"),
            include_str!("model_broker_seam/model_ref.rs"),
            include_str!("model_broker_seam/deployment.rs"),
            include_str!("model_broker_seam/outcome.rs"),
            include_str!("model_broker_seam/resolver.rs"),
            include_str!("model_broker_seam/health.rs"),
        ]
        .join("\n");
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
