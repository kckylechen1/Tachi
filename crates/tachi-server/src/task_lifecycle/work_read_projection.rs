//! Task read adapters feed the shared WorkReadModel projector and views.
//!
//! Legacy exact-ref status reads retain the public CurrentTruth projection.
//! Its retired writer never supplies live freshness: recorded fresh posture
//! is rendered frozen. Canonical project views instead consume explicit local
//! claims, after current admission, holder, project and lease checks. Missing
//! source adapters remain typed unavailable; selectors cannot widen authority.

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
            Ok(mut view) => {
                // The revision is the recorded source view's digest,
                // computed BEFORE the degradation below on purpose: it is
                // source identity, and the degradation is a projection-
                // time overlay that must not alter it (repeated reads of
                // unchanged recorded data keep a stable revision).
                let revision = view_revision(&view);
                // The only production writer of this posture (the retired
                // `gh_ops` refresh) can no longer back a live `fresh`
                // claim: a recorded true is a frozen pre-retirement fact.
                // Degrade it here so neither the surfaced source row nor
                // the projected model presents it as current (#1696);
                // the frozen marker keeps when freshness was last real.
                // The view's derived open_actions remain those of the
                // frozen snapshot: read-only reconciliation keeps them as
                // historical guidance, while the degraded posture's
                // source blocker leads next_actions/top_action.
                let frozen_while_writer_alive = view.posture.fresh;
                if frozen_while_writer_alive {
                    view.posture.fresh = false;
                }
                let mut row = json!({
                    "repo": repo,
                    "available": true,
                    "fresh": view.posture.fresh,
                    "revision": revision,
                });
                if frozen_while_writer_alive {
                    row["fresh_state"] = json!("retired_writer_frozen");
                    row["recorded_fresh_at"] = json!(view.posture.last_fresh_at);
                }
                github_sources.push(row);
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
        .copied()
        .map(render_work_item)
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

/// Project scope is a server-admitted local context, never a caller ref/path.
/// Every view consumes the same whole-project claim snapshot and revision;
/// selectors only reduce the rendered items after that snapshot is admitted.
pub(super) const MAX_SCOPED_CLAIMS: usize = 64;
pub(super) const MAX_SCOPED_RECEIPTS: usize = 8;

fn selected_claim(claim: &memcore::SessionClaim, params: &TachiTaskParams) -> bool {
    params
        .flow_id
        .as_deref()
        .is_none_or(|id| claim.flow_id.as_deref() == Some(id))
        && params
            .issue_ref
            .as_deref()
            .is_none_or(|id| claim.issue_ref.as_deref() == Some(id))
        && params.pr_ref.is_none()
}

pub(crate) fn scoped_work_read_section(
    server: &MemoryServer,
    params: &TachiTaskParams,
) -> Option<Value> {
    use crate::claims_ops::project_ownership::{LocalProject, UNAVAILABLE};
    use memcore::db::session_claims;
    use tachi_params::current_truth::types::VisibilityClassV1;
    use tachi_params::work_read_model::{ClaimModeV1, ClaimStateV1, WorkClaimFactV1};

    let binding = LocalProject::capture(server)?;
    let read_at = Utc::now().to_rfc3339();
    let mut section = json!({
        "authority":"work_read_model_v1", "projection":"project_claims_partial", "available":false,
        "read_at":read_at, "scope":{"binding":"local_canonical_project","project":binding.project},
        "sources":{"work_claims":{"state":"unavailable","reason":UNAVAILABLE},
            "github":{"state":"unavailable","reason":"project_current_truth_not_integrated","fresh":false,"writer":"retired"},
            "run_receipts":{"state":"unavailable"},"exec_envs":{"state":"unavailable"},
            "verification":{"state":"unavailable"},"adjudication":{"state":"unavailable"},"delivery":{"state":"unavailable"}},
        "items":[], "flows":[], "health":{"visible_work_count":0},
    });
    if params
        .project
        .as_deref()
        .is_some_and(|p| p.trim() != binding.project)
    {
        return Some(section);
    }
    let admitted = binding.with_current(server, |conn| {
        if !session_claims::has_current_project_claim(
            conn,
            &binding.project,
            &binding.identity,
            &binding.connection_id,
            &read_at,
        )? {
            return Err(memcore::MemoryError::InvalidArg(UNAVAILABLE.into()));
        }
        // SQL limits and selectors precede all receipt opens. Each receipt
        // has a 1 MiB limit, read once for metadata and once for its payload.
        let claims = session_claims::list_project_work_claims_bounded(
            conn,
            &binding.project,
            MAX_SCOPED_CLAIMS + 1,
        )?;
        if claims.len() > MAX_SCOPED_CLAIMS {
            return Ok(Err("project_claim_limit"));
        }
        let selected = claims
            .iter()
            .filter(|claim| selected_claim(claim, params))
            .collect::<Vec<_>>();
        if selected
            .iter()
            .filter(|claim| claim.flow_id.is_some())
            .count()
            > MAX_SCOPED_RECEIPTS
        {
            return Ok(Err("project_receipt_limit"));
        }
        let flows = selected
            .into_iter()
            .map(|claim| {
                let status = claim
                    .flow_id
                    .as_ref()
                    .and_then(|_| super::ownership::read_owned_status(&binding, claim).ok());
                (claim.claim_id.clone(), status)
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        Ok(Ok((claims, flows)))
    });
    let (claims, flows) = match admitted {
        Ok(Ok(facts)) => facts,
        Ok(Err(reason)) => {
            section["sources"]["work_claims"]["reason"] = json!(reason);
            section["health"]["bounded"] = json!(true);
            section["health"]["incomplete"] = json!(true);
            section["scope"]["limits"] = json!({"claims":MAX_SCOPED_CLAIMS,"receipts":MAX_SCOPED_RECEIPTS,"receipt_bytes":1024*1024,"total_receipt_read_bytes":2*MAX_SCOPED_RECEIPTS*(1024*1024+1)});
            return Some(section);
        }
        Err(_) => return Some(section),
    };
    if params.state_filter.as_deref().is_some_and(|filter| {
        !filter.trim().is_empty() && !filter.trim().eq_ignore_ascii_case("all")
    }) {
        section["scope"]["selection"] = json!("unavailable");
        section["scope"]["reason"] = json!("run_state_filter_source_unavailable");
        return Some(section);
    }
    // Claim facts have one canonical revision independent of receipt selectors.
    let semantic = claims.iter().map(|c| json!({
        "claim_id":c.claim_id,"session_client":c.session_client,"issue_ref":c.issue_ref,"flow_id":c.flow_id,
        "dispatch_id":c.dispatch_id,"branch":c.branch,"worktree_path":c.worktree_path,"scope":c.declared_file_scope,
        "identity":c.agent_identity_id,"role":c.role,"mode":c.mode.map(|m|m.as_str()),"head":c.expected_head,
        "lease":c.lease_expires_at,"version":c.transition_version,"exec_env":c.exec_env_id,"state":c.state.as_str(),
        "orphaned_at":c.orphaned_at,"release_reason":c.release_reason,"created_at":c.created_at,
        "heartbeat_at":c.heartbeat_at,"released_at":c.released_at,
    })).collect::<Vec<_>>();
    let revision = format!(
        "project-claims-{}",
        memcore::canonical_digest::canonical_json_digest_hex(
            &json!({"project":binding.project,"claims":semantic})
        )
    );
    let facts = claims
        .iter()
        .map(|c| WorkClaimFactV1 {
            claim_id: c.claim_id.clone(),
            agent_identity_id: c.agent_identity_id.clone(),
            session_client: c.session_client.clone(),
            issue_ref: c.issue_ref.clone(),
            dispatch_id: c.dispatch_id.clone(),
            branch: c.branch.clone(),
            worktree_path: c.worktree_path.clone(),
            role: c.role.clone(),
            mode: if c.mode == Some(memcore::WorkClaimMode::Writable) {
                ClaimModeV1::Writable
            } else {
                ClaimModeV1::ReadOnly
            },
            expected_head: c.expected_head.clone(),
            lease_expires_at: c.lease_expires_at.clone(),
            transition_version: c.transition_version,
            exec_env_id: c.exec_env_id.clone(),
            state: match c.state {
                memcore::ClaimState::Active => ClaimStateV1::Active,
                memcore::ClaimState::Orphaned => ClaimStateV1::Orphaned,
                memcore::ClaimState::Released => ClaimStateV1::Released,
            },
            heartbeat_at: c.heartbeat_at.clone(),
            visibility: VisibilityClassV1::Private,
        })
        .collect();
    let mut index = WorkProjectionIndex::new();
    let source = SourceSnapshot::new(
        SourceKind::WorkClaims,
        &revision,
        &read_at,
        SourceFacts::WorkClaims(facts),
    );
    if source
        .and_then(|source| index.apply(source).map(|_| ()))
        .is_err()
    {
        return Some(section);
    }
    let mut options = ProjectionOptions::new(&read_at);
    options.authorization.sees_private = true;
    let model = project(&index, &options);
    let selected = claims
        .iter()
        .filter(|claim| selected_claim(claim, params))
        .collect::<Vec<_>>();
    let keys = selected
        .iter()
        .map(|c| {
            c.issue_ref
                .as_deref()
                .and_then(WorkKey::parse_issue_ref)
                .unwrap_or_else(|| WorkKey::Claim(c.claim_id.clone()))
        })
        .collect::<Vec<_>>();
    let items = model
        .items
        .iter()
        .filter(|item| keys.contains(&item.work_id))
        .take(params.limit.unwrap_or(100).min(100))
        .map(render_work_item)
        .collect::<Vec<_>>();
    section["available"] = json!(true);
    section["revision"] = json!(revision);
    section["sources"]["work_claims"] = json!({"state":"available","revision":revision});
    section["sources"]["work_claims"]["claims"] = json!(selected.iter().take(params.limit.unwrap_or(100).min(100)).map(|claim|json!({
        "claim_id":claim.claim_id,"state":claim.state.as_str(),"transition_version":claim.transition_version,
        "agent_identity_id":claim.agent_identity_id,"flow_id":claim.flow_id,"lease_expires_at":claim.lease_expires_at,
    })).collect::<Vec<_>>());
    section["health"]["visible_work_count"] = json!(items.len());
    section["scope"]["selection"] = json!(if selected.is_empty()
        && (params.flow_id.is_some() || params.issue_ref.is_some() || params.pr_ref.is_some())
    {
        "unavailable"
    } else {
        "available"
    });
    section["items"] = json!(items);
    section["flows"] = json!(selected.iter().take(params.limit.unwrap_or(100).min(100)).filter_map(|c| c.flow_id.as_ref().map(|id|json!({
        "flow_id":id,"claim_id":c.claim_id,"state":if flows.get(&c.claim_id).is_some_and(Option::is_some){"available"}else{"unavailable"},
        "revision":memcore::canonical_digest::canonical_json_digest_hex(&json!({"claim_id":c.claim_id,"status":flows.get(&c.claim_id)})),
    }))).collect::<Vec<_>>());
    Some(section)
}

fn render_work_item(item: &WorkReadModelV1) -> Value {
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
}
