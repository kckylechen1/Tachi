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
//! Only the **status** path carries this projection, bound to EXACTLY the
//! refs the cycle-status reader resolved (flow-record refs first, caller
//! refs second — the pre-existing status resolution) under the existing
//! status auth policy with conservative private visibility
//! (`sees_private=false`). The projection renders only the requested
//! work items — the resolved issue/PR identities, plus an explicitly
//! linked PR's owning issue item when the CurrentTruth view's ADMITTED
//! current link set proves the link — never the whole repository: an
//! unrelated same-repo issue must not surface because a neighbor was
//! asked about. Item matching is typed [`WorkKey`] equality (the model's
//! own identity), never string/number matching. The project-scoped
//! board/brief views are deliberately NOT wired: no canonical
//! flow/project ownership authority exists, and wiring them requires a
//! future canonical ownership design (a remaining #1693 gap this slice
//! does not close).
//!
//! Partial by construction (CurrentTruth-only vertical): work claims,
//! run receipts, exec envs, verification, adjudication, and delivery
//! sources render typed `unavailable` in the section — never empty or
//! healthy — and this slice does not claim claims/run/verification truth.

use super::*;
use tachi_params::current_truth::consumer::{self, CallerAuthorizationV1, CurrentTruthViewV1};
use tachi_params::current_truth::types::{PredicateV1, ReductionStatusV1};
use tachi_params::work_read_model::{
    board_view, brief_view, project, status_view, CurrentTruthFactsV1, ProjectionOptions,
    SectionState, SourceFacts, SourceKind, SourceSnapshot, WorkKey, WorkProjectionIndex,
    WorkReadModelV1,
};

/// The exact requested work identity for one status read: the resolved
/// issue/PR refs as typed [`WorkKey`]s, plus the deduped repo spelling
/// needed to read the CurrentTruth views.
#[derive(Debug, Default)]
pub(crate) struct StatusBoundWork {
    /// Sorted, deduped, lowercased repo spellings for view reads.
    pub repos: Vec<String>,
    /// Exact requested issue identities.
    pub issues: Vec<WorkKey>,
    /// Exact requested PR identities.
    pub pull_requests: Vec<WorkKey>,
}

impl StatusBoundWork {
    /// The `owner/repo#N` form of every requested ref, for the response
    /// scope echo.
    fn ref_tokens(&self) -> Vec<String> {
        let mut tokens: Vec<String> = self
            .issues
            .iter()
            .chain(self.pull_requests.iter())
            .map(ref_token)
            .collect();
        tokens.sort();
        tokens
    }
}

/// The `owner/repo#N` spelling of a bound GitHub-object key.
fn ref_token(key: &WorkKey) -> String {
    match key {
        WorkKey::Issue { repo, number } | WorkKey::PullRequest { repo, number } => {
            format!("{repo}#{number}")
        }
        WorkKey::Dispatch(id) => format!("dispatch:{id}"),
        WorkKey::Claim(id) => format!("claim:{id}"),
    }
}

/// Bind the EXACT requested work for one status read from the refs the
/// cycle-status reader resolved. Parsing is the canonical
/// [`WorkKey::parse_issue_ref`] / `owner/repo#N` split (typed identity,
/// never string substring matching), normalized to the CurrentTruth
/// store's stable lowercased repo spelling.
pub(crate) fn status_bound_work(issue_ref: Option<&str>, pr_ref: Option<&str>) -> StatusBoundWork {
    let mut bound = StatusBoundWork::default();
    if let Some(issue_ref) = issue_ref {
        if let Some(WorkKey::Issue { repo, number }) = WorkKey::parse_issue_ref(issue_ref.trim()) {
            bound.issues.push(WorkKey::Issue {
                repo: repo.trim().to_ascii_lowercase(),
                number,
            });
        }
    }
    if let Some(pr_ref) = pr_ref {
        let trimmed = pr_ref.trim();
        if let Some((repo, number)) = trimmed.rsplit_once('#') {
            if repo.matches('/').count() == 1 {
                if let Ok(number) = number.parse::<u64>() {
                    bound.pull_requests.push(WorkKey::PullRequest {
                        repo: repo.trim().to_ascii_lowercase(),
                        number,
                    });
                }
            }
        }
    }
    let mut repos: Vec<String> = bound
        .issues
        .iter()
        .chain(bound.pull_requests.iter())
        .map(|key| match key {
            WorkKey::Issue { repo, .. } | WorkKey::PullRequest { repo, .. } => repo.clone(),
            _ => unreachable!("bound keys are always issue or PR identities"),
        })
        .collect();
    repos.sort();
    repos.dedup();
    bound.repos = repos;
    bound
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
    bound: &StatusBoundWork,
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
    for repo in &bound.repos {
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

    let mut section = unavailable_section(bound, read_at);
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
    // Exact-identity filter BEFORE rendering: only the requested work
    // items (plus an explicitly linked PR's owning issue item, proven by
    // the view's ADMITTED current link set) — unrelated same-repo work
    // never surfaces because a neighbor was asked about. Blockers and
    // transition debt of kept items travel with the item, unfiltered.
    let items: Vec<&WorkReadModelV1> = model
        .items
        .iter()
        .filter(|item| item_is_requested_work(item, bound))
        .collect();
    let rendered = items
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
    // Per-requested-ref presence: an unprojected requested identity is a
    // typed unknown for that ref, never silence (the view may simply hold
    // no row for it).
    let requested_refs: Vec<Value> = bound
        .issues
        .iter()
        .map(|key| requested_ref_row(key, "issue", &items))
        .chain(
            bound
                .pull_requests
                .iter()
                .map(|key| requested_ref_row(key, "pull_request", &items)),
        )
        .collect();
    section["scope"]["requested_refs"] = Value::Array(requested_refs);
    section["available"] = json!(true);
    section["items"] = Value::Array(rendered);
    section["health"] = json!({
        "visible_work_count": items.len(),
        "conflicted_count": items
            .iter()
            .filter(|item| matches!(&item.github, SectionState::Available(section) if section.conflicted))
            .count(),
        "blocked_count": items.iter().filter(|item| !item.blockers.is_empty()).count(),
    });
    section
}

/// Whether one projected item belongs to the requested work: an exact
/// requested issue/PR identity, or an issue item whose ADMITTED current
/// link set explicitly claims a requested PR (that is where a linked
/// PR's work truth lives — linked, non-orphan PRs have no item of their
/// own). Matching is typed key equality plus exact
/// (repo, canonical object-token) equality — the consumer view renders a
/// link set as BARE object tokens (`pull_request:N`) scoped to the issue
/// subject's repo — never string substring/number matching.
fn item_is_requested_work(item: &WorkReadModelV1, bound: &StatusBoundWork) -> bool {
    if bound.issues.contains(&item.work_id) || bound.pull_requests.contains(&item.work_id) {
        return true;
    }
    bound
        .pull_requests
        .iter()
        .any(|key| issue_item_claims_pr(item, key))
}

/// Whether this issue item's ADMITTED current implementation-PR link set
/// explicitly claims the requested PR key: the linked object tokens are
/// bare (`pull_request:N`) and share the issue subject's repo.
fn issue_item_claims_pr(item: &WorkReadModelV1, key: &WorkKey) -> bool {
    let WorkKey::Issue { repo, .. } = &item.work_id else {
        return false;
    };
    let WorkKey::PullRequest {
        repo: pr_repo,
        number,
    } = key
    else {
        return false;
    };
    pr_repo == repo
        && admitted_link_object_tokens(item)
            .iter()
            .any(|token| *token == format!("pull_request:{number}"))
}

/// The item's ADMITTED current implementation-PR link set as canonical
/// BARE object tokens (`pull_request:N`, scoped to the issue subject's
/// repo), from the CurrentTruth consumer view's `implementation_pr_linked`
/// row — existing admitted link evidence, never guessed association.
fn admitted_link_object_tokens(item: &WorkReadModelV1) -> Vec<String> {
    let SectionState::Available(section) = &item.github else {
        return Vec::new();
    };
    let Some(subject) = &section.subject else {
        return Vec::new();
    };
    subject
        .predicates
        .iter()
        .find(|row| {
            row.predicate == PredicateV1::ImplementationPrLinked
                && row.status == ReductionStatusV1::Current
        })
        .map(|row| {
            row.value_token
                .split(',')
                .filter(|token| !token.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// One requested ref's presence row: `projected=false` is the typed
/// unknown for a ref the views hold no row for (and no admitted link
/// claims), never an implied absence of the work itself.
fn requested_ref_row(key: &WorkKey, kind: &str, items: &[&WorkReadModelV1]) -> Value {
    let projected = items.iter().any(|item| match key {
        WorkKey::PullRequest { .. } => item.work_id == *key || issue_item_claims_pr(item, key),
        _ => item.work_id == *key,
    });
    json!({
        "ref": ref_token(key),
        "kind": kind,
        "projected": projected,
    })
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

fn unavailable_section(bound: &StatusBoundWork, read_at: &str) -> Value {
    json!({
        "authority": "work_read_model_v1",
        "available": false,
        "projection": "current_truth_only_partial",
        "authorization": { "sees_private": false },
        "read_at": read_at,
        "scope": {
            "binding": "explicit_status_refs",
            "repos": bound.repos,
            "refs": bound.ref_tokens(),
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
            "conflicted_count": 0,
            "blocked_count": 0,
        },
    })
}

fn unavailable_source() -> Value {
    json!({ "state": "unavailable", "reason": "task_phase1_current_truth_only" })
}
