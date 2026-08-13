//! Apply a bound alias plan, or refuse it and write nothing (tachi#1681 D2).
//!
//! The single write door into `model_aliases` / `model_alias_bindings`. It is
//! `db::vault_accounts`'s apply with a different object: one
//! `BEGIN IMMEDIATE`, every precondition re-read *inside* it, and any drift
//! refusing the whole plan rather than applying the part that still fits.
//!
//! # Why the write lock comes before the re-read
//!
//! `BEGIN IMMEDIATE` rather than the default deferred transaction: taking the
//! lock after reading the preconditions would make the re-read just an earlier
//! read with a shorter race window, which is not a check at all. The lock is
//! held from before the first `SELECT` to after the last write.
//!
//! # Why re-applying an applied plan is a refusal, not a no-op
//!
//! The provider-account plan records a `plan_noop` event when a replay finds
//! the world already matching. An alias plan cannot: every action here
//! advances the alias revision the plan bound, so once the plan has applied,
//! its own preconditions no longer hold and a replay refuses with
//! `alias_revision_drift`. That is the stronger property, and it is the right
//! one for this object — an alias plan is an approved routing change, and
//! "apply exactly once" is what an approval means. A plan that changes nothing
//! (the world already matches) moves no revision and stays replayable, which is
//! the only case where replay is meaningful.

use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

use crate::catalog::alias_plan::{alias_plan_digest, AliasAction, BoundAliasPlan};
use crate::catalog::alias_policy::alias_set_policy_revision;
use crate::db::model_catalog::{
    bind_model_alias_deployment, get_model_alias, get_model_deployment, list_model_alias_bindings,
    list_model_aliases, retire_model_alias, retire_model_alias_binding, stamp_alias_policy_digest,
    upsert_model_alias, AliasDeclaration, AliasWrite,
};
use crate::error::{AliasPlanRefusal, MemoryError};
use crate::MemoryStore;

/// One alias and the revision it now sits at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AliasRevision {
    pub alias_name: String,
    pub revision: i64,
}

/// One alias → deployment pair named in an apply report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AliasBindingRef {
    pub alias_name: String,
    pub deployment_id: String,
}

/// What one alias apply actually did.
///
/// Names, ids, revisions and digests only — there is no secret anywhere on
/// this path, and the report is written to a terminal and pasted into tickets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AliasApplyReport {
    pub plan_digest: String,
    /// Whether anything about the recorded alias set changed. `false` is the
    /// idempotent outcome: every action found the world already matching it,
    /// and no revision moved.
    pub changed: bool,
    pub aliases_declared: Vec<AliasRevision>,
    pub aliases_retired: Vec<AliasRevision>,
    pub bindings_bound: Vec<AliasBindingRef>,
    pub bindings_retired: Vec<AliasBindingRef>,
    /// The alias-set policy revision **after** this apply. Consumers stamp it
    /// onto the `ModelRef`s they mint; the resolver refuses to resolve a
    /// `ModelRef` carrying any other value.
    pub policy_revision: String,
}

impl MemoryStore {
    /// Apply a bound alias plan, or refuse it and write nothing.
    ///
    /// `declared_digest` is the digest the plan was *presented* under — what
    /// the artifact file says and what the operator approved. It is compared
    /// against the digest recomputed from the plan's own content, which catches
    /// both halves of a tampered artifact: edit an action and the recomputed
    /// digest moves, edit the digest and the declared one moves.
    ///
    /// # Errors
    ///
    /// [`MemoryError::ModelAliasPlanRefused`] for every drift and every
    /// malformed plan — in all cases nothing was written. Other variants are
    /// genuine storage failures, which roll back for the same reason.
    pub fn apply_model_alias_plan(
        &mut self,
        plan: &BoundAliasPlan,
        declared_digest: &str,
    ) -> Result<AliasApplyReport, MemoryError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let report = apply_within(&tx, plan, declared_digest)?;
        tx.commit()?;
        Ok(report)
    }
}

fn refuse<T>(reason: AliasPlanRefusal, detail: impl Into<String>) -> Result<T, MemoryError> {
    Err(MemoryError::ModelAliasPlanRefused {
        reason,
        detail: detail.into(),
    })
}

fn apply_within(
    tx: &Transaction<'_>,
    plan: &BoundAliasPlan,
    declared_digest: &str,
) -> Result<AliasApplyReport, MemoryError> {
    let recomputed = alias_plan_digest(plan);
    if recomputed != declared_digest {
        return refuse(
            AliasPlanRefusal::PlanDigestMismatch,
            format!("plan was presented as '{declared_digest}' but hashes to '{recomputed}'"),
        );
    }

    verify_plan_shape(plan)?;
    verify_policy_revision(tx, plan)?;
    verify_aliases(tx, plan)?;
    verify_deployments(tx, plan)?;

    let mut report = AliasApplyReport {
        plan_digest: recomputed,
        changed: false,
        aliases_declared: Vec::new(),
        aliases_retired: Vec::new(),
        bindings_bound: Vec::new(),
        bindings_retired: Vec::new(),
        policy_revision: plan.bindings.policy_revision.clone(),
    };
    for action in &plan.actions {
        run_action(tx, action, &mut report)?;
    }

    report.changed = !report.aliases_declared.is_empty()
        || !report.aliases_retired.is_empty()
        || !report.bindings_bound.is_empty()
        || !report.bindings_retired.is_empty();

    if report.changed {
        // Re-derive the set revision from what is now stored and stamp it on
        // every active alias. Stamping the whole active set, not just the rows
        // this plan touched, is what makes the stamp checkable: after a
        // reviewed apply every active alias carries the current set revision,
        // so a row that does not is the signature of a write that came from
        // somewhere other than plan/apply.
        report.policy_revision = current_policy_revision(tx)?;
        for alias in list_model_aliases(tx)? {
            if alias.status == crate::catalog::ALIAS_STATUS_ACTIVE {
                stamp_alias_policy_digest(tx, &alias.alias_name, &report.policy_revision)?;
            }
        }
    }
    Ok(report)
}

// ── Verification ───────────────────────────────────────────────────────────

/// Structural checks that need no database: identifiers present, declarations
/// well formed, bindings not self-contradictory, every touched object bound.
fn verify_plan_shape(plan: &BoundAliasPlan) -> Result<(), MemoryError> {
    if plan.bindings.policy_revision.trim().is_empty() {
        return refuse(
            AliasPlanRefusal::MalformedPlan,
            "the plan binds no alias-set policy revision, so there is nothing to check the \
             reviewed set against",
        );
    }

    let mut bound_aliases: BTreeSet<&str> = BTreeSet::new();
    for binding in &plan.bindings.aliases {
        if binding.alias_name.trim().is_empty() {
            return refuse(
                AliasPlanRefusal::MalformedPlan,
                "an alias binding carries an empty alias_name",
            );
        }
        if !bound_aliases.insert(binding.alias_name.as_str()) {
            return refuse(
                AliasPlanRefusal::MalformedPlan,
                format!("alias '{}' is bound twice", binding.alias_name),
            );
        }
    }

    let mut bound_deployments: BTreeSet<&str> = BTreeSet::new();
    for binding in &plan.bindings.deployments {
        if binding.deployment_id.trim().is_empty() {
            return refuse(
                AliasPlanRefusal::MalformedPlan,
                "a deployment binding carries an empty deployment_id",
            );
        }
        if !bound_deployments.insert(binding.deployment_id.as_str()) {
            return refuse(
                AliasPlanRefusal::MalformedPlan,
                format!("deployment '{}' is bound twice", binding.deployment_id),
            );
        }
    }

    // One action per (alias, deployment) pair and one per alias-level verb: a
    // plan that both binds and retires the same pair does not describe a state,
    // and applying it in list order would make the outcome depend on which
    // action the author happened to write second.
    let mut seen_actions: BTreeSet<(&str, Option<&str>)> = BTreeSet::new();
    for action in &plan.actions {
        let alias_name = action.alias_name();
        if alias_name.trim().is_empty() {
            return refuse(
                AliasPlanRefusal::MalformedPlan,
                "an action carries an empty alias_name",
            );
        }
        if !bound_aliases.contains(alias_name) {
            return refuse(
                AliasPlanRefusal::UnboundAlias,
                format!("action targets alias '{alias_name}', which the plan never bound"),
            );
        }
        // Keyed by the object the action acts on — the alias itself, or one
        // alias/deployment pair. Declaring an alias and binding a deployment to
        // it in the same plan is therefore legal, while declare-and-retire on
        // one alias, or bind-and-retire on one pair, is not: that pair of
        // actions does not describe a state, and applying it in list order
        // would make the outcome depend on which one the author wrote second.
        if !seen_actions.insert((alias_name, action.deployment_id())) {
            return refuse(
                AliasPlanRefusal::MalformedPlan,
                format!("alias '{alias_name}' carries two actions on the same object"),
            );
        }

        match action {
            AliasAction::DeclareAlias {
                required_capabilities,
                constraints,
                ..
            } => {
                verify_json_object(required_capabilities, "required_capabilities")?;
                verify_json_object(constraints, "constraints")?;
            }
            AliasAction::BindDeployment { deployment_id, .. }
            | AliasAction::RetireBinding { deployment_id, .. } => {
                if deployment_id.trim().is_empty() {
                    return refuse(
                        AliasPlanRefusal::MalformedPlan,
                        format!("an action on alias '{alias_name}' carries an empty deployment_id"),
                    );
                }
                if !bound_deployments.contains(deployment_id.as_str()) {
                    return refuse(
                        AliasPlanRefusal::UnboundDeployment,
                        format!(
                            "action binds alias '{alias_name}' to deployment '{deployment_id}', \
                             which the plan never bound"
                        ),
                    );
                }
            }
            AliasAction::RetireAlias { .. } => {}
        }
    }
    Ok(())
}

fn verify_json_object(raw: &str, field: &str) -> Result<(), MemoryError> {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::Object(_)) => Ok(()),
        _ => refuse(
            AliasPlanRefusal::MalformedPlan,
            format!("`{field}` is not a JSON object: '{raw}'"),
        ),
    }
}

/// The whole active alias set must still be the one the plan was reviewed
/// against — including the aliases the plan does not touch.
fn verify_policy_revision(tx: &Transaction<'_>, plan: &BoundAliasPlan) -> Result<(), MemoryError> {
    let current = current_policy_revision(tx)?;
    if current != plan.bindings.policy_revision {
        return refuse(
            AliasPlanRefusal::PolicyRevisionDrift,
            format!(
                "plan was reviewed against alias set '{}' but the stored set is '{current}'",
                plan.bindings.policy_revision
            ),
        );
    }
    Ok(())
}

fn verify_aliases(tx: &Transaction<'_>, plan: &BoundAliasPlan) -> Result<(), MemoryError> {
    for binding in &plan.bindings.aliases {
        let stored = get_model_alias(tx, &binding.alias_name)?.map(|alias| alias.revision);
        if stored != binding.revision {
            return refuse(
                AliasPlanRefusal::AliasRevisionDrift,
                format!(
                    "alias '{}' was planned at {} and is now {}",
                    binding.alias_name,
                    describe(binding.revision),
                    describe(stored)
                ),
            );
        }
    }
    Ok(())
}

fn verify_deployments(tx: &Transaction<'_>, plan: &BoundAliasPlan) -> Result<(), MemoryError> {
    // Every deployment a `bind_deployment` action names must exist: alias
    // governance binds names to catalog rows, and binding a name to nothing
    // would create an alias that can only ever abstain.
    let binds: BTreeSet<&str> = plan
        .actions
        .iter()
        .filter_map(|action| match action {
            AliasAction::BindDeployment { deployment_id, .. } => Some(deployment_id.as_str()),
            _ => None,
        })
        .collect();

    for binding in &plan.bindings.deployments {
        let stored = get_model_deployment(tx, &binding.deployment_id)?.map(|row| row.revision);
        if stored != binding.revision {
            return refuse(
                AliasPlanRefusal::DeploymentRevisionDrift,
                format!(
                    "deployment '{}' was planned at {} and is now {}",
                    binding.deployment_id,
                    describe(binding.revision),
                    describe(stored)
                ),
            );
        }
        if stored.is_none() && binds.contains(binding.deployment_id.as_str()) {
            return refuse(
                AliasPlanRefusal::UnknownDeployment,
                format!(
                    "plan binds an alias to deployment '{}', which the catalog does not have",
                    binding.deployment_id
                ),
            );
        }
    }
    Ok(())
}

fn describe(revision: Option<i64>) -> String {
    match revision {
        Some(revision) => format!("revision {revision}"),
        None => "absent".to_string(),
    }
}

// ── Execution ──────────────────────────────────────────────────────────────

fn run_action(
    tx: &Transaction<'_>,
    action: &AliasAction,
    report: &mut AliasApplyReport,
) -> Result<(), MemoryError> {
    match action {
        AliasAction::DeclareAlias {
            alias_name,
            required_capabilities,
            constraints,
            source_refs,
        } => {
            let write = upsert_model_alias(
                tx,
                &AliasDeclaration {
                    alias_name: alias_name.clone(),
                    required_capabilities: required_capabilities.clone(),
                    constraints: constraints.clone(),
                    source_refs: source_refs.clone(),
                },
            )?;
            record(write, &mut report.aliases_declared, alias_name);
        }
        AliasAction::RetireAlias { alias_name } => {
            // `None` — the alias is not there. The plan bound its absence and
            // the binding verified, so there is nothing to retire and nothing
            // to report; refusing here would refuse a plan that asked for a
            // state the world is already in.
            if let Some(write) = retire_model_alias(tx, alias_name)? {
                record(write, &mut report.aliases_retired, alias_name);
            }
        }
        AliasAction::BindDeployment {
            alias_name,
            deployment_id,
            priority,
        } => {
            let write = bind_model_alias_deployment(tx, alias_name, deployment_id, *priority)?;
            if write.changed() {
                report.bindings_bound.push(AliasBindingRef {
                    alias_name: alias_name.clone(),
                    deployment_id: deployment_id.clone(),
                });
            }
        }
        AliasAction::RetireBinding {
            alias_name,
            deployment_id,
        } => {
            if let Some(write) = retire_model_alias_binding(tx, alias_name, deployment_id)? {
                if write.changed() {
                    report.bindings_retired.push(AliasBindingRef {
                        alias_name: alias_name.clone(),
                        deployment_id: deployment_id.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn record(write: AliasWrite, into: &mut Vec<AliasRevision>, alias_name: &str) {
    if write.changed() {
        into.push(AliasRevision {
            alias_name: alias_name.to_string(),
            revision: write.revision(),
        });
    }
}

/// The alias-set policy revision of what is stored right now.
fn current_policy_revision(conn: &Connection) -> Result<String, MemoryError> {
    Ok(alias_set_policy_revision(
        &list_model_aliases(conn)?,
        &list_model_alias_bindings(conn)?,
    ))
}

/// Ordered alias → active binding view, for callers assembling a plan.
///
/// Here rather than in `db::model_catalog` because it is a *planning* read: it
/// exists so a planner and this apply agree on what "currently bound" means,
/// and keeping the two on one function is what stops a plan from proposing a
/// binding that already exists.
pub fn current_alias_bindings(
    conn: &Connection,
) -> Result<BTreeMap<String, Vec<String>>, MemoryError> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for binding in list_model_alias_bindings(conn)? {
        if !binding.retired {
            out.entry(binding.alias_name)
                .or_default()
                .push(binding.deployment_id);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
