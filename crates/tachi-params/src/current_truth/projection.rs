//! Derived open-action projection (#1696).
//!
//! A deterministic function from a [`ReductionV1`] plus the repository's
//! refresh posture to per-subject open actions. It invokes no model, executes
//! no external action, and produces data only: each action carries its
//! owner class, required authority, prerequisite refs, blockers, and the
//! source revisions it was derived from.
//!
//! # Priority order (frozen, deterministic)
//!
//! 1. `resolve_conflict` — any conflicted predicate in the subject's chain
//!    (the issue plus its currently linked PRs) blocks success-shaped
//!    projection.
//! 2. `refresh_unavailable_source` — the repository's refresh posture is
//!    unavailable; nothing downstream may present as fresh.
//! 3. `repair_revert_or_reopen` — a merge revert or issue reopen is current
//!    in the chain.
//! 4. `await_implementation` — the issue is currently open and no effective
//!    implementation exists (no link at all, or every linked PR closed
//!    unmerged — a closed-unmerged PR never projects implementation present,
//!    #1696 discrimination 4).
//! 5. `review_pr` — a currently-linked implementation PR is still open.
//! 6. `run_verification` — implementation is effectively present (merged,
//!    not reverted) and the issue is still open with no owner acceptance:
//!    verification is the missing prerequisite (#1696 discrimination 1's
//!    "merged, issue open" intermediate state).
//! 7. `await_owner_acceptance` — implementation present, issue no longer
//!    open (e.g. owner closed it), but no owner acceptance record exists:
//!    closure is not acceptance (#1297: `implemented != merged != accepted !=
//!    owner_closed`).
//! 8. `no_open_action`.

use serde::Serialize;

use super::reducer::{IssueLifecycleView, PrLifecycleView, ReductionV1};
use super::types::{
    EvidenceHeadV1, GithubObjectRefV1, OpenActionKindV1, PredicateV1, ReductionStatusV1,
    SubjectRefV1,
};

/// The repository's refresh posture — whether the last refresh succeeded,
/// and at which source revision (#1696: offline/denied/stale refresh is
/// `unknown/unavailable`; a previously-current claim can never stay fresh
/// without an explicit staleness posture).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefreshPostureV1 {
    /// `true` when the last refresh succeeded and its assertions were
    /// admitted; `false` when the source was unavailable/denied.
    pub fresh: bool,
    /// The source revision of the last successful refresh, when one exists.
    pub last_fresh_revision: Option<String>,
    /// When the last successful refresh was observed, when one exists.
    pub last_fresh_at: Option<String>,
    /// When the posture was last evaluated (caller-supplied; never computed
    /// here — this module reads no clock).
    pub evaluated_at: String,
    /// Human-readable unavailability reason, when not fresh.
    pub unavailable_reason: Option<String>,
}

impl RefreshPostureV1 {
    /// A fresh posture at a recorded revision.
    pub fn fresh(last_fresh_revision: String, last_fresh_at: String, evaluated_at: String) -> Self {
        RefreshPostureV1 {
            fresh: true,
            last_fresh_revision: Some(last_fresh_revision),
            last_fresh_at: Some(last_fresh_at),
            evaluated_at,
            unavailable_reason: None,
        }
    }

    /// An unavailable posture. The last-good revision is retained so the
    /// staleness debt is auditable, but nothing may present as fresh.
    pub fn unavailable(
        reason: String,
        last_fresh_revision: Option<String>,
        last_fresh_at: Option<String>,
        evaluated_at: String,
    ) -> Self {
        RefreshPostureV1 {
            fresh: false,
            last_fresh_revision,
            last_fresh_at,
            evaluated_at,
            unavailable_reason: Some(reason),
        }
    }
}

/// The owner class a pending action belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionOwnerClassV1 {
    /// The owning human (acceptance decisions, adjudication).
    Owner,
    /// The engineering authority (conflict adjudication, repairs).
    EngineeringAuthority,
    /// Any worker/system seat (verification runs, PR review prep).
    Worker,
    /// The source adapter (refresh retries).
    SourceAdapter,
    /// Nothing pending.
    None,
}

/// One derived open action with its full provenance (#1696: "Each action
/// carries owner/required authority/prerequisite refs/blockers/source
/// revisions").
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpenActionV1 {
    pub kind: OpenActionKindV1,
    pub subject: SubjectRefV1,
    pub owner_class: ActionOwnerClassV1,
    /// Prerequisite subject refs (e.g. the PR to review, the issue to
    /// accept).
    pub prerequisite_refs: Vec<GithubObjectRefV1>,
    /// Evidence heads the derivation was computed from — the action's source
    /// revisions.
    pub evidence_heads: Vec<EvidenceHeadV1>,
    /// Blockers: predicate tokens that currently block success-shaped
    /// projection (conflicted predicates, stale posture).
    pub blockers: Vec<String>,
}

/// Content-free projection health: conflict, staleness, and refresh-debt
/// counts only (#1696 storage/replay contract; safe for unauthorized
/// callers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct ProjectionHealthV1 {
    pub conflicted_predicates: usize,
    pub stale_handoff_claims: usize,
    pub repos_with_refresh_debt: usize,
}

/// Every source-side predicate, in canonical order — the row set consumer
/// views iterate. Projection-side predicates (`handoff_*`, `open_action`)
/// are deliberately absent: they are computed per-read, not stored.
pub const ALL_SOURCE_PREDICATES: &[PredicateV1] = &[
    PredicateV1::IssueOpen,
    PredicateV1::IssueClosed,
    PredicateV1::ImplementationPrLinked,
    PredicateV1::PrOpen,
    PredicateV1::PrMerged,
    PredicateV1::PrClosedUnmerged,
    PredicateV1::MergeReverted,
    PredicateV1::IssueReopened,
    PredicateV1::ImplementationPresent,
    PredicateV1::OwnerAcceptancePresent,
];

/// Derive the open action for one issue subject. Pure and deterministic.
pub fn open_action_for(
    reduction: &ReductionV1,
    posture: &RefreshPostureV1,
    issue: &SubjectRefV1,
) -> OpenActionV1 {
    let lifecycle = reduction.issue_lifecycle(issue);
    let chain_conflicted = reduction.chain_conflicted(issue);
    let linked_prs = reduction.linked_prs(issue);
    let merged_prs = reduction.linked_merged_prs(issue);
    let implementation_present = reduction.implementation_present(issue);
    let acceptance = reduction.owner_acceptance_present(issue);
    let reopened_current = reduction.get(issue, PredicateV1::IssueReopened).status
        == ReductionStatusV1::Current;
    let reverted_linked = linked_prs.iter().any(|object| {
        reduction.pr_lifecycle(&pr_subject(issue, object)) == PrLifecycleView::MergeReverted
    });

    // (1) conflict
    if chain_conflicted {
        return OpenActionV1 {
            kind: OpenActionKindV1::ResolveConflict,
            subject: issue.clone(),
            owner_class: ActionOwnerClassV1::EngineeringAuthority,
            prerequisite_refs: linked_prs.clone(),
            evidence_heads: chain_heads(reduction, issue),
            blockers: chain_conflict_tokens(reduction, issue),
        };
    }
    // (2) refresh unavailable
    if !posture.fresh {
        return OpenActionV1 {
            kind: OpenActionKindV1::RefreshUnavailableSource,
            subject: issue.clone(),
            owner_class: ActionOwnerClassV1::SourceAdapter,
            prerequisite_refs: Vec::new(),
            evidence_heads: Vec::new(),
            blockers: vec!["refresh_unavailable".to_string()],
        };
    }
    // (3) revert / reopen repair
    if reverted_linked || reopened_current {
        return OpenActionV1 {
            kind: OpenActionKindV1::RepairRevertOrReopen,
            subject: issue.clone(),
            owner_class: ActionOwnerClassV1::Worker,
            prerequisite_refs: linked_prs.clone(),
            evidence_heads: chain_heads(reduction, issue),
            blockers: Vec::new(),
        };
    }
    // (4) await implementation: issue open, nothing effective, and no live
    // linked PR (a still-open linked PR means implementation is underway —
    // rule 5). A PR closed unmerged never counts as implementation present
    // (#1696 discrimination 4).
    let has_open_linked_pr = linked_prs.iter().any(|object| {
        reduction.pr_lifecycle(&pr_subject(issue, object)) == PrLifecycleView::Open
    });
    if matches!(lifecycle, IssueLifecycleView::Open | IssueLifecycleView::Reopened)
        && !implementation_present
        && !has_open_linked_pr
    {
        return OpenActionV1 {
            kind: OpenActionKindV1::AwaitImplementation,
            subject: issue.clone(),
            owner_class: ActionOwnerClassV1::Worker,
            prerequisite_refs: Vec::new(),
            evidence_heads: chain_heads(reduction, issue),
            blockers: Vec::new(),
        };
    }
    // (5) review PR: linked, still open.
    if linked_prs.iter().any(|object| {
        reduction.pr_lifecycle(&pr_subject(issue, object)) == PrLifecycleView::Open
    }) {
        return OpenActionV1 {
            kind: OpenActionKindV1::ReviewPr,
            subject: issue.clone(),
            owner_class: ActionOwnerClassV1::Worker,
            prerequisite_refs: linked_prs.clone(),
            evidence_heads: chain_heads(reduction, issue),
            blockers: Vec::new(),
        };
    }
    // (6) run verification: landed, issue still open, not accepted.
    if implementation_present
        && matches!(lifecycle, IssueLifecycleView::Open | IssueLifecycleView::Reopened)
        && !acceptance
    {
        return OpenActionV1 {
            kind: OpenActionKindV1::RunVerification,
            subject: issue.clone(),
            owner_class: ActionOwnerClassV1::Worker,
            prerequisite_refs: merged_prs.clone(),
            evidence_heads: chain_heads(reduction, issue),
            blockers: Vec::new(),
        };
    }
    // (7) await owner acceptance: landed, issue no longer open, but no
    // acceptance record — closure is not acceptance.
    if implementation_present && !acceptance {
        return OpenActionV1 {
            kind: OpenActionKindV1::AwaitOwnerAcceptance,
            subject: issue.clone(),
            owner_class: ActionOwnerClassV1::Owner,
            prerequisite_refs: merged_prs.clone(),
            evidence_heads: chain_heads(reduction, issue),
            blockers: Vec::new(),
        };
    }
    // (8) nothing pending.
    OpenActionV1 {
        kind: OpenActionKindV1::NoOpenAction,
        subject: issue.clone(),
        owner_class: ActionOwnerClassV1::None,
        prerequisite_refs: Vec::new(),
        evidence_heads: chain_heads(reduction, issue),
        blockers: Vec::new(),
    }
}

/// Derive open actions for every issue subject in the reduction.
pub fn open_actions(
    reduction: &ReductionV1,
    posture: &RefreshPostureV1,
) -> Vec<OpenActionV1> {
    reduction
        .subjects()
        .into_iter()
        .filter(|subject| {
            matches!(subject.object, GithubObjectRefV1::Issue(_))
        })
        .map(|issue| open_action_for(reduction, posture, &issue))
        .collect()
}

/// Projection health over a reduction, a stale-claim count, and the set of
/// repositories with refresh debt.
pub fn projection_health(
    reduction: &ReductionV1,
    stale_handoff_claims: usize,
    repos_with_refresh_debt: usize,
) -> ProjectionHealthV1 {
    let reduction_health = reduction.health_counts();
    ProjectionHealthV1 {
        conflicted_predicates: reduction_health.conflicted_predicates,
        stale_handoff_claims,
        repos_with_refresh_debt,
    }
}

fn chain_heads(reduction: &ReductionV1, issue: &SubjectRefV1) -> Vec<EvidenceHeadV1> {
    let linked = reduction.linked_prs(issue);
    let mut heads = Vec::new();
    for reduced in reduction.all() {
        if reduced.subject == *issue
            || (reduced.subject.repo == issue.repo && linked.contains(&reduced.subject.object))
        {
            heads.extend(reduced.current_heads.iter().cloned());
        }
    }
    heads
}

fn chain_conflict_tokens(reduction: &ReductionV1, issue: &SubjectRefV1) -> Vec<String> {
    let linked = reduction.linked_prs(issue);
    let mut tokens = Vec::new();
    for reduced in reduction.all() {
        if reduced.status == ReductionStatusV1::Conflicted
            && (reduced.subject == *issue
                || (reduced.subject.repo == issue.repo && linked.contains(&reduced.subject.object)))
        {
            tokens.push(format!(
                "{}#{}",
                reduced.subject.as_token(),
                reduced.predicate.as_str()
            ));
        }
    }
    tokens
}

fn pr_subject(issue: &SubjectRefV1, object: &GithubObjectRefV1) -> SubjectRefV1 {
    SubjectRefV1 {
        repo: issue.repo.clone(),
        object: object.clone(),
    }
}
