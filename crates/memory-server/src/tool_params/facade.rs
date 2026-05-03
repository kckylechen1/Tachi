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

    /// Source identifier (used when kind="facts" or kind="extract_facts")
    #[serde(default)]
    pub source: Option<String>,
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

fn default_dispatch_timeout() -> u64 {
    600
}

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

    /// Timeout in seconds (default: 600)
    #[serde(default = "default_dispatch_timeout")]
    pub timeout_secs: u64,

    /// Permission profile for Claude Code: "full" skips all confirmations,
    /// "allowlist" uses allowed_tools, "default" adds no permission flags.
    /// For Codex: "full" maps to --dangerously-bypass-approvals-and-sandbox.
    #[serde(default)]
    pub permission_profile: Option<String>,

    /// Tool allowlist when permission_profile = "allowlist".
    /// e.g. ["Bash(git*)", "Read", "Write", "Edit", "Glob", "Grep"]
    #[serde(default)]
    pub allowed_tools: Vec<String>,

    /// Maximum conversation turns for the dispatched agent (prevents infinite loops).
    #[serde(default)]
    pub max_turns: Option<u32>,

    /// Sandbox mode for codex: "workspace-write" | "danger-full-access" | "read-only"
    #[serde(default)]
    pub sandbox: Option<String>,

    /// Whether to inject Tachi's own MCP server into the subprocess agent config.
    /// Lets the dispatched agent call tachi_search, tachi_save, etc.
    #[serde(default)]
    pub inject_tachi_mcp: Option<bool>,

    /// Whether to inject Hub-registered MCP servers (e.g. context7) into the subprocess.
    #[serde(default)]
    pub inject_hub_mcps: Option<bool>,

    /// Custom command (when agent="custom"): e.g. ["aider", "--yes-always"]
    #[serde(default)]
    pub command: Vec<String>,

    /// Optional named project DB for context search
    #[serde(default)]
    pub project: Option<String>,

    /// Dispatch stage: "plan" injects plan-writing skill, "execute" injects execution skill,
    /// "auto" injects plan skill + "plan first, wait for review" instruction.
    /// Empty/unset = no automatic skill injection (backward compatible).
    #[serde(default)]
    pub stage: Option<String>,
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

    /// Set to true to execute the merge. When false (default), runs a dry-run
    /// preview (git merge --no-commit --no-ff) and returns the diff without
    /// committing. Callers should preview first, then confirm with confirm=true.
    #[serde(default)]
    pub confirm: bool,
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

// ─── Facade: wiki (search / browse / write) ──────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiWikiParams {
    /// Action: "search", "browse", or "write"
    pub action: String,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub top_k: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub importance: Option<f64>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub force: bool,
}

// ─── Facade: skill (discover / run) ──────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiSkillParams {
    /// Action: "discover" or "run"
    pub action: String,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub cap_type: Option<String>,
    #[serde(default)]
    pub enabled_only: Option<bool>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub skill_id: Option<String>,
    #[serde(default)]
    pub args: Option<serde_json::Value>,
}

// ─── Facade: task (plan / dispatch / board / merge) ──────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiTaskParams {
    /// Action: "plan", "dispatch", "board", or "merge"
    pub action: String,
    // plan fields
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub path_prefix: Option<String>,
    #[serde(default)]
    pub top_k: Option<usize>,
    // dispatch fields
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub context_query: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub permission_profile: Option<String>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub max_turns: Option<u32>,
    #[serde(default)]
    pub sandbox: Option<String>,
    #[serde(default)]
    pub inject_tachi_mcp: Option<bool>,
    #[serde(default)]
    pub inject_hub_mcps: Option<bool>,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub stage: Option<String>,
    // board fields
    #[serde(default)]
    pub state_filter: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    // merge fields
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub strategy: Option<String>,
    #[serde(default = "default_true")]
    pub delete_worktree: bool,
    #[serde(default)]
    pub confirm: bool,
}

// ─── Facade: task board (kanban) ─────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiBoardParams {
    /// Filter by state: "working", "completed", "failed", "all" (default: "all")
    #[serde(default)]
    pub state_filter: Option<String>,

    /// Maximum number of tasks to return (default: 20)
    #[serde(default)]
    pub limit: Option<usize>,

    /// Optional named project DB
    #[serde(default)]
    pub project: Option<String>,
}
