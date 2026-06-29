use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

/// Unified GitHub facade — one tool for all GitHub operations.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct TachiGhParams {
    /// Action to perform: "repo_view", "issue_list", "issue_read", "issue_create", "issue_comment", "pr_list", "pr_read", "pr_comments", "pr_comment", "pr_review_digest", "safe_merge", "link_pr", "pr_status", "pr_handoff", "release_note"
    pub action: String,
    /// Repository in "owner/repo" format. Required for GitHub primitive actions; lifecycle actions may infer from issue_ref/pr_ref/flow_id.
    #[serde(default)]
    pub repo: Option<String>,
    /// Issue or PR number (required for issue_read, pr_read, pr_comments, pr_review_digest, safe_merge when repo is supplied)
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
    /// Optional response shape for lifecycle actions: json (default) or markdown.
    #[serde(default)]
    pub format: Option<String>,
    /// Task summary used by pr_handoff lifecycle artifacts.
    #[serde(default)]
    pub task: Option<String>,
    /// GitHub issue ref for lifecycle actions, e.g. owner/repo#123 or URL.
    #[serde(default)]
    pub issue_ref: Option<String>,
    /// GitHub PR ref for lifecycle actions, e.g. owner/repo#123 or URL.
    #[serde(default)]
    pub pr_ref: Option<String>,
    /// Branch name to record in pr_handoff lifecycle artifacts.
    #[serde(default)]
    pub branch: Option<String>,
    /// Evidence references used by pr_handoff lifecycle artifacts.
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    /// Verification commands used by pr_handoff lifecycle artifacts.
    #[serde(default)]
    pub tests_run: Vec<String>,
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
pub struct GhIssueReadParams {
    /// Repository in "owner/repo" format
    pub repo: String,
    /// Issue number
    pub issue_number: u64,
}

/// Parameters for listing GitHub issues
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GhIssueListParams {
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
pub struct GhIssueCreateParams {
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

/// Parameters for posting a comment to a GitHub issue or PR (write-back arc).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GhCommentParams {
    /// Repository in "owner/repo" format
    pub repo: String,
    /// Issue or PR number to comment on
    pub number: u64,
    /// Comment body (markdown)
    pub body: Option<String>,
    /// When true, return a preview of the comment WITHOUT posting it.
    pub dry_run: bool,
}

/// Parameters for reading a GitHub PR
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GhPrReadParams {
    /// Repository in "owner/repo" format
    pub repo: String,
    /// PR number
    pub pr_number: u64,
}

/// Parameters for reading GitHub PR review submissions and inline comments
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GhPrCommentsParams {
    /// Repository in "owner/repo" format
    pub repo: String,
    /// PR number
    pub pr_number: u64,
}

/// Parameters for listing GitHub PRs
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GhPrListParams {
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
pub struct GhRepoViewParams {
    /// Repository in "owner/repo" format
    pub repo: String,
}

fn default_issue_state() -> String {
    "open".to_string()
}

fn default_gh_limit() -> u32 {
    30
}
