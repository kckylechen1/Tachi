use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

// ─── Facade: agent dispatch ───────────────────────────────────────────────────

fn default_dispatch_timeout() -> u64 {
    600
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct DispatchMcpAccessParams {
    /// Whether the resolved dispatch contract should inject Tachi MCP when the backend supports it.
    #[serde(default)]
    pub inject_tachi_mcp: Option<bool>,

    /// Whether Hub MCPs should be injected when the backend supports it.
    #[serde(default)]
    pub inject_hub_mcps: Option<bool>,

    /// Facade tools this profile expects the subagent to be able to use.
    #[serde(default)]
    pub allowed_facades: Vec<String>,

    /// Hub MCP server ids this profile may inject. Empty means no extra Hub MCP filter.
    #[serde(default)]
    pub allowed_mcp_servers: Vec<String>,

    /// Whether issue/PR reads are allowed for this dispatch.
    #[serde(default)]
    pub github_read: Option<bool>,

    /// Whether public write actions such as comments, merges, or pushes are allowed.
    #[serde(default)]
    pub write_actions: Option<bool>,

    /// GitHub issue references available to the subagent, e.g. owner/repo#194.
    #[serde(default)]
    pub issue_refs: Vec<String>,

    /// GitHub PR references available to the subagent.
    #[serde(default)]
    pub pr_refs: Vec<String>,

    /// Required fallback behavior if MCP/GitHub reads are unavailable.
    #[serde(default)]
    pub fallback: Option<String>,
}

/// Machine-checkable completion predicate for a dispatch. When declared, a
/// self-reported `outcome="success"` (and a watchdog exit-code-0 auto-close)
/// only lands `TASK_STATE_COMPLETED` + `reviewed=true` if the predicate is
/// satisfied; an unsatisfied predicate intercepts the false success and routes
/// the run to `TASK_STATE_FAILED`. Two forms are supported (#878-A).
#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CompletionPredicate {
    /// Passes iff the file at `path` (resolved against the dispatch cwd, path
    /// traversal rejected) exists and is non-empty.
    ArtifactNonEmpty { path: String },
    /// Passes iff `pattern` (a regex) matches the run's `result.md` contents.
    OutputMatches { pattern: String },
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiDispatchParams {
    /// Agent backend: "claude" | "codex" | "grok" | "kimi" | "custom" (aliases accepted)
    #[serde(default)]
    pub agent: Option<String>,

    /// Dispatch profile: routes agent/backend, context, MCP/tool access, and evidence requirements.
    /// Distinct from the server ToolProfile, which only gates visible tools.
    #[serde(default, alias = "dispatch_profile")]
    pub profile: Option<String>,

    /// Credential profile ids to materialize before spawning the worker.
    /// Values resolve from `.tachi/credentials/*.json`; materialization returns
    /// only redacted status while injecting concrete env/file outputs internally.
    #[serde(default)]
    pub credential_profiles: Vec<String>,

    /// Task description / prompt for the agent
    pub task: String,

    /// Working directory for the agent (default: current project root)
    #[serde(default)]
    pub cwd: Option<String>,

    /// Execution-environment lease id (#894 S1). When set, the dispatch cwd is
    /// resolved from the daemon-owned `exec_envs` lease (a managed env); a bare
    /// `cwd` is ignored/rejected in favor of the lease path, and the dispatch is
    /// stamped `env: managed` in the ledger.
    #[serde(default)]
    pub env_id: Option<String>,

    /// Explicit opt-in to dispatch into a bare `cwd` that is NOT backed by a
    /// managed lease (#894 S1). Fail-safe default is managed: a bare `cwd`
    /// without `env_id` is only accepted when this is true, and such dispatches
    /// are stamped `env: unmanaged` in the ledger.
    #[serde(default)]
    pub unmanaged_cwd: Option<bool>,

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

    /// Machine-checkable contract predicate. When set, self-reported success
    /// (and watchdog exit-0 auto-close) must satisfy this predicate to land a
    /// reviewed `TASK_STATE_COMPLETED`; otherwise the run is intercepted as a
    /// false success and routed to `TASK_STATE_FAILED` (#878-A).
    #[serde(default)]
    pub completion_predicate: Option<CompletionPredicate>,

    /// Maximum conversation turns for the dispatched agent (prevents infinite loops).
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u32_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_integer_from_string_or_number_schema")]
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

    #[serde(default)]
    #[schemars(
        description = "Harness/execution transport override, e.g. opencode_serve for an existing local OpenCode server or acpx for the opt-in ACP execution backend."
    )]
    pub harness_transport: Option<String>,

    #[serde(default)]
    #[schemars(
        description = "Harness server URL for attach transports, e.g. http://127.0.0.1:4321 for OpenCode serve."
    )]
    pub harness_server_url: Option<String>,

    /// Optional named project DB for context search
    #[serde(default)]
    pub project: Option<String>,

    /// Dispatch stage: "plan" injects plan-writing skill, "execute" injects execution skill,
    /// "auto" injects plan skill + "plan first, wait for review" instruction.
    /// Empty/unset = no automatic skill injection (backward compatible).
    #[serde(default)]
    pub stage: Option<String>,

    /// GitHub issue reference bound to this dispatch, e.g. owner/repo#194.
    #[serde(default)]
    pub issue_ref: Option<String>,

    /// GitHub PR reference bound to this dispatch.
    #[serde(default)]
    pub pr_ref: Option<String>,

    /// Tachi flow id for feature-scoped briefing/dispatch/eval linkage.
    #[serde(default)]
    pub flow_id: Option<String>,

    /// Tool surface expected by the selected dispatch profile. This does not mutate the
    /// server's active ToolProfile; it is part of the child-agent contract.
    #[serde(default)]
    pub tool_profile: Option<String>,

    /// Include a capability bundle in the dispatch prompt when supported.
    #[serde(default, alias = "include_capability_bundle")]
    pub auto_capability_bundle: Option<bool>,

    /// Explicit MCP/tool access contract for the child agent. Profiles populate this by default.
    #[serde(default)]
    pub mcp_access: Option<DispatchMcpAccessParams>,

    /// Hub MCP server ids allowed when inject_hub_mcps=true. Empty preserves current all-enabled behavior.
    #[serde(default)]
    pub allowed_mcp_servers: Vec<String>,
}

// ─── Facade: worktree merge ──────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiApproveMergeParams {
    /// Path to the git worktree to merge
    pub worktree: String,

    /// Branch name to merge (default: inferred from worktree HEAD)
    #[serde(default)]
    pub branch: Option<String>,

    /// Merge strategy (default: "recursive")
    #[serde(default)]
    pub strategy: Option<String>,

    /// Whether to remove the worktree after merge (default: true)
    #[serde(default = "super::default_true")]
    pub delete_worktree: bool,

    /// Set to true to execute the merge. When false (default), computes a
    /// non-mutating merge-tree preview with Git's default merge algorithm and
    /// returns the diff stat. Callers should preview first, then confirm with
    /// confirm=true.
    #[serde(default)]
    pub confirm: bool,
}

// ─── Facade: task completion + eval ledger ───────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, JsonSchema)]
pub struct TachiSubagentEvalParams {
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
        deserialize_with = "crate::coerce::opt_f64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_number_from_string_or_number_schema")]
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
        deserialize_with = "crate::coerce::opt_u64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_integer_from_string_or_number_schema")]
    pub latency_ms: Option<u64>,

    /// Input tokens/context tokens for this subagent if known
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_integer_from_string_or_number_schema")]
    pub input_tokens: Option<u64>,

    /// Output tokens for this subagent if known
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_integer_from_string_or_number_schema")]
    pub output_tokens: Option<u64>,

    /// Cost in tokens for this subagent if known
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_integer_from_string_or_number_schema")]
    pub cost_tokens: Option<u64>,

    /// Cost in USD for this subagent if known
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_f64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_number_from_string_or_number_schema")]
    pub cost_usd: Option<f64>,

    /// Where execution happened: harness_native | tachi_dispatch | manual_external.
    #[serde(default)]
    pub execution_origin: Option<String>,

    /// Runtime that owns wait/cancel/close semantics: codex | claude | opencode | tachi | manual.
    #[serde(default)]
    pub lifecycle_owner: Option<String>,

    /// Harness/client name when execution_origin is harness_native.
    #[serde(default)]
    pub harness: Option<String>,

    /// Native worker/session id from the owning harness, if any.
    #[serde(default)]
    pub native_agent_id: Option<String>,

    /// Tachi dispatch id when Tachi owns the worker lifecycle.
    #[serde(default)]
    pub tachi_dispatch_id: Option<String>,

    /// Whether an actual worker result was collected, distinct from run metadata.
    #[serde(default)]
    pub result_collected: Option<bool>,

    /// Whether the collected result was usable evidence for evaluation/routing.
    #[serde(default)]
    pub evidence_usable: Option<bool>,

    /// Whether this subagent output was used in the leader's final claim.
    #[serde(default)]
    pub used_in_final_claim: Option<bool>,

    /// Prompt/agent-contract adjustment learned from this subagent run.
    #[serde(default)]
    pub next_prompt_delta: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiCompleteParams {
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

    /// Dispatch profile used for this completion, if any.
    #[serde(default)]
    pub profile: Option<String>,

    /// Risk class used by routing: low | medium | high | critical.
    #[serde(default)]
    pub risk: Option<String>,

    /// Execution duration in milliseconds
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_integer_from_string_or_number_schema")]
    pub duration_ms: Option<u64>,

    /// Skills used during execution (capability IDs)
    #[serde(default)]
    pub skills_used: Vec<String>,

    /// Cost in tokens (total across all turns)
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_integer_from_string_or_number_schema")]
    pub cost_tokens: Option<u64>,

    /// Cost in USD (if known)
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_f64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_number_from_string_or_number_schema")]
    pub cost_usd: Option<f64>,

    /// Quality score 0.0–1.0 (self-reported or computed later)
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_f64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_number_from_string_or_number_schema")]
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

    /// Feedback/prompt-quality rule ids that were applied to this task.
    #[serde(default)]
    pub feedback_rules_applied: Vec<String>,

    /// Parent dispatch ID (links back to tachi_dispatch record)
    #[serde(default)]
    pub dispatch_id: Option<String>,

    /// Feature flow id linked to this completion.
    #[serde(default)]
    pub flow_id: Option<String>,

    /// GitHub issue reference linked to this completion.
    #[serde(default)]
    pub issue_ref: Option<String>,

    /// GitHub PR reference linked to this completion.
    #[serde(default)]
    pub pr_ref: Option<String>,

    /// Evidence references used to verify the outcome (files, issue refs, run artifacts).
    #[serde(default)]
    pub evidence_refs: Vec<String>,

    /// Verification commands run by the leader or subagent.
    #[serde(default)]
    pub tests_run: Vec<String>,

    /// Whether a code diff was present. If absent, inferred from `diff`.
    #[serde(default)]
    pub diff_present: Option<bool>,

    /// Target database scope for the eval entry: "global" or "project" (default)
    #[serde(default)]
    pub scope: Option<String>,

    /// Optional named project DB
    #[serde(default)]
    pub project: Option<String>,

    /// Response shape: default receipt, or "full" for the pre-change verbose review bundle.
    #[serde(default, alias = "output_format")]
    pub format: Option<String>,

    /// Adjudicated, vendor-keyed error signatures to record for this dispatch's
    /// lane (#735). Additive and optional: omitting it leaves `complete`
    /// byte-compatible with pre-existing callers.
    #[serde(default)]
    pub signatures: Vec<SignatureRecordParams>,

    /// Leader adjudication rulings to capture as precedent memory rows (#950
    /// slice 1: capture only). Additive and optional: an empty/omitted array
    /// leaves `complete` byte-compatible with pre-existing callers, writing no
    /// `/precedents` rows. Rulings are stored faithfully as supplied; the
    /// verdict→principle decomposition lane is a later slice.
    #[serde(default)]
    pub rulings: Vec<RulingRecordParams>,
}

/// One caller-supplied leader adjudication captured at `complete` (#950). Stored
/// faithfully under `/precedents/<project>/<date>-<shortid>` with structured
/// fields in metadata and a human-readable rendering in the body. `case` and
/// `ruling` are the two required fields; a ruling missing either is skipped with
/// a warning and never fails the enclosing `complete` call.
#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct RulingRecordParams {
    /// The finding + context the ruling adjudicates (required).
    pub case: String,

    /// The options the leader weighed before ruling.
    #[serde(default)]
    pub options_considered: Option<String>,

    /// The adjudication itself — the decision the leader made (required).
    pub ruling: String,

    /// Constitution clauses / prior precedents cited in support.
    #[serde(default)]
    pub principles_cited: Vec<String>,

    /// Truth-maintenance status: "validated" | "overturned" | "pending".
    /// Defaults to "pending" when omitted.
    #[serde(default)]
    pub outcome: Option<String>,

    /// When `outcome` is "overturned", the ruling/precedent that overturned it.
    #[serde(default)]
    pub overturned_by: Option<String>,
}

/// One leader-adjudicated error signature (or resolution) recorded at
/// `complete`. Keyed onto the `(role, vendor)` lane derived from the dispatch's
/// profile/agent unless `role`/`vendor` are supplied explicitly.
#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct SignatureRecordParams {
    /// Stable taxonomy id, e.g. `fake_security_fix`, `assertion_weakening`.
    pub signature: String,

    /// Severity override: low | medium | high | critical. Defaults to the
    /// taxonomy severity for the signature id.
    #[serde(default)]
    pub severity: Option<String>,

    /// Evidence reference (issue/PR/run id) backing this signature.
    #[serde(default)]
    pub evidence_ref: Option<String>,

    /// When true, append a resolution row marking the signature resolved as of
    /// now (evidence is append-only; nothing is deleted).
    #[serde(default)]
    pub resolved: bool,

    /// Explicit role class override (implementer | reviewer | ...). Defaults to
    /// the dispatch profile's role class.
    #[serde(default)]
    pub role: Option<String>,

    /// Explicit vendor lane override. Defaults to the vendor derived from the
    /// dispatch profile/agent.
    #[serde(default)]
    pub vendor: Option<String>,
}
