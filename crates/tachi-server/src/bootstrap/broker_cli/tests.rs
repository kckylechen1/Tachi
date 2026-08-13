//! `tachi broker` plan discriminators (#1681 D2 / PR-D items 2 and 3b).
//!
//! The plan half is what needs discriminating: apply's drift-refuses-everything
//! property is tested at the store door (`memcore::store::model_alias_plan`),
//! and re-testing it through a CLI would only re-test the store.
//!
//! What is unique here is the *proposal*: what a plan says about env-imported
//! rows, what it refuses to say, and the redaction of its rendered view.

use super::*;

use memcore::catalog::{
    CatalogSource, DeploymentCapabilities, NewModelDeployment, ProtocolKind,
    DEPLOYMENT_STATUS_ACTIVE,
};
use memcore::db::model_catalog::{retire_model_deployment, upsert_model_deployment};

const OBSERVED_AT: &str = "2026-08-13T00:00:00.000Z";

fn store() -> MemoryStore {
    MemoryStore::open_in_memory().expect("open in-memory store")
}

fn seed_lane(store: &MemoryStore, lane: &str, endpoint: &str, model: &str) {
    let mut row = NewModelDeployment::observed(
        env_deployment_id(lane),
        "env:api.example.invalid",
        ProtocolKind::OpenAiChatCompletions,
        model,
        CatalogSource::Env,
        OBSERVED_AT,
    )
    .with_endpoint_ref(endpoint)
    .with_capabilities(DeploymentCapabilities {
        chat: true,
        ..DeploymentCapabilities::default()
    });
    row.status = DEPLOYMENT_STATUS_ACTIVE.to_string();
    upsert_model_deployment(store.connection(), &row).expect("seed lane row");
}

fn plan(store: &MemoryStore) -> PlanOutcome {
    build_plan(store).expect("build plan")
}

fn advisory_codes(outcome: &PlanOutcome) -> Vec<&str> {
    outcome
        .advisories
        .iter()
        .map(|advisory| advisory.code.as_str())
        .collect()
}

#[test]
fn a_lane_with_a_catalog_row_is_proposed_as_a_declared_and_bound_alias() {
    let store = store();
    seed_lane(
        &store,
        "reasoning",
        "https://api.deepseek.com/chat/completions",
        "deepseek-reasoner",
    );

    let outcome = plan(&store);
    assert!(outcome.plan.actions.iter().any(|action| matches!(
        action,
        AliasAction::DeclareAlias { alias_name, .. } if alias_name == "lane.reasoning"
    )));
    assert!(outcome.plan.actions.iter().any(|action| matches!(
        action,
        AliasAction::BindDeployment { alias_name, deployment_id, .. }
            if alias_name == "lane.reasoning" && deployment_id == "env:reasoning"
    )));

    // The lane's own revision is bound, which is the whole point of the debt
    // this closes: another process re-resolving env moves that revision, and
    // apply then refuses rather than landing a routing change nobody reviewed.
    let bound = outcome
        .plan
        .bindings
        .deployments
        .iter()
        .find(|binding| binding.deployment_id == "env:reasoning")
        .expect("the lane's row is a bound precondition");
    assert_eq!(bound.revision, Some(1));

    // The four lanes with no row are advisories, not proposals to import one.
    assert!(advisory_codes(&outcome)
        .iter()
        .all(|code| *code == "lane_not_imported"));
}

#[test]
fn a_lane_the_env_chains_never_imported_is_an_advisory_and_never_an_action() {
    let store = store();
    let outcome = plan(&store);

    assert!(
        outcome.plan.actions.is_empty(),
        "alias governance binds names to catalog rows; it never mints the row"
    );
    // One per chat lane plus the embedding lane.
    assert_eq!(
        outcome.advisories.len(),
        ENV_CHAT_LANES.len() + 1,
        "every lane without a row must be reported, not silently skipped"
    );
    assert!(advisory_codes(&outcome)
        .iter()
        .all(|code| *code == "lane_not_imported"));
}

#[test]
fn a_retired_catalog_row_is_not_a_candidate_and_binds_nothing() {
    let store = store();
    seed_lane(
        &store,
        "extract",
        "https://api.siliconflow.cn/v1/chat/completions",
        "Qwen/Qwen3.5-7B",
    );
    retire_model_deployment(store.connection(), &env_deployment_id("extract")).expect("retire");

    let outcome = plan(&store);
    assert!(
        !outcome.plan.actions.iter().any(|action| matches!(
            action,
            AliasAction::BindDeployment { deployment_id, .. } if deployment_id == "env:extract"
        )),
        "a retired row is not something to route traffic at"
    );
    assert!(advisory_codes(&outcome).contains(&"lane_retired"));
    assert!(
        !outcome
            .plan
            .bindings
            .deployments
            .iter()
            .any(|binding| binding.deployment_id == "env:extract"),
        "nothing is bound for a lane the plan decided not to act on"
    );
}

#[test]
fn a_second_plan_over_an_applied_one_proposes_nothing_and_says_so() {
    let mut store = store();
    seed_lane(
        &store,
        "distill",
        "https://api.deepseek.com/chat/completions",
        "deepseek-chat",
    );

    let outcome = plan(&store);
    let digest = alias_plan_digest(&outcome.plan);
    store
        .apply_model_alias_plan(&outcome.plan, &digest)
        .expect("apply");

    let second = plan(&store);
    assert!(
        second.plan.actions.is_empty(),
        "the alias set already matches the catalog: a plan that re-proposed \
         its own work would make every review cycle look like a change"
    );
    let already = second
        .advisories
        .iter()
        .find(|advisory| advisory.code == "already_bound")
        .expect("the settled binding is reported");
    assert!(already.detail.contains("deepseek-chat"));
}

#[test]
fn an_alias_an_operator_retired_is_left_retired() {
    let mut store = store();
    seed_lane(
        &store,
        "summary",
        "https://api.siliconflow.cn/v1/chat/completions",
        "Qwen/Qwen3.5-7B",
    );
    let outcome = plan(&store);
    let digest = alias_plan_digest(&outcome.plan);
    store
        .apply_model_alias_plan(&outcome.plan, &digest)
        .expect("apply");

    // The operator takes the name out of service.
    let retire = BoundAliasPlan {
        bindings: AliasPlanBindings {
            policy_revision: alias_set_policy_revision(
                &list_model_aliases(store.connection()).expect("aliases"),
                &list_model_alias_bindings(store.connection()).expect("bindings"),
            ),
            aliases: vec![AliasRevisionBinding {
                alias_name: "lane.summary".to_string(),
                revision: Some(2),
            }],
            deployments: Vec::new(),
        },
        actions: vec![AliasAction::RetireAlias {
            alias_name: "lane.summary".to_string(),
        }],
    };
    let digest = alias_plan_digest(&retire);
    store
        .apply_model_alias_plan(&retire, &digest)
        .expect("retire the alias");

    let outcome = plan(&store);
    assert!(
        !outcome.plan.actions.iter().any(|action| matches!(
            action,
            AliasAction::DeclareAlias { alias_name, .. } if alias_name == "lane.summary"
        )),
        "a name somebody deliberately took out of service is not revived by \
         the next planning pass"
    );
    assert!(advisory_codes(&outcome).contains(&"alias_retired"));
}

#[test]
fn the_rendered_plan_shows_what_was_digested_and_nothing_else() {
    let store = store();
    seed_lane(
        &store,
        "reasoning",
        "https://api.deepseek.com/chat/completions?api-version=2026-01-01",
        "deepseek-reasoner",
    );
    let outcome = plan(&store);
    let artifact = AliasPlanArtifact {
        schema: PLAN_SCHEMA.to_string(),
        generated_at: OBSERVED_AT.to_string(),
        plan_digest: alias_plan_digest(&outcome.plan),
        bound: outcome.plan.clone(),
    };

    let rendered = render_plan(&artifact, &outcome.advisories);
    assert!(rendered.contains(&artifact.plan_digest));
    assert!(rendered.contains("ACTION\tdeclare_alias\tlane.reasoning"));
    assert!(rendered.contains("ACTION\tbind\tlane.reasoning\tenv:reasoning"));
    assert!(rendered.contains("BOUND\tdeployment\tenv:reasoning\trevision=1"));
    // Every action in the artifact appears in the render: an artifact that
    // could carry an action the human view omits is the lie surface the
    // no-prose rule exists to close.
    assert_eq!(
        rendered
            .lines()
            .filter(|line| line.starts_with("ACTION\t"))
            .count(),
        artifact.bound.actions.len()
    );
}

#[test]
fn the_rendered_view_carries_an_endpoint_authority_and_never_its_path() {
    let store = store();
    seed_lane(
        &store,
        "extract",
        "https://api.siliconflow.cn:8443/v1/chat/completions?api-version=2026-01-01",
        "Qwen/Qwen3.5-7B",
    );
    let mut store = store;
    let outcome = plan(&store);
    let digest = alias_plan_digest(&outcome.plan);
    store
        .apply_model_alias_plan(&outcome.plan, &digest)
        .expect("apply");

    let advisory = plan(&store)
        .advisories
        .into_iter()
        .find(|advisory| advisory.code == "already_bound")
        .expect("the settled binding is reported");

    assert!(advisory.detail.contains("api.siliconflow.cn:8443"));
    assert!(
        !advisory.detail.contains("/v1/chat/completions"),
        "the view prints the authority, not the request path: {}",
        advisory.detail
    );
    assert!(!advisory.detail.contains("api-version"));
}

#[test]
fn the_alias_view_flags_a_row_whose_stamp_is_not_the_current_set_revision() {
    let mut store = store();
    seed_lane(
        &store,
        "reasoning",
        "https://api.deepseek.com/chat/completions",
        "deepseek-reasoner",
    );
    let outcome = plan(&store);
    let digest = alias_plan_digest(&outcome.plan);
    store
        .apply_model_alias_plan(&outcome.plan, &digest)
        .expect("apply");

    let view = render_alias_set(&store).expect("render");
    assert!(
        view.aliases.iter().all(|alias| alias.stamp_matches),
        "a reviewed apply leaves every active alias stamped with the set revision"
    );

    // Something writes a binding without going through plan/apply. The set
    // revision moves; the stamps do not.
    //
    // Hand-written SQL, and that is the finding: since the #1681 PR-D review
    // (CP4) there is no *Rust* way to reach this table from outside memcore —
    // the write accessors are `pub(crate)` and take an `AliasWriteAuthority`
    // only `apply_model_alias_plan` can mint. This test asserts what the
    // remaining bypass looks like from the operator's side.
    store
        .connection()
        .execute(
            "INSERT INTO model_alias_bindings
                (alias_name, deployment_id, priority, retired, created_at, updated_at)
             VALUES ('lane.reasoning', 'env:reasoning-elsewhere', 1, 0, ?1, ?1)",
            rusqlite::params!["2026-08-13T00:00:00.000Z"],
        )
        .expect("unreviewed write");

    let view = render_alias_set(&store).expect("render");
    assert!(
        view.aliases.iter().any(|alias| !alias.stamp_matches),
        "an unreviewed write is exactly what the stamp exists to make visible"
    );
    assert!(render_alias_set_text(&view).contains("UNREVIEWED"));
}
