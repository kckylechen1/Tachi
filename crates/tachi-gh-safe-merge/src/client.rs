use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{CheckRun, PrState};

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

/// One `pr_view` read plus the raw check runs it aggregated into
/// `pr.checks`, so a caller that also needs the granular list (the
/// check-state ledger) can reuse the same read instead of re-fetching it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrViewSnapshot {
    pub pr: PrState,
    /// The check runs `pr.checks` was aggregated from, when the client read
    /// them as part of this view. `None` means the client does not expose
    /// them; the caller must read `checks_list` itself.
    pub check_runs: Option<Vec<CheckRun>>,
}

/// Async I/O surface for GitHub. Production = `CliGhClient`, tests =
/// `MockGhClient`. Methods are intentionally narrow — the trait grows only
/// when a `tachi_gh` action needs a new capability, never speculatively.
#[async_trait]
pub trait GhClient: Send + Sync {
    async fn pr_view(&self, repo: &str, number: u64) -> Result<PrState, GhError>;
    /// `pr_view` plus the raw check runs behind `pr.checks`. The default
    /// exposes no check runs; transports that read them override this.
    async fn pr_view_snapshot(&self, repo: &str, number: u64) -> Result<PrViewSnapshot, GhError> {
        Ok(PrViewSnapshot {
            pr: self.pr_view(repo, number).await?,
            check_runs: None,
        })
    }
    /// The PR's live head SHA (`headRefOid`), as reported by GitHub now. The
    /// default reads the full `pr_view`; transports override it with a
    /// head-only query. Callers treat an empty string as unknown.
    async fn pr_head_sha(&self, repo: &str, number: u64) -> Result<String, GhError> {
        Ok(self.pr_view(repo, number).await?.head_sha)
    }
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
