use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

/// Unified GitHub facade — one tool for all GitHub operations.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiGhParams {
    /// Action to perform: "repo_view", "issue_list", "issue_read", "issue_create", "pr_list", "pr_read", "pr_comments", "pr_review_digest", "safe_merge"
    pub action: String,
    /// Repository in "owner/repo" format
    pub repo: String,
    /// Issue or PR number (required for issue_read, pr_read, pr_comments, pr_review_digest, safe_merge)
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u64_from_string_or_number"
    )]
    pub number: Option<u64>,
    /// Issue title (required for issue_create)
    #[serde(default)]
    pub title: Option<String>,
    /// Issue/PR body (optional, used by issue_create)
    #[serde(default)]
    pub body: Option<String>,
    /// Labels (optional, used by issue_create and issue_list filter)
    #[serde(default)]
    pub labels: Vec<String>,
    /// Filter by state: "open", "closed", "merged", "all" (used by issue_list, pr_list)
    #[serde(default)]
    pub state: Option<String>,
    /// Maximum results (used by issue_list, pr_list, default: 30)
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u32_from_string_or_number"
    )]
    pub limit: Option<u32>,
    /// Merge strategy for safe_merge: "merge", "squash", "rebase" (default: "squash").
    /// Applies only to the GitHub PR merge path, not local worktree merging.
    #[serde(default)]
    pub merge_strategy: Option<String>,
    /// When true, safe_merge returns a preview and does NOT call `gh pr merge`
    /// even if the gate is Ready. Defaults to true unless confirm=true is supplied.
    #[serde(default)]
    pub dry_run: Option<bool>,
    /// Explicit confirmation required to execute `gh pr merge` when the gate is Ready.
    /// Use confirm=false or dry_run=true for a preflight-only preview.
    #[serde(default)]
    pub confirm: bool,
    /// Optional Tachi flow id; when provided, safe_merge persists status + event to .tachi/runs/<flow_id>/
    #[serde(default)]
    pub flow_id: Option<String>,
    /// Merge gate policy mode: permissive | standard | strict. Defaults to standard.
    /// Standard waits on missing checks or missing review decisions instead of treating them as green.
    #[serde(default)]
    pub merge_policy: Option<String>,
    /// Optional author/login substring for pr_review_digest. Defaults to "gemini".
    #[serde(default)]
    pub author_filter: Option<String>,
    /// Write pr_review_digest artifacts under .tachi/reviews. Defaults to true.
    #[serde(default)]
    pub write_digest: Option<bool>,
}

/// Parameters for reading a GitHub issue
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct GhIssueReadParams {
    /// Repository in "owner/repo" format
    pub repo: String,
    /// Issue number
    pub issue_number: u64,
}

/// Parameters for listing GitHub issues
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct GhIssueListParams {
    /// Repository in "owner/repo" format
    pub repo: String,
    /// Filter by state: "open", "closed", "all" (default: "open")
    #[serde(default = "default_issue_state")]
    pub state: String,
    /// Filter by labels (comma-separated)
    #[serde(default)]
    pub labels: Option<String>,
    /// Maximum results to return (default: 30)
    #[serde(default = "default_gh_limit")]
    pub limit: u32,
}

/// Parameters for creating a GitHub issue
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct GhIssueCreateParams {
    /// Repository in "owner/repo" format
    pub repo: String,
    /// Issue title
    pub title: String,
    /// Issue body (markdown)
    #[serde(default)]
    pub body: Option<String>,
    /// Labels to add
    #[serde(default)]
    pub labels: Vec<String>,
}

/// Parameters for reading a GitHub PR
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct GhPrReadParams {
    /// Repository in "owner/repo" format
    pub repo: String,
    /// PR number
    pub pr_number: u64,
}

/// Parameters for reading GitHub PR review submissions and inline comments
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct GhPrCommentsParams {
    /// Repository in "owner/repo" format
    pub repo: String,
    /// PR number
    pub pr_number: u64,
}

/// Parameters for listing GitHub PRs
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct GhPrListParams {
    /// Repository in "owner/repo" format
    pub repo: String,
    /// Filter by state: "open", "closed", "merged", "all" (default: "open")
    #[serde(default = "default_issue_state")]
    pub state: String,
    /// Maximum results to return (default: 30)
    #[serde(default = "default_gh_limit")]
    pub limit: u32,
}

/// Parameters for viewing repository info
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct GhRepoViewParams {
    /// Repository in "owner/repo" format
    pub repo: String,
}

fn default_issue_state() -> String {
    "open".to_string()
}

fn default_gh_limit() -> u32 {
    30
}
