//! Typed assertion vocabulary for CurrentTruth v1 (#1696 / #1297 §1.2).
//!
//! Every enum here is a **closed** vocabulary frozen by the #1696 contract:
//! the predicate list, the reduction statuses, the authority classes, the
//! review states, and the open-action kinds. Adding a variant is a contract
//! change, not a refactor.
//!
//! Nothing in this module performs I/O, reads a clock, or calls a model.

use serde::{Deserialize, Serialize};

/// One typed GitHub object in one repository — the subject of an assertion.
///
/// Subjects are typed objects, never free-text references: `repo` must be
/// `owner/name`, issues and PRs are addressed by number, commits by full SHA.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SubjectRefV1 {
    /// `owner/name` repository identity.
    pub repo: String,
    /// The typed object inside that repository.
    pub object: GithubObjectRefV1,
}

/// The typed GitHub object kinds CurrentTruth v1 reconciles.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubObjectRefV1 {
    /// GitHub issue number.
    Issue(u64),
    /// GitHub pull-request number.
    PullRequest(u64),
    /// Full commit SHA.
    Commit(String),
}

impl GithubObjectRefV1 {
    /// Stable wire token for the object kind (`issue:123`, `pull_request:45`,
    /// `commit:<sha>`).
    pub fn as_token(&self) -> String {
        match self {
            GithubObjectRefV1::Issue(number) => format!("issue:{number}"),
            GithubObjectRefV1::PullRequest(number) => format!("pull_request:{number}"),
            GithubObjectRefV1::Commit(sha) => format!("commit:{sha}"),
        }
    }
}

impl SubjectRefV1 {
    /// Stable subject token: `owner/name#issue:123` style, used as the store
    /// key and in evidence-head views. Deliberately not a URL — this is an
    /// internal identity, not a link.
    pub fn as_token(&self) -> String {
        format!("{}#{}", self.repo, self.object.as_token())
    }
}

/// The first-slice predicate vocabulary, verbatim from #1696.
///
/// `handoff_current` / `handoff_stale` / `open_action` are projection-side
/// predicates: they are computed by the deterministic projection, never
/// asserted by a source adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredicateV1 {
    /// The issue is currently open (typed GitHub object state).
    IssueOpen,
    /// The issue is currently closed (typed GitHub object state).
    IssueClosed,
    /// The issue's implementation PR is linked by a **typed** relation
    /// (never title/body similarity).
    ImplementationPrLinked,
    /// The PR is currently open.
    PrOpen,
    /// The PR merged; value carries the merge commit SHA.
    PrMerged,
    /// The PR closed without merging.
    PrClosedUnmerged,
    /// The merge was reverted; value carries the revert commit SHA.
    MergeReverted,
    /// The issue was reopened after a close (typed observation).
    IssueReopened,
    /// Implementation is present (merged code or reviewed disposition).
    ImplementationPresent,
    /// Owner acceptance for the subject work is present (owner decision).
    OwnerAcceptancePresent,
    /// Projection: the handoff's claim about this predicate is current.
    HandoffCurrent,
    /// Projection: the handoff's claim about this predicate is stale.
    HandoffStale,
    /// Projection: the derived open action (value carries the action kind).
    OpenAction,
}

impl PredicateV1 {
    /// Wire token matching the #1696 body's predicate list.
    pub fn as_str(self) -> &'static str {
        match self {
            PredicateV1::IssueOpen => "issue_open",
            PredicateV1::IssueClosed => "issue_closed",
            PredicateV1::ImplementationPrLinked => "implementation_pr_linked",
            PredicateV1::PrOpen => "pr_open",
            PredicateV1::PrMerged => "pr_merged",
            PredicateV1::PrClosedUnmerged => "pr_closed_unmerged",
            PredicateV1::MergeReverted => "merge_reverted",
            PredicateV1::IssueReopened => "issue_reopened",
            PredicateV1::ImplementationPresent => "implementation_present",
            PredicateV1::OwnerAcceptancePresent => "owner_acceptance_present",
            PredicateV1::HandoffCurrent => "handoff_current",
            PredicateV1::HandoffStale => "handoff_stale",
            PredicateV1::OpenAction => "open_action",
        }
    }

    /// Whether this predicate may ever be established by a source adapter, as
    /// opposed to being computed by the projection.
    pub fn is_source_predicate(self) -> bool {
        !matches!(
            self,
            PredicateV1::HandoffCurrent | PredicateV1::HandoffStale | PredicateV1::OpenAction
        )
    }
}

impl std::fmt::Display for PredicateV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The closed predicate-specific value vocabulary (#1696: "closed
/// predicate-specific value").
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertionValueV1 {
    /// The predicate carries no payload beyond its subject.
    Unit,
    /// A typed object reference (e.g. `implementation_pr_linked` → the PR).
    ObjectRef(GithubObjectRefV1),
    /// A full commit SHA (`pr_merged` → merge SHA, `merge_reverted` → revert
    /// SHA, `implementation_present` → the merged SHA).
    CommitSha(String),
    /// A handoff packet id (`handoff_current` / `handoff_stale`).
    HandoffId(String),
    /// The derived action kind (`open_action`).
    Action(OpenActionKindV1),
}

/// The authority class of an assertion — who may establish what (#1297 §1.2
/// authority table, frozen).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityClassV1 {
    /// The typed GitHub object itself at a recorded source revision. May
    /// establish object-state predicates and typed relations.
    GitHubTypedObject,
    /// A named owner decision (acceptance, owner close/reopen).
    OwnerDecision,
    /// A reviewed disposition (e.g. a reviewed `implemented_by` mapping).
    ReviewedDisposition,
    /// Model prose or a generated summary — candidate evidence only; this
    /// class can never establish any predicate.
    ModelProse,
}

impl AuthorityClassV1 {
    /// Whether this authority class may establish `predicate` at all. This is
    /// the admission law's first gate; a violation is rejected at append time
    /// and never reaches the reducer.
    pub fn may_establish(self, predicate: PredicateV1) -> bool {
        match self {
            AuthorityClassV1::GitHubTypedObject => {
                predicate.is_source_predicate()
                    && !matches!(predicate, PredicateV1::OwnerAcceptancePresent)
            }
            AuthorityClassV1::OwnerDecision => matches!(
                predicate,
                PredicateV1::OwnerAcceptancePresent
                    | PredicateV1::IssueClosed
                    | PredicateV1::IssueReopened
            ),
            AuthorityClassV1::ReviewedDisposition => matches!(
                predicate,
                PredicateV1::ImplementationPrLinked | PredicateV1::ImplementationPresent
            ),
            // #1297: model prose is candidate evidence only — citation
            // establishes provenance, not authority.
            AuthorityClassV1::ModelProse => false,
        }
    }
}

/// Review state of an assertion (#1297 §1.2). Only `Observed` and `Reviewed`
/// assertions participate in reduction; `Candidate` and `Rejected` never
/// affect current truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStateV1 {
    /// Admitted observational fact.
    Observed,
    /// Model-proposed evidence — never authoritative.
    Candidate,
    /// Human-reviewed and admitted.
    Reviewed,
    /// Reviewed and rejected — retained as history, never current.
    Rejected,
}

impl ReviewStateV1 {
    /// Whether an assertion in this review state may influence reduction.
    pub fn is_admitted(self) -> bool {
        matches!(self, ReviewStateV1::Observed | ReviewStateV1::Reviewed)
    }
}

/// Visibility of an assertion's subject. Private subjects are filtered from
/// unauthorized reads entirely — no counts, no refs, no existence signal
/// (#1696 discrimination 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityClassV1 {
    /// Visible to any caller of the consumer view.
    Public,
    /// Visible only to authorized callers.
    Private,
}

/// Identity and revision of the source system that emitted an assertion.
///
/// `source` names the asserting system (e.g. a snapshot adapter identity);
/// `revision` is that source's **immutable revision token** (snapshot hash,
/// event id, or commit SHA). Source revision is ordering authority; arrival
/// order never is.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourceRefV1 {
    /// Source system identity (adapter id, owner-decision record id, …).
    pub source: String,
    /// Immutable revision token from that source.
    pub revision: String,
}

/// One revisioned assertion — the append-only unit of current-truth authority
/// (#1696 "Typed assertion contract", field-for-field).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssertionV1 {
    /// Immutable assertion identity (content-derived or source-minted).
    pub assertion_id: String,
    /// The typed subject this assertion is about.
    pub subject: SubjectRefV1,
    /// The predicate being asserted.
    pub predicate: PredicateV1,
    /// The closed predicate-specific value.
    pub value: AssertionValueV1,
    /// Who emitted the assertion (adapter instance, owner, model seat…).
    pub issuer: String,
    /// Authority class gating what this issuer may establish.
    pub authority_class: AuthorityClassV1,
    /// Source identity + immutable source revision.
    pub source_ref: SourceRefV1,
    /// When the source observed the fact (RFC 3339). Ordering authority,
    /// together with `source_ref.revision`. Never the arrival time.
    pub observed_at: String,
    /// When the fact became effective, if the source distinguishes.
    pub effective_at: String,
    /// Explicit supersession edge, if the source recorded one. The reducer
    /// follows these edges in addition to revision ordering.
    pub supersedes_assertion_id: Option<String>,
    /// Immutable evidence references (commit SHAs, event ids, snapshot
    /// hashes).
    pub evidence_refs: Vec<String>,
    /// Review state; only admitted states reduce.
    pub review_state: ReviewStateV1,
    /// Subject visibility for consumer reads.
    pub visibility: VisibilityClassV1,
}

impl AssertionV1 {
    /// The reduction ordering key: `(observed_at, source revision,
    /// assertion_id)`. All three components are source-supplied or immutable
    /// identity — arrival time (`recorded_at`, store-side) is deliberately
    /// absent (#1696: "event arrival order is not source revision").
    pub fn order_key(&self) -> (String, String, String) {
        (
            self.observed_at.clone(),
            self.source_ref.revision.clone(),
            self.assertion_id.clone(),
        )
    }

    /// The supersession lineage key: assertions may supersede each other only
    /// within the same `(subject, predicate, authority_class, issuer, source)`
    /// scope (#1696 reduction law).
    pub fn lineage_key(&self) -> (String, PredicateV1, AuthorityClassV1, String, String) {
        (
            self.subject.as_token(),
            self.predicate,
            self.authority_class,
            self.issuer.clone(),
            self.source_ref.source.clone(),
        )
    }

    /// The immutable ingestion identity: same source, same revision, same
    /// asserted fact must be the same assertion (idempotent re-append).
    pub fn ingestion_key(&self) -> AssertionIngestionKey {
        AssertionIngestionKey {
            subject_token: self.subject.as_token(),
            predicate: self.predicate,
            authority_class: self.authority_class,
            issuer: self.issuer.clone(),
            source: self.source_ref.source.clone(),
            source_revision: self.source_ref.revision.clone(),
        }
    }
}

/// Immutable ingestion identity used for idempotent append (#1696
/// storage/replay contract).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AssertionIngestionKey {
    pub subject_token: String,
    pub predicate: PredicateV1,
    pub authority_class: AuthorityClassV1,
    pub issuer: String,
    pub source: String,
    pub source_revision: String,
}

/// The reducer's explicit output vocabulary, verbatim from #1696. The
/// reducer emits only these four statuses — never a narrative, never a
/// guessed "most plausible" state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReductionStatusV1 {
    /// The predicate holds now, with evidence heads.
    Current,
    /// A newer admitted assertion in the same lineage replaced this one;
    /// history retained, cited as superseded evidence.
    Superseded,
    /// Authoritative facts contradict; nothing is selected. Success-shaped
    /// projections are blocked until resolved.
    Conflicted,
    /// No admitted assertion exists for this (subject, predicate). Never
    /// defaulted, never guessed.
    Unknown,
}

/// The derived open-action vocabulary (#1696 "Derived open-action
/// projection" example list, closed). Actions are data — carrying owners,
/// required authority, prerequisites, blockers, and source revisions — never
/// executed and never model-generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenActionKindV1 {
    /// Issue open, no implementation link/presence.
    AwaitImplementation,
    /// Implementation linked, PR currently open.
    ReviewPr,
    /// Implementation landed; verification evidence is the pending
    /// prerequisite before acceptance can be requested.
    RunVerification,
    /// Implementation landed; the owner's acceptance record is missing.
    AwaitOwnerAcceptance,
    /// A merge was reverted or an issue reopened; repair before proceeding.
    RepairRevertOrReopen,
    /// The source refresh is unavailable; current state is stale/unknown.
    RefreshUnavailableSource,
    /// Contradictory authoritative facts must be adjudicated first.
    ResolveConflict,
    /// Nothing pending from this projection.
    NoOpenAction,
}

impl OpenActionKindV1 {
    /// Wire token matching the #1696 example list.
    pub fn as_str(self) -> &'static str {
        match self {
            OpenActionKindV1::AwaitImplementation => "await_implementation",
            OpenActionKindV1::ReviewPr => "review_pr",
            OpenActionKindV1::RunVerification => "run_verification",
            OpenActionKindV1::AwaitOwnerAcceptance => "await_owner_acceptance",
            OpenActionKindV1::RepairRevertOrReopen => "repair_revert_or_reopen",
            OpenActionKindV1::RefreshUnavailableSource => "refresh_unavailable_source",
            OpenActionKindV1::ResolveConflict => "resolve_conflict",
            OpenActionKindV1::NoOpenAction => "no_open_action",
        }
    }
}

/// One evidence head: the assertion id, its source revision, and when the
/// source observed it. This is the downstream contract unit (#1693) —
/// consumers bind to heads, never to raw assertions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceHeadV1 {
    pub assertion_id: String,
    pub source: String,
    pub source_revision: String,
    pub observed_at: String,
}

impl EvidenceHeadV1 {
    /// Build the head for an assertion.
    pub fn of(assertion: &AssertionV1) -> Self {
        EvidenceHeadV1 {
            assertion_id: assertion.assertion_id.clone(),
            source: assertion.source_ref.source.clone(),
            source_revision: assertion.source_ref.revision.clone(),
            observed_at: assertion.observed_at.clone(),
        }
    }
}
