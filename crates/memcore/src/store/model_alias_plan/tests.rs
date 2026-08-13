//! Apply-side discriminators for the alias plan (#1681 D2 / PR-D item 2).
//!
//! The governing property is **drift = zero writes**, and every refusal test
//! asserts it against a full before/after snapshot of both alias tables rather
//! than against the one row the test was thinking about — "nothing was
//! written" is a statement about the database.

use super::*;
use crate::catalog::alias_plan::{
    AliasAction, AliasPlanBindings, AliasRevisionBinding, BoundAliasPlan, DeploymentRevisionBinding,
};
use crate::catalog::{CatalogSource, NewModelDeployment, ProtocolKind, ALIAS_STATUS_RETIRED};
use crate::db::model_catalog::{
    list_all_model_alias_events, list_model_alias_events, upsert_model_deployment,
};

const OBSERVED_AT: &str = "2026-08-13T00:00:00.000Z";

fn store() -> MemoryStore {
    MemoryStore::open_in_memory().expect("open in-memory store")
}

fn seed_deployment(store: &MemoryStore, deployment_id: &str) {
    upsert_model_deployment(
        store.connection(),
        &NewModelDeployment::observed(
            deployment_id,
            "env:api.example.invalid",
            ProtocolKind::OpenAiChatCompletions,
            format!("{deployment_id}-model"),
            CatalogSource::Env,
            OBSERVED_AT,
        ),
    )
    .expect("seed deployment");
}

/// Everything both alias tables hold, in a comparable shape.
fn snapshot(
    store: &MemoryStore,
) -> (
    Vec<crate::catalog::ModelAlias>,
    Vec<crate::catalog::ModelAliasBinding>,
) {
    (
        list_model_aliases(store.connection()).expect("aliases"),
        list_model_alias_bindings(store.connection()).expect("bindings"),
    )
}

fn policy_revision(store: &MemoryStore) -> String {
    current_policy_revision(store.connection()).expect("policy revision")
}

fn declare(alias_name: &str) -> AliasAction {
    AliasAction::DeclareAlias {
        alias_name: alias_name.to_string(),
        required_capabilities: r#"{"chat":true}"#.to_string(),
        constraints: "{}".to_string(),
        source_refs: vec!["env:reasoning".to_string()],
    }
}

fn bind(alias_name: &str, deployment_id: &str, priority: i64) -> AliasAction {
    AliasAction::BindDeployment {
        alias_name: alias_name.to_string(),
        deployment_id: deployment_id.to_string(),
        priority,
    }
}

/// A plan that declares `chat.default` and binds it to `env:reasoning`,
/// bound against the store's current state.
fn first_plan(store: &MemoryStore) -> BoundAliasPlan {
    BoundAliasPlan {
        bindings: AliasPlanBindings {
            policy_revision: policy_revision(store),
            aliases: vec![AliasRevisionBinding {
                alias_name: "chat.default".to_string(),
                revision: None,
            }],
            deployments: vec![DeploymentRevisionBinding {
                deployment_id: "env:reasoning".to_string(),
                revision: Some(1),
            }],
        },
        actions: vec![
            declare("chat.default"),
            bind("chat.default", "env:reasoning", 0),
        ],
    }
}

fn apply(store: &mut MemoryStore, plan: &BoundAliasPlan) -> Result<AliasApplyReport, MemoryError> {
    let digest = alias_plan_digest(plan);
    store.apply_model_alias_plan(plan, &digest)
}

fn refusal(error: MemoryError) -> AliasPlanRefusal {
    match error {
        MemoryError::ModelAliasPlanRefused { reason, .. } => reason,
        other => panic!("expected an alias plan refusal, got {other:?}"),
    }
}

// ─── The happy path, and what it stamps ─────────────────────────────────────

#[test]
fn a_reviewed_plan_declares_binds_and_stamps_the_new_policy_revision() {
    let mut store = store();
    seed_deployment(&store, "env:reasoning");
    let before = policy_revision(&store);

    let plan = first_plan(&store);
    let report = apply(&mut store, &plan).expect("apply");

    assert!(report.changed);
    assert_eq!(report.aliases_declared.len(), 1);
    assert_eq!(report.bindings_bound.len(), 1);
    assert_ne!(report.policy_revision, before);
    assert_eq!(report.policy_revision, policy_revision(&store));

    // Every active alias carries the post-apply set revision, so a row that
    // does not is the signature of a write that bypassed plan/apply.
    let (aliases, bindings) = snapshot(&store);
    assert_eq!(aliases.len(), 1);
    assert_eq!(
        aliases[0].policy_digest.as_deref(),
        Some(report.policy_revision.as_str())
    );
    assert_eq!(bindings.len(), 1);
    assert!(!bindings[0].retired);
}

#[test]
fn a_plan_that_changes_nothing_writes_nothing_and_stays_replayable() {
    let mut store = store();
    seed_deployment(&store, "env:reasoning");
    {
        let plan = first_plan(&store);
        apply(&mut store, &plan).expect("first apply");
    }
    let settled = snapshot(&store);

    // Re-declare and re-bind exactly what is already there, bound against the
    // now-current state.
    let plan = BoundAliasPlan {
        bindings: AliasPlanBindings {
            policy_revision: policy_revision(&store),
            aliases: vec![AliasRevisionBinding {
                alias_name: "chat.default".to_string(),
                revision: Some(2),
            }],
            deployments: vec![DeploymentRevisionBinding {
                deployment_id: "env:reasoning".to_string(),
                revision: Some(1),
            }],
        },
        actions: vec![
            declare("chat.default"),
            bind("chat.default", "env:reasoning", 0),
        ],
    };
    let report = apply(&mut store, &plan).expect("replay apply");

    assert!(
        !report.changed,
        "the world already matched: nothing may move"
    );
    assert_eq!(snapshot(&store), settled);
    // A no-op plan is applyable again, because it spent no revision.
    assert!(apply(&mut store, &plan).is_ok());
}

#[test]
fn re_applying_a_plan_that_did_change_something_is_a_drift_refusal() {
    let mut store = store();
    seed_deployment(&store, "env:reasoning");
    let plan = first_plan(&store);
    apply(&mut store, &plan).expect("first apply");
    let settled = snapshot(&store);

    let error = apply(&mut store, &plan).expect_err("a spent plan is not applyable twice");
    assert_eq!(refusal(error), AliasPlanRefusal::PolicyRevisionDrift);
    assert_eq!(snapshot(&store), settled, "a refused replay writes nothing");
}

// ─── Drift = zero writes ────────────────────────────────────────────────────

#[test]
fn an_edited_action_leaves_the_digest_behind_and_writes_nothing() {
    let mut store = store();
    seed_deployment(&store, "env:reasoning");
    seed_deployment(&store, "env:extract");
    let before = snapshot(&store);

    let plan = first_plan(&store);
    let honest_digest = alias_plan_digest(&plan);
    // The operator approved a plan binding env:reasoning; the artifact now
    // binds env:extract under the approved digest.
    let mut tampered = plan;
    tampered.actions[1] = bind("chat.default", "env:extract", 0);
    tampered
        .bindings
        .deployments
        .push(DeploymentRevisionBinding {
            deployment_id: "env:extract".to_string(),
            revision: Some(1),
        });

    let error = store
        .apply_model_alias_plan(&tampered, &honest_digest)
        .expect_err("a rewritten action must not apply under the reviewed digest");
    assert_eq!(refusal(error), AliasPlanRefusal::PlanDigestMismatch);
    assert_eq!(snapshot(&store), before);
}

#[test]
fn an_alias_this_plan_never_mentions_moving_is_still_drift() {
    let mut store = store();
    seed_deployment(&store, "env:reasoning");
    seed_deployment(&store, "env:extract");

    // Reviewed against a world with one alias…
    let plan = first_plan(&store);

    // …and a concurrent reviewed apply binds a *different* alias, re-routing
    // traffic this operator reviewed nothing about. No per-row binding on
    // `chat.default` could notice it; the set revision does.
    let other = BoundAliasPlan {
        bindings: AliasPlanBindings {
            policy_revision: policy_revision(&store),
            aliases: vec![AliasRevisionBinding {
                alias_name: "chat.cheap".to_string(),
                revision: None,
            }],
            deployments: vec![DeploymentRevisionBinding {
                deployment_id: "env:extract".to_string(),
                revision: Some(1),
            }],
        },
        actions: vec![declare("chat.cheap"), bind("chat.cheap", "env:extract", 0)],
    };
    apply(&mut store, &other).expect("the concurrent plan applies");
    let settled = snapshot(&store);

    let error = apply(&mut store, &plan).expect_err("the reviewed set moved");
    assert_eq!(refusal(error), AliasPlanRefusal::PolicyRevisionDrift);
    assert_eq!(snapshot(&store), settled);
}

#[test]
fn a_deployment_retired_between_plan_and_apply_refuses_the_whole_plan() {
    let mut store = store();
    seed_deployment(&store, "env:reasoning");
    let plan = first_plan(&store);

    // The deployment advances (a retirement bumps its revision), so the plan's
    // statement "bind the alias to this row as I read it" is no longer true.
    crate::db::model_catalog::retire_model_deployment(store.connection(), "env:reasoning")
        .expect("retire");
    let settled = snapshot(&store);

    let error = apply(&mut store, &plan).expect_err("the bound deployment moved");
    assert_eq!(refusal(error), AliasPlanRefusal::DeploymentRevisionDrift);
    assert_eq!(snapshot(&store), settled);
}

#[test]
fn binding_an_alias_to_a_deployment_the_catalog_does_not_have_is_refused() {
    let mut store = store();
    let before = snapshot(&store);

    let plan = BoundAliasPlan {
        bindings: AliasPlanBindings {
            policy_revision: policy_revision(&store),
            aliases: vec![AliasRevisionBinding {
                alias_name: "chat.default".to_string(),
                revision: None,
            }],
            deployments: vec![DeploymentRevisionBinding {
                deployment_id: "env:never-imported".to_string(),
                revision: None,
            }],
        },
        actions: vec![
            declare("chat.default"),
            bind("chat.default", "env:never-imported", 0),
        ],
    };

    let error = apply(&mut store, &plan).expect_err("alias governance never mints a deployment");
    assert_eq!(refusal(error), AliasPlanRefusal::UnknownDeployment);
    assert_eq!(snapshot(&store), before);
}

#[test]
fn an_action_on_an_unbound_object_is_refused_even_when_nothing_drifted() {
    let mut store = store();
    seed_deployment(&store, "env:reasoning");
    let before = snapshot(&store);

    let unbound_alias = BoundAliasPlan {
        bindings: AliasPlanBindings {
            policy_revision: policy_revision(&store),
            aliases: Vec::new(),
            deployments: Vec::new(),
        },
        actions: vec![declare("chat.default")],
    };
    assert_eq!(
        refusal(apply(&mut store, &unbound_alias).expect_err("unbound alias")),
        AliasPlanRefusal::UnboundAlias
    );

    let unbound_deployment = BoundAliasPlan {
        bindings: AliasPlanBindings {
            policy_revision: policy_revision(&store),
            aliases: vec![AliasRevisionBinding {
                alias_name: "chat.default".to_string(),
                revision: None,
            }],
            deployments: Vec::new(),
        },
        actions: vec![bind("chat.default", "env:reasoning", 0)],
    };
    assert_eq!(
        refusal(apply(&mut store, &unbound_deployment).expect_err("unbound deployment")),
        AliasPlanRefusal::UnboundDeployment
    );
    assert_eq!(snapshot(&store), before);
}

#[test]
fn two_contradictory_actions_on_one_object_are_refused_before_any_write() {
    let mut store = store();
    seed_deployment(&store, "env:reasoning");
    {
        let plan = first_plan(&store);
        apply(&mut store, &plan).expect("seed the alias");
    }
    let settled = snapshot(&store);

    let plan = BoundAliasPlan {
        bindings: AliasPlanBindings {
            policy_revision: policy_revision(&store),
            aliases: vec![AliasRevisionBinding {
                alias_name: "chat.default".to_string(),
                revision: Some(2),
            }],
            deployments: vec![DeploymentRevisionBinding {
                deployment_id: "env:reasoning".to_string(),
                revision: Some(1),
            }],
        },
        actions: vec![
            bind("chat.default", "env:reasoning", 0),
            AliasAction::RetireBinding {
                alias_name: "chat.default".to_string(),
                deployment_id: "env:reasoning".to_string(),
            },
        ],
    };

    assert_eq!(
        refusal(apply(&mut store, &plan).expect_err("bind and retire do not describe a state")),
        AliasPlanRefusal::MalformedPlan
    );
    assert_eq!(snapshot(&store), settled);
}

#[test]
fn a_declaration_that_is_not_a_json_object_is_refused() {
    let mut store = store();
    let before = snapshot(&store);

    let plan = BoundAliasPlan {
        bindings: AliasPlanBindings {
            policy_revision: policy_revision(&store),
            aliases: vec![AliasRevisionBinding {
                alias_name: "chat.default".to_string(),
                revision: None,
            }],
            deployments: Vec::new(),
        },
        actions: vec![AliasAction::DeclareAlias {
            alias_name: "chat.default".to_string(),
            required_capabilities: "\"chat\"".to_string(),
            constraints: "{}".to_string(),
            source_refs: Vec::new(),
        }],
    };

    assert_eq!(
        refusal(apply(&mut store, &plan).expect_err("a string is not a capability object")),
        AliasPlanRefusal::MalformedPlan
    );
    assert_eq!(snapshot(&store), before);
}

// ─── Retirement keeps history ───────────────────────────────────────────────

#[test]
fn retiring_a_binding_leaves_the_reverse_lookup_intact() {
    let mut store = store();
    seed_deployment(&store, "env:reasoning");
    {
        let plan = first_plan(&store);
        apply(&mut store, &plan).expect("seed the alias");
    }

    let plan = BoundAliasPlan {
        bindings: AliasPlanBindings {
            policy_revision: policy_revision(&store),
            aliases: vec![AliasRevisionBinding {
                alias_name: "chat.default".to_string(),
                revision: Some(2),
            }],
            deployments: vec![DeploymentRevisionBinding {
                deployment_id: "env:reasoning".to_string(),
                revision: Some(1),
            }],
        },
        actions: vec![AliasAction::RetireBinding {
            alias_name: "chat.default".to_string(),
            deployment_id: "env:reasoning".to_string(),
        }],
    };
    let report = apply(&mut store, &plan).expect("retire the binding");

    assert_eq!(report.bindings_retired.len(), 1);
    let (_, bindings) = snapshot(&store);
    assert_eq!(
        bindings.len(),
        1,
        "the row stays: 'who referenced this deployment' survives"
    );
    assert!(bindings[0].retired);
    // …and the retired binding is out of the routable set.
    assert!(!current_alias_bindings(store.connection())
        .expect("bindings")
        .contains_key("chat.default"));
}

// ─── The single write door and its log (#1681 PR-D review, CP4) ─────────────

/// Every alias paired with the revision its newest event says it reached.
///
/// This is the reconciliation the append-only log exists to make possible: the
/// door writes the row and its event in the same transaction, so an alias whose
/// current revision is not its newest event's revision — or which has no event
/// at all — was moved by something that did not come through the door.
fn revisions_against_the_log(store: &MemoryStore) -> Vec<(String, i64, Option<i64>)> {
    list_model_aliases(store.connection())
        .expect("aliases")
        .into_iter()
        .map(|alias| {
            let newest = list_model_alias_events(store.connection(), &alias.alias_name)
                .expect("events")
                .last()
                .map(|event| event.revision);
            (alias.alias_name, alias.revision, newest)
        })
        .collect()
}

#[test]
fn every_write_the_door_makes_is_logged_and_an_idempotent_replay_logs_nothing() {
    let mut store = store();
    seed_deployment(&store, "env:reasoning");

    let plan = first_plan(&store);
    let report = apply(&mut store, &plan).expect("apply");

    let events = list_all_model_alias_events(store.connection()).expect("events");
    let kinds: Vec<&str> = events
        .iter()
        .map(|event| event.event_kind.as_str())
        .collect();
    assert_eq!(
        kinds,
        vec![
            AliasEventKind::AliasDeclared.as_str(),
            AliasEventKind::AliasBindingBound.as_str()
        ],
        "one event per write, in the order the plan's actions ran"
    );
    assert!(
        events
            .iter()
            .all(|event| event.plan_digest.as_deref() == Some(report.plan_digest.as_str())),
        "every event names the approved plan that caused it"
    );
    assert_eq!(
        events[1].evidence, r#"{"deployment_id":"env:reasoning","priority":0}"#,
        "a binding event carries the pair it bound, and nothing else"
    );

    // The row and the log agree, which is the property a bypass breaks.
    assert_eq!(
        revisions_against_the_log(&store),
        vec![("chat.default".to_string(), 2, Some(2))]
    );

    // Re-declare and re-bind exactly what is there. Nothing moves, so nothing
    // is logged: an idempotent replay that appended events would turn "nothing
    // happened" into an audit trail saying otherwise.
    let replay = BoundAliasPlan {
        bindings: AliasPlanBindings {
            policy_revision: policy_revision(&store),
            aliases: vec![AliasRevisionBinding {
                alias_name: "chat.default".to_string(),
                revision: Some(2),
            }],
            deployments: vec![DeploymentRevisionBinding {
                deployment_id: "env:reasoning".to_string(),
                revision: Some(1),
            }],
        },
        actions: vec![
            declare("chat.default"),
            bind("chat.default", "env:reasoning", 0),
        ],
    };
    assert!(!apply(&mut store, &replay).expect("replay").changed);
    assert_eq!(
        list_all_model_alias_events(store.connection())
            .expect("events")
            .len(),
        2,
        "a no-change apply appends no event"
    );
}

#[test]
fn a_refused_plan_writes_no_event_either() {
    let mut store = store();
    seed_deployment(&store, "env:reasoning");
    let plan = first_plan(&store);
    let honest_digest = alias_plan_digest(&plan);

    let mut tampered = plan;
    tampered.actions[0] = AliasAction::RetireAlias {
        alias_name: "chat.default".to_string(),
    };

    store
        .apply_model_alias_plan(&tampered, &honest_digest)
        .expect_err("a rewritten action does not apply under the reviewed digest");
    assert!(
        list_all_model_alias_events(store.connection())
            .expect("events")
            .is_empty(),
        "'drift = zero writes' covers the log: a refused plan leaves no trace \
         claiming it did something"
    );
}

#[test]
fn a_write_that_goes_around_the_door_is_the_one_the_log_cannot_account_for() {
    let mut store = store();
    seed_deployment(&store, "env:reasoning");
    let plan = first_plan(&store);
    apply(&mut store, &plan).expect("apply");
    assert_eq!(
        revisions_against_the_log(&store),
        vec![("chat.default".to_string(), 2, Some(2))]
    );

    // There is no Rust API that could do this any more: since CP4 the alias
    // write accessors are `pub(crate)` and take an `AliasWriteAuthority` whose
    // only constructor is private to the apply module. Hand-written SQL is the
    // one remaining bypass, and this is what it leaves behind.
    store
        .connection()
        .execute(
            "UPDATE model_aliases SET revision = revision + 1 WHERE alias_name = 'chat.default'",
            [],
        )
        .expect("unreviewed write");

    assert_eq!(
        revisions_against_the_log(&store),
        vec![("chat.default".to_string(), 3, Some(2))],
        "the row moved and the log did not: an alias whose revision is not its \
         newest event's revision was written by something other than plan/apply"
    );
}

#[test]
fn retiring_an_alias_takes_the_name_out_of_the_policy_but_not_the_table() {
    let mut store = store();
    seed_deployment(&store, "env:reasoning");
    {
        let plan = first_plan(&store);
        apply(&mut store, &plan).expect("seed the alias");
    }

    let plan = BoundAliasPlan {
        bindings: AliasPlanBindings {
            policy_revision: policy_revision(&store),
            aliases: vec![AliasRevisionBinding {
                alias_name: "chat.default".to_string(),
                revision: Some(2),
            }],
            deployments: Vec::new(),
        },
        actions: vec![AliasAction::RetireAlias {
            alias_name: "chat.default".to_string(),
        }],
    };
    let report = apply(&mut store, &plan).expect("retire the alias");

    assert_eq!(report.aliases_retired.len(), 1);
    let (aliases, bindings) = snapshot(&store);
    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases[0].status, ALIAS_STATUS_RETIRED);
    assert_eq!(bindings.len(), 1, "the bindings are history, not garbage");
}
