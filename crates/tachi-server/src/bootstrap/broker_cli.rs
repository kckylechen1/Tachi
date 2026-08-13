//! `tachi broker` — model-broker alias plan/apply (tachi#1681 D2, PR-D).
//!
//! Four stages, and the boundary between them is the design, exactly as in
//! `vault_cli::reconcile`:
//!
//! 1. **discover** — read-only, over the catalog rows the env import already
//!    wrote. No new source vocabulary, and nothing on the filesystem is read.
//! 2. **plan** — a typed [`memcore::catalog::alias_plan::BoundAliasPlan`] plus
//!    the `bp1:` digest over it, written to an artifact the operator can read
//!    and approve.
//! 3. **read** — the operator's job, which is why the artifact carries no
//!    prose: both `plan` and `apply` render the human view *from* the digested
//!    actions, so what is displayed is always what is digested.
//! 4. **apply** — hands the plan to the store's single write transaction,
//!    which re-reads every binding before it writes anything.
//!
//! # The debt this closes: env rows now have a governance surface
//!
//! `env:{lane}` catalog rows are re-resolved by *every* process from *its own*
//! environment (#1681 D3's compatibility window). Two daemons with different
//! env therefore flip the same row back and forth, each bumping its revision,
//! and until now nothing anywhere reviewed that. Binding an alias to such a row
//! makes the flip visible and refusable: the plan records the deployment
//! revision it read, and apply refuses on `deployment_revision_drift` if
//! another process moved it in between. The flip-flop is not prevented here —
//! that is #1685's cutover — but it can no longer happen underneath a reviewed
//! routing decision without saying so.
//!
//! # What this module deliberately does not do
//!
//! - **It never writes a deployment.** Alias governance binds names to catalog
//!   rows; a governance path that could also mint the row it governs is not a
//!   governance path. A lane with no imported row is an advisory, not a
//!   proposal to import it.
//! - **It never reads a path the plan names.** Apply re-reads its preconditions
//!   from the database, and the plan artifact is an operator-readable record
//!   rather than an authority. There is no source-descriptor grammar here at
//!   all, which is why there is no `PlanSourceDigests` equivalent.
//! - **It never prints an endpoint.** The rendered view carries the endpoint's
//!   *authority* (host and port) through `memcore`'s shared parser and never
//!   its path or query — the import already refuses a credential-bearing URL,
//!   and printing less than was stored costs nothing and cannot regress.

use std::path::PathBuf;

use memcore::catalog::alias_plan::{
    alias_plan_digest, AliasAction, AliasPlanBindings, AliasRevisionBinding, BoundAliasPlan,
    DeploymentRevisionBinding,
};
use memcore::catalog::alias_policy::alias_set_policy_revision;
use memcore::catalog::endpoint::endpoint_authority;
use memcore::catalog::{ModelDeployment, ALIAS_STATUS_ACTIVE};
use memcore::db::model_catalog::{
    get_model_deployment, list_model_alias_bindings, list_model_aliases,
};
use memcore::store::model_alias_plan::AliasApplyReport;
use memcore::MemoryStore;
use serde::{Deserialize, Serialize};
use tachi_bootstrap::cli::BrokerAction;
use tachi_llm::{env_deployment_id, ENV_CHAT_LANES, ENV_EMBEDDING_LANE};

use super::{open_cli_store, open_cli_store_read_only};

/// Schema tag of the on-disk plan artifact. Checked as an exact literal at
/// apply: a plan from a build that meant something else by these fields is
/// refused rather than reinterpreted.
const PLAN_SCHEMA: &str = "tachi.model-alias-plan.v1";

/// The alias name a lane's env-imported deployment is offered under.
///
/// One function so plan time and every future reader agree; the lane name is
/// passed in rather than parsed back out of a `deployment_id`, for the reason
/// `catalog_import` states about re-deriving structure from a string you were
/// just handed.
fn lane_alias_name(lane: &str) -> String {
    format!("lane.{lane}")
}

/// What `plan` writes and `apply` reads.
///
/// Carries no rendered prose, for the reason `vault_cli::reconcile`'s artifact
/// does not: a summary stored beside the digest is a lie surface — edit only
/// the description and the digest still verifies while the operator approves a
/// paragraph the plan does not implement.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AliasPlanArtifact {
    schema: String,
    generated_at: String,
    plan_digest: String,
    bound: BoundAliasPlan,
}

/// A note for the operator that is not an action. Advisories never enter the
/// artifact: they describe what the plan chose *not* to do, which is exactly
/// the part that must not be able to masquerade as approved content.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Advisory {
    code: String,
    subject: String,
    detail: String,
}

struct PlanOutcome {
    plan: BoundAliasPlan,
    advisories: Vec<Advisory>,
}

// ─── CLI entry ──────────────────────────────────────────────────────────────

pub(super) fn run_broker_command(
    global_db_path: &PathBuf,
    action: BrokerAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        BrokerAction::Aliases { json } => {
            let store = open_cli_store_read_only(global_db_path)?;
            let rendered = render_alias_set(&store)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rendered)?);
            } else {
                print!("{}", render_alias_set_text(&rendered));
            }
        }
        BrokerAction::Plan { json, out } => {
            let store = open_cli_store_read_only(global_db_path)?;
            let outcome = build_plan(&store)?;
            let artifact = AliasPlanArtifact {
                schema: PLAN_SCHEMA.to_string(),
                generated_at: chrono::Utc::now().to_rfc3339(),
                plan_digest: alias_plan_digest(&outcome.plan),
                bound: outcome.plan,
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&artifact)?);
            } else {
                print!("{}", render_plan(&artifact, &outcome.advisories));
            }
            if let Some(path) = out {
                std::fs::write(&path, serde_json::to_string_pretty(&artifact)?)?;
                println!("wrote alias plan to {}", path.display());
            }
        }
        BrokerAction::Apply { plan, json } => {
            let raw = std::fs::read_to_string(&plan)?;
            let artifact: AliasPlanArtifact = serde_json::from_str(&raw)?;
            if artifact.schema != PLAN_SCHEMA {
                return Err(format!(
                    "plan schema '{}' is not '{PLAN_SCHEMA}'; refusing to guess what it meant",
                    artifact.schema
                )
                .into());
            }
            // Re-render from the digested content before applying: what the
            // operator sees at apply time is the plan itself, never a stored
            // description of it.
            if !json {
                print!("{}", render_plan(&artifact, &[]));
            }
            let mut store = open_cli_store(global_db_path)?;
            let report = store.apply_model_alias_plan(&artifact.bound, &artifact.plan_digest)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", render_apply(&report));
            }
        }
    }
    Ok(())
}

// ─── Stage 1-2: discover and plan ───────────────────────────────────────────

/// Propose one alias per env-imported lane, bound to that lane's catalog row.
///
/// The proposal is deliberately narrow. It does not invent aliases for
/// deployments the env chains did not produce, it does not re-prioritize a
/// binding an operator set by hand, and it does not retire anything it did not
/// itself propose — the plan says what the *env compatibility window* implies
/// and leaves every other alias alone. Anything it cannot express as an action
/// becomes an advisory.
fn build_plan(store: &MemoryStore) -> Result<PlanOutcome, Box<dyn std::error::Error>> {
    let conn = store.connection();
    let aliases = list_model_aliases(conn)?;
    let bindings = list_model_alias_bindings(conn)?;

    let mut advisories = Vec::new();
    let mut actions: Vec<AliasAction> = Vec::new();
    let mut alias_bindings: Vec<AliasRevisionBinding> = Vec::new();
    let mut deployment_bindings: Vec<DeploymentRevisionBinding> = Vec::new();

    let lanes = ENV_CHAT_LANES
        .iter()
        .copied()
        .chain(std::iter::once(ENV_EMBEDDING_LANE));

    for lane in lanes {
        let alias_name = lane_alias_name(lane);
        let deployment_id = env_deployment_id(lane);
        let deployment = get_model_deployment(conn, &deployment_id)?;

        let Some(deployment) = deployment else {
            advisories.push(Advisory {
                code: "lane_not_imported".to_string(),
                subject: deployment_id,
                detail: "the env chains produced no catalog row for this lane; alias governance \
                         binds names to rows and never mints one"
                    .to_string(),
            });
            continue;
        };
        if deployment.status != memcore::catalog::DEPLOYMENT_STATUS_ACTIVE {
            advisories.push(Advisory {
                code: "lane_retired".to_string(),
                subject: deployment.deployment_id.clone(),
                detail: format!(
                    "the catalog row is '{}', so it is not a candidate to bind",
                    deployment.status
                ),
            });
            continue;
        }

        let existing = aliases.iter().find(|alias| alias.alias_name == alias_name);
        let already_bound = bindings.iter().any(|binding| {
            binding.alias_name == alias_name
                && binding.deployment_id == deployment.deployment_id
                && !binding.retired
        });

        // Bind the objects this lane's actions will touch — even when no
        // action is produced, so that the plan's preconditions cover the state
        // it *read* and not merely the state it means to change. A plan that
        // decided "nothing to do here" decided it against a revision, and that
        // revision moving is drift like any other.
        alias_bindings.push(AliasRevisionBinding {
            alias_name: alias_name.clone(),
            revision: existing.map(|alias| alias.revision),
        });
        deployment_bindings.push(DeploymentRevisionBinding {
            deployment_id: deployment.deployment_id.clone(),
            revision: Some(deployment.revision),
        });

        match existing {
            None => {
                actions.push(AliasAction::DeclareAlias {
                    alias_name: alias_name.clone(),
                    required_capabilities: lane_required_capabilities(&deployment),
                    constraints: "{}".to_string(),
                    source_refs: vec![format!("env_lane:{lane}")],
                });
            }
            Some(alias) if alias.status != ALIAS_STATUS_ACTIVE => {
                advisories.push(Advisory {
                    code: "alias_retired".to_string(),
                    subject: alias_name.clone(),
                    detail: "an operator retired this alias; the plan leaves it retired rather \
                             than reviving a name somebody deliberately took out of service"
                        .to_string(),
                });
                continue;
            }
            Some(_) => {}
        }

        if already_bound {
            advisories.push(Advisory {
                code: "already_bound".to_string(),
                subject: format!("{alias_name} -> {}", deployment.deployment_id),
                detail: format!(
                    "bound and current at revision {} ({} via {})",
                    deployment.revision,
                    deployment.provider_model_id,
                    redacted_endpoint(&deployment)
                ),
            });
        } else {
            actions.push(AliasAction::BindDeployment {
                alias_name,
                deployment_id: deployment.deployment_id.clone(),
                priority: 0,
            });
        }
    }

    advisories
        .sort_by(|left, right| (&left.code, &left.subject).cmp(&(&right.code, &right.subject)));

    Ok(PlanOutcome {
        plan: BoundAliasPlan {
            bindings: AliasPlanBindings {
                policy_revision: alias_set_policy_revision(&aliases, &bindings),
                aliases: alias_bindings,
                deployments: deployment_bindings,
            },
            actions,
        },
        advisories,
    })
}

/// What an alias requires of a candidate, taken from the row it is being bound
/// to.
///
/// Declared rather than left empty because the requirement is what makes the
/// alias meaningful once a *second* deployment is bound to it: a chat alias
/// must not silently resolve to an embeddings-only deployment somebody added
/// later.
fn lane_required_capabilities(deployment: &ModelDeployment) -> String {
    let capabilities = &deployment.capabilities;
    serde_json::json!({
        "chat": capabilities.chat,
        "embeddings": capabilities.embeddings.is_some(),
    })
    .to_string()
}

/// Host and port only — never the path or query a stored endpoint carries.
fn redacted_endpoint(deployment: &ModelDeployment) -> String {
    deployment
        .endpoint_ref
        .as_deref()
        .map(|endpoint| endpoint_authority(endpoint).to_string())
        .unwrap_or_else(|| "(no endpoint)".to_string())
}

// ─── Rendering ──────────────────────────────────────────────────────────────

/// The alias set as an operator sees it: names, bindings, revisions, and
/// whether each row's stamp still matches the recomputed set revision.
#[derive(Debug, Clone, Serialize)]
struct RenderedAliasSet {
    policy_revision: String,
    aliases: Vec<RenderedAlias>,
}

#[derive(Debug, Clone, Serialize)]
struct RenderedAlias {
    alias_name: String,
    status: String,
    revision: i64,
    /// `false` when the row's `policy_digest` is not the recomputed set
    /// revision — the signature of a write that did not come through
    /// plan/apply.
    stamp_matches: bool,
    bindings: Vec<RenderedBinding>,
}

#[derive(Debug, Clone, Serialize)]
struct RenderedBinding {
    deployment_id: String,
    priority: i64,
    retired: bool,
    /// `None` when the catalog no longer has the row this alias names.
    deployment_revision: Option<i64>,
}

fn render_alias_set(store: &MemoryStore) -> Result<RenderedAliasSet, Box<dyn std::error::Error>> {
    let conn = store.connection();
    let aliases = list_model_aliases(conn)?;
    let bindings = list_model_alias_bindings(conn)?;
    let policy_revision = alias_set_policy_revision(&aliases, &bindings);

    let mut rendered = Vec::new();
    for alias in &aliases {
        let mut alias_bindings = Vec::new();
        for binding in bindings
            .iter()
            .filter(|binding| binding.alias_name == alias.alias_name)
        {
            alias_bindings.push(RenderedBinding {
                deployment_id: binding.deployment_id.clone(),
                priority: binding.priority,
                retired: binding.retired,
                deployment_revision: get_model_deployment(conn, &binding.deployment_id)?
                    .map(|row| row.revision),
            });
        }
        rendered.push(RenderedAlias {
            alias_name: alias.alias_name.clone(),
            status: alias.status.clone(),
            revision: alias.revision,
            stamp_matches: alias.status != ALIAS_STATUS_ACTIVE
                || alias.policy_digest.as_deref() == Some(policy_revision.as_str()),
            bindings: alias_bindings,
        });
    }
    Ok(RenderedAliasSet {
        policy_revision,
        aliases: rendered,
    })
}

fn render_alias_set_text(set: &RenderedAliasSet) -> String {
    let mut out = String::new();
    out.push_str(&format!("POLICY\t{}\n", set.policy_revision));
    if set.aliases.is_empty() {
        out.push_str("(no aliases recorded)\n");
    }
    for alias in &set.aliases {
        out.push_str(&format!(
            "ALIAS\t{}\t{}\trevision={}{}\n",
            alias.alias_name,
            alias.status,
            alias.revision,
            if alias.stamp_matches {
                ""
            } else {
                "\tUNREVIEWED (policy_digest is not the current set revision)"
            }
        ));
        for binding in &alias.bindings {
            out.push_str(&format!(
                "  BINDING\t{}\tpriority={}\t{}\t{}\n",
                binding.deployment_id,
                binding.priority,
                if binding.retired { "retired" } else { "active" },
                match binding.deployment_revision {
                    Some(revision) => format!("deployment_revision={revision}"),
                    None => "deployment=absent".to_string(),
                }
            ));
        }
    }
    out
}

fn render_plan(artifact: &AliasPlanArtifact, advisories: &[Advisory]) -> String {
    let mut out = String::new();
    out.push_str(&format!("PLAN\t{}\n", artifact.plan_digest));
    out.push_str(&format!(
        "BOUND\tpolicy\t{}\n",
        artifact.bound.bindings.policy_revision
    ));
    if artifact.bound.actions.is_empty() {
        out.push_str("(no alias actions; the recorded alias set already matches the catalog)\n");
    }
    for action in &artifact.bound.actions {
        out.push_str(&render_action(action));
    }
    for binding in &artifact.bound.bindings.aliases {
        out.push_str(&format!(
            "BOUND\talias\t{}\t{}\n",
            binding.alias_name,
            describe_revision(binding.revision)
        ));
    }
    for binding in &artifact.bound.bindings.deployments {
        out.push_str(&format!(
            "BOUND\tdeployment\t{}\t{}\n",
            binding.deployment_id,
            describe_revision(binding.revision)
        ));
    }
    for advisory in advisories {
        out.push_str(&format!(
            "ADVISORY\t{}\t{}\t{}\n",
            advisory.code, advisory.subject, advisory.detail
        ));
    }
    out
}

fn describe_revision(revision: Option<i64>) -> String {
    match revision {
        Some(revision) => format!("revision={revision}"),
        None => "absent".to_string(),
    }
}

fn render_action(action: &AliasAction) -> String {
    match action {
        AliasAction::DeclareAlias {
            alias_name,
            required_capabilities,
            ..
        } => format!("ACTION\tdeclare_alias\t{alias_name}\t{required_capabilities}\n"),
        AliasAction::BindDeployment {
            alias_name,
            deployment_id,
            priority,
        } => format!("ACTION\tbind\t{alias_name}\t{deployment_id}\tpriority={priority}\n"),
        AliasAction::RetireBinding {
            alias_name,
            deployment_id,
        } => format!("ACTION\tretire_binding\t{alias_name}\t{deployment_id}\n"),
        AliasAction::RetireAlias { alias_name } => {
            format!("ACTION\tretire_alias\t{alias_name}\n")
        }
    }
}

fn render_apply(report: &AliasApplyReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "APPLIED\t{}\tchanged={}\n",
        report.plan_digest, report.changed
    ));
    out.push_str(&format!("POLICY\t{}\n", report.policy_revision));
    for alias in &report.aliases_declared {
        out.push_str(&format!(
            "DECLARED\talias\t{}\trevision={}\n",
            alias.alias_name, alias.revision
        ));
    }
    for alias in &report.aliases_retired {
        out.push_str(&format!(
            "RETIRED\talias\t{}\trevision={}\n",
            alias.alias_name, alias.revision
        ));
    }
    for binding in &report.bindings_bound {
        out.push_str(&format!(
            "BOUND\t{}\t{}\n",
            binding.alias_name, binding.deployment_id
        ));
    }
    for binding in &report.bindings_retired {
        out.push_str(&format!(
            "RETIRED\tbinding\t{}\t{}\n",
            binding.alias_name, binding.deployment_id
        ));
    }
    out
}

#[cfg(test)]
mod tests;
