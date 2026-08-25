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
        "Required Tachi task facade action. Harness-native subagents are the default for ordinary local delegation; worker launch is tachi_staff(action='start'), not tachi_task. GitHub PR lifecycle is tachi_gh only. Route tuning lives on tachi_tune. action='brief' returns the feature-scoped handoff and layered source board; action='status'/'board' read existing worker state, while status also returns the lifecycle read model when flow_id, issue_ref, or pr_ref is supplied (Task is a unified work read model, not a worker-status authority); action='complete' records eval; action='adjudicate' records a post-hoc terminal judgment on an existing outcome; action='intake' binds issues.",
        generator,
    )
}

// ─── Facade: task (briefing / status / board / lifecycle / claims) ───────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiTaskParams {
    /// Primary actions (F4 enum): intake, claim, heartbeat, handoff, release,
    /// board, status, complete, adjudicate, brief.
    /// Worker launch/wait/cancel left Task in #1319-C2 — use
    /// tachi_staff(action='start'|'status') for worker lifecycle.
    /// action="intake" binds a GitHub issue to a flow.
    /// action="status" with flow_id/issue_ref/pr_ref is a read-only lifecycle model;
    /// dispatch_id takes precedence and preserves the flat status snapshot.
    /// GitHub PR and closure lifecycle: use **tachi_gh only** (#757, #1713).
    #[schemars(schema_with = "tachi_task_action_schema")]
    pub action: TachiTaskAction,
    /// Response shape: default JSON for agent automation. Pass "markdown" on
    /// any surviving action for human-readable text.
    #[serde(default)]
    pub format: Option<String>,
    /// tachi#1173 items 1+2+3: request the full payload instead of the
    /// default slim shape. Response verbosity for board/status payloads.
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
        description = "Tachi flow id for feature-scoped artifacts, also linking a completion back to its flow (brief/intake/status/complete)."
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

        // Serde path (the wire/MCP path): route through TachiTaskAction's
        // canonical FromStr parser so the typed staffing guidance remains
        // visible at the boundary.
        for retired in ["dispatch", "wait", "cancel"] {
            let err =
                serde_json::from_str::<TachiTaskParams>(&format!("{{\"action\":\"{retired}\"}}"))
                    .expect_err("retired action must not deserialize as tachi_task action");
            let msg = err.to_string();
            assert!(
                !msg.contains("unknown variant") && msg.contains("tachi_staff"),
                "retired action {retired} must retain typed tachi_staff guidance, got: {msg}"
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
    fn task_wire_actions_require_exact_lowercase_primary_tokens() {
        use super::super::action_enums::TachiTaskAction;

        for &action in TachiTaskAction::PRIMARY {
            let token = action.as_str();
            let exact = serde_json::json!({"action": token});
            let params: TachiTaskParams = serde_json::from_value(exact)
                .expect("every exact lowercase primary action must deserialize");
            assert_eq!(params.action, action);

            for malformed in [
                token.to_ascii_uppercase(),
                format!(" {token}"),
                format!("{token} "),
                format!(" {token} "),
            ] {
                let wire = serde_json::json!({"action": malformed});
                let error = serde_json::from_value::<TachiTaskParams>(wire)
                    .expect_err("normalized action aliases must not deserialize");
                let message = error.to_string();
                assert!(
                    message.contains("must exactly match one of")
                        && !message.contains("unknown variant"),
                    "wire action {malformed:?} must receive an exact-token refusal, got: {message}"
                );
            }
        }

        let unknown = serde_json::json!({"action": "not_a_task_action"});
        let error = serde_json::from_value::<TachiTaskParams>(unknown)
            .expect_err("unknown action must not deserialize");
        assert!(
            error.to_string().contains("must exactly match one of"),
            "unknown wire action must receive an exact-token refusal: {error}"
        );
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
                !msg.contains("unknown variant") && msg.contains("#1683 C1a"),
                "retired action {retired} must retain typed #1683 C1a guidance, got: {msg}"
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
                !msg.contains("unknown variant") && msg.contains("#1712 C1b"),
                "folded action {retired} must retain typed #1712 C1b guidance, got: {msg}"
            );

            let err = TachiTaskAction::from_str(retired)
                .expect_err("folded action must not parse as tachi_task action");
            assert!(
                err.contains("#1712 C1b"),
                "folded action {retired} error should name #1712 C1b, got: {err}"
            );
        }
    }

    #[test]
    fn c1c_profile_actions_left_task() {
        use std::str::FromStr;

        use super::super::action_enums::TachiTaskAction;

        for retired in ["profiles", "profile", "card"] {
            let err =
                serde_json::from_str::<TachiTaskParams>(&format!("{{\"action\":\"{retired}\"}}"))
                    .expect_err("retired profile action must not deserialize as tachi_task action");
            let msg = err.to_string();
            assert!(
                !msg.contains("unknown variant")
                    && msg.contains("tachi card")
                    && msg.contains("append-only eval")
                    && msg.contains("static profile admission"),
                "retired profile action {retired} must retain typed operator/eval/admission guidance, got: {msg}"
            );

            let err = TachiTaskAction::from_str(retired)
                .expect_err("retired profile action must not parse as tachi_task action");
            assert!(
                err.contains("tachi card")
                    && err.contains("append-only eval")
                    && err.contains("static profile admission"),
                "retired profile action {retired} guidance is incomplete: {err}"
            );
        }
    }

    #[test]
    fn explicit_retired_actions_keep_typed_guidance_on_wire() {
        use std::str::FromStr;

        use super::super::action_enums::TachiTaskAction;

        let cases = [
            ("dispatch", "tachi_staff"),
            ("wait", "tachi_staff"),
            ("cancel", "tachi_staff"),
            ("plan", "#1683 C1a"),
            ("cycle_plan", "#1683 C1a"),
            ("recommend", "#1683 C1a"),
            ("refine_issues", "#1683 C1a"),
            ("merge", "#1683 C1a"),
            ("ux_matrix", "#1683 C1a"),
            ("briefing", "#1712 C1b"),
            ("doc_index", "#1712 C1b"),
            ("cycle_status", "#1712 C1b"),
            ("profiles", "#1687 C1c"),
            ("profile", "#1687 C1c"),
            ("card", "#1687 C1c"),
            ("build_references", "#1713"),
            ("close_loop", "#1713"),
            ("route_simulate", "tachi_tune"),
            ("proposals", "tachi_tune"),
            ("review_proposal", "tachi_tune"),
            ("apply_proposals", "tachi_tune"),
            ("link_pr", "tachi_gh"),
            ("pr_status", "tachi_gh"),
            ("pr_handoff", "tachi_gh"),
            ("release_note", "tachi_gh"),
        ];

        for (retired, guidance) in cases {
            let wire = format!("{{\"action\":\"{retired}\"}}");
            let serde_error = serde_json::from_str::<TachiTaskParams>(&wire)
                .expect_err("explicit retired action must fail on the wire")
                .to_string();
            let typed_error = TachiTaskAction::from_str(retired)
                .expect_err("explicit retired action must fail in the typed parser");
            assert!(
                !serde_error.contains("unknown variant"),
                "wire error for {retired} must not fall back to generic serde text: {serde_error}"
            );
            assert!(
                serde_error.contains(guidance) && serde_error.contains(&typed_error),
                "wire error for {retired} must preserve typed guidance {guidance:?}: {serde_error}"
            );
        }
    }
}
