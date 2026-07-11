use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

/// Unified GitHub facade — one tool for all GitHub operations.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct TachiGhParams {
    /// Action to perform: "repo_view", "issue_list", "issue_read", "issue_create", "issue_comment", "issue_label", "issue_freshness_scan", "pr_list", "pr_read", "pr_comments", "pr_comment", "pr_review_digest", "safe_merge", "ship", "link_pr", "pr_status", "pr_handoff", "release_note"
    pub action: String,
    /// Repository in "owner/repo" format. Required for GitHub primitive actions; lifecycle actions may infer from issue_ref/pr_ref/flow_id.
    #[serde(default)]
    pub repo: Option<String>,
    /// Issue number, or PR number when paired with repo. PR actions also accept pr_ref.
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u64_from_string_or_number"
    )]
    #[schemars(schema_with = "super::coerce::opt_integer_from_string_or_number_schema")]
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
    #[schemars(schema_with = "super::coerce::opt_integer_from_string_or_number_schema")]
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
    /// GitHub issue ref for lifecycle actions, e.g. owner/repo#123 or URL. Contract-mode ship emits `Refs <issue_ref>` in the generated PR body when set.
    #[serde(default)]
    pub issue_ref: Option<String>,
    /// GitHub PR ref, e.g. owner/repo#123 or URL. Accepted by PR actions including pr_read, pr_comment, pr_comments, pr_review_digest, safe_merge, and lifecycle actions.
    #[serde(default)]
    pub pr_ref: Option<String>,
    /// Branch name to record in pr_handoff lifecycle artifacts.
    #[serde(default)]
    pub branch: Option<String>,
    /// Evidence references used by pr_handoff lifecycle artifacts.
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    /// Verification commands used by pr_handoff lifecycle artifacts, contract-mode ship PR body (`## Tested` section), and safe_merge local verification recording when no ledger exists.
    #[serde(default)]
    pub tests_run: Vec<String>,
    /// Merge gate policy mode: permissive | standard | strict. Defaults to standard.
    /// Standard waits on missing checks or missing review decisions instead of treating them as green.
    #[serde(default)]
    pub merge_policy: Option<String>,
    /// Explicit override for safe_merge to allow closing protected umbrella/no-close issues.
    /// Defaults false; plain confirm does not disable the protected-close gate.
    #[serde(default)]
    pub allow_umbrella_close: bool,
    /// Optional author/login substring for pr_review_digest. Defaults to "gemini".
    #[serde(default)]
    pub author_filter: Option<String>,
    /// Write pr_review_digest artifacts under .tachi/reviews. Defaults to true.
    #[serde(default)]
    pub write_digest: Option<bool>,
    /// Exact file list to stage for action="ship" mechanical mode. Omit with commit_message for contract mode (zero-prose PR from git log).
    #[serde(default)]
    pub files: Vec<String>,
    /// Commit message for action="ship" mechanical mode (byte-verbatim). Omit with empty files for contract mode.
    #[serde(default)]
    pub commit_message: Option<String>,
    /// Pull request title for action="ship". Mechanical mode: PR creation only when pr_title and pr_body are both supplied. Contract mode: optional override; defaults to last commit subject.
    #[serde(default)]
    pub pr_title: Option<String>,
    /// Pull request body for action="ship" mechanical mode (byte-verbatim). Contract mode ignores this and builds the body from issue_ref, git log, and tests_run.
    #[serde(default)]
    pub pr_body: Option<String>,
    /// Pull request base branch for action="ship". Defaults to "main". Contract mode resolves origin/<base> or local <base> before listing commits.
    #[serde(default)]
    pub pr_base: Option<String>,
    /// Expected current branch guard for action="ship".
    #[serde(default)]
    pub expect_branch: Option<String>,
    /// Repository root override for action="ship". Defaults to current process cwd.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Optional local worktree path to reclaim after a successful safe_merge
    /// (disk-governor reclamation hook, #484). When supplied and the GitHub PR
    /// merge succeeds, `tachi-clean wt-remove` reclaims the worktree + branch +
    /// target dir. Best-effort: a missing worktree logs a warning and does NOT
    /// fail the merge. No-op when absent (no PR→worktree mapping recorded).
    ///
    /// Note: this MUST point at the PR-head worktree (the checkout whose HEAD
    /// matches the PR's head branch), not an arbitrary worktree — the handler
    /// does NOT run `detect_branch_safety_signals` to validate it, so passing a
    /// wrong worktree would delete the wrong tree + branch on the reclaim path.
    #[serde(default)]
    pub worktree: Option<String>,
    /// When true (default), safe_merge reclaims the supplied worktree after a
    /// successful merge. Default true: the local worktree at `worktree` is
    /// deleted after a successful merge. Set false to preserve it (opt OUT of
    /// this destructive local-disk op on the GitHub path).
    #[serde(default)]
    pub reclaim_worktree: Option<bool>,
    /// Canonical docs to fold into action="release_note" lifecycle artifacts
    /// (in addition to any docs recorded on the flow itself).
    #[serde(default)]
    pub doc_paths: Vec<String>,
    /// Canonical spec docs to fold into action="release_note" lifecycle artifacts
    /// (in addition to any specs recorded on the flow itself).
    #[serde(default)]
    pub spec_paths: Vec<String>,
    /// PR title override for action="pr_handoff" lifecycle artifacts. Falls back
    /// to the flow's automation-plan title, then the task summary, when unset.
    #[serde(default)]
    pub notes: Option<String>,
    /// Label mode for action="issue_label": "add" (default) or "remove".
    #[serde(default)]
    pub label_mode: Option<String>,
    /// Max merged PRs to scan for action="issue_freshness_scan" (default 100).
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u32_from_string_or_number"
    )]
    #[schemars(schema_with = "super::coerce::opt_integer_from_string_or_number_schema")]
    pub scan_limit: Option<u32>,
    /// Minimum number of distinct recently-merged PRs touching the same
    /// file-surface as an inactive open issue before action="issue_freshness_scan"
    /// flags it as a same-surface-churn candidate (default 3).
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u32_from_string_or_number"
    )]
    #[schemars(schema_with = "super::coerce::opt_integer_from_string_or_number_schema")]
    pub churn_threshold: Option<u32>,
    /// RFC3339 activity cutoff for action="issue_freshness_scan"'s
    /// same-surface-churn heuristic: issues updated/commented at or after this
    /// timestamp count as active and are excluded. Defaults to 30 days before
    /// the scan runs.
    #[serde(default)]
    pub churn_activity_since: Option<String>,
}

/// Parameters for adding/removing labels on a GitHub issue or PR (write-back arc).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GhLabelParams {
    /// Repository in "owner/repo" format
    pub repo: String,
    /// Issue or PR number to label
    pub number: u64,
    /// Labels to add or remove
    pub labels: Vec<String>,
    /// "add" (default) or "remove"
    #[serde(default)]
    pub mode: Option<String>,
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
