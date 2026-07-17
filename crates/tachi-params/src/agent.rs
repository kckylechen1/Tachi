use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

fn default_copilot_top_k() -> usize {
    6
}

// ─── Handoff ────────────────────────────────────────────────────────────────
//
// #1099: `HandoffLeaveParams`/`HandoffCheckParams` retired along with the
// `handoff_leave`/`handoff_check` routes and `tachi_handoff`'s 'leave'/
// 'check' actions (see #1016 — sticky/orchestrator replace them).
// `HandoffPromoteIssueParams` survives; `promote_issue` is the one
// documented handoff capability without a replacement.

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct HandoffPromoteIssueParams {
    /// Handoff memo ID to promote (with or without "handoff:" prefix)
    pub memo_id: String,
    /// GitHub repo in "owner/repo" format
    pub repo: String,
    /// Issue title (defaults to first 120 chars of memo summary)
    #[serde(default)]
    pub title: Option<String>,
    /// Issue labels (defaults to ["handoff"])
    #[serde(default)]
    pub labels: Vec<String>,
    /// Shell flow ID for artifact linkage (writes status.json + events.jsonl)
    #[serde(default)]
    pub flow_id: Option<String>,
    /// Force re-promote even if memo already has a GitHub issue link
    #[serde(default)]
    pub force: bool,
}

// ─── Copilot / Task Guidance ────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TaskBriefParams {
    /// Natural-language task the agent is about to work on.
    pub task: String,

    /// Optional canonical agent id for sandbox filtering and context scoping.
    #[serde(default)]
    pub agent_id: Option<String>,

    /// Optional named project DB.
    #[serde(default)]
    pub project: Option<String>,

    /// Optional memory path prefix for non-wiki context search.
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Optional domain filter.
    #[serde(default)]
    pub domain: Option<String>,

    /// Number of wiki and memory hits to return.
    #[serde(default = "default_copilot_top_k")]
    pub top_k: usize,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProgressCheckParams {
    /// Natural-language task currently being attempted.
    pub task: String,

    /// Attempts already made, in chronological order.
    #[serde(default)]
    pub attempts: Vec<String>,

    /// Latest error, symptom, or failed observation.
    #[serde(default)]
    pub latest_error: Option<String>,

    /// Optional canonical agent id for sandbox filtering and context scoping.
    #[serde(default)]
    pub agent_id: Option<String>,

    /// Optional named project DB.
    #[serde(default)]
    pub project: Option<String>,

    /// Optional domain filter.
    #[serde(default)]
    pub domain: Option<String>,

    /// Number of relevant wiki hits to return.
    #[serde(default = "default_copilot_top_k")]
    pub top_k: usize,

    /// Optional flow id for append-only progress.jsonl logging under .tachi/runs/<flow_id>.
    #[serde(default)]
    pub flow_id: Option<String>,
}
