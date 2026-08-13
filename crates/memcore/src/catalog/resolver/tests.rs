//! Discriminating tests for the operational resolver (#1681 D5, D7 row D:
//! discriminations 1, 2, 3, 8, 9, plus discrimination 4's sibling half, which
//! the review log moved here from PR-C).
//!
//! Every test names the property it discriminates. A test that would still
//! pass if the resolver silently substituted a default deployment is not a
//! test of this file's subject.

use super::*;
use crate::catalog::{ModelAlias, ModelAliasBinding, ALIAS_STATUS_ACTIVE, ALIAS_STATUS_RETIRED};
use crate::model_broker_seam::{
    AccountAvailability, AccountSnapshot, BudgetContext, CatalogSnapshot, DeploymentBounds,
    DeploymentCooldown, HealthSnapshot, ModelRef, PinContext, ResolvedDeploymentParts,
    RetryContext, WireDialect,
};

const OBSERVED_AT: &str = "2026-08-13T00:00:00.000Z";
const CATALOG_REVISION: &str = "catalog-rev-1";

// ─── Fixtures ───────────────────────────────────────────────────────────────

fn alias_row(name: &str) -> ModelAlias {
    ModelAlias {
        alias_name: name.to_string(),
        required_capabilities: "{}".to_string(),
        constraints: "{}".to_string(),
        status: ALIAS_STATUS_ACTIVE.to_string(),
        revision: 1,
        policy_digest: None,
        source_refs: Vec::new(),
        created_at: OBSERVED_AT.to_string(),
        updated_at: OBSERVED_AT.to_string(),
    }
}

fn binding_row(alias: &str, deployment_id: &str, priority: i64) -> ModelAliasBinding {
    ModelAliasBinding {
        alias_name: alias.to_string(),
        deployment_id: deployment_id.to_string(),
        priority,
        retired: false,
        created_at: OBSERVED_AT.to_string(),
        updated_at: OBSERVED_AT.to_string(),
    }
}

fn deployment(id: &str, account: &str) -> ResolvedDeployment {
    ResolvedDeployment::new(ResolvedDeploymentParts {
        deployment_id: id.to_string(),
        wire_dialect: WireDialect::OpenAiCompat,
        endpoint_ref: format!("https://example.invalid/{id}"),
        provider_model_id: format!("{id}-model"),
        capabilities: DeploymentCapabilities {
            chat: true,
            ..DeploymentCapabilities::default()
        },
        bounds: DeploymentBounds::default(),
        account_ref: account.to_string(),
        pricing_snapshot_ref: None,
    })
    .expect("fixture deployment is well formed")
}

fn priced(id: &str, account: &str, snapshot: &str) -> ResolvedDeployment {
    let mut parts = deployment(id, account).into_parts();
    parts.pricing_snapshot_ref = Some(snapshot.to_string());
    ResolvedDeployment::new(parts).expect("fixture deployment is well formed")
}

fn price(snapshot: &str, prompt: u64, completion: u64) -> SnapshotPrice {
    SnapshotPrice {
        pricing_snapshot_ref: snapshot.to_string(),
        prompt_micros_per_mtok: prompt,
        completion_micros_per_mtok: completion,
    }
}

/// An input whose every axis is permissive, so a test that wants to prove one
/// axis excludes has to turn that axis on itself.
fn input(
    reference: &str,
    policy_revision: &str,
    candidates: Vec<ResolvedDeployment>,
) -> ResolverInput {
    ResolverInput {
        model_ref: ModelRef::new(reference, policy_revision).expect("fixture ref is well formed"),
        admitted_candidates: candidates,
        catalog: CatalogSnapshot {
            catalog_revision: CATALOG_REVISION.to_string(),
            stale_deployment_ids: Vec::new(),
        },
        health: HealthSnapshot {
            observed_at: OBSERVED_AT.to_string(),
            cooldowns: Vec::new(),
        },
        accounts: AccountSnapshot::default(),
        budget: BudgetContext::default(),
        pin: PinContext::default(),
        retry: RetryContext::default(),
    }
}

/// The alias set every test starts from: `chat.default` bound to two
/// deployments.
fn two_way_alias() -> AliasSetSnapshot {
    AliasSetSnapshot::from_rows(
        &[alias_row("chat.default")],
        &[
            binding_row("chat.default", "dep-a", 0),
            binding_row("chat.default", "dep-b", 1),
        ],
    )
}

fn resolver(aliases: AliasSetSnapshot) -> CatalogResolver {
    CatalogResolver::new(
        aliases,
        Vec::new(),
        Vec::new(),
        RequestRequirements::default(),
    )
}

fn chosen_id(outcome: &ResolutionOutcome) -> &str {
    outcome
        .selection()
        .chosen()
        .expect("expected a chosen deployment")
        .deployment_id()
}

fn exclusion_of(outcome: &ResolutionOutcome, deployment_id: &str) -> Option<ExclusionReason> {
    outcome
        .candidates()
        .iter()
        .find(|candidate| candidate.deployment_id == deployment_id)
        .unwrap_or_else(|| panic!("{deployment_id} is not in the candidate list"))
        .exclusion
}

// ─── Discrimination 1: determinism against a frozen snapshot ────────────────

#[test]
fn identical_snapshots_resolve_identically_regardless_of_candidate_order() {
    let aliases = two_way_alias();
    let revision = aliases.policy_revision().to_string();
    let resolver = CatalogResolver::new(
        aliases,
        Vec::new(),
        vec![price("ps1:cheap", 10, 10), price("ps1:dear", 900, 900)],
        RequestRequirements {
            prompt_tokens: Some(1_000),
            ..RequestRequirements::default()
        },
    );

    let forward = resolver
        .try_resolve(&input(
            "chat.default",
            &revision,
            vec![
                priced("dep-a", "acct-a", "ps1:dear"),
                priced("dep-b", "acct-b", "ps1:cheap"),
            ],
        ))
        .expect("stamped input");
    let reversed = resolver
        .try_resolve(&input(
            "chat.default",
            &revision,
            vec![
                priced("dep-b", "acct-b", "ps1:cheap"),
                priced("dep-a", "acct-a", "ps1:dear"),
            ],
        ))
        .expect("stamped input");

    // The winner and the fallback chain are the same both ways: ordering is a
    // property of the snapshot, not of the order the caller happened to
    // assemble it in.
    assert_eq!(chosen_id(&forward), "dep-b");
    assert_eq!(chosen_id(&reversed), "dep-b");
    assert_eq!(forward.fallback_order(), &["dep-a"]);
    assert_eq!(reversed.fallback_order(), &["dep-a"]);
    assert_eq!(forward.selection(), reversed.selection());
}

#[test]
fn the_outcome_is_stamped_with_the_snapshot_it_was_computed_against() {
    let aliases = two_way_alias();
    let revision = aliases.policy_revision().to_string();
    let outcome = resolver(aliases)
        .try_resolve(&input(
            "chat.default",
            &revision,
            vec![deployment("dep-a", "acct-a")],
        ))
        .expect("stamped input");

    assert_eq!(outcome.revisions().catalog_revision(), CATALOG_REVISION);
    assert_eq!(outcome.revisions().health_observed_at(), OBSERVED_AT);
    assert_eq!(outcome.revisions().policy_revision(), revision);
}

#[test]
fn an_unstamped_snapshot_is_refused_rather_than_resolved() {
    let aliases = two_way_alias();
    let revision = aliases.policy_revision().to_string();
    let mut unstamped = input(
        "chat.default",
        &revision,
        vec![deployment("dep-a", "acct-a")],
    );
    unstamped.catalog.catalog_revision = "   ".to_string();

    assert_eq!(
        resolver(aliases).try_resolve(&unstamped),
        Err(ResolveRefusal::UnstampableInput {
            field: "catalog_revision"
        })
    );
}

// ─── Discrimination 2: the gate's cut is not reopened ───────────────────────

#[test]
fn a_gate_cut_deployment_is_not_reintroduced_by_being_cheapest() {
    let aliases = two_way_alias();
    let revision = aliases.policy_revision().to_string();
    let resolver = CatalogResolver::new(
        aliases,
        // The resolver even *knows about* dep-a as a catalog placement…
        vec![DeploymentPlacement::unconstrained("dep-a")],
        vec![price("ps1:cheap", 1, 1), price("ps1:dear", 500, 500)],
        RequestRequirements {
            prompt_tokens: Some(1_000),
            ..RequestRequirements::default()
        },
    );

    // …but the semantic gate admitted only dep-b, so dep-a is invisible.
    let outcome = resolver
        .try_resolve(&input(
            "chat.default",
            &revision,
            vec![priced("dep-b", "acct-b", "ps1:dear")],
        ))
        .expect("stamped input");

    assert_eq!(chosen_id(&outcome), "dep-b");
    assert!(
        !outcome
            .candidates()
            .iter()
            .any(|candidate| candidate.deployment_id == "dep-a"),
        "a deployment the gate cut must not even be reported on: gates cut the \
         set, they do not report beside it"
    );
    assert!(!outcome.fallback_order().contains(&"dep-a".to_string()));
}

// ─── Discrimination 3: one exact reason per axis, and no call ───────────────

#[test]
fn each_operational_axis_reports_its_own_exclusion_reason() {
    let aliases = AliasSetSnapshot::from_rows(
        &[alias_row("chat.default")],
        &[
            binding_row("chat.default", "dep-capability", 0),
            binding_row("chat.default", "dep-context", 0),
            binding_row("chat.default", "dep-output", 0),
            binding_row("chat.default", "dep-attachment", 0),
            binding_row("chat.default", "dep-region", 0),
            binding_row("chat.default", "dep-policy", 0),
            binding_row("chat.default", "dep-account", 0),
            binding_row("chat.default", "dep-stale", 0),
            binding_row("chat.default", "dep-retired", 0),
            binding_row("chat.default", "dep-cooling", 0),
            binding_row("chat.default", "dep-budget", 0),
        ],
    );
    let revision = aliases.policy_revision().to_string();

    let bounded = |id: &str, bounds: DeploymentBounds| {
        let mut parts = deployment(id, "acct-ok").into_parts();
        parts.bounds = bounds;
        ResolvedDeployment::new(parts).expect("fixture deployment is well formed")
    };
    let no_chat = {
        let mut parts = deployment("dep-capability", "acct-ok").into_parts();
        parts.capabilities = DeploymentCapabilities::default();
        ResolvedDeployment::new(parts).expect("fixture deployment is well formed")
    };

    let resolver = CatalogResolver::new(
        aliases,
        vec![
            DeploymentPlacement::unconstrained("dep-region").with_region("eu-west"),
            DeploymentPlacement::unconstrained("dep-policy")
                .with_region("us-east")
                .with_data_policy("trains-on-input"),
            DeploymentPlacement::unconstrained("dep-retired")
                .with_region("us-east")
                .with_data_policy("zero-retention")
                .retired(),
            DeploymentPlacement::unconstrained("dep-capability")
                .with_region("us-east")
                .with_data_policy("zero-retention"),
            DeploymentPlacement::unconstrained("dep-context")
                .with_region("us-east")
                .with_data_policy("zero-retention"),
            DeploymentPlacement::unconstrained("dep-output")
                .with_region("us-east")
                .with_data_policy("zero-retention"),
            DeploymentPlacement::unconstrained("dep-attachment")
                .with_region("us-east")
                .with_data_policy("zero-retention"),
            DeploymentPlacement::unconstrained("dep-account")
                .with_region("us-east")
                .with_data_policy("zero-retention"),
            DeploymentPlacement::unconstrained("dep-stale")
                .with_region("us-east")
                .with_data_policy("zero-retention"),
            DeploymentPlacement::unconstrained("dep-cooling")
                .with_region("us-east")
                .with_data_policy("zero-retention"),
            DeploymentPlacement::unconstrained("dep-budget")
                .with_region("us-east")
                .with_data_policy("zero-retention"),
        ],
        vec![price("ps1:pricey", 10_000_000, 10_000_000)],
        RequestRequirements {
            required_capabilities: DeploymentCapabilities {
                chat: true,
                ..DeploymentCapabilities::default()
            },
            prompt_tokens: Some(1_000),
            max_output_tokens: Some(500),
            attachment_bytes: Some(4_096),
            required_embedding_dimensions: None,
            allowed_regions: vec!["us-east".to_string()],
            allowed_data_policies: vec!["zero-retention".to_string()],
        },
    );

    let mut request = input(
        "chat.default",
        &revision,
        vec![
            no_chat,
            bounded(
                "dep-context",
                DeploymentBounds {
                    context_window: Some(999),
                    ..DeploymentBounds::default()
                },
            ),
            bounded(
                "dep-output",
                DeploymentBounds {
                    max_output: Some(499),
                    ..DeploymentBounds::default()
                },
            ),
            bounded(
                "dep-attachment",
                DeploymentBounds {
                    attachment_bytes: Some(4_095),
                    ..DeploymentBounds::default()
                },
            ),
            deployment("dep-region", "acct-ok"),
            deployment("dep-policy", "acct-ok"),
            deployment("dep-account", "acct-blocked"),
            deployment("dep-stale", "acct-ok"),
            deployment("dep-retired", "acct-ok"),
            deployment("dep-cooling", "acct-ok"),
            priced("dep-budget", "acct-ok", "ps1:pricey"),
        ],
    );
    request.catalog.stale_deployment_ids = vec!["dep-stale".to_string()];
    request.health.cooldowns = vec![DeploymentCooldown {
        deployment_id: "dep-cooling".to_string(),
        cooldown_until: None,
    }];
    request.accounts = AccountSnapshot {
        accounts: vec![AccountAvailability {
            account_ref: "acct-blocked".to_string(),
            admitted: false,
        }],
    };
    request.budget = BudgetContext {
        ceiling_usd: Some(0.001),
    };

    let outcome = resolver.try_resolve(&request).expect("stamped input");

    for (id, expected) in [
        ("dep-capability", ExclusionReason::CapabilityMismatch),
        ("dep-context", ExclusionReason::ContextWindowExceeded),
        ("dep-output", ExclusionReason::MaxOutputExceeded),
        ("dep-attachment", ExclusionReason::AttachmentBoundsExceeded),
        ("dep-region", ExclusionReason::RegionBlocked),
        ("dep-policy", ExclusionReason::DataPolicyBlocked),
        ("dep-account", ExclusionReason::AccountNotAdmitted),
        ("dep-stale", ExclusionReason::StaleCatalog),
        ("dep-retired", ExclusionReason::DeploymentInactive),
        ("dep-cooling", ExclusionReason::HealthCooldown),
        ("dep-budget", ExclusionReason::BudgetExceeded),
    ] {
        assert_eq!(
            exclusion_of(&outcome, id),
            Some(expected),
            "{id} must report exactly its own axis"
        );
    }

    // Every candidate was cut, so the resolution abstains — it does not pick
    // the "least bad" one.
    assert_eq!(
        outcome.selection().abstain_reason(),
        Some(AbstainReason::NoEligibleCandidate)
    );
    assert!(outcome.account_ref().is_none());
    assert!(outcome.fallback_order().is_empty());
}

#[test]
fn an_embedding_width_that_disagrees_with_the_index_is_refused_loudly() {
    let aliases = AliasSetSnapshot::from_rows(
        &[alias_row("memory.embed")],
        &[
            binding_row("memory.embed", "dep-1024", 0),
            binding_row("memory.embed", "dep-silent", 1),
        ],
    );
    let revision = aliases.policy_revision().to_string();
    let with_dimensions = |id: &str, dimensions: Option<u32>| {
        let mut parts = deployment(id, "acct-ok").into_parts();
        parts.capabilities = DeploymentCapabilities {
            embeddings: true,
            ..DeploymentCapabilities::default()
        };
        parts.bounds = DeploymentBounds {
            embedding_dimensions: dimensions,
            ..DeploymentBounds::default()
        };
        ResolvedDeployment::new(parts).expect("fixture deployment is well formed")
    };

    let outcome = CatalogResolver::new(
        aliases,
        Vec::new(),
        Vec::new(),
        RequestRequirements {
            required_capabilities: DeploymentCapabilities {
                embeddings: true,
                ..DeploymentCapabilities::default()
            },
            required_embedding_dimensions: Some(1024),
            ..RequestRequirements::default()
        },
    )
    .try_resolve(&input(
        "memory.embed",
        &revision,
        vec![
            with_dimensions("dep-1024", Some(2048)),
            // Declares no width at all: silence is not agreement.
            with_dimensions("dep-silent", None),
        ],
    ))
    .expect("stamped input");

    assert_eq!(
        exclusion_of(&outcome, "dep-1024"),
        Some(ExclusionReason::EmbeddingDimensionMismatch)
    );
    assert_eq!(
        exclusion_of(&outcome, "dep-silent"),
        Some(ExclusionReason::EmbeddingDimensionMismatch)
    );
    assert_eq!(
        outcome.selection().abstain_reason(),
        Some(AbstainReason::NoEligibleCandidate)
    );
}

#[test]
fn a_ceiling_with_no_price_for_the_candidate_refuses_rather_than_passes() {
    let aliases = two_way_alias();
    let revision = aliases.policy_revision().to_string();
    let mut request = input(
        "chat.default",
        &revision,
        // No pricing_snapshot_ref at all: the cost cannot be bounded.
        vec![deployment("dep-a", "acct-a")],
    );
    request.budget = BudgetContext {
        ceiling_usd: Some(100.0),
    };

    let outcome = resolver(aliases)
        .try_resolve(&request)
        .expect("stamped input");
    assert_eq!(
        exclusion_of(&outcome, "dep-a"),
        Some(ExclusionReason::BudgetExceeded)
    );
}

#[test]
fn an_undeclared_deployment_bound_is_not_an_exclusion() {
    // The env-imported catalog declares no context window; a request that
    // states a prompt size must not empty the candidate set because of it.
    let aliases = two_way_alias();
    let revision = aliases.policy_revision().to_string();
    let outcome = CatalogResolver::new(
        aliases,
        Vec::new(),
        Vec::new(),
        RequestRequirements {
            prompt_tokens: Some(1_000_000),
            max_output_tokens: Some(1_000_000),
            attachment_bytes: Some(u64::MAX),
            ..RequestRequirements::default()
        },
    )
    .try_resolve(&input(
        "chat.default",
        &revision,
        vec![deployment("dep-a", "acct-a")],
    ))
    .expect("stamped input");

    assert_eq!(chosen_id(&outcome), "dep-a");
}

// ─── Discrimination 4 (sibling half, moved here from PR-C) ──────────────────

#[test]
fn a_throttled_deployment_cools_while_its_healthy_sibling_is_selected() {
    let aliases = two_way_alias();
    let revision = aliases.policy_revision().to_string();
    let mut request = input(
        "chat.default",
        &revision,
        vec![
            deployment("dep-a", "acct-shared"),
            deployment("dep-b", "acct-shared"),
        ],
    );
    // dep-a took the 429; the credential is shared, and the account authority
    // is untouched by it (#1681 D4's dual-record rule).
    request.health.cooldowns = vec![DeploymentCooldown {
        deployment_id: "dep-a".to_string(),
        cooldown_until: Some("2026-08-13T00:05:00.000Z".to_string()),
    }];

    let outcome = resolver(aliases)
        .try_resolve(&request)
        .expect("stamped input");

    assert_eq!(
        exclusion_of(&outcome, "dep-a"),
        Some(ExclusionReason::HealthCooldown)
    );
    assert_eq!(chosen_id(&outcome), "dep-b");
    assert_eq!(outcome.account_ref(), Some("acct-shared"));
}

#[test]
fn every_sibling_cooling_yields_a_bounded_typed_abstain_not_a_gamble() {
    let aliases = two_way_alias();
    let revision = aliases.policy_revision().to_string();
    let mut request = input(
        "chat.default",
        &revision,
        vec![
            deployment("dep-a", "acct-shared"),
            deployment("dep-b", "acct-shared"),
        ],
    );
    request.health.cooldowns = vec![
        DeploymentCooldown {
            deployment_id: "dep-a".to_string(),
            cooldown_until: None,
        },
        DeploymentCooldown {
            deployment_id: "dep-b".to_string(),
            cooldown_until: None,
        },
    ];

    let outcome = resolver(aliases)
        .try_resolve(&request)
        .expect("stamped input");
    assert_eq!(
        outcome.selection().abstain_reason(),
        Some(AbstainReason::NoEligibleCandidate)
    );
    assert!(outcome.selection().chosen().is_none());
}

#[test]
fn a_cooldown_that_already_lifted_does_not_exclude_but_an_unreadable_one_does() {
    let aliases = two_way_alias();
    let revision = aliases.policy_revision().to_string();

    let mut lifted = input(
        "chat.default",
        &revision,
        vec![deployment("dep-a", "acct-a")],
    );
    lifted.health.cooldowns = vec![DeploymentCooldown {
        deployment_id: "dep-a".to_string(),
        // Lifted a minute before the snapshot was observed.
        cooldown_until: Some("2026-08-12T23:59:00.000Z".to_string()),
    }];
    let outcome = resolver(two_way_alias())
        .try_resolve(&lifted)
        .expect("stamped input");
    assert_eq!(chosen_id(&outcome), "dep-a");

    let mut unreadable = lifted;
    unreadable.health.cooldowns = vec![DeploymentCooldown {
        deployment_id: "dep-a".to_string(),
        cooldown_until: Some("whenever".to_string()),
    }];
    let outcome = resolver(two_way_alias())
        .try_resolve(&unreadable)
        .expect("stamped input");
    assert_eq!(
        exclusion_of(&outcome, "dep-a"),
        Some(ExclusionReason::HealthCooldown),
        "an unreadable cooldown is still a cooldown: the fail-safe reading is \
         that the deployment is cooling"
    );
}

// ─── Discrimination 8: the fallback chain is real, bounded provenance ───────

#[test]
fn the_fallback_order_is_capped_at_four_and_carries_evaluated_deployment_ids() {
    let ids = ["dep-1", "dep-2", "dep-3", "dep-4", "dep-5", "dep-6"];
    let aliases = AliasSetSnapshot::from_rows(
        &[alias_row("chat.default")],
        &ids.iter()
            .enumerate()
            .map(|(index, id)| binding_row("chat.default", id, index as i64))
            .collect::<Vec<_>>(),
    );
    let revision = aliases.policy_revision().to_string();

    let outcome = resolver(aliases)
        .try_resolve(&input(
            "chat.default",
            &revision,
            ids.iter().map(|id| deployment(id, "acct-shared")).collect(),
        ))
        .expect("stamped input");

    assert_eq!(chosen_id(&outcome), "dep-1");
    assert_eq!(
        outcome.fallback_order(),
        &["dep-2", "dep-3", "dep-4", "dep-5"],
        "the chain is truncated at the frozen cap, in resolution order — it \
         does not grow with the catalog"
    );
    assert_eq!(outcome.fallback_order().len(), FALLBACK_ORDER_CAP);
    // dep-6 was evaluated and eligible, and is still reported as a candidate:
    // truncating the chain does not erase the evaluation.
    assert_eq!(exclusion_of(&outcome, "dep-6"), None);
}

#[test]
fn an_excluded_candidate_never_reaches_the_fallback_chain() {
    let aliases = two_way_alias();
    let revision = aliases.policy_revision().to_string();
    let mut request = input(
        "chat.default",
        &revision,
        vec![deployment("dep-a", "acct-a"), deployment("dep-b", "acct-b")],
    );
    request.catalog.stale_deployment_ids = vec!["dep-b".to_string()];

    let outcome = resolver(aliases)
        .try_resolve(&request)
        .expect("stamped input");
    assert_eq!(chosen_id(&outcome), "dep-a");
    assert!(
        outcome.fallback_order().is_empty(),
        "a stale deployment is not a place to fall back to"
    );
}

#[test]
fn a_pin_wins_the_order_but_does_not_survive_an_exclusion() {
    let aliases = two_way_alias();
    let revision = aliases.policy_revision().to_string();

    let mut pinned = input(
        "chat.default",
        &revision,
        vec![deployment("dep-a", "acct-a"), deployment("dep-b", "acct-b")],
    );
    pinned.pin = PinContext {
        pinned_deployment_id: Some("dep-b".to_string()),
    };
    let outcome = resolver(two_way_alias())
        .try_resolve(&pinned)
        .expect("stamped input");
    assert_eq!(chosen_id(&outcome), "dep-b");
    assert_eq!(outcome.fallback_order(), &["dep-a"]);

    // A pin is a preference among the eligible, never an override of an
    // operational refusal.
    let mut cooling = pinned;
    cooling.health.cooldowns = vec![DeploymentCooldown {
        deployment_id: "dep-b".to_string(),
        cooldown_until: None,
    }];
    let outcome = resolver(aliases)
        .try_resolve(&cooling)
        .expect("stamped input");
    assert_eq!(chosen_id(&outcome), "dep-a");
    assert_eq!(
        exclusion_of(&outcome, "dep-b"),
        Some(ExclusionReason::HealthCooldown)
    );
}

// ─── Discrimination 9: stale/unknown fails or abstains, never guesses ───────

#[test]
fn an_unknown_alias_abstains_even_with_a_perfectly_good_deployment_in_hand() {
    let aliases = two_way_alias();
    let revision = aliases.policy_revision().to_string();

    let outcome = resolver(aliases)
        .try_resolve(&input(
            "chat.does-not-exist",
            &revision,
            // Healthy, admitted, unconstrained — and still not selectable,
            // because nothing asked for it.
            vec![deployment("dep-a", "acct-a")],
        ))
        .expect("stamped input");

    assert_eq!(
        outcome.selection().abstain_reason(),
        Some(AbstainReason::UnknownAlias),
        "an unknown reference must never silently become 'the default model'"
    );
    assert!(outcome.selection().chosen().is_none());
    assert!(
        outcome.candidates().is_empty(),
        "nothing was evaluated, so nothing may be reported as evaluated"
    );
    assert!(outcome.account_ref().is_none());
}

#[test]
fn a_reference_that_is_both_an_alias_and_a_deployment_id_abstains_as_ambiguous() {
    // The operator bound an alias whose name collides with a deployment id.
    // Two readings, no deterministic winner, so the resolver refuses to pick
    // one rather than making routing depend on which reading it tried first.
    let aliases =
        AliasSetSnapshot::from_rows(&[alias_row("dep-a")], &[binding_row("dep-a", "dep-b", 0)]);
    let revision = aliases.policy_revision().to_string();

    let outcome = resolver(aliases)
        .try_resolve(&input(
            "dep-a",
            &revision,
            vec![deployment("dep-a", "acct-a"), deployment("dep-b", "acct-b")],
        ))
        .expect("stamped input");

    assert_eq!(
        outcome.selection().abstain_reason(),
        Some(AbstainReason::AmbiguousAlias)
    );
    assert!(outcome.candidates().is_empty());
}

#[test]
fn a_model_ref_minted_under_a_different_alias_set_abstains() {
    let aliases = two_way_alias();
    let outcome = resolver(aliases)
        .try_resolve(&input(
            "chat.default",
            "ar1:0000000000000000000000000000000000000000000000000000000000000000",
            vec![deployment("dep-a", "acct-a")],
        ))
        .expect("stamped input");

    assert_eq!(
        outcome.selection().abstain_reason(),
        Some(AbstainReason::PolicyRevisionMismatch),
        "a reference minted under a drifted alias set is not resolvable, even \
         if the name still happens to exist"
    );
    assert!(outcome.candidates().is_empty());
}

#[test]
fn a_retired_alias_is_not_a_name_anymore() {
    let mut retired = alias_row("chat.default");
    retired.status = ALIAS_STATUS_RETIRED.to_string();
    let aliases =
        AliasSetSnapshot::from_rows(&[retired], &[binding_row("chat.default", "dep-a", 0)]);
    let revision = aliases.policy_revision().to_string();

    let outcome = resolver(aliases)
        .try_resolve(&input(
            "chat.default",
            &revision,
            vec![deployment("dep-a", "acct-a")],
        ))
        .expect("stamped input");
    assert_eq!(
        outcome.selection().abstain_reason(),
        Some(AbstainReason::UnknownAlias)
    );
}

#[test]
fn an_alias_bound_only_to_deployments_the_gate_cut_abstains_with_no_candidates() {
    let aliases = two_way_alias();
    let revision = aliases.policy_revision().to_string();

    let outcome = resolver(aliases)
        .try_resolve(&input(
            "chat.default",
            &revision,
            // Admitted, but bound to nothing this alias names.
            vec![deployment("dep-elsewhere", "acct-a")],
        ))
        .expect("stamped input");

    assert_eq!(
        outcome.selection().abstain_reason(),
        Some(AbstainReason::EmptyCandidateSet)
    );
    assert!(outcome.candidates().is_empty());
}

#[test]
fn a_concrete_deployment_reference_resolves_to_exactly_that_deployment() {
    let aliases = two_way_alias();
    let revision = aliases.policy_revision().to_string();

    let outcome = resolver(aliases)
        .try_resolve(&input(
            "dep-b",
            &revision,
            vec![deployment("dep-a", "acct-a"), deployment("dep-b", "acct-b")],
        ))
        .expect("stamped input");

    assert_eq!(chosen_id(&outcome), "dep-b");
    assert_eq!(
        outcome.candidates().len(),
        1,
        "a concrete reference evaluates the deployment it names, not the \
         alias set around it"
    );
    assert!(outcome.fallback_order().is_empty());
}

// ─── The alias-set policy revision ──────────────────────────────────────────

#[test]
fn the_policy_revision_moves_when_any_part_of_the_active_set_moves() {
    let base = AliasSetSnapshot::from_rows(
        &[alias_row("chat.default")],
        &[binding_row("chat.default", "dep-a", 0)],
    );

    let rebound = AliasSetSnapshot::from_rows(
        &[alias_row("chat.default")],
        &[binding_row("chat.default", "dep-b", 0)],
    );
    assert_ne!(base.policy_revision(), rebound.policy_revision());

    let reprioritized = AliasSetSnapshot::from_rows(
        &[alias_row("chat.default")],
        &[binding_row("chat.default", "dep-a", 7)],
    );
    assert_ne!(base.policy_revision(), reprioritized.policy_revision());

    let mut bumped = alias_row("chat.default");
    bumped.revision = 2;
    let revised =
        AliasSetSnapshot::from_rows(&[bumped], &[binding_row("chat.default", "dep-a", 0)]);
    assert_ne!(
        base.policy_revision(),
        revised.policy_revision(),
        "a third-party write that lands identical content still advances the \
         row revision, and the digest must notice"
    );

    // Row order and constraint-JSON key order are layout, not content.
    let mut reordered_json = alias_row("chat.default");
    reordered_json.constraints = "{}".to_string();
    let same = AliasSetSnapshot::from_rows(
        &[reordered_json],
        &[binding_row("chat.default", "dep-a", 0)],
    );
    assert_eq!(base.policy_revision(), same.policy_revision());
}

#[test]
fn a_retired_binding_leaves_the_policy_but_not_the_history() {
    let mut retired = binding_row("chat.default", "dep-b", 1);
    retired.retired = true;
    let snapshot = AliasSetSnapshot::from_rows(
        &[alias_row("chat.default")],
        &[binding_row("chat.default", "dep-a", 0), retired],
    );

    let entry = snapshot.get("chat.default").expect("alias is active");
    assert_eq!(entry.bindings.len(), 1);
    assert_eq!(entry.bindings[0].deployment_id, "dep-a");
    assert_eq!(
        snapshot.policy_revision(),
        AliasSetSnapshot::from_rows(
            &[alias_row("chat.default")],
            &[binding_row("chat.default", "dep-a", 0)]
        )
        .policy_revision()
    );
}

#[test]
fn reference_classification_is_the_same_function_the_resolution_uses() {
    let resolver = resolver(two_way_alias());
    assert_eq!(
        resolver.classify_reference("chat.default", ["dep-a"]),
        ReferenceClass::Alias
    );
    assert_eq!(
        resolver.classify_reference("dep-a", ["dep-a"]),
        ReferenceClass::Deployment
    );
    assert_eq!(
        resolver.classify_reference("gpt-5-turbo", ["dep-a"]),
        ReferenceClass::Unknown
    );
    assert_eq!(
        resolver.classify_reference("chat.default", ["chat.default"]),
        ReferenceClass::Ambiguous
    );
}
