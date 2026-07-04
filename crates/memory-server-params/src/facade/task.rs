use super::{string_enum_schema, DispatchMcpAccessParams, TachiSubagentEvalParams};
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

fn tachi_task_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &[
            "plan",
            "briefing",
            "doc_index",
            "recommend",
            "dispatch",
            "complete",
            "profiles",
            "profile",
            "card",
            "route_simulate",
            "proposals",
            "review_proposal",
            "apply_proposals",
            "status",
            "cancel",
            "board",
            "wait",
            "merge",
            "intake",
            "link_pr",
            "cycle_status",
            "cycle_plan",
            "pr_status",
            "pr_handoff",
            "release_note",
            "ux_matrix",
            "build_references",
            "close_loop",
        ],
        "Required Tachi task facade action. action='briefing' returns a feature-scoped handoff board; action='doc_index' returns the same project-first layered source index for agent context assembly; action='status' reads one dispatch ledger and may query backend-local status; action='cancel' requests cooperative cancellation for a dispatch backend that supports it; action='wait' polls a dispatch until terminal state; action='complete' records evaluated completion evidence; action='route_simulate' replays recent /eval rows across current, cost_sensitive, and quality_first routing policies without mutating policy; action='proposals' lists/generates route-policy and loadout-evolution proposals from replay/eval evidence; action='review_proposal' approves/rejects a proposal; action='apply_proposals' persists an approved route-policy rule without silently mutating recommendation scoring; approved loadout-evolution proposals wait for MBIT/profile-card projection; action='intake' binds a GitHub issue to a Tachi flow; action='cycle_status' returns a read-only project lifecycle status from linked issue/PR, docs/specs, flow artifacts, verification, and closure state; action='cycle_plan' turns cycle_status into a read-only ordered action plan/checklist for agents; action='ux_matrix' writes/returns a feature UX workflow checklist for the issue→briefing→dispatch→PR→release lifecycle; action='close_loop' writes issue/doc/wiki closure; action='merge' is local dispatched worktree git merge only; use tachi_gh(action='safe_merge') to execute GitHub PR merges. GitHub PR lifecycle actions link_pr/pr_status/pr_handoff/release_note remain accepted here for compatibility, but their canonical surface is tachi_gh.",
        generator,
    )
}

// ─── Facade: task (plan / recommend / dispatch / board / merge / lifecycle) ──

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiTaskParams {
    /// Action: "plan", "briefing", "doc_index", "recommend", "dispatch", "complete", "profiles", "profile", "card", "route_simulate", "proposals", "review_proposal", "apply_proposals", "status", "cancel", "board", "wait", "merge", "intake", "link_pr", "cycle_status", "cycle_plan", "pr_status", "pr_handoff", "release_note", "ux_matrix", "build_references", or "close_loop".
    /// action="merge" is local dispatched worktree git merge only; use
    /// tachi_gh(action='safe_merge') to execute GitHub PR merges.
    /// action="intake" reads/binds a GitHub issue to a Tachi flow and seeds flow artifacts.
    /// action="link_pr" attaches a GitHub PR to an existing flow. Prefer tachi_gh(action="link_pr").
    /// action="cycle_status" returns a read-only lifecycle status from linked issue/PR, docs/specs, flow artifacts, verification, and closure state.
    /// action="cycle_plan" returns a read-only ordered lifecycle action plan derived from cycle_status.
    /// action="pr_status" previews GitHub PR safe-merge status and may persist flow status. Prefer tachi_gh(action="pr_status").
    /// action="pr_handoff" writes a PR body/branch handoff from flow, issue, verification, and gaps. Prefer tachi_gh(action="pr_handoff").
    /// action="release_note" synthesizes a release/changelog note from a flow or PR and writes
    /// release_note.md when flow_id is supplied. Prefer tachi_gh(action="release_note").
    /// action="ux_matrix" returns a feature workflow UX checklist and writes ux_matrix.json
    /// when flow_id is supplied.
    /// action="build_references" previews the issue/doc/related reference array.
    /// action="close_loop" writes durable wiki closure through the task lifecycle.
    /// action="doc_index" exposes the layered GitHub/docs/wiki/guide/feedback/eval/runtime source index.
    /// Use "recommend" before assigning external workers so Tachi can choose a dispatch profile
    /// from the task, risk, and live eval evidence.
    /// Use "route_simulate" to replay recent /eval rows across policy variants before
    /// proposing routing changes.
    #[schemars(schema_with = "tachi_task_action_schema")]
    pub action: String,
    /// Response shape: default JSON for agent automation; pass "markdown" for human-readable text.
    #[serde(default)]
    pub format: Option<String>,
    // plan fields
    #[serde(default)]
    #[schemars(
        description = "[action=plan|recommend|dispatch|route_simulate|complete|intake|pr_handoff|ux_matrix] Task description / prompt text."
    )]
    pub task: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=plan|recommend|dispatch] Requesting agent id.")]
    pub agent_id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=plan|recommend|briefing] Area tag (e.g. rust, mcp) to scope context."
    )]
    pub domain: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=plan|recommend|briefing] Optional recall path prefix filter."
    )]
    pub path_prefix: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=plan|recommend|briefing] Maximum context fragments to recall."
    )]
    pub top_k: Option<usize>,
    // feature briefing fields
    #[serde(default)]
    #[schemars(
        description = "Canonical docs to prioritize in action='briefing', e.g. docs/engineering/architecture/subagent-eval-system.md."
    )]
    pub doc_paths: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Related GitHub issues/PRs for action='close_loop' or action='build_references'."
    )]
    pub related_issues: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Canonical spec docs to prioritize in action='briefing'. Kept separate from memory/wiki fragments."
    )]
    pub spec_paths: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "When true, action='briefing' may include broader global memory fragments. Default false keeps briefing feature/project scoped."
    )]
    pub include_global: bool,
    #[serde(default)]
    pub compact: Option<bool>,
    // dispatch / complete fields
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch|recommend|complete] Agent backend, e.g. claude, codex, grok, kimi."
    )]
    pub agent: Option<String>,
    /// [action=complete] Outcome: success | failure | partial | aborted.
    #[serde(default)]
    pub outcome: Option<String>,
    /// [action=complete] Optional task id.
    #[serde(default)]
    pub task_id: Option<String>,
    /// [action=complete] Standard task type, e.g. fix_request or plan_request.
    #[serde(default)]
    pub task_type: Option<String>,
    /// [action=complete] Execution duration in milliseconds.
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u64_from_string_or_number"
    )]
    pub duration_ms: Option<u64>,
    /// [action=complete] Skills actually used. Distinct from dispatch prompt skills.
    #[serde(default)]
    pub skills_used: Vec<String>,
    /// [action=complete] Cost in tokens.
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u64_from_string_or_number"
    )]
    pub cost_tokens: Option<u64>,
    /// [action=complete] Cost in USD.
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_f64_from_string_or_number"
    )]
    pub cost_usd: Option<f64>,
    /// [action=complete] Quality score 0.0-1.0.
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_f64_from_string_or_number"
    )]
    pub quality_score: Option<f64>,
    /// [action=complete] Completion notes or summary.
    #[serde(default)]
    pub notes: Option<String>,
    /// [action=complete] Execution trajectory. Store compact step objects, not raw transcripts.
    #[serde(default)]
    pub trajectory: Option<serde_json::Value>,
    /// [action=complete] Unified diff or patch evidence.
    #[serde(default)]
    pub diff: Option<String>,
    /// [action=complete] Structured subagent eval records.
    #[serde(default)]
    pub subagents: Vec<TachiSubagentEvalParams>,
    /// [action=complete] Feedback/prompt-quality rule ids that were applied to this task.
    #[serde(default)]
    pub feedback_rules_applied: Vec<String>,
    /// [action=complete] Evidence references for verification.
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    /// [action=complete] Verification commands run.
    #[serde(default)]
    pub tests_run: Vec<String>,
    /// [action=complete] Whether a diff was present; inferred from diff if absent.
    #[serde(default)]
    pub diff_present: Option<bool>,
    /// [action=complete] Target DB scope: global or project.
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=dispatch] Working directory for the spawned agent.")]
    pub cwd: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=dispatch] Skill ids to inject into the agent prompt.")]
    pub skills: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch] Extra recall query; top hits are injected into the prompt."
    )]
    pub context_query: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=dispatch] Model override for the spawned agent.")]
    pub model: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u64_from_string_or_number"
    )]
    #[schemars(
        description = "[action=dispatch|wait] Timeout in seconds for the spawned agent (dispatch) or terminal poll loop (wait)."
    )]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch] Permission profile, e.g. full, allowlist, default."
    )]
    pub permission_profile: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch] Tool allowlist when permission_profile=allowlist."
    )]
    pub allowed_tools: Vec<String>,
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u32_from_string_or_number"
    )]
    #[schemars(
        description = "[action=dispatch] Maximum conversation turns for the spawned agent."
    )]
    pub max_turns: Option<u32>,
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch] Codex sandbox: workspace-write | danger-full-access | read-only."
    )]
    pub sandbox: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=dispatch] Inject Tachi MCP when the backend supports it.")]
    pub inject_tachi_mcp: Option<bool>,
    #[serde(default)]
    #[schemars(description = "[action=dispatch] Inject Hub MCPs when the backend supports it.")]
    pub inject_hub_mcps: Option<bool>,
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch] Explicit command argv override for custom backends."
    )]
    pub command: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Harness transport override, e.g. opencode_serve to dispatch through an existing local OpenCode server via opencode run --attach."
    )]
    pub harness_transport: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Harness server URL for attach transports, e.g. http://127.0.0.1:4321 for OpenCode serve."
    )]
    pub harness_server_url: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Named project DB for context/dispatch. Shared across actions; omit for the daemon-bound workspace DB."
    )]
    pub project: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch|recommend] Workflow stage hint, e.g. plan, build, review."
    )]
    pub stage: Option<String>,
    #[serde(default, alias = "dispatch_profile")]
    #[schemars(
        description = "Dispatch profile id, e.g. claude_plan, glm_51_impl, opencode_builder, codex_55_review, codex_53_fast, kimi_arch, deepseek_explore, or kimi_ux. Distinct from the server ToolProfile."
    )]
    pub profile: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Credential profile ids to materialize before spawning the worker, e.g. codex_shared. Values resolve from .tachi/credentials/*.json."
    )]
    pub credential_profiles: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "GitHub repository in owner/repo format for action='intake', action='link_pr', or action='pr_status'. Optional when issue_ref/pr_ref is owner/repo#123 or a GitHub URL."
    )]
    pub repo: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u64_from_string_or_number"
    )]
    #[schemars(
        description = "GitHub issue/PR number for action='intake', action='link_pr', or action='pr_status' when repo is supplied."
    )]
    pub number: Option<u64>,
    #[serde(default)]
    #[schemars(
        description = "[action=intake|link_pr|pr_status|pr_handoff|dispatch|complete] GitHub issue ref, e.g. owner/repo#123 or URL."
    )]
    pub issue_ref: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=link_pr|pr_status|pr_handoff|release_note|dispatch|complete] GitHub PR ref, e.g. owner/repo#123 or URL."
    )]
    pub pr_ref: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Tachi flow id for feature-scoped artifacts, also linking a dispatch/complete back to its flow (briefing/intake/link_pr/pr_handoff/release_note/ux_matrix/close_loop/status/wait/dispatch/complete)."
    )]
    pub flow_id: Option<String>,
    /// [action=complete|wait|status|cancel] Dispatch id linked to this task lifecycle event.
    #[serde(default)]
    pub dispatch_id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Risk override for recommendation/routing: low | medium | high | critical."
    )]
    pub risk: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Expected child-agent tool surface, e.g. readonly, delegate, reviewer."
    )]
    pub tool_profile: Option<String>,
    #[serde(default, alias = "include_capability_bundle")]
    #[schemars(
        description = "[action=dispatch|recommend] Include a capability bundle in the dispatch prompt when supported."
    )]
    pub auto_capability_bundle: Option<bool>,
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch] MCP/GitHub access contract for the spawned agent."
    )]
    pub mcp_access: Option<DispatchMcpAccessParams>,
    #[serde(default)]
    #[schemars(description = "[action=dispatch] Hub MCP server ids the dispatch may inject.")]
    pub allowed_mcp_servers: Vec<String>,
    // board fields
    #[serde(default)]
    #[schemars(description = "[action=board] Filter dispatch ledger rows by state.")]
    pub state_filter: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=board|proposals] Maximum ledger/proposal rows to return.")]
    pub limit: Option<usize>,
    #[serde(default)]
    #[schemars(
        description = "Proposal id for action='review_proposal' or action='apply_proposals'. Route-policy proposals persist rules; loadout-evolution proposals project reviewed profile/card overlays."
    )]
    pub proposal_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "Review status for action='review_proposal': approved or rejected.")]
    pub review_status: Option<String>,
    // merge fields. These apply only to local dispatch worktree merge via
    // approve_merge; GitHub PR gates/merges go through tachi_gh safe_merge.
    #[serde(default)]
    #[schemars(
        description = "Local dispatched worktree path to merge. Do not pass a GitHub PR ref here; use tachi_gh(action='safe_merge') for PR gates/merges."
    )]
    pub worktree: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=merge|pr_handoff] Branch to merge from the local dispatched worktree (merge), or the branch name to record in the handoff (pr_handoff)."
    )]
    pub branch: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Local dispatch worktree merge strategy for action='merge'. Ignored by action='pr_status', which always runs GitHub safe_merge in preview mode."
    )]
    pub strategy: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "GitHub PR gate policy for action='pr_status': permissive | standard | strict. Defaults to standard."
    )]
    pub merge_policy: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Explicitly allow pr_status/safe_merge preview to pass a PR that would close protected umbrella/no-close issues. Defaults false; plain confirm does not disable this gate."
    )]
    pub allow_umbrella_close: bool,
    #[serde(default = "super::default_true")]
    #[schemars(
        description = "[action=merge] Remove the local worktree after a successful merge. Defaults true."
    )]
    pub delete_worktree: bool,
    #[serde(default)]
    #[schemars(
        description = "[action=merge|dispatch] Confirm the local worktree merge (merge), or bypass the leader confirmation gate when dispatching an issue flow (dispatch)."
    )]
    pub confirm: bool,
    // close_loop fields
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure entry title.")]
    pub wiki_title: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure entry body text.")]
    pub wiki_text: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure path, e.g. /wiki/....")]
    pub wiki_path: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure topic label.")]
    pub wiki_topic: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure short summary.")]
    pub wiki_summary: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure category.")]
    pub wiki_category: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure keywords/tags.")]
    pub wiki_keywords: Vec<String>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure named entities.")]
    pub wiki_entities: Vec<String>,
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_f64_from_string_or_number"
    )]
    #[schemars(description = "[action=close_loop] Wiki closure importance 0.0-1.0.")]
    pub wiki_importance: Option<f64>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure scope: global or project.")]
    pub wiki_scope: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure area/domain tag.")]
    pub wiki_domain: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Bypass wiki noise filtering for action='close_loop'. Does not force dispatch or merge behavior."
    )]
    pub force: bool,
}
