use super::*;

/// Unified GitHub facade — one tool for all GitHub operations.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiGhParams {
    /// Action to perform: "repo_view", "issue_list", "issue_read", "issue_create", "pr_list", "pr_read", "safe_merge"
    pub action: String,
    /// Repository in "owner/repo" format
    pub repo: String,
    /// Issue or PR number (required for issue_read, pr_read, safe_merge)
    #[serde(default)]
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
    #[serde(default)]
    pub limit: Option<u32>,
    /// Merge strategy for safe_merge: "merge", "squash", "rebase" (default: "squash")
    #[serde(default)]
    pub merge_strategy: Option<String>,
    /// When true, safe_merge evaluates the gate but does NOT call `gh pr merge` even if Ready
    #[serde(default)]
    pub dry_run: bool,
    /// Optional Tachi flow id; when provided, safe_merge persists status + event to .tachi/runs/<flow_id>/
    #[serde(default)]
    pub flow_id: Option<String>,
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
