use super::{string_enum_schema, DispatchMcpAccessParams};
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

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

fn tachi_verify_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &["start", "record", "status", "board"],
        "Required Tachi verification ledger action.",
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

// ─── Facade: tachi_arena (tracked worker mission ledger) ────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiArenaParams {
    /// Action: "open", "spawn", "board", "collect", "abort", "reap", or "close"
    #[schemars(schema_with = "tachi_arena_action_schema")]
    pub action: String,

    /// Response shape: default JSON for agent automation; pass "markdown" for human-readable text.
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

    /// Working directory passed to launched dispatches when launch=true; stored
    /// as mission metadata for document-only lanes.
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

    /// Mission timeout in seconds. For launch=true this is the dispatch timeout;
    /// for active arena missions it is also the reap threshold when supplied.
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u64_from_string_or_number"
    )]
    pub timeout_secs: Option<u64>,

    /// When true, spawn creates the tracked mission documents and also launches
    /// the supported worker harness through the existing dispatch runtime.
    #[serde(default)]
    pub launch: bool,

    /// Optional dispatch profile used when launch=true.
    #[serde(default, alias = "dispatch_profile")]
    pub profile: Option<String>,

    /// Optional model override used when launch=true.
    #[serde(default)]
    pub model: Option<String>,

    /// Optional named project DB for dispatch context.
    #[serde(default)]
    pub project: Option<String>,

    /// Tachi flow id for feature-scoped dispatch/eval linkage.
    #[serde(default)]
    pub flow_id: Option<String>,

    /// GitHub issue reference bound to the launched dispatch.
    #[serde(default)]
    pub issue_ref: Option<String>,

    /// GitHub PR reference bound to the launched dispatch.
    #[serde(default)]
    pub pr_ref: Option<String>,

    /// Dispatch permission profile passed through when launch=true.
    #[serde(default)]
    pub permission_profile: Option<String>,

    /// Codex sandbox mode passed through when launch=true.
    #[serde(default)]
    pub sandbox: Option<String>,

    /// Credential profile ids to materialize before launching the worker.
    #[serde(default)]
    pub credential_profiles: Vec<String>,

    /// Expected child-agent tool surface when launch=true.
    #[serde(default)]
    pub tool_profile: Option<String>,

    /// Include a capability bundle in the launched dispatch prompt when supported.
    #[serde(default, alias = "include_capability_bundle")]
    pub auto_capability_bundle: Option<bool>,

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

// ─── Facade: tachi_verify (background verification ledger) ──────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiVerifyParams {
    /// Action: "start", "record", "status", or "board".
    #[schemars(schema_with = "tachi_verify_action_schema")]
    pub action: String,

    /// Response shape: "markdown" (default, agent-readable) or "json" (automation).
    #[serde(default)]
    pub format: Option<String>,

    /// Tachi flow id whose .tachi/runs/<flow_id>/verification.json ledger is read or updated.
    #[serde(default)]
    pub flow_id: Option<String>,

    /// Optional GitHub PR reference, e.g. owner/repo#209.
    #[serde(default)]
    pub pr_ref: Option<String>,

    /// Git head SHA the check result was produced for. Safe-merge treats mismatched required checks as stale.
    #[serde(default)]
    pub head_sha: Option<String>,

    /// Stable check id, e.g. gitleaks, cargo-check, clippy, memory-server-tests.
    #[serde(default)]
    pub check_id: Option<String>,

    /// Check kind, e.g. gitleaks, cargo_check, clippy, cargo_test, custom.
    #[serde(default)]
    pub kind: Option<String>,

    /// Command recorded for a single verification result.
    #[serde(default)]
    pub command: Option<String>,

    /// Commands to seed or update in bulk. Used by action=start to mark multiple gates pending.
    #[serde(default)]
    pub commands: Vec<String>,

    /// Verification status: pending, running, passed, failed, skipped, or stale.
    #[serde(default)]
    pub status: Option<String>,

    /// Process exit code when known.
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_i64_from_string_or_number"
    )]
    pub exit_code: Option<i64>,

    /// Path to a durable log/artifact for this verification run.
    #[serde(default)]
    pub log_path: Option<String>,

    /// Short result summary, e.g. "no leaks found" or "608 passed; 1 ignored".
    #[serde(default)]
    pub summary: Option<String>,

    /// Working directory the command was run in.
    #[serde(default)]
    pub cwd: Option<String>,

    /// Whether this check is required for safe_merge. Defaults true.
    #[serde(default)]
    pub required: Option<bool>,

    /// Board/status result limit when flow_id is omitted.
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u32_from_string_or_number"
    )]
    pub limit: Option<u32>,
}

// ─── Facade: tachi_shell (skill-gated flow orchestration) ────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiShellDispatchSliceParams {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default, alias = "dispatch_profile")]
    pub profile: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub tool_profile: Option<String>,
    #[serde(default)]
    pub mcp_access: Option<DispatchMcpAccessParams>,
    #[serde(default)]
    pub allowed_mcp_servers: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub validation: Vec<String>,
    #[serde(default)]
    pub allowed_scope: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiShellParams {
    /// Action: "brainstorm" | "plan" | "dispatch" | "kanban" | "status" | "review" | "ship"
    #[schemars(schema_with = "tachi_shell_action_schema")]
    pub action: String,

    /// Response shape: default JSON for agent automation; pass "markdown" for human-readable text.
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

    /// Optional dispatch profile forwarded to the underlying tachi_dispatch.
    #[serde(default, alias = "dispatch_profile")]
    pub profile: Option<String>,

    /// Optional cwd override for downstream dispatch.
    #[serde(default)]
    pub cwd: Option<String>,

    /// Tachi MCP ToolProfile for async-dispatched workers.
    #[serde(default)]
    pub tool_profile: Option<String>,

    /// Optional explicit MCP/tool access contract for async-dispatched workers.
    #[serde(default)]
    pub mcp_access: Option<DispatchMcpAccessParams>,

    /// Hub MCP allowlist for async-dispatched workers.
    #[serde(default)]
    pub allowed_mcp_servers: Vec<String>,

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
pub struct TachiOrchestratorParams {
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
pub struct TachiAgentEvalParams {
    /// aggregate_live | telemetry | perf. aggregate replays a local JSONL fixture only when
    /// TACHI_AGENT_EVAL_ALLOW_FIXTURE=1 is set.
    pub action: String,
    #[serde(default)]
    pub fixture_path: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

// ─── Facade: agent registry / router ─────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiAgentsParams {
    /// list | select
    pub action: String,
    #[serde(default)]
    pub intent: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
}

// ─── Facade: task board (kanban) ─────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiBoardParams {
    /// Filter by state: "active", "working", "completed", "failed", "all" (default: "all")
    #[serde(default)]
    pub state_filter: Option<String>,

    /// Maximum number of tasks to return (default: 20)
    #[serde(default)]
    pub limit: Option<usize>,

    /// Optional named project DB
    #[serde(default)]
    pub project: Option<String>,

    /// Optional Tachi flow id; when set, return only dispatches linked to that flow.
    #[serde(default)]
    pub flow_id: Option<String>,
}
