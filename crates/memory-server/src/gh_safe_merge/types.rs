use serde::{Deserialize, Serialize};

/// Snapshot of PR state used by `evaluate_merge_gate`. Field names mirror the
/// `gh pr view --json` response keys so `CliGhClient` can deserialize without
/// rename gymnastics, but the type is otherwise plain data — no `gh`
/// dependency leaks into the gate logic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrState {
    pub number: u64,
    pub state: PrLifecycleState,
    /// `gh` `mergeable` field. `Mergeable::Unknown` means GitHub hasn't
    /// finished computing the merge-conflict check yet — treated as Pending.
    pub mergeable: Mergeable,
    /// `gh` `reviewDecision` field. `None` is policy-dependent: permissive
    /// mode treats it as no review policy, while standard/strict wait instead
    /// of silently treating missing review data as approval.
    pub review_decision: Option<ReviewDecision>,
    /// Aggregated check status, derived from `gh pr checks` (or any equivalent
    /// CI surface). Computed by `ChecksState::aggregate`.
    pub checks: ChecksState,
    /// `true` when the PR is still a draft. Drafts are unconditionally
    /// `Blocked` regardless of CI / review state — they aren't ready to merge
    /// by definition.
    pub is_draft: bool,
    /// HEAD branch SHA at the time of the gate evaluation. Surfaced into the
    /// `github_merge_blocked` / `github_pr_merged` events so an audit can pin
    /// down exactly which commit was (or wasn't) merged.
    pub head_sha: String,
    /// GitHub closing issue references associated with the PR, e.g. values
    /// from `closingIssuesReferences`. Strict policy accepts either one of
    /// these links or an explicit Tachi `flow_id`.
    #[serde(default)]
    pub linked_issue_refs: Vec<String>,
    /// Labels fetched for each GitHub issue that this PR would close on merge.
    /// `closingIssuesReferences` itself does not include labels, so production
    /// clients enrich this field after parsing the pure PR-view payload.
    #[serde(default)]
    pub closing_issue_labels: Vec<ClosingIssueLabels>,
    /// Whether the caller has proven the merge gate input is current for
    /// `head_sha`. `None` means the caller has not evaluated that proof.
    #[serde(default)]
    pub head_consistent: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosingIssueLabels {
    pub reference: String,
    /// The closing issue's label names, or `None` when the label lookup failed
    /// (transient GitHub error, or an unparsable ref). `None` fails the merge
    /// CLOSED — we cannot prove the issue is safe to auto-close and a wrong
    /// close is irreversible. `Some(vec![])` means the lookup succeeded and the
    /// issue simply has no labels (safe to close).
    pub labels: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeGatePolicyMode {
    Permissive,
    Standard,
    Strict,
}

impl MergeGatePolicyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            MergeGatePolicyMode::Permissive => "permissive",
            MergeGatePolicyMode::Standard => "standard",
            MergeGatePolicyMode::Strict => "strict",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeGatePolicy {
    pub mode: MergeGatePolicyMode,
    pub require_checks: bool,
    pub allow_missing_checks: bool,
    pub require_review_approval: bool,
    pub allow_missing_review_decision: bool,
    pub require_linked_issue_or_flow: bool,
    pub require_head_consistency: bool,
    pub block_protected_umbrella_close: bool,
}

impl MergeGatePolicy {
    pub fn permissive() -> Self {
        Self {
            mode: MergeGatePolicyMode::Permissive,
            require_checks: false,
            allow_missing_checks: true,
            require_review_approval: false,
            allow_missing_review_decision: true,
            require_linked_issue_or_flow: false,
            require_head_consistency: false,
            block_protected_umbrella_close: true,
        }
    }

    pub fn standard() -> Self {
        Self {
            mode: MergeGatePolicyMode::Standard,
            require_checks: true,
            allow_missing_checks: false,
            require_review_approval: true,
            allow_missing_review_decision: false,
            require_linked_issue_or_flow: false,
            require_head_consistency: false,
            block_protected_umbrella_close: true,
        }
    }

    pub fn strict() -> Self {
        Self {
            mode: MergeGatePolicyMode::Strict,
            require_checks: true,
            allow_missing_checks: false,
            require_review_approval: true,
            allow_missing_review_decision: false,
            require_linked_issue_or_flow: true,
            require_head_consistency: true,
            block_protected_umbrella_close: true,
        }
    }

    pub fn from_mode(mode: MergeGatePolicyMode) -> Self {
        match mode {
            MergeGatePolicyMode::Permissive => Self::permissive(),
            MergeGatePolicyMode::Standard => Self::standard(),
            MergeGatePolicyMode::Strict => Self::strict(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PrLifecycleState {
    Open,
    Closed,
    Merged,
}

/// Mirrors GitHub's `mergeable` / `mergeStateStatus` response. We keep the
/// vocabulary minimal: anything we can't act on safely is folded into
/// `Unknown` so `evaluate_merge_gate` errs on the side of waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Mergeable {
    /// `mergeable: MERGEABLE` and no conflicts.
    Mergeable,
    /// `mergeable: CONFLICTING` — base/head diverged.
    Conflicting,
    /// `mergeable: UNKNOWN` — GitHub still computing.
    Unknown,
}

/// Mirrors GitHub's `reviewDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

/// Aggregated check-suite status across all configured checks for the PR
/// HEAD SHA. Computed from a `Vec<CheckRun>` by `ChecksState::aggregate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecksState {
    /// No check runs were reported. This is policy-dependent: permissive mode
    /// allows it for repos without CI, while standard/strict wait because the
    /// API cannot distinguish "no CI" from "checks missing/not started".
    None,
    /// At least one check is still pending and none have failed.
    Pending,
    /// GitHub reported checks, but every completed check was skipped. This is
    /// distinct from a passed suite because no required verification actually ran.
    Skipped,
    /// Every check has completed with a successful conclusion.
    Success,
    /// At least one required check has a failure / cancelled / timed-out
    /// conclusion.
    Failure,
}

/// Single CI check run, loosely mirroring `gh pr checks --json` output. We
/// only care about the fields needed to drive `ChecksState::aggregate` and
/// the per-check breakdown surfaced in the `github_checks_polled` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRun {
    pub name: String,
    /// `null` for in-progress runs, otherwise one of `success | failure |
    /// cancelled | timed_out | action_required | neutral | skipped`.
    pub conclusion: Option<String>,
    /// `queued | in_progress | completed`.
    pub status: String,
}

impl ChecksState {
    /// Fold a list of check runs into a single aggregate state. Order:
    /// any failure-class conclusion → `Failure`; any non-completed status
    /// → `Pending`; empty input → `None`; otherwise `Success`.
    pub fn aggregate(runs: &[CheckRun]) -> Self {
        if runs.is_empty() {
            return ChecksState::None;
        }
        let mut any_pending = false;
        let mut any_ran_successfully = false;
        let mut any_skipped = false;
        for run in runs {
            if run.status != "completed" {
                any_pending = true;
                continue;
            }
            match run.conclusion.as_deref() {
                Some("success") | Some("neutral") => any_ran_successfully = true,
                Some("skipped") => any_skipped = true,
                Some(_other) => return ChecksState::Failure,
                None => any_pending = true,
            }
        }
        if any_pending {
            ChecksState::Pending
        } else if any_skipped && !any_ran_successfully {
            ChecksState::Skipped
        } else {
            ChecksState::Success
        }
    }
}

/// Outcome of `evaluate_merge_gate`. Maps 1:1 to the `merge_state` vocabulary
/// in `status.json`'s `github` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum MergeDecision {
    /// Every gate passed. Caller MAY now invoke `pr_merge`. The decision
    /// itself does NOT perform the merge — that's a separate explicit step.
    Ready,
    /// At least one gate is hard-red. `reasons` is a human-readable list of
    /// short codes (e.g. `"draft"`, `"checks:failure"`, `"review:changes_requested"`,
    /// `"mergeable:conflicting"`, `"state:closed"`). Surfaced verbatim into
    /// the `github_merge_blocked` event payload.
    Blocked { reasons: Vec<String> },
    /// At least one gate is soft-yellow (still computing). `waiting_on` lists
    /// the gates the caller should re-poll. No event is emitted for pending
    /// — it's the steady state during polling.
    Pending { waiting_on: Vec<String> },
}

impl MergeDecision {
    /// Map this decision to the `merge_state` string used in
    /// `status.json::github::merge_state`.
    pub fn merge_state_label(&self) -> &'static str {
        match self {
            MergeDecision::Ready => "ready",
            MergeDecision::Blocked { .. } => "blocked",
            MergeDecision::Pending { .. } => "pending",
        }
    }
}
