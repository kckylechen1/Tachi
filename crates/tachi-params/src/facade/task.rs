use super::{
    string_enum_schema, AdjudicationParams, CompletionPredicate, DispatchMcpAccessParams,
    RulingRecordParams, SignatureRecordParams, TachiSubagentEvalParams, TachiTaskAction,
};
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

fn tachi_task_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    // #757: GH PR lifecycle is only on tachi_gh — not accepted by tachi_task.
    string_enum_schema(
        &super::action_enums::TachiTaskAction::primary_wire_strings(),
        "Required Tachi task facade action. GitHub PR lifecycle (link_pr/pr_status/pr_handoff/release_note) is tachi_gh only. action='briefing' returns a feature-scoped handoff board; action='doc_index' returns the layered source index; action='status'/'wait'/'board'/'cancel' manage dispatches; action='complete' records eval; action='adjudicate' records a post-hoc terminal judgment on an existing outcome; action='recommend'/'route_simulate'/'proposals' manage routing; action='intake' binds issues; action='cycle_status'/'cycle_plan' lifecycle read models; action='ux_matrix' UX checklist; action='close_loop' wiki closure; action='merge' is local worktree merge only (use tachi_gh safe_merge for GitHub PRs); action='refine_issues' is a manually-triggered, read-only, proposal-only semantic refinement of one GitHub issue (#1002) — it never closes/reopens/edits/writes back.",
        generator,
    )
}

// ─── Facade: task (plan / recommend / dispatch / board / merge / lifecycle) ──

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiTaskParams {
    /// Primary actions (F4 enum): plan, briefing,
    /// doc_index, recommend, dispatch, complete, adjudicate, profiles, profile, card,
    /// route_simulate, proposals, review_proposal, apply_proposals, status,
    /// cancel, board, wait, merge, intake, cycle_status, cycle_plan, ux_matrix,
    /// build_references, close_loop.
    /// action="merge" is local dispatched worktree git merge only; use
    /// tachi_gh(action='safe_merge') for GitHub PR merges.
    /// action="intake" binds a GitHub issue to a flow.
    /// action="cycle_status" / "cycle_plan" are read-only lifecycle models.
    /// action="ux_matrix" / "build_references" / "close_loop" close the issue loop.
    /// action="refine_issues" (#1002) reads one GitHub issue and returns typed
    /// evidence + a proposal-only disposition; uses `issue_ref` (or `repo`+`number`).
    /// GitHub PR lifecycle (link_pr/pr_status/pr_handoff/release_note): use **tachi_gh only** (#757).
    #[schemars(schema_with = "tachi_task_action_schema")]
    pub action: TachiTaskAction,
    /// Response shape: default JSON for agent automation, with two
    /// exceptions — [action=recommend|profiles|profile|card] (tachi#1201)
    /// default to compact markdown (a table plus key fields) when `format`
    /// is omitted, since these are read-heavy discovery endpoints most often
    /// consumed by a human/agent skimming a summary, not parsing JSON. Pass
    /// format="json" to get the machine-readable shape for those four
    /// actions; every other action's default is unaffected. Pass "markdown"
    /// on any action for human-readable text.
    #[serde(default)]
    pub format: Option<String>,
    /// tachi#1173 items 1+2: request the full payload instead of the default
    /// slim shape. [action=dispatch]: the dispatch response includes the full
    /// routing card (`profile`, `identity_receipt`, `dispatch_profile`) rather
    /// than just dispatch_id/state/run_dir/suggested_complete_command.
    /// [action=profiles|profile|card]: each row includes the full mbit_card
    /// (stats/guidance/moves/personality/skill_loadout/evidence_contract)
    /// rather than just name/backend/model/role.
    /// [action=recommend] (tachi#1201, format="json" only): when true, the
    /// response includes `identity_receipt`; when omitted/false it is
    /// dropped from the slim JSON shape.
    #[serde(default)]
    pub verbose: Option<bool>,
    /// [action=recommend] (tachi#1201, format="json" only): when true,
    /// include one top-level `mbit_card` for the recommended profile in the
    /// JSON response. Default false keeps the JSON response to a
    /// candidate-summary shape (`profile`/`role`/`score`/`reasons` per row,
    /// no per-row or top-level `mbit_card`). Has no effect on markdown
    /// output or on other actions.
    #[serde(default)]
    pub include_card: Option<bool>,
    // plan fields
    #[serde(default)]
    #[schemars(
        description = "[action=plan|recommend|dispatch|route_simulate|complete|intake|ux_matrix] Task description / prompt text."
    )]
    pub task: Option<String>,
    /// [action=dispatch|recommend|route_simulate] Declared side-effect level:
    /// L0 source/metadata read, L1 temporary local state, L2 product-data
    /// diagnostics, or L3 product data/resident runtime side effects. Omitted
    /// values resolve to L1 for host-profile admission without task-text inference.
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch|recommend|route_simulate] Declared side-effect level L0–L3. Omitted → L1 for host admission."
    )]
    pub execution_level: Option<super::ExecutionLevel>,
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
    // #527: agent-facing default is compact when omitted; set false for full boards.
    #[serde(default)]
    #[schemars(
        description = "[action=briefing|doc_index] When true or omitted, use the tight agent packet (smaller top_k). Set false for the full feature board."
    )]
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
    #[schemars(schema_with = "crate::coerce::opt_integer_from_string_or_number_schema")]
    pub duration_ms: Option<u64>,
    /// [action=complete] Skills actually used. Distinct from dispatch prompt skills.
    #[serde(default)]
    pub skills_used: Vec<String>,
    /// [action=complete] Cost in tokens.
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_integer_from_string_or_number_schema")]
    pub cost_tokens: Option<u64>,
    /// [action=complete] Cost in USD.
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_f64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_number_from_string_or_number_schema")]
    pub cost_usd: Option<f64>,
    /// [action=complete] Quality score 0.0-1.0.
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_f64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_number_from_string_or_number_schema")]
    pub quality_score: Option<f64>,
    /// [action=complete] Completion notes or summary.
    /// [action=close_loop] When wiki_title/wiki_text are omitted and result.md
    /// is missing, used as a draft source for the wiki body (#925).
    /// [action=pr_handoff] Optional PR title override.
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
    /// [action=complete] `eval_run_id`s from the #1066 mirror eval intake
    /// (`tachi_agent_eval` register/observe/adjudicate) to project into this
    /// completion's eval row. Only rows that are ADJUDICATED and
    /// evidence-usable (and not a self-eval) are projected; unresolved or
    /// ineligible ids are silently skipped — this never fails the
    /// completion. Additive/optional — an empty/omitted array is
    /// byte-compatible with existing callers and leaves `subagents[]`
    /// untouched.
    #[serde(default)]
    pub eval_run_ids: Vec<String>,
    /// [action=complete] Feedback/prompt-quality rule ids that were applied to this task.
    #[serde(default)]
    pub feedback_rules_applied: Vec<String>,
    /// [action=complete] Adjudicated vendor-keyed error signatures to record for
    /// this dispatch's lane (#735). Additive/optional — omitting it is
    /// byte-compatible with existing callers.
    #[serde(default)]
    pub signatures: Vec<SignatureRecordParams>,
    /// [action=complete] Leader adjudication rulings to capture as precedent
    /// memory rows (#950 slice 1: capture only). Additive/optional — an
    /// empty/omitted array writes no `/precedents` rows and is byte-compatible
    /// with existing callers.
    #[serde(default)]
    pub rulings: Vec<RulingRecordParams>,
    /// [action=complete|adjudicate] Leader terminal adjudication for a dispatch
    /// outcome (#1035). On `complete` it writes an append-only row linked to
    /// the freshly-recorded outcome; on `adjudicate` it writes a post-hoc
    /// event linked to an already-existing outcome.
    #[serde(default)]
    pub adjudication: Option<AdjudicationParams>,
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
    #[schemars(
        description = "[action=dispatch] Execution-environment lease id (#894 S1); resolves the agent cwd from the daemon-owned lease (managed env)."
    )]
    pub env_id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch] Explicit opt-in to dispatch into a bare cwd not backed by a lease; stamped env: unmanaged. Fail-safe default is managed."
    )]
    pub unmanaged_cwd: Option<bool>,
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
        schema_with = "crate::coerce::opt_integer_from_string_or_number_schema",
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
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch] Machine-checkable completion predicate. When set, self-reported success must satisfy it to land a reviewed TASK_STATE_COMPLETED; otherwise the run is intercepted as a false success and routed to TASK_STATE_FAILED (#878-A)."
    )]
    pub completion_predicate: Option<CompletionPredicate>,
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u32_from_string_or_number"
    )]
    #[schemars(
        schema_with = "crate::coerce::opt_integer_from_string_or_number_schema",
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
    /// #1041 B7: see `crate::memory::SaveMemoryParams::project_explicit` —
    /// same wire signal, same purpose. `tachi_task` is one of the tools
    /// `session_identity::enforce_session_project` auto-defaults `project`
    /// onto when the caller omits it (a bound-session convenience) — WITHOUT
    /// this field, `action='complete'`'s downstream eval/lesson/precedent
    /// writes (`build_complete_eval_record`/`run_lesson_post_complete_hook`/
    /// `record_complete_rulings`) had no way to tell that transport-injected
    /// default apart from a caller's own deliberate `project=`, and treated
    /// `project.is_some()` alone as proof of deliberate intent — exactly the
    /// inverted-polarity bug `SaveMemoryParams::project_explicit` was
    /// introduced to fix for `save_memory` itself.
    #[serde(default, rename = "__tachi_project_explicit")]
    #[schemars(skip)]
    pub project_explicit: bool,
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
        description = "GitHub repository in owner/repo format for action='intake'. Optional when issue_ref is owner/repo#123 or a GitHub URL. For PR lifecycle use tachi_gh."
    )]
    pub repo: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u64_from_string_or_number"
    )]
    #[schemars(
        schema_with = "crate::coerce::opt_integer_from_string_or_number_schema",
        description = "GitHub issue number for action='intake' when repo is supplied. For PR lifecycle use tachi_gh."
    )]
    pub number: Option<u64>,
    #[serde(default)]
    #[schemars(
        description = "[action=intake|dispatch|complete] GitHub issue ref, e.g. owner/repo#123 or URL."
    )]
    pub issue_ref: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch|complete] GitHub PR ref, e.g. owner/repo#123 or URL. PR lifecycle (link/status/handoff/release) is tachi_gh only."
    )]
    pub pr_ref: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Tachi flow id for feature-scoped artifacts, also linking a dispatch/complete back to its flow (briefing/intake/ux_matrix/close_loop/status/wait/dispatch/complete)."
    )]
    pub flow_id: Option<String>,
    /// [action=complete|wait|status|cancel] Dispatch id linked to this task lifecycle event.
    #[serde(default)]
    pub dispatch_id: Option<String>,
    /// [action=adjudicate] Direct outcome id to adjudicate. When omitted,
    /// `dispatch_id` is resolved to an outcome via `find_outcome_by_dispatch_id`.
    /// Supplying neither → error (do not guess).
    #[serde(default)]
    pub outcome_id: Option<String>,
    /// [action=status] When true, include the size-capped content of result.md
    /// from the dispatch run directory in the response. Lets a leader whose FS
    /// access doesn't include ~/.tachi read the lane's report without local
    /// file access (#878-C).
    #[serde(default)]
    pub include_result: bool,
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
        description = "[action=merge] Branch to merge from the local dispatched worktree. For PR handoff branch recording use tachi_gh(action='pr_handoff')."
    )]
    pub branch: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Local dispatch worktree merge strategy for action='merge'. For PR gate preview use tachi_gh(action='pr_status')."
    )]
    pub strategy: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "GitHub PR gate policy passed through to tachi_gh lifecycle helpers (not a tachi_task action): permissive | standard | strict."
    )]
    pub merge_policy: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Explicit umbrella/no-close override for tachi_gh safe_merge/pr_status helpers shared via lifecycle field bags. Defaults false."
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
    #[schemars(
        schema_with = "crate::coerce::opt_number_from_string_or_number_schema",
        description = "[action=close_loop] Wiki closure importance 0.0-1.0."
    )]
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
