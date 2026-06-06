use super::*;

// ─── Facade: unified search ──────────────────────────────────────────────────

fn default_facade_search_scope() -> String {
    "all".to_string()
}

fn default_facade_top_k() -> usize {
    6
}

fn string_enum_schema(
    values: &[&str],
    description: &str,
    _: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    rmcp::schemars::json_schema!({
        "type": "string",
        "enum": values,
        "description": description
    })
}

fn tachi_memory_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &[
            "search",
            "save",
            "extract_facts",
            "briefing",
            "checkpoint",
            "alerts",
            "ask",
            "consolidate",
            "progress",
            "readiness",
        ],
        "Required Tachi memory facade action.",
        generator,
    )
}

fn tachi_wiki_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &["search", "browse", "read", "write"],
        "Required Tachi wiki facade action.",
        generator,
    )
}

fn tachi_skill_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &["discover", "run"],
        "Required Tachi skill facade action.",
        generator,
    )
}

fn tachi_task_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &["plan", "dispatch", "board", "merge"],
        "Required Tachi task facade action.",
        generator,
    )
}

fn tachi_arena_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &[
            "open", "spawn", "board", "collect", "abort", "reap", "close",
        ],
        "Required Tachi arena action.",
        generator,
    )
}

fn tachi_shell_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &[
            "brainstorm",
            "plan",
            "dispatch",
            "kanban",
            "status",
            "review",
            "ship",
        ],
        "Required Tachi shell workflow stage.",
        generator,
    )
}

fn tachi_orchestrator_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &[
            "todo_list",
            "todo_update",
            "handoff_write",
            "handoff_read",
            "recovery_briefing",
        ],
        "Required Tachi orchestrator action.",
        generator,
    )
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiSearchParams {
    /// Search query text
    pub query: String,

    /// Scope: "wiki" searches wiki entries, "memory" searches general memory, "all" searches both (default), "sft" searches training/distillation corpus.
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
    #[schemars(
        description = "Named project library under ~/.tachi/projects/<name>/memory.db. When set, search/save targets ONLY that library (not the daemon-bound workspace DB). Omit to use global + daemon-bound project DB."
    )]
    pub project: Option<String>,

    /// Optional domain filter
    #[serde(default)]
    #[schemars(
        description = "Optional area tag filter (e.g. rust, mcp). Does not select the DB — use project for library targeting."
    )]
    pub domain: Option<String>,

    #[serde(default)]
    pub file_context: Option<String>,

    #[serde(default)]
    pub error_context: Option<String>,

    /// Wiki category filter (only used when scope includes wiki)
    #[serde(default)]
    pub category: Option<String>,

    /// Whether to include archived entries
    #[serde(default)]
    pub include_archived: bool,

    /// Include training/distillation corpus entries such as `/sft/...`.
    /// Defaults to false for agent recall; use `scope="sft"` or this flag to opt in.
    #[serde(default)]
    pub include_training: bool,

    /// Enable adaptive Voyage reranking for close top results in memory search.
    #[serde(default)]
    pub enable_rerank: bool,

    /// Point-in-time validity filter (ISO 8601). Returns only memories valid at this time.
    #[serde(default)]
    pub as_of: Option<String>,
}

// ─── Facade: web search ──────────────────────────────────────────────────────

fn default_web_search_top_k() -> usize {
    8
}

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
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    pub importance: Option<f64>,

    /// Category: "fact" | "decision" | "experience" | "preference" | "entity" | "other"
    #[serde(default)]
    pub category: Option<String>,

    /// Tags for recall and FTS (modules, crates, topics)
    #[serde(default, alias = "indexed_tags")]
    #[schemars(description = "Tags for recall/FTS, e.g. rust, mcp, refactor.")]
    pub keywords: Vec<String>,

    /// People, repos, services, tools (used for auto-link)
    #[serde(default)]
    #[schemars(description = "Named entities, e.g. sigil, memory-server, postgres.")]
    pub entities: Vec<String>,

    /// Scope: "user" | "project" | "general"
    #[serde(default)]
    pub scope: Option<String>,

    /// Optional named project DB
    #[serde(default)]
    pub project: Option<String>,

    /// Optional codebase area tag (does not select DB)
    #[serde(default)]
    pub domain: Option<String>,

    /// Retention policy: "ephemeral" | "durable" | "permanent" | "pinned"
    #[serde(default)]
    pub retention_policy: Option<String>,

    /// Bypass noise filter
    #[serde(default)]
    pub force: bool,

    /// External references: URLs, absolute paths, or GitHub shorthands (#N, repo#N, owner/repo#N).
    #[serde(default)]
    pub references: Vec<String>,

    /// Topic / subject area
    #[serde(default)]
    pub topic: Option<String>,

    /// Source identifier (used when kind="facts" or kind="extract_facts")
    #[serde(default)]
    pub source: Option<String>,
    /// When this memory became true/effective. Defaults to timestamp.
    #[serde(default)]
    pub valid_from: Option<String>,

    /// When this memory stopped being true/effective. None = still valid.
    #[serde(default)]
    pub valid_until: Option<String>,

    /// Arbitrary metadata payload merged before provenance injection.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,

    /// Source files this memory references (stored as `metadata.files`). Surfaced
    /// inline on search results so agents can jump to the referenced file without
    /// a follow-up `get_memory`. Merged with paths auto-parsed from `spec:` pointers.
    #[serde(default)]
    #[schemars(description = "Referenced source files, e.g. docs/SPEC.md, src/lib.rs.")]
    pub files: Vec<String>,
}

// ─── Facade: unified memory / agent session UX ───────────────────────────────

fn default_memory_top_k() -> usize {
    6
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub(crate) struct TachiMemoryParams {
    #[schemars(
        schema_with = "tachi_memory_action_schema",
        description = "Required. One of: search (hybrid vector+FTS+symbolic recall), save (persist memory entry; prefer tachi_save for decisions), extract_facts (LLM atomize raw text into entries), briefing (session-start context), checkpoint (mid-task handoff summary), alerts (compact warnings when stuck), ask (Q&A over evidence; set synthesize=true for LLM answer), consolidate (merge related memories), progress (long-running flow status), readiness (health + tool visibility)."
    )]
    pub action: String,
    #[serde(default, alias = "output_format")]
    #[schemars(
        description = "Response shape: \"markdown\" (default, agent-readable) or \"json\" (minified, for automation)."
    )]
    pub format: Option<String>,

    // --- search fields ---
    #[serde(default)]
    #[schemars(description = "Search or ask query text (required for action=search or ask).")]
    pub query: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Recall scope for search/ask: \"all\" (default), \"memory\", \"wiki\", or \"sft\"."
    )]
    pub scope: Option<String>,
    #[serde(default = "default_memory_top_k")]
    #[schemars(description = "Maximum results to return (default: 6).")]
    pub top_k: usize,
    #[serde(default)]
    #[schemars(description = "Optional path prefix filter, e.g. /scratch/sigil/.")]
    pub path_prefix: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional open-file path hint to bias recall.")]
    pub file_context: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional error message hint to bias recall.")]
    pub error_context: Option<String>,
    #[serde(default)]
    #[schemars(description = "Wiki category filter when scope includes wiki.")]
    pub category: Option<String>,
    #[serde(default)]
    #[schemars(description = "Include archived wiki/memory entries in search results.")]
    pub include_archived: bool,
    #[serde(default)]
    #[schemars(
        description = "Include training/distillation corpus entries such as /sft/ in normal recall. Defaults false; scope='sft' opts in."
    )]
    pub include_training: bool,
    #[serde(default)]
    #[schemars(
        description = "Enable adaptive Voyage reranking when top hybrid scores are close (search/ask)."
    )]
    pub enable_rerank: bool,
    #[serde(default)]
    #[schemars(
        description = "Point-in-time validity filter (ISO 8601). Returns only memories valid at this time."
    )]
    pub as_of: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "When action=ask or consolidate, call the configured LLM to synthesize from evidence."
    )]
    pub synthesize: bool,
    #[serde(default)]
    #[schemars(description = "Optional model override for ask/consolidate synthesis.")]
    pub model: Option<String>,

    // --- save fields ---
    #[serde(default)]
    #[schemars(description = "Full text to save (action=save, checkpoint, extract_facts source).")]
    pub text: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional title (wiki-style entries).")]
    pub title: Option<String>,
    #[serde(default)]
    #[schemars(description = "Short summary stored alongside text.")]
    pub summary: Option<String>,
    #[serde(default)]
    #[schemars(description = "Topic label for the entry.")]
    pub topic: Option<String>,
    #[serde(default, alias = "indexed_tags")]
    #[schemars(description = "Tags for recall/FTS, e.g. rust, mcp, refactor.")]
    pub keywords: Vec<String>,
    #[serde(default)]
    #[schemars(description = "Named entities, e.g. sigil, memory-server, postgres.")]
    pub entities: Vec<String>,
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    #[schemars(description = "Importance score 0.0–1.0 (default: 0.5 on save).")]
    pub importance: Option<f64>,
    #[serde(default)]
    #[schemars(description = "Retention policy name (save).")]
    pub retention_policy: Option<String>,
    #[serde(default)]
    #[schemars(description = "Save kind hint: memory, note, or wiki (auto-detected if omitted).")]
    pub kind: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Hierarchical path for saving. Working notes: /scratch/<project>/..., review notes: /code-review/<project>/..., wiki: /wiki/... e.g. /scratch/sigil/schema-bug-fix"
    )]
    pub path: Option<String>,
    #[serde(default)]
    #[schemars(description = "Existing memory id to update (save) instead of creating a new row.")]
    pub id: Option<String>,
    #[serde(default)]
    #[schemars(description = "Bypass dedup guards on save when true.")]
    pub force: bool,
    #[serde(default)]
    #[schemars(description = "Provenance source label, e.g. cursor, codex, openclaw.")]
    pub source: Option<String>,
    #[serde(default)]
    #[schemars(description = "Validity start (ISO 8601) for time-bounded facts.")]
    pub valid_from: Option<String>,
    #[serde(default)]
    #[schemars(description = "Validity end (ISO 8601) for time-bounded facts.")]
    pub valid_until: Option<String>,
    #[serde(default)]
    #[schemars(description = "Arbitrary JSON metadata merged into the stored entry.")]
    pub metadata: Option<serde_json::Value>,
    #[serde(default)]
    #[schemars(description = "Referenced source files, e.g. docs/SPEC.md, src/lib.rs.")]
    pub files: Vec<String>,

    // --- progress / long-running command fields ---
    #[serde(default)]
    #[schemars(description = "Flow id for action=progress (create or resume a tracked command).")]
    pub flow_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "Progress event name, e.g. step_done, failed.")]
    pub event: Option<String>,
    #[serde(default)]
    #[schemars(description = "Progress state payload or status line for action=progress.")]
    pub state: Option<String>,

    // --- shared ---
    #[serde(default)]
    #[schemars(
        description = "Named project library under ~/.tachi/projects/<name>/memory.db. When set, recall/save targets ONLY that library. Omit to use global + the daemon-bound workspace project DB (shown in every response)."
    )]
    pub project: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional area tag (e.g. rust, ci, mcp). Filter on search; stored on save. Does not select the DB — use project for that."
    )]
    pub domain: Option<String>,

    // --- briefing shape ---
    #[serde(default)]
    #[schemars(
        description = "When true (action=briefing), emit a tight 6-row summary: top 6 memories, top 3 wiki, top 3 kanban, top 2 checkpoints, no health snapshot. Default false (full briefing)."
    )]
    pub compact: bool,
}

// ─── Facade: unified handoff ─────────────────────────────────────────────────

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiHandoffParams {
    /// Action: "leave" to leave a handoff memo, "check" to check for pending memos, "promote_issue" to create a GitHub issue from a memo
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

    /// Handoff memo ID to promote (required for action="promote_issue", with or without "handoff:" prefix)
    #[serde(default)]
    pub memo_id: Option<String>,

    /// GitHub repo in "owner/repo" format (required for action="promote_issue")
    #[serde(default)]
    pub repo: Option<String>,

    /// Issue title override (used by action="promote_issue"; defaults to memo summary truncated to 120 chars)
    #[serde(default)]
    pub title: Option<String>,

    /// Issue labels (used by action="promote_issue"; defaults to ["handoff"])
    #[serde(default)]
    pub labels: Vec<String>,

    /// Shell flow ID for artifact linkage (optional for action="promote_issue"; writes status.json + events.jsonl)
    #[serde(default)]
    pub flow_id: Option<String>,

    /// Force re-promote even if memo already has a GitHub issue link (used by action="promote_issue")
    #[serde(default)]
    pub force: bool,
}

// ─── Facade: agent dispatch ───────────────────────────────────────────────────

fn default_dispatch_timeout() -> u64 {
    600
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiDispatchParams {
    /// Agent backend: "claude" | "codex" | "grok" | "kimi" | "custom" (aliases accepted)
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
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u32_from_string_or_number"
    )]
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

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub(crate) struct TachiSubagentEvalParams {
    /// Subagent role: explore | critic | specialist | executor | verifier | other
    pub role: String,

    /// Agent/provider name, e.g. kimi, deepseek, glm, codex
    pub agent: String,

    /// Concrete model name when known
    #[serde(default)]
    pub model: Option<String>,

    /// Bounded task slice assigned to this subagent
    #[serde(default)]
    pub task: Option<String>,

    /// Standard task type, e.g. fix_request, plan_request, research_request
    #[serde(default)]
    pub task_type: Option<String>,

    /// Outcome: useful | partial | failed | not_used
    #[serde(default)]
    pub outcome: Option<String>,

    /// Usefulness score 0.0-1.0 as judged by the leader
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    pub usefulness_score: Option<f64>,

    /// Failure mode when this subagent was unhelpful or wrong
    #[serde(default)]
    pub failure_mode: Option<String>,

    /// How this subagent affected final verification or plan quality
    #[serde(default)]
    pub verification_impact: Option<String>,

    /// Whether the leader verified this subagent output with independent evidence
    #[serde(default)]
    pub verification_present: bool,

    /// Who assigned the usefulness score: leader | human | self | auto_verifier
    #[serde(default)]
    pub evaluator: Option<String>,

    /// How the subagent changed the final plan: accepted | modified | rejected | superseded
    #[serde(default)]
    pub plan_delta: Option<String>,

    /// Whether the human overrode or materially corrected the subagent output
    #[serde(default)]
    pub human_override: bool,

    /// Number of retries or re-prompts needed for this subagent slice
    #[serde(default)]
    pub retry_count: u32,

    /// Short leader-facing summary; do not store raw transcript
    #[serde(default)]
    pub notes: Option<String>,

    /// Execution latency in milliseconds for this subagent if known
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u64_from_string_or_number"
    )]
    pub latency_ms: Option<u64>,

    /// Input tokens/context tokens for this subagent if known
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u64_from_string_or_number"
    )]
    pub input_tokens: Option<u64>,

    /// Output tokens for this subagent if known
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u64_from_string_or_number"
    )]
    pub output_tokens: Option<u64>,

    /// Cost in tokens for this subagent if known
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u64_from_string_or_number"
    )]
    pub cost_tokens: Option<u64>,

    /// Cost in USD for this subagent if known
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    pub cost_usd: Option<f64>,
}

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

    /// Standard task type for eval aggregation, e.g. fix_request or plan_request
    #[serde(default)]
    pub task_type: Option<String>,

    /// Execution duration in milliseconds
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u64_from_string_or_number"
    )]
    pub duration_ms: Option<u64>,

    /// Skills used during execution (capability IDs)
    #[serde(default)]
    pub skills_used: Vec<String>,

    /// Cost in tokens (total across all turns)
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u64_from_string_or_number"
    )]
    pub cost_tokens: Option<u64>,

    /// Cost in USD (if known)
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    pub cost_usd: Option<f64>,

    /// Quality score 0.0–1.0 (self-reported or computed later)
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
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

    /// Structured subagent usage/eval records captured by the leader.
    /// Store concise summaries only; raw child transcripts should stay out of memory.
    #[serde(default)]
    pub subagents: Vec<TachiSubagentEvalParams>,

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

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub(crate) struct TachiWikiParams {
    /// Action: "search", "browse", "read", or "write"
    #[schemars(schema_with = "tachi_wiki_action_schema")]
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
    #[serde(default, alias = "indexed_tags")]
    #[schemars(description = "Wiki tags for recall/FTS.")]
    pub keywords: Vec<String>,
    #[serde(default)]
    #[schemars(description = "Related repos, tools, or people.")]
    pub entities: Vec<String>,
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    pub importance: Option<f64>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Named project library under ~/.tachi/projects/<name>/memory.db. When set, wiki recall targets ONLY that library."
    )]
    pub project: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional area tag on the wiki entry.")]
    pub domain: Option<String>,
    #[serde(default)]
    pub force: bool,

    /// External references (URLs, absolute paths, GitHub shorthands). Validated on write.
    #[serde(default)]
    pub references: Vec<String>,
}

// ─── Facade: workflow closure (Issue → Doc → Memory) ─────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiWorkflowParams {
    /// close_loop | build_references
    pub action: String,
    #[serde(default)]
    pub issue_ref: Option<String>,
    #[serde(default)]
    pub doc_paths: Vec<String>,
    #[serde(default)]
    pub related_issues: Vec<String>,
    #[serde(default)]
    pub wiki_title: Option<String>,
    #[serde(default)]
    pub wiki_text: Option<String>,
    #[serde(default)]
    pub wiki_path: Option<String>,
    #[serde(default)]
    pub wiki_topic: Option<String>,
    #[serde(default)]
    pub wiki_summary: Option<String>,
    #[serde(default)]
    pub wiki_category: Option<String>,
    #[serde(default)]
    pub wiki_keywords: Vec<String>,
    #[serde(default)]
    pub wiki_entities: Vec<String>,
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    pub wiki_importance: Option<f64>,
    #[serde(default)]
    pub wiki_scope: Option<String>,
    #[serde(default)]
    pub wiki_domain: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub force: bool,
}

// ─── Facade: skill (discover / run) ──────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiSkillParams {
    /// Action: "discover" or "run"
    #[schemars(schema_with = "tachi_skill_action_schema")]
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

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiTaskParams {
    /// Action: "plan", "dispatch", "board", or "merge"
    #[schemars(schema_with = "tachi_task_action_schema")]
    pub action: String,
    /// Response shape: "markdown" (default, agent-readable) or "json" (automation).
    #[serde(default)]
    pub format: Option<String>,
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
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u64_from_string_or_number"
    )]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub permission_profile: Option<String>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u32_from_string_or_number"
    )]
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

// ─── Facade: tachi_arena (tracked worker mission ledger) ────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiArenaParams {
    /// Action: "open", "spawn", "board", "collect", "abort", "reap", or "close"
    #[schemars(schema_with = "tachi_arena_action_schema")]
    pub action: String,

    /// Response shape: "markdown" (default, agent-readable) or "json" (automation).
    #[serde(default)]
    pub format: Option<String>,

    /// Existing arena id for spawn/board/collect/abort/reap/close.
    #[serde(default)]
    pub arena_id: Option<String>,

    /// Existing or requested mission id for spawn/collect/abort.
    #[serde(default)]
    pub mission_id: Option<String>,

    /// Human-readable arena title for open.
    #[serde(default)]
    pub title: Option<String>,

    /// Arena objective for open.
    #[serde(default)]
    pub objective: Option<String>,

    /// Mission prompt for spawn, or objective fallback for open.
    #[serde(default)]
    pub prompt: Option<String>,

    /// Harness lane hint. Golden lanes: "opencode" (default worker), "claude"/"claude-code" (MCP-capable worker), "gemini-advisor"/"ask-gemini" (brainstorm artifact advisor), or "manual" (document-only fallback). Unknown values are treated as manual tracked-document missions.
    #[serde(default)]
    pub harness: Option<String>,

    /// Worker role/lane, e.g. "explore", "critic", "executor", "verifier".
    #[serde(default)]
    pub role: Option<String>,

    /// Working directory hint for a future harness adapter.
    #[serde(default)]
    pub cwd: Option<String>,

    /// Required skill ids the worker must invoke/report.
    #[serde(default)]
    pub skills: Vec<String>,

    /// Allowed file/module scope for the worker mission.
    #[serde(default)]
    pub scope: Vec<String>,

    /// Permission notes or tool names granted to this mission.
    #[serde(default)]
    pub permissions: Vec<String>,

    /// Mission timeout hint for a future harness adapter.
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u64_from_string_or_number"
    )]
    pub timeout_secs: Option<u64>,

    /// Reason for abort/reap.
    #[serde(default)]
    pub reason: Option<String>,

    /// For reap, defaults to true.
    #[serde(default)]
    pub dry_run: Option<bool>,

    /// Close even when active/uncollected missions remain.
    #[serde(default)]
    pub force: bool,

    /// Close requires written results to have been collected. Defaults to true.
    #[serde(default)]
    pub require_collected: Option<bool>,
}

// ─── Facade: tachi_shell (skill-gated flow orchestration) ────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiShellDispatchSliceParams {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub validation: Vec<String>,
    #[serde(default)]
    pub allowed_scope: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiShellParams {
    /// Action: "brainstorm" | "plan" | "dispatch" | "kanban" | "status" | "review" | "ship"
    #[schemars(schema_with = "tachi_shell_action_schema")]
    pub action: String,

    /// Response shape: "markdown" (default, agent-readable) or "json" (automation).
    #[serde(default)]
    pub format: Option<String>,

    /// Existing flow id to continue (optional). When omitted, a new flow_id is generated
    /// for stage-bearing actions (brainstorm/plan/dispatch/review/ship).
    #[serde(default)]
    pub flow_id: Option<String>,

    /// Free-form task / goal description. Required for brainstorm/plan/dispatch when no
    /// existing flow_id is supplied.
    #[serde(default)]
    pub task: Option<String>,

    /// Short title used to slug the flow_id when creating a new flow.
    #[serde(default)]
    pub title: Option<String>,

    /// Optional clanker backend hint for dispatch (e.g. "claude" | "codex" | "custom").
    /// Forwarded to the underlying tachi_dispatch when async hook fires.
    #[serde(default)]
    pub agent: Option<String>,

    /// Optional cwd override for downstream dispatch.
    #[serde(default)]
    pub cwd: Option<String>,

    /// When true, attempt to spawn the underlying async dispatch immediately
    /// (Phase 4). When false (MVP default), only the instruction.md artifact is
    /// produced and the caller is expected to dispatch separately.
    #[serde(default)]
    pub async_dispatch: bool,

    /// Project DB hint passed through to underlying handlers (kanban etc.).
    #[serde(default)]
    pub project: Option<String>,

    /// Filter for kanban (passed through to tachi_task board): "working" | "completed" | etc.
    #[serde(default)]
    pub state_filter: Option<String>,

    /// Limit for kanban / status list views.
    #[serde(default)]
    pub limit: Option<usize>,

    /// Free-form notes appended to the generated instruction.md (e.g. acceptance criteria).
    #[serde(default)]
    pub notes: Option<String>,

    /// Validation commands to embed in the instruction packet.
    #[serde(default)]
    pub validation: Vec<String>,

    /// Allowed scope (file globs / module names) to embed in the instruction packet.
    #[serde(default)]
    pub allowed_scope: Vec<String>,

    #[serde(default)]
    pub slices: Vec<TachiShellDispatchSliceParams>,
}

// ─── Facade: orchestrator (persistent TODO / handoff) ────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiOrchestratorParams {
    /// todo_list | todo_update | handoff_write | handoff_read | recovery_briefing
    #[schemars(schema_with = "tachi_orchestrator_action_schema")]
    pub action: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub todo_id: Option<String>,
    #[serde(default)]
    pub todo_content: Option<String>,
    #[serde(default)]
    pub todo_status: Option<String>,
    #[serde(default)]
    pub parent_todo_id: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub issue_ref: Option<String>,
    #[serde(default)]
    pub blocked_reason: Option<String>,
    #[serde(default)]
    pub verification: Option<String>,
    #[serde(default)]
    pub references: Vec<String>,
    #[serde(default)]
    pub objective: Option<String>,
    #[serde(default)]
    pub current_state: Option<String>,
    #[serde(default)]
    pub completed_steps: Vec<String>,
    #[serde(default)]
    pub remaining_steps: Vec<String>,
    #[serde(default)]
    pub files_touched: Vec<String>,
    #[serde(default)]
    pub commands_run: Vec<String>,
    #[serde(default)]
    pub tests_run: Vec<String>,
    #[serde(default)]
    pub known_blockers: Vec<String>,
    #[serde(default)]
    pub next_action: Option<String>,
    #[serde(default)]
    pub newest_user_instruction: Option<String>,
}

// ─── Facade: agent eval harness ──────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiAgentEvalParams {
    /// aggregate | aggregate_live
    pub action: String,
    #[serde(default)]
    pub fixture_path: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

// ─── Facade: agent registry / router ─────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiAgentsParams {
    /// list | select
    pub action: String,
    #[serde(default)]
    pub intent: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
}

// ─── Facade: task board (kanban) ─────────────────────────────────────────────

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
