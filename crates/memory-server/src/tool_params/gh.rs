use super::*;

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
