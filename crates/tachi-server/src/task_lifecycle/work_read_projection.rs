//! #1693 Task consumer: the CurrentTruth → WorkReadModel projection
//! behind the `tachi_task` status lifecycle view.
//!
//! The mint recipe is the existing production one from
//! `gh_ops/current_truth_refresh.rs` `consume_refresh` (lines 615-667):
//! `consumer::read_view` under `CallerAuthorizationV1 { sees_private: false }`
//! → `current-truth-view-{digest}` revision → `SourceSnapshot` →
//! `WorkProjectionIndex` → `project` → `board_view`/`status_view`/`brief_view`.
//! This module owns no store handle, adds no schema, and performs no refresh
//! or network side effect: it reads the CurrentTruth authority through the
//! server's existing store accessor only, and a repository with no recorded
//! posture stays typed unknown (`no_refresh_posture`) — never empty/healthy.
//!
//! # Scope (owner adjudication 2026-09-25)
//!
//! Only the **status** path carries this projection, bound to exactly the
//! refs the cycle-status reader resolved (flow-record refs first, caller
//! refs second — the pre-existing status resolution) under the existing
//! status auth policy with conservative private visibility
//! (`sees_private=false`). The project-scoped board/brief views are
//! deliberately NOT wired: no canonical flow/project ownership authority
//! exists (flow records, WorkClaims, and orchestrator rows are global and
//! project-free; kanban cards are dispatch-keyed caller-supplied data), so
//! consuming a caller-named flow's repos there would let one project read
//! another's work. Wiring board/brief requires a future canonical
//! ownership design — the remaining #1693 gap this slice does not close.
//!
//! Partial by construction (CurrentTruth-only vertical): work claims,
//! run receipts, exec envs, verification, adjudication, and delivery
//! sources render typed `unavailable` in the section — never empty or
//! healthy — and this slice does not claim claims/run/verification truth.

use super::*;
use tachi_params::current_truth::consumer::{self, CallerAuthorizationV1, CurrentTruthViewV1};
use tachi_params::work_read_model::{
    board_view, brief_view, project, status_view, CurrentTruthFactsV1, ProjectionOptions,
    SourceFacts, SourceKind, SourceSnapshot, WorkProjectionIndex,
};

/// The repos bound by the explicit status refs as resolved by the cycle
/// status reader: sorted, deduped, lowercased to the CurrentTruth store's
/// stable spelling (GitHub owner/repository identity is ASCII
/// case-insensitive; the refresh authority records the lowercased form —
/// one stable spelling prevents posture and subject history from
/// splitting across caller-selected casing).
pub(crate) fn status_bound_repos(issue_ref: Option<&str>, pr_ref: Option<&str>) -> Vec<String> {
    let mut repos: Vec<String> = [issue_ref, pr_ref]
        .into_iter()
        .flatten()
        .filter_map(|raw| raw.split('#').next().map(normalize_bound_repo))
        .collect();
    repos.sort();
    repos.dedup();
    repos
}

fn normalize_bound_repo(repo: &str) -> String {
    repo.trim().to_ascii_lowercase()
}

/// The `work_read_model` section for the status lifecycle response. One
/// `read_at` timestamp per call keeps a single call's views coherent;
/// work tokens and revision fingerprints are deterministic functions of
/// the consumed source revisions, so the same input revision renders the
/// same token/revision across the per-item views and repeated reads
/// (`read_at` is reader time, never freshness — posture carries
/// freshness).
pub(crate) fn task_work_read_section(
    server: &MemoryServer,
    repos: &[String],
    read_at: &str,
) -> Value {
    // Conservative existing policy: the public Task facade has no
    // private-read grant, exactly like the refresh boundary's
    // `sees_private=false` mint.
    let authorization = CallerAuthorizationV1 {
        sees_private: false,
    };
    let mut github_sources = Vec::new();
    let mut minted: Vec<(String, CurrentTruthViewV1, String)> = Vec::new();
    for repo in repos {
        // Store failures are content-free at this public boundary (same
        // law as the refresh handler): the error shape must not reveal
        // whether inaccessible history exists.
        let view = server
            .with_current_truth_store(|store| {
                Ok(
                    consumer::read_view(store, repo, authorization).map_err(|error| match error {
                        consumer::ConsumerViewError::NoPosture(repo) => {
                            (repo, "no_refresh_posture".to_string())
                        }
                        consumer::ConsumerViewError::Store(_) => {
                            (repo.to_string(), "current_truth_unavailable".to_string())
                        }
                    }),
                )
            })
            .unwrap_or_else(|_| Err((repo.to_string(), "current_truth_unavailable".to_string())));
        match view {
            Ok(view) => {
                let revision = view_revision(&view);
                github_sources.push(json!({
                    "repo": repo,
                    "available": true,
                    "fresh": view.posture.fresh,
                    "revision": revision,
                }));
                minted.push((repo.clone(), view, revision));
            }
            Err((repo, reason)) => {
                // Typed unknown, never fresh and never empty/healthy.
                github_sources.push(json!({
                    "repo": repo,
                    "available": false,
                    "state": "unknown",
                    "reason": reason,
                }));
            }
        }
    }

    let mut section = unavailable_section(read_at, repos);
    section["sources"]["github"] = Value::Array(github_sources);
    if minted.is_empty() {
        // No bound repo produced a usable view: every repo row above
        // already carries its typed unknown reason.
        return section;
    }

    let mut index = WorkProjectionIndex::new();
    for (repo, view, revision) in minted {
        let snapshot = SourceSnapshot::new(
            SourceKind::CurrentTruth { repo },
            revision,
            read_at,
            SourceFacts::CurrentTruth(Box::new(CurrentTruthFactsV1 {
                view,
                minted_authorization: authorization,
            })),
        );
        // Fail-closed: an unusable snapshot degrades the whole section to
        // typed unavailable rather than projecting partial content.
        if snapshot
            .and_then(|snapshot| index.apply(snapshot).map(|_| ()))
            .is_err()
        {
            section["scope"]["reason"] = json!("projection_snapshot_invalid");
            return section;
        }
    }
    let Ok(options) = ProjectionOptions::try_new(read_at) else {
        section["scope"]["reason"] = json!("projection_read_at_invalid");
        return section;
    };
    let model = project(&index, &options);
    let items = model
        .items
        .iter()
        .map(|item| {
            let board = board_view(item);
            let status = status_view(item);
            let brief = brief_view(item);
            json!({
                "work_token": item.work_token(),
                "revision": item.revision,
                "read_at": item.read_at,
                "board": {
                    "column": board.column,
                    "blocker_count": board.blocker_count,
                    "top_action": board.top_action,
                },
                "status": {
                    "github": status.github,
                    "claim": status.claim,
                    "run": status.run,
                    "adjudication": status.adjudication,
                    "delivery": status.delivery,
                    "success_shaped": status.success_shaped,
                },
                "brief": {
                    "sections": Value::Object(
                        brief
                            .sections
                            .into_iter()
                            .map(|(key, value)| (key, Value::String(value)))
                            .collect::<serde_json::Map<String, Value>>(),
                    ),
                },
            })
        })
        .collect::<Vec<_>>();
    section["available"] = json!(true);
    section["items"] = Value::Array(items);
    section["health"] = json!({
        "visible_work_count": model.health.visible_work_count,
        "orphaned_revert_debt_count": model.health.orphaned_revert_debt_count,
        "unbound_verification_count": model.health.unbound_verification_count,
        "conflicted_count": model.health.conflicted_count,
        "blocked_count": model.health.blocked_count,
        "refresh_debt_count": model.health.refresh_debt_count,
    });
    section
}

/// The `current-truth-view-{digest}` revision token from the production
/// mint recipe (`consume_refresh`): the canonical-JSON digest of the whole
/// consumer view, so the same view revision always yields the same token.
fn view_revision(view: &CurrentTruthViewV1) -> String {
    let digest = memcore::canonical_digest::canonical_json_digest_hex(
        &serde_json::to_value(view).unwrap_or_else(|_| json!({})),
    );
    format!("current-truth-view-{digest}")
}

fn unavailable_section(read_at: &str, repos: &[String]) -> Value {
    json!({
        "authority": "work_read_model_v1",
        "available": false,
        "projection": "current_truth_only_partial",
        "authorization": { "sees_private": false },
        "read_at": read_at,
        "scope": {
            "binding": "explicit_status_refs",
            "repos": repos,
        },
        "sources": {
            "github": [],
            "work_claims": unavailable_source(),
            "run_receipts": unavailable_source(),
            "exec_envs": unavailable_source(),
            "verification": unavailable_source(),
            "adjudication": unavailable_source(),
            "delivery": unavailable_source(),
        },
        "items": [],
        "health": {
            "visible_work_count": 0,
            "orphaned_revert_debt_count": 0,
            "unbound_verification_count": 0,
            "conflicted_count": 0,
            "blocked_count": 0,
            "refresh_debt_count": 0,
        },
    })
}

fn unavailable_source() -> Value {
    json!({ "state": "unavailable", "reason": "task_phase1_current_truth_only" })
}
