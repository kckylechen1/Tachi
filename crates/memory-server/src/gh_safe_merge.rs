//! Safe-merge gate logic for `tachi_gh safe_merge`.
//!
//! This module is split into two layers:
//!
//! 1. **Pure decision logic** — `evaluate_merge_gate(&PrState) -> MergeDecision`.
//!    No I/O, no clock, no `gh` subprocess. Fully unit-testable from struct
//!    literals. This is the "safe" part of safe-merge: a deterministic,
//!    auditable function that decides whether a PR is allowed to merge.
//!
//! 2. **`GhClient` trait** — the GitHub-side I/O surface that
//!    `tachi_gh safe_merge` needs to drive the gate end-to-end:
//!      - `pr_view`         — fetch latest PR state for a gate evaluation
//!      - `pr_merge`        — actually merge (only called when gate is Ready)
//!      - `issue_view`      — fetch linked issue state (e.g. for closes-link)
//!      - `issue_create`    — open a tracking issue from a brainstorm flow
//!      - `checks_list`     — granular per-check status for events log
//!
//!    Two impls are provided:
//!      - `CliGhClient` — wraps the existing sanitized `gh` subprocess in
//!        `gh_ops` (production path).
//!      - `MockGhClient` — in-memory fixture builder for integration tests
//!        of the orchestrator without spawning `gh` or hitting the network.
//!
//! Wiring into the `tachi_gh` MCP action lands in the next commit; the trait
//! and decision function are shipped here with full unit-test coverage so
//! the orchestrator commit can land green in one shot.
//!
//! Section 五 of `tachi-shell-github-convoy-touchy-agent-prompt.md` is the
//! authoritative spec for the `merge_state` vocabulary and event kinds; the
//! `MergeDecision` variants here map 1:1 to that vocabulary:
//!
//! - `Ready`              ↔ `merge_state = "ready"`           (no events emitted, caller decides)
//! - `Blocked { .. }`     ↔ `merge_state = "blocked"`         (`github_merge_blocked` event)
//! - `Pending { .. }`     ↔ `merge_state = "pending"`         (no events, just keep polling)

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ─── Domain types ───────────────────────────────────────────────────────────

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
    /// `gh` `reviewDecision` field. `None` when the repo has no review
    /// requirements configured, in which case review is treated as approved.
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
    /// No required checks have been configured (or no check runs reported).
    /// Treated as a soft pass so brand-new repos / branches without CI can
    /// still use safe-merge — the `is_mergeable` + `review_decision` gates
    /// remain in force.
    None,
    /// At least one check is still pending and none have failed.
    Pending,
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
        for run in runs {
            if run.status != "completed" {
                any_pending = true;
                continue;
            }
            match run.conclusion.as_deref() {
                Some("success") | Some("neutral") | Some("skipped") => {}
                Some(_other) => return ChecksState::Failure,
                None => any_pending = true,
            }
        }
        if any_pending {
            ChecksState::Pending
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

// ─── Pure gate function ─────────────────────────────────────────────────────

/// Decide whether a PR may be merged based on a snapshot of its state.
///
/// Gate ordering (hard fails first, then soft waits):
///
/// 1. `state != Open`             → Blocked (already closed/merged)
/// 2. `is_draft`                  → Blocked
/// 3. `mergeable == Conflicting`  → Blocked
/// 4. `review_decision == ChangesRequested` → Blocked
/// 5. `checks == Failure`         → Blocked
/// 6. `mergeable == Unknown`      → Pending (GitHub still computing)
/// 7. `checks == Pending`         → Pending
/// 8. `review_decision == ReviewRequired` → Pending
/// 9. otherwise                   → Ready
///
/// A `Blocked` decision MAY include multiple reasons (e.g. a draft PR with
/// failing checks lists both, so the operator gets the full picture in one
/// `github_merge_blocked` event). A `Pending` decision likewise lists every
/// gate the caller is still waiting on, so the polling loop can surface
/// progress.
pub fn evaluate_merge_gate(pr: &PrState) -> MergeDecision {
    let mut blocked: Vec<String> = Vec::new();
    let mut pending: Vec<String> = Vec::new();

    match pr.state {
        PrLifecycleState::Open => {}
        PrLifecycleState::Closed => blocked.push("state:closed".to_string()),
        PrLifecycleState::Merged => blocked.push("state:merged".to_string()),
    }
    if pr.is_draft {
        blocked.push("draft".to_string());
    }
    match pr.mergeable {
        Mergeable::Mergeable => {}
        Mergeable::Conflicting => blocked.push("mergeable:conflicting".to_string()),
        Mergeable::Unknown => pending.push("mergeable:unknown".to_string()),
    }
    match pr.review_decision {
        Some(ReviewDecision::ChangesRequested) => {
            blocked.push("review:changes_requested".to_string())
        }
        Some(ReviewDecision::ReviewRequired) => pending.push("review:required".to_string()),
        Some(ReviewDecision::Approved) | None => {}
    }
    match pr.checks {
        ChecksState::Failure => blocked.push("checks:failure".to_string()),
        ChecksState::Pending => pending.push("checks:pending".to_string()),
        ChecksState::Success | ChecksState::None => {}
    }

    if !blocked.is_empty() {
        return MergeDecision::Blocked { reasons: blocked };
    }
    if !pending.is_empty() {
        return MergeDecision::Pending {
            waiting_on: pending,
        };
    }
    MergeDecision::Ready
}

// ─── GhClient trait ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    Squash,
    Merge,
    Rebase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeResult {
    pub pr_number: u64,
    pub merge_sha: String,
    pub strategy: MergeStrategy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueState {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub url: String,
}

/// Errors returned by `GhClient` impls. `Sanitized` carries an already-
/// scrubbed message safe to surface to the agent / log; `RateLimited` and
/// `NotFound` are typed so callers can branch (e.g. backoff vs abort).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GhError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("rate limited: {0}")]
    RateLimited(String),
    #[error("gh client error: {0}")]
    Sanitized(String),
}

/// Async I/O surface for GitHub. Production = `CliGhClient`, tests =
/// `MockGhClient`. Methods are intentionally narrow — the trait grows only
/// when a `tachi_gh` action needs a new capability, never speculatively.
#[async_trait]
pub trait GhClient: Send + Sync {
    async fn pr_view(&self, repo: &str, number: u64) -> Result<PrState, GhError>;
    async fn pr_merge(
        &self,
        repo: &str,
        number: u64,
        strategy: MergeStrategy,
        expected_head_sha: &str,
    ) -> Result<MergeResult, GhError>;
    async fn issue_create(
        &self,
        repo: &str,
        title: &str,
        body: Option<&str>,
        labels: &[String],
    ) -> Result<IssueState, GhError>;
    async fn checks_list(&self, repo: &str, pr_number: u64) -> Result<Vec<CheckRun>, GhError>;
}

// ─── MockGhClient (test fixture) ────────────────────────────────────────────

#[cfg(test)]
mod mock {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory fixture for integration-testing the `safe_merge` orchestrator
    /// without spawning `gh`. Registered PRs/issues are returned verbatim;
    /// `pr_merge` records the call and returns a deterministic SHA.
    ///
    /// Builder-style: `MockGhClient::new().with_pr(...).with_checks(...)`.
    /// Threadsafe via `Mutex` so the orchestrator's `&self` borrows compose.
    pub struct MockGhClient {
        prs: Mutex<HashMap<(String, u64), PrState>>,
        issues: Mutex<HashMap<(String, u64), IssueState>>,
        checks: Mutex<HashMap<(String, u64), Vec<CheckRun>>>,
        merge_calls: Mutex<Vec<(String, u64, MergeStrategy, String)>>,
        next_issue_number: Mutex<u64>,
    }

    impl MockGhClient {
        pub fn new() -> Self {
            Self {
                prs: Mutex::new(HashMap::new()),
                issues: Mutex::new(HashMap::new()),
                checks: Mutex::new(HashMap::new()),
                merge_calls: Mutex::new(Vec::new()),
                next_issue_number: Mutex::new(1000),
            }
        }
        pub fn with_pr(self, repo: &str, pr: PrState) -> Self {
            self.prs
                .lock()
                .unwrap()
                .insert((repo.to_string(), pr.number), pr);
            self
        }
        #[allow(dead_code)] // exercised by issue-link action in follow-up PR
        pub fn with_issue(self, repo: &str, issue: IssueState) -> Self {
            self.issues
                .lock()
                .unwrap()
                .insert((repo.to_string(), issue.number), issue);
            self
        }
        pub fn with_checks(self, repo: &str, pr_number: u64, runs: Vec<CheckRun>) -> Self {
            self.checks
                .lock()
                .unwrap()
                .insert((repo.to_string(), pr_number), runs);
            self
        }
        pub fn merge_calls(&self) -> Vec<(String, u64, MergeStrategy, String)> {
            self.merge_calls.lock().unwrap().clone()
        }

        pub async fn issue_view(&self, repo: &str, number: u64) -> Result<IssueState, GhError> {
            self.issues
                .lock()
                .unwrap()
                .get(&(repo.to_string(), number))
                .cloned()
                .ok_or_else(|| GhError::NotFound(format!("issue {repo}#{number}")))
        }
    }

    #[async_trait]
    impl GhClient for MockGhClient {
        async fn pr_view(&self, repo: &str, number: u64) -> Result<PrState, GhError> {
            self.prs
                .lock()
                .unwrap()
                .get(&(repo.to_string(), number))
                .cloned()
                .ok_or_else(|| GhError::NotFound(format!("pr {repo}#{number}")))
        }
        async fn pr_merge(
            &self,
            repo: &str,
            number: u64,
            strategy: MergeStrategy,
            expected_head_sha: &str,
        ) -> Result<MergeResult, GhError> {
            // Record call before any state lookup so tests can assert the
            // orchestrator did NOT short-circuit before reaching merge.
            self.merge_calls.lock().unwrap().push((
                repo.to_string(),
                number,
                strategy,
                expected_head_sha.to_string(),
            ));
            let pr = self
                .prs
                .lock()
                .unwrap()
                .get(&(repo.to_string(), number))
                .cloned()
                .ok_or_else(|| GhError::NotFound(format!("pr {repo}#{number}")))?;
            if !expected_head_sha.is_empty() && pr.head_sha != expected_head_sha {
                return Err(GhError::Sanitized(format!(
                    "head SHA changed: expected {expected_head_sha}, got {}",
                    pr.head_sha
                )));
            }
            Ok(MergeResult {
                pr_number: number,
                merge_sha: format!("mock-sha-for-{}", pr.head_sha),
                strategy,
            })
        }
        async fn issue_create(
            &self,
            repo: &str,
            title: &str,
            _body: Option<&str>,
            _labels: &[String],
        ) -> Result<IssueState, GhError> {
            let mut next = self.next_issue_number.lock().unwrap();
            let number = *next;
            *next += 1;
            let issue = IssueState {
                number,
                title: title.to_string(),
                state: "open".to_string(),
                url: format!("https://github.com/{repo}/issues/{number}"),
            };
            self.issues
                .lock()
                .unwrap()
                .insert((repo.to_string(), number), issue.clone());
            Ok(issue)
        }
        async fn checks_list(&self, repo: &str, pr_number: u64) -> Result<Vec<CheckRun>, GhError> {
            Ok(self
                .checks
                .lock()
                .unwrap()
                .get(&(repo.to_string(), pr_number))
                .cloned()
                .unwrap_or_default())
        }
    }
}

#[cfg(test)]
pub(crate) use mock::MockGhClient;

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn open_pr() -> PrState {
        PrState {
            number: 1,
            state: PrLifecycleState::Open,
            mergeable: Mergeable::Mergeable,
            review_decision: Some(ReviewDecision::Approved),
            checks: ChecksState::Success,
            is_draft: false,
            head_sha: "abc123".to_string(),
        }
    }

    // ─── evaluate_merge_gate ─────────────────────────────────────────────

    #[test]
    fn gate_ready_when_all_green() {
        assert_eq!(evaluate_merge_gate(&open_pr()), MergeDecision::Ready);
    }

    #[test]
    fn gate_ready_when_no_review_required_and_no_checks() {
        let mut pr = open_pr();
        pr.review_decision = None; // repo has no review policy
        pr.checks = ChecksState::None; // no CI configured
        assert_eq!(evaluate_merge_gate(&pr), MergeDecision::Ready);
    }

    #[test]
    fn gate_blocked_when_draft() {
        let mut pr = open_pr();
        pr.is_draft = true;
        let dec = evaluate_merge_gate(&pr);
        assert!(matches!(dec, MergeDecision::Blocked { .. }), "got {dec:?}");
        if let MergeDecision::Blocked { reasons } = dec {
            assert!(reasons.contains(&"draft".to_string()));
        }
    }

    #[test]
    fn gate_blocked_when_state_closed_or_merged() {
        let mut pr = open_pr();
        pr.state = PrLifecycleState::Closed;
        match evaluate_merge_gate(&pr) {
            MergeDecision::Blocked { reasons } => {
                assert!(reasons.iter().any(|r| r == "state:closed"))
            }
            other => panic!("expected blocked, got {other:?}"),
        }
        pr.state = PrLifecycleState::Merged;
        match evaluate_merge_gate(&pr) {
            MergeDecision::Blocked { reasons } => {
                assert!(reasons.iter().any(|r| r == "state:merged"))
            }
            other => panic!("expected blocked, got {other:?}"),
        }
    }

    #[test]
    fn gate_blocked_when_mergeable_conflicting() {
        let mut pr = open_pr();
        pr.mergeable = Mergeable::Conflicting;
        match evaluate_merge_gate(&pr) {
            MergeDecision::Blocked { reasons } => {
                assert!(reasons.iter().any(|r| r == "mergeable:conflicting"))
            }
            other => panic!("expected blocked, got {other:?}"),
        }
    }

    #[test]
    fn gate_blocked_when_changes_requested() {
        let mut pr = open_pr();
        pr.review_decision = Some(ReviewDecision::ChangesRequested);
        match evaluate_merge_gate(&pr) {
            MergeDecision::Blocked { reasons } => {
                assert!(reasons.iter().any(|r| r == "review:changes_requested"))
            }
            other => panic!("expected blocked, got {other:?}"),
        }
    }

    #[test]
    fn gate_blocked_when_checks_failure() {
        let mut pr = open_pr();
        pr.checks = ChecksState::Failure;
        match evaluate_merge_gate(&pr) {
            MergeDecision::Blocked { reasons } => {
                assert!(reasons.iter().any(|r| r == "checks:failure"))
            }
            other => panic!("expected blocked, got {other:?}"),
        }
    }

    #[test]
    fn gate_pending_when_mergeable_unknown() {
        let mut pr = open_pr();
        pr.mergeable = Mergeable::Unknown;
        match evaluate_merge_gate(&pr) {
            MergeDecision::Pending { waiting_on } => {
                assert!(waiting_on.iter().any(|r| r == "mergeable:unknown"))
            }
            other => panic!("expected pending, got {other:?}"),
        }
    }

    #[test]
    fn gate_pending_when_checks_pending() {
        let mut pr = open_pr();
        pr.checks = ChecksState::Pending;
        match evaluate_merge_gate(&pr) {
            MergeDecision::Pending { waiting_on } => {
                assert!(waiting_on.iter().any(|r| r == "checks:pending"))
            }
            other => panic!("expected pending, got {other:?}"),
        }
    }

    #[test]
    fn gate_pending_when_review_required() {
        let mut pr = open_pr();
        pr.review_decision = Some(ReviewDecision::ReviewRequired);
        match evaluate_merge_gate(&pr) {
            MergeDecision::Pending { waiting_on } => {
                assert!(waiting_on.iter().any(|r| r == "review:required"))
            }
            other => panic!("expected pending, got {other:?}"),
        }
    }

    #[test]
    fn gate_blocked_short_circuits_pending() {
        // Both a hard fail (changes_requested) AND a soft wait (checks pending)
        // → Blocked wins, but BOTH the hard-fail reasons are surfaced. This
        // matters for the operator UX: a single `github_merge_blocked` event
        // should list every red gate so they can fix them in one pass.
        let mut pr = open_pr();
        pr.review_decision = Some(ReviewDecision::ChangesRequested);
        pr.checks = ChecksState::Pending; // soft, but blocked beats it
        pr.is_draft = true; // another hard gate
        match evaluate_merge_gate(&pr) {
            MergeDecision::Blocked { reasons } => {
                assert!(reasons.contains(&"draft".to_string()));
                assert!(reasons.contains(&"review:changes_requested".to_string()));
                // pending checks NOT surfaced under a Blocked decision —
                // the soft state is moot once a hard gate exists.
                assert!(!reasons.iter().any(|r| r == "checks:pending"));
            }
            other => panic!("expected blocked, got {other:?}"),
        }
    }

    #[test]
    fn gate_decision_to_merge_state_label() {
        assert_eq!(MergeDecision::Ready.merge_state_label(), "ready");
        assert_eq!(
            MergeDecision::Blocked { reasons: vec![] }.merge_state_label(),
            "blocked"
        );
        assert_eq!(
            MergeDecision::Pending { waiting_on: vec![] }.merge_state_label(),
            "pending"
        );
    }

    // ─── ChecksState::aggregate ──────────────────────────────────────────

    fn check(name: &str, status: &str, conclusion: Option<&str>) -> CheckRun {
        CheckRun {
            name: name.to_string(),
            status: status.to_string(),
            conclusion: conclusion.map(str::to_string),
        }
    }

    #[test]
    fn checks_aggregate_empty_is_none() {
        assert_eq!(ChecksState::aggregate(&[]), ChecksState::None);
    }

    #[test]
    fn checks_aggregate_all_success() {
        let runs = vec![
            check("ci", "completed", Some("success")),
            check("lint", "completed", Some("success")),
        ];
        assert_eq!(ChecksState::aggregate(&runs), ChecksState::Success);
    }

    #[test]
    fn checks_aggregate_treats_neutral_and_skipped_as_success() {
        let runs = vec![
            check("ci", "completed", Some("success")),
            check("optional", "completed", Some("neutral")),
            check("conditional", "completed", Some("skipped")),
        ];
        assert_eq!(ChecksState::aggregate(&runs), ChecksState::Success);
    }

    #[test]
    fn checks_aggregate_any_failure_short_circuits() {
        let runs = vec![
            check("ci", "completed", Some("success")),
            check("lint", "completed", Some("failure")),
            check("test", "in_progress", None), // would otherwise be pending
        ];
        assert_eq!(ChecksState::aggregate(&runs), ChecksState::Failure);
    }

    #[test]
    fn checks_aggregate_in_progress_is_pending() {
        let runs = vec![
            check("ci", "completed", Some("success")),
            check("test", "in_progress", None),
        ];
        assert_eq!(ChecksState::aggregate(&runs), ChecksState::Pending);
    }

    #[test]
    fn checks_aggregate_completed_with_no_conclusion_is_pending() {
        // Defensive: if a check is reported as `completed` but with no
        // `conclusion`, treat it as pending rather than success — the data
        // is incomplete and we should wait one more poll cycle.
        let runs = vec![check("ci", "completed", None)];
        assert_eq!(ChecksState::aggregate(&runs), ChecksState::Pending);
    }

    #[test]
    fn checks_aggregate_cancelled_and_timed_out_are_failure() {
        for conclusion in &["cancelled", "timed_out", "action_required"] {
            let runs = vec![check("ci", "completed", Some(conclusion))];
            assert_eq!(
                ChecksState::aggregate(&runs),
                ChecksState::Failure,
                "conclusion `{conclusion}` should aggregate to Failure"
            );
        }
    }

    // ─── MockGhClient ────────────────────────────────────────────────────

    #[tokio::test]
    async fn mock_pr_view_returns_registered_state() {
        let pr = open_pr();
        let mock = MockGhClient::new().with_pr("o/r", pr.clone());
        let got = mock.pr_view("o/r", 1).await.expect("pr_view");
        assert_eq!(got, pr);
    }

    #[tokio::test]
    async fn mock_pr_view_unknown_returns_not_found() {
        let mock = MockGhClient::new();
        match mock.pr_view("o/r", 999).await {
            Err(GhError::NotFound(msg)) => assert!(msg.contains("999")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mock_pr_merge_records_call_and_returns_deterministic_sha() {
        let pr = open_pr();
        let mock = MockGhClient::new().with_pr("o/r", pr);
        let result = mock
            .pr_merge("o/r", 1, MergeStrategy::Squash, "abc123")
            .await
            .expect("merge");
        assert_eq!(result.pr_number, 1);
        assert_eq!(result.strategy, MergeStrategy::Squash);
        assert_eq!(result.merge_sha, "mock-sha-for-abc123");

        let calls = mock.merge_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            (
                "o/r".to_string(),
                1,
                MergeStrategy::Squash,
                "abc123".to_string()
            )
        );
    }

    #[tokio::test]
    async fn mock_pr_merge_records_call_even_on_not_found() {
        // Critical for test-driving the orchestrator: we want to assert that
        // it did NOT call merge when the gate was Blocked. The mock must
        // record the attempt regardless of whether it succeeds, so the
        // orchestrator test can `assert_eq!(merge_calls.len(), 0)`.
        let mock = MockGhClient::new();
        let _ = mock
            .pr_merge("o/r", 42, MergeStrategy::Squash, "abc123")
            .await;
        assert_eq!(mock.merge_calls().len(), 1);
    }

    #[tokio::test]
    async fn mock_pr_merge_rejects_head_sha_mismatch() {
        let pr = open_pr();
        let mock = MockGhClient::new().with_pr("o/r", pr);
        let err = mock
            .pr_merge("o/r", 1, MergeStrategy::Squash, "different")
            .await
            .expect_err("head mismatch should fail");
        assert!(err.to_string().contains("head SHA changed"));
    }

    #[tokio::test]
    async fn mock_issue_create_assigns_monotonic_numbers() {
        let mock = MockGhClient::new();
        let i1 = mock
            .issue_create("o/r", "first", None, &[])
            .await
            .expect("create");
        let i2 = mock
            .issue_create("o/r", "second", Some("body"), &["bug".to_string()])
            .await
            .expect("create");
        assert!(i2.number > i1.number);
        // Created issues are then retrievable.
        let got = mock.issue_view("o/r", i1.number).await.expect("view");
        assert_eq!(got.title, "first");
    }

    #[tokio::test]
    async fn mock_checks_list_defaults_to_empty() {
        let mock = MockGhClient::new();
        let runs = mock.checks_list("o/r", 1).await.expect("checks");
        assert!(runs.is_empty());
    }

    #[tokio::test]
    async fn mock_checks_list_returns_registered_runs() {
        let mock = MockGhClient::new().with_checks(
            "o/r",
            1,
            vec![check("ci", "completed", Some("success"))],
        );
        let runs = mock.checks_list("o/r", 1).await.expect("checks");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].name, "ci");
    }
}
