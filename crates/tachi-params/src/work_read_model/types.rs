//! Typed projection output for the Unified Work Read Model (#1693).
//!
//! These rows are **derived views, never a ledger**: every field is either
//! copied from a source snapshot, stamped with the snapshot it came from,
//! or explicitly `Unknown`/`Unavailable`. Dropping the projection and
//! rebuilding it from the same snapshots reproduces it canonically; no
//! field here is ever an input back into any authority.

use crate::current_truth::consumer::SubjectTruthViewV1;
use crate::current_truth::projection::ActionOwnerClassV1;
use crate::current_truth::types::ReductionStatusV1;
use crate::taskintent::mapping::adjudication::AdjudicationState;

use super::sources::{
    AdjudicationFactV1, DeliveryObservationV1, ExecEnvFactV1, RunReceiptFactV1, SourceKind,
    SourceStamp, VerificationFactV1, WorkClaimFactV1,
};

/// The stable identity of one work item. `Issue` keys use the CurrentTruth
/// subject token shape (`owner/repo#issue:N`) so GitHub-domain joins are
/// token-equality; `PullRequest` keys exist for orphaned revert debt — a
/// reverted PR no issue's CURRENT link set claims still carries R6-2
/// transition debt, and that debt must remain an attributable, actionable
/// work item (unlinking is not a causal resolution); `Dispatch` and `Claim`
/// keys exist for work that has not (yet) resolved to an issue — they are
/// never guessed into one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WorkKey {
    Issue { repo: String, number: u64 },
    PullRequest { repo: String, number: u64 },
    Dispatch(String),
    Claim(String),
}

impl WorkKey {
    /// Stable token (`owner/repo#issue:N`, `owner/repo#pull_request:N`,
    /// `dispatch:<id>`, `claim:<id>`).
    pub fn as_token(&self) -> String {
        match self {
            WorkKey::Issue { repo, number } => format!("{repo}#issue:{number}"),
            WorkKey::PullRequest { repo, number } => format!("{repo}#pull_request:{number}"),
            WorkKey::Dispatch(id) => format!("dispatch:{id}"),
            WorkKey::Claim(id) => format!("claim:{id}"),
        }
    }

    /// Parse an `owner/repo#N` issue ref into an issue key.
    pub fn parse_issue_ref(raw: &str) -> Option<Self> {
        let (repo, number) = raw.rsplit_once('#')?;
        if !repo.contains('/') || number.is_empty() {
            return None;
        }
        let number: u64 = number.parse().ok()?;
        Some(WorkKey::Issue {
            repo: repo.to_string(),
            number,
        })
    }
}

/// Per-section availability. `Unavailable` names which source is missing so
/// the gap is explicit — a missing source is never a guessed default
/// (#1693 build/replay contract). `NotApplicable` marks sections that have
/// no subject for this work item at all (e.g. GitHub state for a
/// dispatch-keyed item with no issue binding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionState<T> {
    Available(T),
    Unavailable { source: SourceKind },
    NotApplicable,
}

impl<T> SectionState<T> {
    pub fn is_available(&self) -> bool {
        matches!(self, SectionState::Available(_))
    }
}

/// Resolved GitHub lifecycle summary for the work item's issue subject,
/// derived only from reduced CurrentTruth predicates. This is a thin reader
/// of the consumer view — the reconciliation law itself lives in #1696.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImplementationStatusV1 {
    /// The CurrentTruth source itself is unavailable (missing snapshot,
    /// non-fresh posture, or absent subject).
    Unknown,
    /// Subject observed, no typed implementation link.
    NotLinked,
    /// Implementation linked, PR currently open.
    UnderReview,
    /// Implementation effectively present at the reduced resolution AND no
    /// outstanding revert debt.
    Present,
    /// An outstanding transition debt blocks effective implementation.
    Reverted,
    /// Conflicting authoritative facts; success-shaped projection blocked.
    Conflicted,
}

impl ImplementationStatusV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            ImplementationStatusV1::Unknown => "unknown",
            ImplementationStatusV1::NotLinked => "not_linked",
            ImplementationStatusV1::UnderReview => "under_review",
            ImplementationStatusV1::Present => "present",
            ImplementationStatusV1::Reverted => "reverted",
            ImplementationStatusV1::Conflicted => "conflicted",
        }
    }
}

/// The R6-2 transition-debt projection (owner-ruled law, #1693): a
/// `merge_reverted` or `issue_reopened` fact is a **transition** that a
/// later steady-state snapshot must NOT clear, even when the frozen
/// max-key family law lets that newer node/state snapshot win the
/// CurrentTruth lifecycle resolution. Debt clears only through the causal
/// resolutions named in the ruling. This is derived from the admitted
/// CurrentTruth consumer view plus owner dispositions — it is a
/// rebuildable projection, not a second truth store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionDebtV1 {
    pub revert: DebtStateV1,
    pub reopen: DebtStateV1,
}

impl TransitionDebtV1 {
    /// Whether any transition debt is outstanding (blocks success-shaped
    /// projection and surfaces a repair action).
    pub fn any_outstanding(&self) -> bool {
        matches!(self.revert, DebtStateV1::Outstanding { .. })
            || matches!(self.reopen, DebtStateV1::Outstanding { .. })
    }
}

/// One transition's debt state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebtStateV1 {
    /// No transition fact observed.
    None,
    /// Transition observed; no causal resolution evidence. A later
    /// steady-state snapshot (unchanged original-PR merged state, a plain
    /// `issue_open` refresh) is node/state evidence and NEVER resolution
    /// evidence.
    Outstanding {
        evidence_heads: Vec<crate::current_truth::consumer::EvidenceHeadViewV1>,
    },
    /// Cleared by one of the causal resolutions the ruling names.
    Cleared {
        by: DebtClearingV1,
        evidence_heads: Vec<crate::current_truth::consumer::EvidenceHeadViewV1>,
    },
}

/// The causal clearing authorities named by the R6-2 ruling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebtClearingV1 {
    /// `issue_reopened` cleared by a causally later authoritative
    /// `issue_closed` observation. Close is NOT owner acceptance — the
    /// acceptance dimension stays separate.
    LaterAuthoritativeIssueClosed,
    /// `merge_reverted` cleared by a causally later merged repair PR
    /// explicitly linked to the issue (a PR other than the reverted one —
    /// GitHub keeps reporting the original PR as merged after a revert, so
    /// the original PR's own re-snapshot is never repair evidence).
    PostRevertRepairPrMerged,
    /// `merge_reverted` cleared by an explicit owner-reviewed
    /// `no_repair_required` disposition.
    OwnerNoRepairRequired,
}

impl DebtClearingV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            DebtClearingV1::LaterAuthoritativeIssueClosed => "later_authoritative_issue_closed",
            DebtClearingV1::PostRevertRepairPrMerged => "post_revert_repair_pr_merged",
            DebtClearingV1::OwnerNoRepairRequired => "owner_no_repair_required",
        }
    }
}

/// The GitHub section: one repository's CurrentTruth view sliced to the
/// work item's issue subject, plus the R6-2 transition-debt projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubSectionV1 {
    pub repo: String,
    pub posture_fresh: bool,
    /// The issue's subject row, when the (authorized) view contains it.
    pub subject: Option<SubjectTruthViewV1>,
    pub implementation_status: ImplementationStatusV1,
    /// Merge SHA of the effective implementation, when evidenced.
    pub merge_sha: Option<String>,
    /// Any conflicted predicate in the subject row (including linked-PR
    /// rows and lifecycle-family conflicts surfaced by CurrentTruth's
    /// `ResolveConflict` open action).
    pub conflicted: bool,
    /// Subject+predicate tokens naming the conflicted rows this section
    /// knows about, INCLUDING linked-PR rows (which are not carried on
    /// the issue subject). A conflict blocker must always carry the
    /// offending object — evidence-free conflict blockers are not
    /// actionable.
    pub conflict_refs: Vec<String>,
    /// R6-2 transition debt (owner-ruled semantics).
    pub transition_debt: TransitionDebtV1,
}

/// One claim row projected with reader-side staleness. Reader-side
/// staleness mirrors `is_claim_stale`: the read model never writes an
/// orphaning transition back to the claim store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimRowV1 {
    pub fact: WorkClaimFactV1,
    /// `Some(true)` when the heartbeat is older than the projection's
    /// TTL policy (caller-supplied); `None` when no TTL policy was given.
    pub effectively_expired: Option<bool>,
}

/// The claim section: every claim bound to the work item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimSectionV1 {
    pub claims: Vec<ClaimRowV1>,
}

/// Derived execution state of one run — typed from timestamps and exit
/// code, never from the receipt's prose token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionStateV1 {
    Running,
    Finished { exit_code: Option<i32> },
    Unknown,
}

impl ExecutionStateV1 {
    pub fn as_str(&self) -> String {
        match self {
            ExecutionStateV1::Running => "running".to_string(),
            ExecutionStateV1::Finished { exit_code } => {
                format!(
                    "finished:{}",
                    exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "no_exit_code".to_string())
                )
            }
            ExecutionStateV1::Unknown => "unknown".to_string(),
        }
    }
}

/// One run row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRowV1 {
    pub fact: RunReceiptFactV1,
    pub execution_state: ExecutionStateV1,
}

/// The run section: every receipt bound to the work item. The lifecycle
/// owner stays per-row — managed and attached modes never collapse into one
/// vocabulary (#1693 discrimination 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSectionV1 {
    pub runs: Vec<RunRowV1>,
}

/// One ExecEnv lease row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecEnvRowV1 {
    pub fact: ExecEnvFactV1,
}

/// Head drift evidence: the expected head the claim pinned, the head the
/// GitHub authority evidenced, and their disagreement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadDriftV1 {
    pub claim_expected_head: Option<String>,
    pub github_merge_sha: Option<String>,
    pub env_base_sha: Option<String>,
}

/// The ExecEnv section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecEnvSectionV1 {
    pub envs: Vec<ExecEnvRowV1>,
}

/// One verification observation projected verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationRowV1 {
    pub fact: VerificationFactV1,
}

/// The verification section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationSectionV1 {
    pub observations: Vec<VerificationRowV1>,
}

/// The adjudication section: the spine facts folded through the existing
/// taskintent mapping table (the ONLY place `AdjudicationState` is
/// produced). A worker submit/exit alone leaves it `Unreviewed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdjudicationSectionV1 {
    pub facts: Vec<AdjudicationFactV1>,
    pub state: AdjudicationState,
}

/// The delivery section. Honest by construction: #1679 is not integrated,
/// so the only reportable observation is [`DeliveryObservationV1::NotIntegrated`].
/// Delivery availability never rewrites execution or adjudication state
/// (#1693 discrimination 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliverySectionV1 {
    pub observation: DeliveryObservationV1,
}

/// What authority an action requires before anyone may execute it. Data
/// only — the projection never executes anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredAuthorityV1 {
    /// Owner (human) decision.
    Owner,
    /// Engineering-authority adjudication seat.
    AdjudicatorSeat,
    /// The current claim holder (handoff/release).
    ClaimHolder,
    /// A GitHub-credentialed actor (review/merge/close need their own
    /// authority and current evidence; the projection never performs them).
    GithubCredential,
    /// Source-adapter retry (refresh).
    SourceAdapterRefresh,
    /// No authority needed (informational awaiting).
    None,
}

impl RequiredAuthorityV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            RequiredAuthorityV1::Owner => "owner",
            RequiredAuthorityV1::AdjudicatorSeat => "adjudicator_seat",
            RequiredAuthorityV1::ClaimHolder => "claim_holder",
            RequiredAuthorityV1::GithubCredential => "github_credential",
            RequiredAuthorityV1::SourceAdapterRefresh => "source_adapter_refresh",
            RequiredAuthorityV1::None => "none",
        }
    }
}

/// The `next_action` vocabulary (#1693): a typed projection of
/// already-established state and prerequisites. Never an LLM plan; the
/// derivation table lives in `projector.rs` and invokes no model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NextActionKindV1 {
    /// Contradictory authoritative facts must be adjudicated first.
    ResolveConflict,
    /// A source refresh is unavailable; nothing downstream may present as
    /// fresh.
    RefreshUnavailableSource,
    /// Branch/worktree/head drift blocks complete/merge-ready; repair owner
    /// named.
    RepairHeadDrift,
    /// A merge was reverted or an issue reopened; repair first.
    RepairRevertOrReopen,
    /// Verification evidence is the missing prerequisite.
    RunVerification,
    /// A terminal submission awaits independent adjudication.
    Adjudicate,
    /// A claimed run has not finished.
    AwaitWorkerResult,
    /// Issue open, no implementation link.
    AwaitImplementation,
    /// Implementation present, owner acceptance record missing.
    AwaitOwnerAcceptance,
    /// The current claim holder should hand off or release (orphaned /
    /// reader-expired lease).
    HandoffOrRelease,
    /// Authorized GitHub review of an open implementation PR.
    AuthorizedGithubReview,
    /// Authorized GitHub merge — requires its own authority and current
    /// evidence.
    AuthorizedGithubMerge,
    /// Authorized GitHub close of an accepted issue.
    AuthorizedGithubClose,
}

impl NextActionKindV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            NextActionKindV1::ResolveConflict => "resolve_conflict",
            NextActionKindV1::RefreshUnavailableSource => "refresh_unavailable_source",
            NextActionKindV1::RepairHeadDrift => "repair_head_drift",
            NextActionKindV1::RepairRevertOrReopen => "repair_revert_or_reopen",
            NextActionKindV1::RunVerification => "run_verification",
            NextActionKindV1::Adjudicate => "adjudicate",
            NextActionKindV1::AwaitWorkerResult => "await_worker_result",
            NextActionKindV1::AwaitImplementation => "await_implementation",
            NextActionKindV1::AwaitOwnerAcceptance => "await_owner_acceptance",
            NextActionKindV1::HandoffOrRelease => "handoff_or_release",
            NextActionKindV1::AuthorizedGithubReview => "authorized_github_review",
            NextActionKindV1::AuthorizedGithubMerge => "authorized_github_merge",
            NextActionKindV1::AuthorizedGithubClose => "authorized_github_close",
        }
    }

    /// The frozen priority rank (lower = more urgent). Deterministic
    /// ordering for the action list; alternatives at different ranks are all
    /// exposed, never silently picked between.
    pub fn priority_rank(self) -> u8 {
        match self {
            NextActionKindV1::ResolveConflict => 0,
            NextActionKindV1::RefreshUnavailableSource => 1,
            NextActionKindV1::RepairHeadDrift => 2,
            NextActionKindV1::RepairRevertOrReopen => 3,
            NextActionKindV1::RunVerification => 4,
            NextActionKindV1::Adjudicate => 5,
            NextActionKindV1::AwaitWorkerResult => 6,
            NextActionKindV1::AwaitImplementation => 7,
            NextActionKindV1::AwaitOwnerAcceptance => 8,
            NextActionKindV1::HandoffOrRelease => 9,
            NextActionKindV1::AuthorizedGithubReview => 10,
            NextActionKindV1::AuthorizedGithubMerge => 11,
            NextActionKindV1::AuthorizedGithubClose => 12,
        }
    }
}

/// One projected next action with full provenance (#1693 `next_action`
/// law: kind / owner / required authority / prerequisite refs / blocker
/// refs / source revisions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextActionV1 {
    pub kind: NextActionKindV1,
    pub owner_class: ActionOwnerClassV1,
    pub required_authority: RequiredAuthorityV1,
    pub prerequisite_refs: Vec<String>,
    pub blocker_refs: Vec<String>,
    pub source_revisions: Vec<String>,
}

/// One blocker: what blocks success-shaped projection, who owns clearing
/// it, and the evidence refs that name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockerV1 {
    pub kind: BlockerKindV1,
    pub owner_class: ActionOwnerClassV1,
    pub evidence_refs: Vec<String>,
}

/// Blocker vocabulary — typed from established state only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockerKindV1 {
    /// Conflicted GitHub predicates (CurrentTruth).
    GithubConflict,
    /// A contributing source snapshot is unavailable.
    SourceUnavailable,
    /// Outstanding R6-2 transition debt (revert/reopen not causally
    /// resolved; steady-state freshness does not clear it).
    OutstandingTransitionDebt,
    /// Two or more active claims bind the same work item.
    ClaimCollision,
    /// The binding claim is orphaned or reader-expired.
    ClaimOrphaned,
    /// Expected head disagrees with the evidenced GitHub head.
    HeadDrift,
    /// Adjudication verdicts conflict.
    AdjudicationInconsistent,
    /// A terminal submission awaits adjudication (never success-shaped).
    AwaitingAdjudication,
    /// Verification evidence missing on a terminal submission.
    VerificationMissing,
}

impl BlockerKindV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            BlockerKindV1::GithubConflict => "github_conflict",
            BlockerKindV1::SourceUnavailable => "source_unavailable",
            BlockerKindV1::OutstandingTransitionDebt => "outstanding_transition_debt",
            BlockerKindV1::ClaimCollision => "claim_collision",
            BlockerKindV1::ClaimOrphaned => "claim_orphaned",
            BlockerKindV1::HeadDrift => "head_drift",
            BlockerKindV1::AdjudicationInconsistent => "adjudication_inconsistent",
            BlockerKindV1::AwaitingAdjudication => "awaiting_adjudication",
            BlockerKindV1::VerificationMissing => "verification_missing",
        }
    }
}

/// One work item's unified projection. Every section records which source
/// snapshot it came from (`source_stamps`), so staleness and rebuild
/// equivalence are auditable per row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkReadModelV1 {
    pub work_id: WorkKey,
    pub read_at: String,
    /// Deterministic revision fingerprint over the contributing source
    /// stamps (`kind@revision`, sorted) — the consumer-contract revision
    /// (#1693 discrimination 11: every consumer reads the same
    /// work id/revision/state).
    pub revision: String,
    pub github: SectionState<GithubSectionV1>,
    pub claim: SectionState<ClaimSectionV1>,
    pub run: SectionState<RunSectionV1>,
    pub exec_env: SectionState<ExecEnvSectionV1>,
    pub verification: SectionState<VerificationSectionV1>,
    pub adjudication: SectionState<AdjudicationSectionV1>,
    pub delivery: DeliverySectionV1,
    pub blockers: Vec<BlockerV1>,
    /// Alternatives in frozen priority order — multiple actions are exposed,
    /// never collapsed into one picked winner.
    pub next_actions: Vec<NextActionV1>,
    /// `true` only when no blocker stands, the GitHub section is available
    /// and unconflicted, and adjudication is Accepted/NotRequired. Unknown
    /// is never success-shaped.
    pub success_shaped: bool,
    pub source_stamps: Vec<SourceStamp>,
}

impl WorkReadModelV1 {
    /// The work token consumers key on.
    pub fn work_token(&self) -> String {
        self.work_id.as_token()
    }

    /// Whether the terminal run(s) can project complete: submission stays
    /// distinct from adjudication (#1693 discrimination 4).
    pub fn can_project_complete(&self) -> bool {
        self.success_shaped
    }
}

/// Content-free projection health over the visible set only — counts never
/// include hidden (private) work, so an unauthorized caller cannot infer
/// its existence (#1693 discrimination 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkProjectionHealthV1 {
    pub visible_work_count: usize,
    /// PRs with evidenced `merge_reverted` that no issue's CURRENT link
    /// set claims (R6-2 debt that the v1 consumer view cannot attribute
    /// per-issue after an unlink — counted so it cannot disappear
    /// silently; per-issue attribution is the #1696 integration-slice
    /// follow-up).
    pub orphaned_revert_debt_count: usize,
    /// Verification facts that can anchor to no work item at all (no
    /// parseable issue ref and no dispatch id joining a claim) — counted
    /// so they cannot vanish silently (codex R2 round-7 finding 4).
    pub unbound_verification_count: usize,
    pub conflicted_count: usize,
    pub blocked_count: usize,
    pub refresh_debt_count: usize,
}

/// The whole projected set for one read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkReadModelSetV1 {
    pub read_at: String,
    pub items: Vec<WorkReadModelV1>,
    pub health: WorkProjectionHealthV1,
}

/// Options for one projection read. All caller-supplied; the projector
/// reads no clock and no ambient policy.
#[derive(Debug, Clone)]
pub struct ProjectionOptions {
    /// RFC 3339 read time (audit/staleness stamp).
    pub read_at: String,
    /// Reader-side claim-heartbeat TTL in seconds. `None` disables
    /// reader-side staleness (recorded state only).
    pub claim_ttl_secs: Option<u64>,
    /// Caller authorization granted by the owning server surface.
    pub authorization: crate::current_truth::consumer::CallerAuthorizationV1,
}

impl ProjectionOptions {
    pub fn new(read_at: impl Into<String>) -> Self {
        ProjectionOptions {
            read_at: read_at.into(),
            claim_ttl_secs: None,
            authorization: crate::current_truth::consumer::CallerAuthorizationV1 {
                sees_private: false,
            },
        }
    }

    pub fn with_claim_ttl_secs(mut self, ttl: u64) -> Self {
        self.claim_ttl_secs = Some(ttl);
        self
    }

    pub fn with_sees_private(mut self, sees_private: bool) -> Self {
        self.authorization.sees_private = sees_private;
        self
    }
}

/// Predicate convenience: reduced status of one predicate on a subject row.
pub(crate) fn predicate_status(
    subject: &SubjectTruthViewV1,
    predicate: crate::current_truth::types::PredicateV1,
) -> ReductionStatusV1 {
    subject
        .predicates
        .iter()
        .find(|row| row.predicate == predicate)
        .map(|row| row.status)
        .unwrap_or(ReductionStatusV1::Unknown)
}

/// Predicate convenience: first value token for one predicate.
pub(crate) fn predicate_value(
    subject: &SubjectTruthViewV1,
    predicate: crate::current_truth::types::PredicateV1,
) -> Option<String> {
    subject
        .predicates
        .iter()
        .find(|row| row.predicate == predicate)
        .filter(|row| row.status == ReductionStatusV1::Current)
        .map(|row| row.value_token.clone())
}
