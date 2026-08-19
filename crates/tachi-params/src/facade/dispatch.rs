use super::task::TachiDispatchReason;
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

/// Declared side-effect level for a dispatched task. This describes the
/// target state, not the agent's permission flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize, JsonSchema)]
pub enum ExecutionLevel {
    #[serde(rename = "L0", alias = "l0", alias = "0")]
    L0,
    #[serde(rename = "L1", alias = "l1", alias = "1")]
    L1,
    #[serde(rename = "L2", alias = "l2", alias = "2")]
    L2,
    #[serde(rename = "L3", alias = "l3", alias = "3")]
    L3,
}

impl ExecutionLevel {
    pub const fn rank(self) -> u8 {
        match self {
            Self::L0 => 0,
            Self::L1 => 1,
            Self::L2 => 2,
            Self::L3 => 3,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::L0 => "L0",
            Self::L1 => "L1",
            Self::L2 => "L2",
            Self::L3 => "L3",
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiDispatchParams {
    /// #1319 admission contract: the typed reason execution is leaving the
    /// host harness instead of using its native subagent. REQUIRED (non-
    /// optional) — the canonical kernel fails closed without it, and
    /// `tachi_staff(start)` rejects a missing reason at deserialization.
    /// Reuses [`TachiDispatchReason`] so the vocabulary cannot drift from the
    /// retired `tachi_task(dispatch)` `require_tachi_dispatch_reason` gate.
    /// Stamped into the canonical receipt (status.json) so staffing is
    /// auditable.
    ///
    /// Note: `ExplicitUserRequest` is a *recorded override* naming why a user
    /// asked for external staffing — it is NOT a self-granted authority
    /// elevation. The #894 authority gate (`compile_dispatch_contract`) is
    /// independent and still applies; this reason is evidence, not a bypass.
    pub staffing_reason: TachiDispatchReason,

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

    /// Declared execution level for host-profile routing. Omitted legacy
    /// dispatches default to L1 (temporary local state), never L0.
    #[serde(default)]
    pub execution_level: Option<ExecutionLevel>,

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

    /// tachi#1173 item 1: the default dispatch response is a slim receipt
    /// (dispatch_id, state, run_dir, suggested_complete_command, plus other
    /// small metadata) — the full routing card (`profile`, `identity_receipt`,
    /// `dispatch_profile`/mbit_card) is selection-time information, not
    /// receipt information, and is omitted by default. Set verbose=true to
    /// get the full payload back on the dispatch response itself; operator
    /// profile diagnostics remain on the local CLI surface.
    #[serde(default)]
    pub verbose: Option<bool>,

    /// tachi#1202/#993 (L2 packet projection): when the resolved profile/vendor
    /// maps to a `/cards/<seat>` lane-card mirror row, its 反制条款
    /// (counter-clause) section is inlined into the prompt by default. Set
    /// `inject_card=false` to suppress this — e.g. a caller that already
    /// hand-copies its own countermeasures and does not want them doubled.
    #[serde(default)]
    pub inject_card: Option<bool>,
}

// ─── Staffing Ownership Boundaries (Issue #1692 C5) ─────────────────────────

/// Typed public semantic request for staffing (Issue #1692 C5).
/// Contains only semantic intent, context, and user constraints.
/// Machine/authority/transport plumbing is strictly omitted.
#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct StaffAssignmentRequest {
    /// #1319 admission contract: typed reason execution is leaving the host harness.
    pub staffing_reason: TachiDispatchReason,

    /// Task description / semantic outcome required from the worker.
    pub task: String,

    /// Semantic dispatch profile hint (e.g. "claude_plan", "codex_55_review").
    #[serde(default)]
    pub profile: Option<String>,

    /// Semantic worker backend hint (e.g. "claude", "codex", "custom").
    #[serde(default)]
    pub worker: Option<String>,

    /// Task class/scope hints.
    #[serde(default)]
    pub stage: Option<String>,

    /// Declared execution level.
    #[serde(default)]
    pub execution_level: Option<ExecutionLevel>,

    /// GitHub issue reference bound to this assignment.
    #[serde(default)]
    pub issue_ref: Option<String>,

    /// GitHub PR reference bound to this assignment.
    #[serde(default)]
    pub pr_ref: Option<String>,

    /// Tachi flow id for feature-scoped linkage.
    #[serde(default)]
    pub flow_id: Option<String>,

    /// Optional named project DB for context search.
    #[serde(default)]
    pub project: Option<String>,

    /// Machine-checkable completion predicate for the assignment.
    #[serde(default)]
    pub completion_predicate: Option<CompletionPredicate>,

    /// Recommendation reference if this assignment followed prior advice.
    #[serde(default)]
    pub recommendation_ref: Option<String>,
}

impl StaffAssignmentRequest {
    pub fn from_dispatch_params(params: &TachiDispatchParams) -> Self {
        Self {
            staffing_reason: params.staffing_reason,
            task: params.task.clone(),
            profile: params.profile.clone(),
            worker: params.agent.clone(),
            stage: params.stage.clone(),
            execution_level: params.execution_level,
            issue_ref: params.issue_ref.clone(),
            pr_ref: params.pr_ref.clone(),
            flow_id: params.flow_id.clone(),
            project: params.project.clone(),
            completion_predicate: params.completion_predicate.clone(),
            recommendation_ref: None,
        }
    }

    pub fn into_dispatch_params(self) -> TachiDispatchParams {
        TachiDispatchParams {
            task: self.task,
            staffing_reason: self.staffing_reason,
            agent: self.worker,
            profile: self.profile,
            project: self.project,
            stage: self.stage,
            issue_ref: self.issue_ref,
            pr_ref: self.pr_ref,
            flow_id: self.flow_id,
            execution_level: self.execution_level,
            completion_predicate: self.completion_predicate,
            cwd: None,
            env_id: None,
            unmanaged_cwd: None,
            command: Vec::new(),
            harness_transport: None,
            harness_server_url: None,
            sandbox: None,
            allowed_tools: Vec::new(),
            permission_profile: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            allowed_mcp_servers: Vec::new(),
            tool_profile: None,
            mcp_access: None,
            credential_profiles: Vec::new(),
            skills: Vec::new(),
            context_query: None,
            model: None,
            max_turns: None,
            timeout_secs: default_dispatch_timeout(),
            auto_capability_bundle: None,
            verbose: None,
            inject_card: None,
        }
    }

    pub fn into_params(self) -> TachiDispatchParams {
        self.into_dispatch_params()
    }
}

/// Server-produced admission and policy resolution result (Issue #1692 C5).
/// Produced strictly by policy/admission gates.
#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct ResolvedStaffAssignment {
    pub assignment_id: String,
    pub staffing_reason: TachiDispatchReason,
    pub selected_worker: String,
    pub selected_profile: Option<String>,
    pub selected_backend: String,
    pub selected_model: Option<String>,
    pub host_adapter: Option<String>,
    pub evidence_required: Vec<String>,
    pub fallback_chain: Vec<String>,
    pub route_explanation: Vec<String>,
    pub identity_receipt: serde_json::Value,
}

/// Authority-layer output granting permissions, sandbox, credentials, and tools (Issue #1692 C5).
/// Minted exclusively by authority/resource enforcement layers.
#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct ExecutionGrant {
    pub grant_id: String,
    #[serde(default)]
    pub env_id: Option<String>,
    #[serde(default)]
    pub unmanaged_cwd_allowed: bool,
    #[serde(default)]
    pub allowed_cwd: Option<std::path::PathBuf>,
    #[serde(default)]
    pub credential_profiles: Vec<String>,
    #[serde(default)]
    pub mcp_access: Option<DispatchMcpAccessParams>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub permission_profile: Option<String>,
    #[serde(default)]
    pub sandbox: Option<String>,
    #[serde(default)]
    pub max_turns: Option<u32>,
    #[serde(default = "default_dispatch_timeout")]
    pub timeout_secs: u64,
}

impl ExecutionGrant {
    pub fn from_dispatch_params(params: &TachiDispatchParams, grant_id: impl Into<String>) -> Self {
        Self {
            grant_id: grant_id.into(),
            env_id: params.env_id.clone(),
            unmanaged_cwd_allowed: params.unmanaged_cwd.unwrap_or(false),
            allowed_cwd: params.cwd.as_ref().map(std::path::PathBuf::from),
            credential_profiles: params.credential_profiles.clone(),
            mcp_access: params.mcp_access.clone(),
            allowed_tools: params.allowed_tools.clone(),
            permission_profile: params.permission_profile.clone(),
            sandbox: params.sandbox.clone(),
            max_turns: params.max_turns,
            timeout_secs: params.timeout_secs,
        }
    }
}

/// Backend adapter execution mechanics (Issue #1692 C5).
/// Consumed strictly by execution backends/adapters.
#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct LaunchSpec {
    pub backend: String,
    pub command: Vec<String>,
    pub cwd: std::path::PathBuf,
    pub env_vars: std::collections::HashMap<String, String>,
    pub prompt: String,
    pub timeout_secs: u64,
    #[serde(default)]
    pub harness_transport: Option<String>,
    #[serde(default)]
    pub harness_server_url: Option<String>,
}

/// Append-only observed lifecycle facts for a staffing run (Issue #1692 C5).
/// Readback and observation only — never caller-authored success.
#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct StaffRunReceipt {
    pub dispatch_id: String,
    #[serde(default)]
    pub assignment_id: Option<String>,
    pub state: String,
    pub run_dir: String,
    pub suggested_complete_command: serde_json::Value,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub identity_receipt: Option<serde_json::Value>,
}

impl StaffRunReceipt {
    pub fn new(
        dispatch_id: impl Into<String>,
        run_dir: impl Into<String>,
        suggested_complete_command: serde_json::Value,
    ) -> Self {
        Self {
            dispatch_id: dispatch_id.into(),
            assignment_id: None,
            state: "working".to_string(),
            run_dir: run_dir.into(),
            suggested_complete_command,
            started_at: None,
            finished_at: None,
            exit_code: None,
            identity_receipt: None,
        }
    }
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

    /// `eval_run_id`s from the #1066 `tachi_agent_eval` mirror eval intake
    /// (register/observe/adjudicate) to project into this completion's eval
    /// row. Only ADJUDICATED, evidence-usable, non-self-eval rows are
    /// projected into `subagents[]`-compatible aggregation; unresolved or
    /// ineligible ids are silently skipped and never fail the completion.
    /// Additive/optional — an empty/omitted array leaves `subagents[]`
    /// byte-compatible with existing callers.
    #[serde(default)]
    pub eval_run_ids: Vec<String>,

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

    /// Leader terminal adjudication for this dispatch outcome (#1035). When
    /// present, an append-only `dispatch_adjudications` row is written linked
    /// to the outcome by `outcome_id`. Additive and optional: omitting it
    /// leaves `complete` byte-compatible with pre-existing callers.
    #[serde(default)]
    pub adjudication: Option<AdjudicationParams>,
}

/// Leader terminal judgment for a dispatch outcome, captured at `complete`
/// (#1035). Exactly one of `verdict` / `not_required_reason` must be present
/// — mirroring the DB CHECK constraint on `dispatch_adjudications`. When
/// `not_required_reason` is present it MUST be a member of
/// [`memcore::NOT_REQUIRED_REASONS`] (the closed-set gate lives in the
/// memcore write primitive). Each signature id in the enclosing
/// `TachiCompleteParams::signatures` must resolve via
/// `tachi_dispatch::resolve_signature_id` or the entire adjudication is
/// rejected loudly.
#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct AdjudicationParams {
    /// Terminal verdict, e.g. "accepted" | "rejected". Mutually exclusive
    /// with `not_required_reason`.
    #[serde(default)]
    pub verdict: Option<String>,

    /// Closed-set reason code explaining why adjudication was not required.
    /// Mutually exclusive with `verdict`; must be a member of
    /// `NOT_REQUIRED_REASONS` when present.
    #[serde(default)]
    pub not_required_reason: Option<String>,

    /// Who adjudicated (the leader/seat identity). Required.
    pub adjudicator: String,

    /// Evidence reference backing this judgment. Defaults to the outcome id
    /// when absent (the DB column is NOT NULL).
    #[serde(default)]
    pub evidence_ref: Option<String>,
}

impl AdjudicationParams {
    /// Validate that exactly one of `verdict` / `not_required_reason` is
    /// present and non-empty. Returns an error message string when invalid.
    pub fn validate_exactly_one(&self) -> Result<(), String> {
        let has_verdict = self
            .verdict
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty());
        let has_reason = self
            .not_required_reason
            .as_deref()
            .is_some_and(|r| !r.trim().is_empty());
        match (has_verdict, has_reason) {
            (true, false) | (false, true) => Ok(()),
            (true, true) => Err(
                "adjudication verdict and not_required_reason are mutually exclusive; \
                 supply exactly one"
                    .to_string(),
            ),
            (false, false) => Err(
                "adjudication requires exactly one of verdict or not_required_reason".to_string(),
            ),
        }
    }
}

/// One caller-supplied leader adjudication captured at `complete` (#950). Stored
/// faithfully under `/precedents/<project>/<shortid>`, where `shortid` is a
/// deterministic hash of the ruling's case identity and content — project,
/// `issue_ref`, and every normalized ruling field; never the capture date,
/// capture provenance (dispatch/flow/pr), or randomness — see
/// `precedent_ops::precedent_short_id`. Re-capturing the same ruling dedupes
/// to the first row (which keeps the first capture's provenance), with structured
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

    /// Who made this specific ruling (the leader/owner seat identity). Feeds
    /// #1076 principle-candidate decomposition's `authority_complete` gate —
    /// omitting it never fails capture, but the resulting candidate(s) stay
    /// unable to claim adjudicator authority (see `precedent_candidate_ops`).
    #[serde(default)]
    pub adjudicator: Option<String>,

    /// Immutable evidence refs (issue/comment/PR/commit/doc/verification)
    /// backing this ruling, each pinned to a specific revision (#1076).
    /// Optional and additive: an empty array leaves `complete` byte-compatible
    /// with pre-#1076 callers and simply means the resulting candidate(s)
    /// cannot claim source authority.
    #[serde(default)]
    pub source_refs: Vec<RulingSourceRefParams>,

    /// Effective decomposition/adjudication engine identity for this ruling
    /// (#1076). Absence, an unknown provider/model, a non-empty fallback
    /// chain, or a degraded run all mark the resulting candidate(s)
    /// `identity_status = "preview_only"` rather than `"known"`.
    #[serde(default)]
    pub engine_receipt: Option<RulingEngineReceiptParams>,
}

/// One immutable evidence reference backing a [`RulingRecordParams`] (#1076
/// principle-candidate decomposition). Conceptually mirrors the canon doc's
/// `EvidenceRefV1` / `ImmutableRevisionV1`
/// (`docs/engineering/architecture/issue-refinery-memory-lanes.md` §3), but
/// is deliberately its own flat, always-tagged wire type rather than a reuse
/// of `tachi_params::refinery::ImmutableRevisionV1` — that type's own doc
/// comment flags an unresolved `#[serde(untagged)]` deserialize ambiguity
/// across its six bare-string variants (ok for its current outward-only
/// serialization use, not safe as an *inbound* wire shape). `target_kind`
/// plus whichever concrete revision fields are present disambiguate which
/// immutable-revision shape this ref carries without inheriting that
/// ambiguity.
#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct RulingSourceRefParams {
    /// How this evidence relates to the ruling: "derived_from" | "supports" |
    /// "contradicts" | "supersedes" | "applies_to". Defaults to "supports".
    #[serde(default)]
    pub relation: Option<String>,

    /// Source kind: "issue" | "comment" | "pr" | "commit" | "canonical_doc" |
    /// "verification" (required).
    pub target_kind: String,

    /// The source's own reference, e.g. "kckylechen1/tachi#530" or
    /// "kckylechen1/tachi#530#issuecomment-123" (required).
    pub target_ref: String,

    /// Comment id, when `target_kind == "comment"`. Combined with
    /// `updated_at` + `body_hash`, pins the exact comment revision this
    /// ruling relied on — an edited comment (same `comment_id`, different
    /// `updated_at`/`body_hash`) is a different immutable revision and, per
    /// #1076, must not be treated as an identical replay.
    #[serde(default)]
    pub comment_id: Option<String>,

    /// The source's `updated_at` at capture time.
    #[serde(default)]
    pub updated_at: Option<String>,

    /// Content hash of the pinned revision (comment body hash, issue body
    /// hash, PR snapshot hash, blob SHA, ...) depending on `target_kind`.
    #[serde(default)]
    pub body_hash: Option<String>,

    /// Repo-revision anchor, when applicable (commit SHA / PR head SHA).
    #[serde(default)]
    pub commit_sha: Option<String>,

    /// Optional section/span within the source this evidence covers.
    #[serde(default)]
    pub section_or_span: Option<String>,
}

/// Effective decomposition/adjudication engine identity for a
/// [`RulingRecordParams`] (#1076 required behavior: "Record effective
/// provider/model/version/fallback/degraded receipt. Unknown/fallback
/// identity is preview-only.").
#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct RulingEngineReceiptParams {
    /// Role requested for this ruling (e.g. "leader", "adjudicator").
    #[serde(default)]
    pub requested_role: Option<String>,

    /// Provider actually used (e.g. "anthropic"). Absence makes identity
    /// unknown.
    #[serde(default)]
    pub effective_provider: Option<String>,

    /// Model actually used (e.g. "claude-sonnet-5"). Absence makes identity
    /// unknown.
    #[serde(default)]
    pub effective_model: Option<String>,

    /// Model/version string, when known.
    #[serde(default)]
    pub effective_version: Option<String>,

    /// Non-empty when the run fell back from its originally requested
    /// provider/model.
    #[serde(default)]
    pub fallback_chain: Vec<String>,

    /// True when the run completed in a degraded state (timeout, partial
    /// tool access, etc.).
    #[serde(default)]
    pub degraded: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staff_assignment_request_roundtrip_and_mapping() {
        let raw = serde_json::json!({
            "task": "Refactor staffing types",
            "staffing_reason": "durable_cross_session",
            "profile": "claude_plan",
            "worker": "claude",
            "stage": "plan",
            "execution_level": "L1",
            "issue_ref": "kckylechen1/tachi#1692",
            "flow_id": "flow-c5",
        });

        let req: StaffAssignmentRequest = serde_json::from_value(raw).expect("deserializes");
        assert_eq!(req.task, "Refactor staffing types");
        assert_eq!(
            req.staffing_reason,
            TachiDispatchReason::DurableCrossSession
        );
        assert_eq!(req.execution_level, Some(ExecutionLevel::L1));

        let params = req.into_dispatch_params();
        assert_eq!(params.task, "Refactor staffing types");
        assert_eq!(
            params.staffing_reason,
            TachiDispatchReason::DurableCrossSession
        );
        assert_eq!(params.profile.as_deref(), Some("claude_plan"));
        assert_eq!(params.agent.as_deref(), Some("claude"));
        assert_eq!(params.execution_level, Some(ExecutionLevel::L1));
        assert_eq!(params.timeout_secs, 600);
    }

    #[test]
    fn execution_grant_from_dispatch_params_extracts_authorities() {
        let raw = serde_json::json!({
            "task": "Test grant",
            "staffing_reason": "explicit_user_request",
            "cwd": "/workspace/target",
            "unmanaged_cwd": true,
            "sandbox": "workspace-write",
            "permission_profile": "allowlist",
            "allowed_tools": ["Bash", "Read"],
            "max_turns": 25,
            "timeout_secs": 1200,
        });

        let params: TachiDispatchParams = serde_json::from_value(raw).expect("deserializes");
        let grant = ExecutionGrant::from_dispatch_params(&params, "grant-123");
        assert_eq!(grant.grant_id, "grant-123");
        assert!(grant.unmanaged_cwd_allowed);
        assert_eq!(
            grant.allowed_cwd,
            Some(std::path::PathBuf::from("/workspace/target"))
        );
        assert_eq!(grant.sandbox.as_deref(), Some("workspace-write"));
        assert_eq!(grant.allowed_tools, vec!["Bash", "Read"]);
        assert_eq!(grant.max_turns, Some(25));
        assert_eq!(grant.timeout_secs, 1200);
    }

    #[test]
    fn staff_run_receipt_initial_state() {
        let receipt = StaffRunReceipt::new(
            "dispatch-20260819-claude-test",
            "/tmp/runs/dispatch-20260819-claude-test",
            serde_json::json!({"action": "complete", "status": "success"}),
        );
        assert_eq!(receipt.dispatch_id, "dispatch-20260819-claude-test");
        assert_eq!(receipt.state, "working");
        assert_eq!(receipt.exit_code, None);
        assert_eq!(receipt.finished_at, None);
    }
}
