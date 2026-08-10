use super::{
    string_enum_schema, AdjudicationParams, CompletionPredicate, DispatchMcpAccessParams,
    RulingRecordParams, SignatureRecordParams, TachiSubagentEvalParams, TachiTaskAction,
};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

/// Why execution is leaving the host harness instead of using its native
/// subagent. Tachi is memory/ledger by default; these are the only admitted
/// exceptions for its legacy durable dispatch backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TachiDispatchReason {
    ExplicitUserRequest,
    DurableCrossSession,
    CrossDeviceRemote,
    NativeSubagentUnavailable,
}

fn tachi_task_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    // #757: GH PR lifecycle is only on tachi_gh — not accepted by tachi_task.
    // #1319-C2: worker launch/wait/cancel left Task; use tachi_staff instead.
    string_enum_schema(
        &super::action_enums::TachiTaskAction::primary_wire_strings(),
        "Required Tachi task facade action. Harness-native subagents are the default for ordinary local delegation; worker launch is tachi_staff(action='start'), not tachi_task. GitHub PR lifecycle (link_pr/pr_status/pr_handoff/release_note) is tachi_gh only. Route tuning lives on tachi_tune. action='brief' returns the feature-scoped handoff and layered source board; action='status'/'board' read existing worker state, while status also returns the lifecycle read model when flow_id, issue_ref, or pr_ref is supplied (Task is a unified work read model, not a worker-status authority); action='complete' records eval; action='adjudicate' records a post-hoc terminal judgment on an existing outcome; action='intake' binds issues; action='build_references' prepares closure references; action='close_loop' writes wiki closure.",
        generator,
    )
}

// ─── Facade: task (briefing / status / board / lifecycle / claims) ───────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiTaskParams {
    /// Primary actions (F4 enum): intake, claim, heartbeat, handoff, release,
    /// board, status, complete, adjudicate, brief,
    /// profiles, profile, card, build_references, close_loop.
    /// Worker launch/wait/cancel left Task in #1319-C2 — use
    /// tachi_staff(action='start'|'status') for worker lifecycle.
    /// action="intake" binds a GitHub issue to a flow.
    /// action="status" with flow_id/issue_ref/pr_ref is a read-only lifecycle model;
    /// dispatch_id takes precedence and preserves the flat status snapshot.
    /// action="build_references" / "close_loop" close the issue loop.
    /// GitHub PR lifecycle (link_pr/pr_status/pr_handoff/release_note): use **tachi_gh only** (#757).
    #[schemars(schema_with = "tachi_task_action_schema")]
    pub action: TachiTaskAction,
    /// Response shape: default JSON for agent automation, with two
    /// exceptions — [action=profiles|profile|card] (tachi#1201)
    /// default to compact markdown (a table plus key fields) when `format`
    /// is omitted, since these are read-heavy discovery endpoints most often
    /// consumed by a human/agent skimming a summary, not parsing JSON. Pass
    /// format="json" to get the machine-readable shape for those three
    /// actions; every other action's default is unaffected. Pass "markdown"
    /// on any action for human-readable text.
    #[serde(default)]
    pub format: Option<String>,
    /// tachi#1173 items 1+2+3: request the full payload instead of the
    /// default slim shape. Response verbosity for board/profile/status
    /// payloads.
    /// [action=profiles|profile|card]: each row includes the full mbit_card
    /// (stats/guidance/moves/personality/skill_loadout/evidence_contract)
    /// rather than just name/backend/model/role.
    /// [action=board]: forwarded to `TachiBoardParams.verbose` -- restores
    /// identity_receipt/acpx/acpx_events per row and stops folding terminal
    /// rows into count rows.
    #[serde(default)]
    pub verbose: Option<bool>,
    // task text fields
    #[serde(default)]
    #[schemars(description = "[action=complete|intake] Task description / prompt text.")]
    pub task: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Requesting agent id; shared by task context and ledger-aware operations."
    )]
    pub agent_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "Area tag (e.g. rust, mcp) used to scope task context.")]
    pub domain: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional recall path prefix filter for task context.")]
    pub path_prefix: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Maximum context fragments to recall when task context is requested."
    )]
    pub top_k: Option<usize>,
    // feature briefing fields
    #[serde(default)]
    #[schemars(
        description = "Canonical docs to prioritize for task context and lifecycle records, e.g. docs/engineering/architecture/subagent-eval-system.md."
    )]
    pub doc_paths: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Related GitHub issues/PRs for action='close_loop' or action='build_references'."
    )]
    pub related_issues: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Canonical spec docs to prioritize for task context and lifecycle records. Kept separate from memory/wiki fragments."
    )]
    pub spec_paths: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "When true, action='brief' may include broader global memory fragments. Default false keeps briefing feature/project scoped."
    )]
    pub include_global: bool,
    // #527: agent-facing default is compact when omitted; set false for full boards.
    #[serde(default)]
    #[schemars(
        description = "[action=brief] When true or omitted, use the tight agent packet (smaller top_k). Set false for the full feature board."
    )]
    pub compact: Option<bool>,
    // complete fields
    #[serde(default)]
    #[schemars(
        description = "[action=complete] Agent backend that ran the completed task, e.g. claude, codex, grok, kimi. When omitted, the dispatched run's agent is used (requires a readable dispatch_id)."
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
    /// [action=complete] Skills actually used.
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
    /// this task's lane (#735). Additive/optional — omitting it is
    /// byte-compatible with existing callers.
    #[serde(default)]
    pub signatures: Vec<SignatureRecordParams>,
    /// [action=complete] Leader adjudication rulings to capture as precedent
    /// memory rows (#950 slice 1: capture only). Additive/optional — an
    /// empty/omitted array writes no `/precedents` rows and is byte-compatible
    /// with existing callers.
    #[serde(default)]
    pub rulings: Vec<RulingRecordParams>,
    /// [action=complete|adjudicate] Leader terminal adjudication for a task
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
    #[schemars(
        description = "Working directory used to resolve relative doc_paths/spec_paths and shared by task context/lifecycle records; also echoed into the brief packet."
    )]
    pub cwd: Option<String>,
    #[serde(default)]
    #[schemars(skip)]
    pub env_id: Option<String>,
    #[serde(default)]
    #[schemars(skip)]
    pub unmanaged_cwd: Option<bool>,
    #[serde(default)]
    #[schemars(skip)]
    pub skills: Vec<String>,
    #[serde(default)]
    #[schemars(skip)]
    pub context_query: Option<String>,
    #[serde(default)]
    #[schemars(skip)]
    pub model: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u64_from_string_or_number"
    )]
    #[schemars(
        schema_with = "crate::coerce::opt_integer_from_string_or_number_schema",
        description = "[action=status] Timeout in seconds for the acpx control command issued by status; default 30, capped at 300."
    )]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    #[schemars(skip)]
    pub permission_profile: Option<String>,
    #[serde(default)]
    #[schemars(skip)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    #[schemars(skip)]
    pub completion_predicate: Option<CompletionPredicate>,
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u32_from_string_or_number"
    )]
    #[schemars(skip)]
    pub max_turns: Option<u32>,
    #[serde(default)]
    #[schemars(skip)]
    pub sandbox: Option<String>,
    #[serde(default)]
    #[schemars(skip)]
    pub inject_tachi_mcp: Option<bool>,
    #[serde(default)]
    #[schemars(skip)]
    pub inject_hub_mcps: Option<bool>,
    #[serde(default)]
    #[schemars(skip)]
    pub command: Vec<String>,
    #[serde(default)]
    #[schemars(skip)]
    pub harness_transport: Option<String>,
    #[serde(default)]
    #[schemars(skip)]
    pub harness_server_url: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Named project DB. Shared across actions; omit for the daemon-bound workspace DB."
    )]
    pub project: Option<String>,
    /// #1041 B7: wire explicitness signal. When `action='complete'`, tells
    /// the downstream eval/lesson/precedent writes whether `project` came
    /// from the caller's own wire input or was transport-injected by
    /// `session_identity::enforce_session_project` — `project.is_some()`
    /// alone cannot distinguish deliberate intent (#1041 B7).
    #[serde(default, rename = "__tachi_project_explicit")]
    #[schemars(
        description = "[action=complete] True when the caller explicitly set project on the wire; distinguishes deliberate intent from a transport-injected bound-project default (#1041 B7)."
    )]
    pub project_explicit: bool,
    #[serde(default)]
    #[schemars(description = "[action=brief] Workflow stage hint, e.g. plan, build, review.")]
    pub stage: Option<String>,
    #[serde(default, alias = "dispatch_profile")]
    #[schemars(
        description = "Worker profile id, e.g. claude_plan, glm_51_impl, opencode_builder, codex_55_review, codex_53_fast, kimi_arch, deepseek_explore, or kimi_ux. Distinct from the server ToolProfile."
    )]
    pub profile: Option<String>,
    #[serde(default)]
    #[schemars(skip)]
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
        description = "[action=intake|complete] GitHub issue ref, e.g. owner/repo#123 or URL."
    )]
    pub issue_ref: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=complete] GitHub PR ref, e.g. owner/repo#123 or URL. PR lifecycle (link/status/handoff/release) is tachi_gh only."
    )]
    pub pr_ref: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Tachi flow id for feature-scoped artifacts, also linking a completion back to its flow (brief/intake/close_loop/status/complete)."
    )]
    pub flow_id: Option<String>,
    /// [action=complete|status] Dispatch id linked to this task lifecycle event.
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
    #[schemars(description = "Risk override for intake: low | medium | high | critical.")]
    pub risk: Option<String>,
    #[serde(default)]
    #[schemars(skip)]
    pub tool_profile: Option<String>,
    #[serde(default, alias = "include_capability_bundle")]
    #[schemars(
        description = "[action=brief] When true, includes the capability bundle in the briefing context when supported."
    )]
    pub auto_capability_bundle: Option<bool>,
    #[serde(default)]
    #[schemars(skip)]
    pub mcp_access: Option<DispatchMcpAccessParams>,
    #[serde(default)]
    #[schemars(skip)]
    pub allowed_mcp_servers: Vec<String>,
    // board fields
    #[serde(default)]
    #[schemars(description = "[action=board] Filter dispatch ledger rows by state.")]
    pub state_filter: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=board] Maximum ledger rows to return.")]
    pub limit: Option<usize>,
    // #1426: proposal review/apply left Task for the admin-only `tachi_tune`
    // surface, so no surviving `tachi_task` action reads these two. They stay
    // on the struct for stored-JSON back-compat but are hidden from the public
    // schema — the same treatment [1319-C2] gave its orphaned dispatch knobs.
    #[serde(default)]
    #[schemars(skip)]
    pub proposal_id: Option<String>,
    #[serde(default)]
    #[schemars(skip)]
    pub review_status: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=complete] Local dispatched worktree path recorded for completion evidence."
    )]
    pub worktree: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=claim] Branch associated with a WorkClaim.")]
    pub branch: Option<String>,
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
    #[schemars(description = "Bypass wiki noise filtering for action='close_loop'.")]
    pub force: bool,
    // --- canonical WorkClaim fields (#1253) ---
    #[serde(default)]
    #[schemars(
        description = "[action=claim|handoff] Stable AgentIdentity id. It is never a session id or connection id."
    )]
    pub agent_identity_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=claim|handoff] Per-work claimant role.")]
    pub claim_role: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=claim|handoff] WorkClaim mode: read_only or writable.")]
    pub claim_mode: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=claim|handoff] Canonical writable worktree path.")]
    pub worktree_path: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=claim|handoff] Declared file scope for collision checks.")]
    pub claim_scope: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=claim|handoff] Expected Git head; incompatible heads conflict loudly."
    )]
    pub expected_head: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=claim|heartbeat|handoff|release] Lease expiry as UTC RFC3339."
    )]
    pub lease_expires_at: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=heartbeat|handoff|release] Required WorkClaim transition version for compare-and-swap."
    )]
    pub transition_version: Option<i64>,
    #[serde(default)]
    #[schemars(description = "[action=heartbeat|handoff|release] WorkClaim id.")]
    pub claim_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=release] Explicit release reason.")]
    pub release_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::TachiTaskParams;

    /// #1319-C2: worker launch/wait/cancel left Task. `dispatch_reason` and
    /// `inject_card` were strictly dispatch-only fields and are removed from
    /// `TachiTaskParams` together with the Dispatch arm. The typed staffing
    /// vocabulary now lives on `TachiStaffParams` / `TachiDispatchParams`
    /// (TachiArenaParams was deleted in [1319-D2]). Pin that `tachi_task` no
    /// longer accepts `dispatch`/`wait`/`cancel` and points callers at
    /// `tachi_staff`.
    #[test]
    fn dispatch_wait_cancel_actions_left_task() {
        use std::str::FromStr;

        use super::super::action_enums::TachiTaskAction;

        // Serde path (the wire/MCP path): the #[serde(rename_all)] derive
        // rejects retired variants with a standard "unknown variant" error
        // listing the surviving actions. The accepted-variants list (after
        // "expected one of") must NOT contain dispatch/wait/cancel anymore.
        for retired in ["dispatch", "wait", "cancel"] {
            let err =
                serde_json::from_str::<TachiTaskParams>(&format!("{{\"action\":\"{retired}\"}}"))
                    .expect_err("retired action must not deserialize as tachi_task action");
            let msg = err.to_string();
            assert!(
                msg.contains("unknown variant"),
                "retired action {retired} should be an unknown variant, got: {msg}"
            );
            // The "expected one of `<...>`" list is the accepted surface;
            // the retired action must not be admitted there. (The message
            // does echo the rejected value back, so check the accepted list
            // fragment specifically.)
            let accepted = msg.split("expected one of ").nth(1).unwrap_or_default();
            assert!(
                !accepted.contains(&format!("`{retired}`")),
                "retired action {retired} must not appear in the accepted-variants list, got: {msg}"
            );
        }

        // FromStr path (the typed-action parse used by router-side matching):
        // #1319-C2 points callers at tachi_staff for the retired worker
        // lifecycle actions.
        for retired in ["dispatch", "wait", "cancel"] {
            let err = TachiTaskAction::from_str(retired)
                .expect_err("retired action must not parse as tachi_task action");
            assert!(
                err.contains("tachi_staff"),
                "retired action {retired} error should point at tachi_staff, got: {err}"
            );
        }
    }

    #[test]
    fn c1a_actions_left_task() {
        use std::str::FromStr;

        use super::super::action_enums::TachiTaskAction;

        for retired in [
            "plan",
            "cycle_plan",
            "recommend",
            "refine_issues",
            "merge",
            "ux_matrix",
        ] {
            let err =
                serde_json::from_str::<TachiTaskParams>(&format!("{{\"action\":\"{retired}\"}}"))
                    .expect_err("retired action must not deserialize as tachi_task action");
            let msg = err.to_string();
            assert!(
                msg.contains("unknown variant"),
                "retired action {retired} should be an unknown variant, got: {msg}"
            );
            let accepted = msg.split("expected one of ").nth(1).unwrap_or_default();
            assert!(
                !accepted.contains(&format!("`{retired}`")),
                "retired action {retired} must not appear in the accepted-variants list, got: {msg}"
            );

            let err = TachiTaskAction::from_str(retired)
                .expect_err("retired action must not parse as tachi_task action");
            assert!(
                err.contains("#1683 C1a"),
                "retired action {retired} error should name #1683 C1a, got: {err}"
            );
        }
    }

    #[test]
    fn c1b_folded_actions_left_task() {
        use std::str::FromStr;

        use super::super::action_enums::TachiTaskAction;

        for retired in ["briefing", "doc_index", "cycle_status"] {
            let err =
                serde_json::from_str::<TachiTaskParams>(&format!("{{\"action\":\"{retired}\"}}"))
                    .expect_err("folded action must not deserialize as tachi_task action");
            let msg = err.to_string();
            assert!(
                msg.contains("unknown variant"),
                "folded action {retired} should be an unknown variant, got: {msg}"
            );
            let accepted = msg.split("expected one of ").nth(1).unwrap_or_default();
            assert!(
                !accepted.contains(&format!("`{retired}`")),
                "folded action {retired} must not appear in the accepted-variants list, got: {msg}"
            );

            let err = TachiTaskAction::from_str(retired)
                .expect_err("folded action must not parse as tachi_task action");
            assert!(
                err.contains("#1712 C1b"),
                "folded action {retired} error should name #1712 C1b, got: {err}"
            );
        }
    }
}
