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
