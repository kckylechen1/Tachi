//! GitHub reconciliation v1 (#1696): refresh reads current GitHub state
//! through the authoritative adapter, records source revision/evidence, and
//! reduces without mutating GitHub.
//!
//! The adapter trait is the seam: `tachi-github-runtime`'s corpus fixtures
//! implement it for tests; a future live adapter wraps the bounded `gh`
//! read path. Refresh only **reads** — the mutation refusal belongs to the
//! adapter, mirroring `github_corpus_ops::reader::refuse_github_mutation`.
//!
//! Offline/denied/stale refresh yields [`RefreshOutcomeV1::Unavailable`] —
//! no assertions are minted, and the projection must surface
//! unknown/stale posture, never a fabricated current result (#1696
//! discrimination 8).

use serde::{Deserialize, Serialize};

use super::types::{
    AssertionV1, AssertionValueV1, AuthorityClassV1, GithubObjectRefV1, PredicateV1,
    ReviewStateV1, SourceRefV1, SubjectRefV1, VisibilityClassV1,
};

/// The authoritative source adapter for one repository's current GitHub
/// state. Implementations must be read-only against GitHub.
pub trait GithubRefreshAdapter {
    /// Refresh the current state of `repo`. Errors are surfaced as
    /// [`RefreshOutcomeV1::Unavailable`], never as a guessed state.
    fn refresh(&self, repo: &str) -> RefreshOutcomeV1;
}

/// The result of one refresh attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcomeV1 {
    /// Current state observed at a recorded revision.
    Fresh(Box<GithubRepositoryStateV1>),
    /// The source was unavailable, denied, or stale. Carries the reason;
    /// mints nothing.
    Unavailable { repo: String, reason: String },
}

/// The lifecycle states a snapshot can carry for an issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotIssueStateV1 {
    Open,
    Closed,
}

/// The lifecycle states a snapshot can carry for a PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotPrStateV1 {
    Open,
    Merged,
    ClosedUnmerged,
}

/// One issue's current snapshot state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotIssueV1 {
    pub number: u64,
    pub state: SnapshotIssueStateV1,
    /// RFC 3339 `updated_at` from the source — part of the ordering key.
    pub updated_at: String,
    /// The source's immutable revision token for this snapshot (e.g. the
    /// `IssueSnapshotV1::issue_snapshot_hash`).
    pub snapshot_revision: String,
    /// Subject visibility carried from the source.
    pub visibility: VisibilityClassV1,
}

/// One PR's current snapshot state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotPrV1 {
    pub number: u64,
    pub state: SnapshotPrStateV1,
    /// Merge commit SHA when `Merged`.
    pub merge_commit_sha: Option<String>,
    pub updated_at: String,
    /// The source's immutable revision token for this snapshot (e.g.
    /// `PullRequestSnapshotV1::pr_snapshot_hash`).
    pub snapshot_revision: String,
    /// **Typed** issue relations observed by the adapter (from GitHub's own
    /// cross-reference/development data — never inferred from title/body
    /// similarity by this layer).
    pub linked_issues: Vec<u64>,
    pub visibility: VisibilityClassV1,
}

/// A typed lifecycle observation the adapter extracted from source event
/// history (timeline/commits) — reopen and revert transitions are not
/// recoverable from state snapshots alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotObservationV1 {
    pub kind: SnapshotObservationKindV1,
    /// RFC 3339 observation time from the source.
    pub observed_at: String,
    /// Immutable revision token for the observation (event id / commit SHA).
    pub revision: String,
}

/// The typed observation kinds v1 reconciles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotObservationKindV1 {
    /// Issue `number` was reopened after a close.
    IssueReopened { number: u64 },
    /// PR `number`'s merge (original `merged_sha`) was reverted by
    /// `revert_commit_sha`.
    MergeReverted {
        number: u64,
        revert_commit_sha: String,
        original_merge_sha: String,
    },
}

/// One repository's current GitHub state at one refresh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubRepositoryStateV1 {
    pub repo: String,
    /// The refresh's repository-wide revision token (e.g. a digest over the
    /// per-object snapshot revisions).
    pub refresh_revision: String,
    /// RFC 3339 refresh timestamp from the source.
    pub refreshed_at: String,
    pub issues: Vec<SnapshotIssueV1>,
    pub pull_requests: Vec<SnapshotPrV1>,
    pub observations: Vec<SnapshotObservationV1>,
}

impl GithubRepositoryStateV1 {
    /// The canonical subject for an issue in this repository.
    pub fn issue_subject(&self, number: u64) -> SubjectRefV1 {
        SubjectRefV1 {
            repo: self.repo.clone(),
            object: GithubObjectRefV1::Issue(number),
        }
    }

    /// The canonical subject for a PR in this repository.
    pub fn pr_subject(&self, number: u64) -> SubjectRefV1 {
        SubjectRefV1 {
            repo: self.repo.clone(),
            object: GithubObjectRefV1::PullRequest(number),
        }
    }
}

/// The default source identity stamped on adapter-minted assertions. The
/// adapter is the authoritative reader; its identity participates in the
/// supersession lineage.
pub const GITHUB_SNAPSHOT_SOURCE_ID: &str = "github-snapshot-adapter";
/// The default issuer stamp for adapter-minted assertions.
pub const GITHUB_SNAPSHOT_ISSUER: &str = "github-refresh-v1";

/// Mint assertions from a fresh repository state. Pure: no I/O, no clock,
/// no model; every assertion carries the snapshot's immutable revision and
/// the snapshot's `updated_at` as `observed_at`. Relations come only from
/// typed adapter data — title/body similarity is never consulted.
pub fn mint_assertions(state: &GithubRepositoryStateV1) -> Vec<AssertionV1> {
    // STUB (RED commit): minting law not implemented.
    let _ = state;
    Vec::new()
}

#[allow(dead_code)]
fn __mint_assertions_real(state: &GithubRepositoryStateV1) -> Vec<AssertionV1> {
    let mut out = Vec::new();
    let source = SourceRefV1 {
        source: GITHUB_SNAPSHOT_SOURCE_ID.to_string(),
        revision: String::new(),
    };

    for issue in &state.issues {
        let subject = state.issue_subject(issue.number);
        let predicate = match issue.state {
            SnapshotIssueStateV1::Open => PredicateV1::IssueOpen,
            SnapshotIssueStateV1::Closed => PredicateV1::IssueClosed,
        };
        out.push(base_assertion(
            &subject,
            predicate,
            AssertionValueV1::Unit,
            &source.clone(),
            &issue.snapshot_revision,
            &issue.updated_at,
            issue.visibility,
        ));
    }

    for pr in &state.pull_requests {
        let subject = state.pr_subject(pr.number);
        let (predicate, value) = match pr.state {
            SnapshotPrStateV1::Open => (PredicateV1::PrOpen, AssertionValueV1::Unit),
            SnapshotPrStateV1::Merged => (
                PredicateV1::PrMerged,
                AssertionValueV1::CommitSha(
                    pr.merge_commit_sha.clone().unwrap_or_else(|| "unknown".to_string()),
                ),
            ),
            SnapshotPrStateV1::ClosedUnmerged => {
                (PredicateV1::PrClosedUnmerged, AssertionValueV1::Unit)
            }
        };
        out.push(base_assertion(
            &subject,
            predicate,
            value,
            &source.clone(),
            &pr.snapshot_revision,
            &pr.updated_at,
            pr.visibility,
        ));

        // Typed links only: the adapter observed the relation.
        for issue_number in &pr.linked_issues {
            let issue_subject = state.issue_subject(*issue_number);
            let issue_state = state.issues.iter().find(|i| i.number == *issue_number);
            let (revision, observed_at) = match issue_state {
                Some(issue) => (issue.snapshot_revision.clone(), issue.updated_at.clone()),
                None => (pr.snapshot_revision.clone(), pr.updated_at.clone()),
            };
            out.push(base_assertion(
                &issue_subject,
                PredicateV1::ImplementationPrLinked,
                AssertionValueV1::ObjectRef(GithubObjectRefV1::PullRequest(pr.number)),
                &source.clone(),
                &revision,
                &observed_at,
                pr.visibility,
            ));
        }

        // A merged PR asserts implementation present on every linked issue —
        // typed relation + typed merge, both from the object layer.
        if matches!(pr.state, SnapshotPrStateV1::Merged) {
            for issue_number in &pr.linked_issues {
                let issue_subject = state.issue_subject(*issue_number);
                out.push(base_assertion(
                    &issue_subject,
                    PredicateV1::ImplementationPresent,
                    AssertionValueV1::CommitSha(
                        pr.merge_commit_sha.clone().unwrap_or_else(|| "unknown".to_string()),
                    ),
                    &source.clone(),
                    &pr.snapshot_revision,
                    &pr.updated_at,
                    pr.visibility,
                ));
            }
        }
    }

    for observation in &state.observations {
        match &observation.kind {
            SnapshotObservationKindV1::IssueReopened { number } => {
                let subject = state.issue_subject(*number);
                out.push(base_assertion(
                    &subject,
                    PredicateV1::IssueReopened,
                    AssertionValueV1::Unit,
                    &source.clone(),
                    &observation.revision,
                    &observation.observed_at,
                    VisibilityClassV1::Public,
                ));
            }
            SnapshotObservationKindV1::MergeReverted {
                number,
                revert_commit_sha,
                original_merge_sha,
            } => {
                let subject = state.pr_subject(*number);
                let mut assertion = base_assertion(
                    &subject,
                    PredicateV1::MergeReverted,
                    AssertionValueV1::CommitSha(revert_commit_sha.clone()),
                    &source.clone(),
                    &observation.revision,
                    &observation.observed_at,
                    VisibilityClassV1::Public,
                );
                assertion.evidence_refs.push(original_merge_sha.clone());
                out.push(assertion);
            }
        }
    }

    // Deterministic ids derived from the immutable inputs: subject,
    // predicate, source revision. Same state ⇒ same ids ⇒ idempotent append.
    for assertion in &mut out {
        assertion.assertion_id = format!(
            "ct-{}-{}-{}",
            assertion.subject.as_token(),
            assertion.predicate.as_str(),
            assertion.source_ref.revision
        );
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn base_assertion(
    subject: &SubjectRefV1,
    predicate: PredicateV1,
    value: AssertionValueV1,
    source: &SourceRefV1,
    revision: &str,
    observed_at: &str,
    visibility: VisibilityClassV1,
) -> AssertionV1 {
    AssertionV1 {
        assertion_id: String::new(),
        subject: subject.clone(),
        predicate,
        value,
        issuer: GITHUB_SNAPSHOT_ISSUER.to_string(),
        authority_class: AuthorityClassV1::GitHubTypedObject,
        source_ref: SourceRefV1 {
            source: source.source.clone(),
            revision: revision.to_string(),
        },
        observed_at: observed_at.to_string(),
        effective_at: observed_at.to_string(),
        supersedes_assertion_id: None,
        evidence_refs: Vec::new(),
        review_state: ReviewStateV1::Observed,
        visibility,
    }
}

/// Run one reconciliation round against an adapter for one repository:
/// refresh, then mint. An unavailable refresh mints nothing — the caller
/// records the posture and the reduction stands on its recorded history,
/// explicitly stale (#1696: "cannot preserve a previously-current claim as
/// fresh without an explicit staleness posture").
pub fn reconcile_refresh(
    adapter: &dyn GithubRefreshAdapter,
    repo: &str,
) -> (RefreshOutcomeV1, Vec<AssertionV1>) {
    match adapter.refresh(repo) {
        RefreshOutcomeV1::Fresh(state) => {
            let assertions = mint_assertions(&state);
            (RefreshOutcomeV1::Fresh(state), assertions)
        }
        unavailable @ RefreshOutcomeV1::Unavailable { .. } => {
            (unavailable, Vec::new())
        }
    }
}
