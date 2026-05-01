use super::*;

// ─── Facade: unified search ──────────────────────────────────────────────────

fn default_facade_search_scope() -> String {
    "all".to_string()
}

fn default_facade_top_k() -> usize {
    6
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiSearchParams {
    /// Search query text
    pub query: String,

    /// Scope: "wiki" searches wiki entries, "memory" searches general memory, "all" searches both (default)
    #[serde(default = "default_facade_search_scope")]
    pub scope: String,

    /// Number of results to return (default: 6)
    #[serde(default = "default_facade_top_k")]
    pub top_k: usize,

    /// Optional path prefix filter
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Optional named project DB
    #[serde(default)]
    pub project: Option<String>,

    /// Optional domain filter
    #[serde(default)]
    pub domain: Option<String>,

    /// Wiki category filter (only used when scope includes wiki)
    #[serde(default)]
    pub category: Option<String>,

    /// Whether to include archived entries
    #[serde(default)]
    pub include_archived: bool,
}

// ─── Facade: web search ──────────────────────────────────────────────────────

fn default_web_search_top_k() -> usize {
    8
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiWebSearchParams {
    /// Web search query text
    pub query: String,

    /// Number of results to request when the backend supports it
    #[serde(default = "default_web_search_top_k")]
    pub top_k: usize,

    /// Backend selector: "auto" (default), "exa", "tavily", "bigmodel", or a concrete capability id
    #[serde(default)]
    pub backend: Option<String>,

    /// Explicit tool name override for advanced/debug usage
    #[serde(default)]
    pub tool_name: Option<String>,

    /// Optional domains to include, if the selected backend supports it
    #[serde(default)]
    pub include_domains: Vec<String>,

    /// Optional domains to exclude, if the selected backend supports it
    #[serde(default)]
    pub exclude_domains: Vec<String>,
}

// ─── Facade: unified save ────────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiSaveParams {
    /// Full text content
    pub text: String,

    /// Existing memory ID to update. When set, updates the entry instead of creating a new one.
    #[serde(default)]
    pub id: Option<String>,

    /// What to save: "wiki" for wiki entry, "note" for a quick note, "memory" for full memory entry.
    /// If omitted, auto-detected: title present → wiki; short/casual text → note; otherwise → memory.
    #[serde(default)]
    pub kind: Option<String>,

    /// Title (required for wiki entries, ignored for notes)
    #[serde(default)]
    pub title: Option<String>,

    /// Short summary
    #[serde(default)]
    pub summary: Option<String>,

    /// Hierarchical path
    #[serde(default)]
    pub path: Option<String>,

    /// 0.0–1.0 importance score
    #[serde(default)]
    pub importance: Option<f64>,

    /// Category: "fact" | "decision" | "experience" | "preference" | "entity" | "other"
    #[serde(default)]
    pub category: Option<String>,

    /// Keyword tags
    #[serde(default)]
    pub keywords: Vec<String>,

    /// Entity names mentioned
    #[serde(default)]
    pub entities: Vec<String>,

    /// Scope: "user" | "project" | "general"
    #[serde(default)]
    pub scope: Option<String>,

    /// Optional named project DB
    #[serde(default)]
    pub project: Option<String>,

    /// Optional domain
    #[serde(default)]
    pub domain: Option<String>,

    /// Retention policy: "ephemeral" | "durable" | "permanent" | "pinned"
    #[serde(default)]
    pub retention_policy: Option<String>,

    /// Bypass noise filter
    #[serde(default)]
    pub force: bool,

    /// Topic / subject area
    #[serde(default)]
    pub topic: Option<String>,
}

// ─── Facade: unified handoff ─────────────────────────────────────────────────

fn default_true() -> bool {
    true
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiHandoffParams {
    /// Action: "leave" to leave a handoff memo, "check" to check for pending memos
    pub action: String,

    /// Summary of what was accomplished (required when action="leave")
    #[serde(default)]
    pub summary: Option<String>,

    /// Next steps for the receiving agent (used when action="leave")
    #[serde(default)]
    pub next_steps: Vec<String>,

    /// Target agent ID (used when action="leave")
    #[serde(default)]
    pub target_agent: Option<String>,

    /// Optional context (used when action="leave")
    #[serde(default)]
    pub context: Option<serde_json::Value>,

    /// Agent ID to check for (used when action="check")
    #[serde(default)]
    pub agent_id: Option<String>,

    /// Whether to acknowledge retrieved memos (used when action="check", default: true)
    #[serde(default = "default_true")]
    pub acknowledge: bool,
}

// ─── Facade: agent dispatch ───────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiDispatchParams {
    /// Agent backend to use: "claude" | "codex" | "custom"
    pub agent: String,

    /// Task description / prompt for the agent
    pub task: String,

    /// Working directory for the agent (default: current project root)
    #[serde(default)]
    pub cwd: Option<String>,

    /// Skills to inject into the agent's prompt (capability IDs)
    #[serde(default)]
    pub skills: Vec<String>,

    /// Extra context query — runs tachi_search and injects top hits into prompt
    #[serde(default)]
    pub context_query: Option<String>,

    /// Model override (e.g. "gpt-5.3-codex", "claude-sonnet-4")
    #[serde(default)]
    pub model: Option<String>,

    /// Timeout in seconds (default: 300)
    #[serde(default)]
    pub timeout_secs: Option<u64>,

    /// Approval policy for codex: "never" | "on-failure" | "on-request"
    #[serde(default)]
    pub approval_policy: Option<String>,

    /// Sandbox mode for codex: "workspace-write" | "danger-full-access" | "read-only"
    #[serde(default)]
    pub sandbox: Option<String>,

    /// Custom command (when agent="custom"): e.g. ["aider", "--yes-always"]
    #[serde(default)]
    pub command: Vec<String>,

    /// Optional named project DB for context search
    #[serde(default)]
    pub project: Option<String>,
}

// ─── Facade: worktree merge ──────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiApproveMergeParams {
    /// Path to the git worktree to merge
    pub worktree: String,

    /// Branch name to merge (default: inferred from worktree HEAD)
    #[serde(default)]
    pub branch: Option<String>,

    /// Merge strategy (default: "recursive")
    #[serde(default)]
    pub strategy: Option<String>,

    /// Whether to remove the worktree after merge (default: true)
    #[serde(default = "default_true")]
    pub delete_worktree: bool,
}

// ─── Facade: task completion + eval ledger ───────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiCompleteParams {
    /// Task ID (if absent, one is generated from timestamp + agent)
    #[serde(default)]
    pub task_id: Option<String>,

    /// Task description / what was asked
    pub task: String,

    /// Agent that executed the task (e.g. "claude-code", "codex", "self")
    pub agent: String,

    /// Outcome: "success" | "failure" | "partial" | "aborted"
    pub outcome: String,

    /// Execution duration in milliseconds
    #[serde(default)]
    pub duration_ms: Option<u64>,

    /// Skills used during execution (capability IDs)
    #[serde(default)]
    pub skills_used: Vec<String>,

    /// Cost in tokens (total across all turns)
    #[serde(default)]
    pub cost_tokens: Option<u64>,

    /// Cost in USD (if known)
    #[serde(default)]
    pub cost_usd: Option<f64>,

    /// Quality score 0.0–1.0 (self-reported or computed later)
    #[serde(default)]
    pub quality_score: Option<f64>,

    /// Free-form notes / summary of what was done
    #[serde(default)]
    pub notes: Option<String>,

    /// Execution trajectory for later distillation. Array of step objects.
    #[serde(default)]
    pub trajectory: Option<serde_json::Value>,

    /// Git diff / unified patch (if any files were changed)
    #[serde(default)]
    pub diff: Option<String>,

    /// Worktree path (if dispatched via tachi_dispatch with isolation)
    #[serde(default)]
    pub worktree: Option<String>,

    /// Parent dispatch ID (links back to tachi_dispatch record)
    #[serde(default)]
    pub dispatch_id: Option<String>,

    /// Target database scope for the eval entry: "global" or "project" (default)
    #[serde(default)]
    pub scope: Option<String>,

    /// Optional named project DB
    #[serde(default)]
    pub project: Option<String>,
}
