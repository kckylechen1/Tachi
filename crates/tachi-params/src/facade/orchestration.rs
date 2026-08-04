use super::{action_inventory, string_enum_schema, TachiDispatchReason};
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

fn tachi_arena_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        action_inventory::TACHI_ARENA_ACTIONS,
        "Required Tachi arena action.",
        generator,
    )
}

fn tachi_verify_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &super::action_enums::TachiVerifyAction::all_wire_strings(),
        "Required Tachi verification ledger action (start/record/status/board).",
        generator,
    )
}

fn tachi_staff_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        action_inventory::TACHI_STAFF_ACTIONS,
        "Required Tachi staffing action.",
        generator,
    )
}

fn tachi_orchestrator_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        action_inventory::TACHI_ORCHESTRATOR_ACTIONS,
        "Required Tachi orchestrator action.",
        generator,
    )
}

// ─── Facade: tachi_arena (tracked worker mission ledger) ────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiArenaParams {
    /// Action: "spawn", "board", or "collect"
    #[schemars(schema_with = "tachi_arena_action_schema")]
    pub action: String,

    /// Response shape: default JSON for agent automation; pass "markdown" for human-readable text.
    #[serde(default)]
    pub format: Option<String>,

    /// Existing arena id for spawn/board/collect.
    #[serde(default)]
    pub arena_id: Option<String>,

    /// Existing or requested mission id for spawn/collect.
    #[serde(default)]
    pub mission_id: Option<String>,

    /// Human-readable arena title (legacy open field; retained for spawn metadata only).
    #[serde(default)]
    pub title: Option<String>,

    /// Arena objective (legacy open field; retained for spawn metadata only).
    #[serde(default)]
    pub objective: Option<String>,

    /// Mission prompt for spawn.
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

    /// Mission timeout in seconds. For launch=true this is the dispatch timeout.
    /// (The reap threshold use was removed with the reap action in [1319-D1].)
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_integer_from_string_or_number_schema")]
    pub timeout_secs: Option<u64>,

    /// When true, spawn creates the tracked mission documents and also launches
    /// the supported worker harness through the existing dispatch runtime.
    /// Ordinary local delegation should leave this false and hand the returned
    /// tracked prompt to the host harness's native subagent.
    #[serde(default)]
    pub launch: bool,

    /// Required when launch=true selects a launch-capable Tachi worker lane.
    /// Packet/document-only missions do not require a dispatch exception.
    #[serde(default)]
    #[schemars(
        description = "[action=spawn, launch=true] Required native-first exception for a launch-capable lane: explicit_user_request, durable_cross_session, cross_device_remote, or native_subagent_unavailable."
    )]
    pub dispatch_reason: Option<TachiDispatchReason>,

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
    // [1319-D1] removed the abort/reap/close-only fields (reason, dry_run, force,
    // require_collected) alongside those no-real-authority actions.
}

// ─── Facade: tachi_verify (background verification ledger) ──────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiVerifyCheckItem {
    /// Stable check id, e.g. gitleaks, cargo-check, clippy.
    pub check_id: String,

    /// Check kind, e.g. gitleaks, cargo_check, clippy, cargo_test, custom.
    pub kind: String,

    /// Verification status: pending, running, passed, failed, skipped, or stale.
    pub status: String,

    /// Whether this check is required for safe_merge. Defaults true.
    #[serde(default)]
    pub required: Option<bool>,

    /// Command recorded for this verification result.
    #[serde(default)]
    pub command: Option<String>,

    /// Short result summary.
    #[serde(default)]
    pub summary: Option<String>,

    /// Git head SHA this check result was produced for.
    #[serde(default)]
    pub head_sha: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiVerifyParams {
    /// Action: start / record / status / board (F4 typed enum).
    #[schemars(schema_with = "tachi_verify_action_schema")]
    pub action: super::TachiVerifyAction,

    /// Response shape: "markdown" (default receipt), "json" (automation receipt), or "full" (pre-change verbose payload).
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

    /// Stable check id, e.g. gitleaks, cargo-check, clippy, tachi-server-tests.
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
    #[schemars(schema_with = "crate::coerce::opt_integer_from_string_or_number_schema")]
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
    #[schemars(schema_with = "crate::coerce::opt_integer_from_string_or_number_schema")]
    pub limit: Option<u32>,

    /// Batch record/start payload. Cannot be combined with single-check fields (check_id/kind/command/commands).
    #[serde(default)]
    pub checks: Vec<TachiVerifyCheckItem>,
}

// ─── Facade: tachi_staff (external staffing via canonical dispatch kernel) ────

/// Parameters for the `tachi_staff` facade — external staffing via the
/// canonical dispatch kernel.
///
/// This is a flat, MCP-compatible struct (not a `#[serde(tag)]` enum, which
/// produces a schema without the root `type: object` the MCP spec requires).
/// `action` selects the path; the facade handler enforces per-action
/// admission:
/// - `action='start'`: the handler REQUIRES a typed `staffing_reason`
///   (native-first exception gate) — a `start` request with `staffing_reason =
///   None` is rejected with zero artifacts, same fail-closed shape as the
///   retired `require_tachi_dispatch_reason`.
/// - `action='status'`: only `dispatch_id` is required; `staffing_reason` is
///   ignored and MUST NOT be required, so a read-only probe is never forced to
///   fabricate an admission reason.
///
/// `staffing_reason` is therefore `Option<TachiDispatchReason>` at the schema
/// level (so `status` can omit it) but REQUIRED semantically for `start` —
/// enforced by the handler, not by a cross-action struct field.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiStaffParams {
    /// Action: "start" | "status"
    #[schemars(schema_with = "tachi_staff_action_schema")]
    pub action: String,

    /// Response shape: default JSON for agent automation; pass "markdown" for human-readable text.
    #[serde(default)]
    pub format: Option<String>,

    /// Existing canonical dispatch_id for `action='status'`. Required for
    /// status; ignored for start.
    #[serde(default)]
    pub dispatch_id: Option<String>,

    /// Task description / prompt for the worker. Required for `start`
    /// (the route validates non-empty); ignored for `status`.
    #[serde(default)]
    pub task: Option<String>,

    /// Typed reason execution is leaving the host harness. REQUIRED for
    /// `action='start'` (the handler rejects `None` with zero artifacts — the
    /// admission gate); IGNORED for `action='status'` (a read-only probe never
    /// needs a reason, and MUST NOT be pressured to fabricate one). Optional
    /// at the schema level precisely so `status` can omit it; the `start`
    /// handler enforces presence. Reuses [`TachiDispatchReason`] so the
    /// vocabulary cannot drift.
    #[serde(default)]
    pub staffing_reason: Option<TachiDispatchReason>,

    /// Semantic dispatch profile hint — resolved through the existing profile
    /// pipeline, not a raw agent/transport override. Start-only.
    #[serde(default)]
    pub profile: Option<String>,

    /// Worker/agent backend hint (e.g. "claude", "codex", "custom"). Start-only.
    #[serde(default)]
    pub worker: Option<String>,

    /// Optional named project DB for context search. Start-only.
    #[serde(default)]
    pub project: Option<String>,

    /// Dispatch stage: "plan" | "execute" | "auto". Start-only.
    #[serde(default)]
    pub stage: Option<String>,

    /// GitHub issue reference bound to this dispatch. Start-only.
    #[serde(default)]
    pub issue_ref: Option<String>,

    /// GitHub PR reference bound to this dispatch. Start-only.
    #[serde(default)]
    pub pr_ref: Option<String>,

    /// Tachi flow id for feature-scoped briefing/dispatch/eval linkage. Start-only.
    #[serde(default)]
    pub flow_id: Option<String>,
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
    /// aggregate | aggregate_live | telemetry | perf | register | observe |
    /// adjudicate | get. aggregate replays a local JSONL fixture only when
    /// TACHI_AGENT_EVAL_ALLOW_FIXTURE=1 is set. register/observe/adjudicate/get
    /// (#1066) are the mirror eval intake for harness-native subagents — work
    /// Tachi did not dispatch and only observes.
    pub action: String,
    #[serde(default)]
    pub fixture_path: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,

    /// action=register payload (#1066): records the parent contract,
    /// execution origin, lifecycle owner, harness/native child id, and
    /// requested identity for a harness-native subagent Tachi did not
    /// dispatch. Returns a stable `eval_run_id`.
    #[serde(default)]
    pub register: Option<MirrorEvalRegisterParams>,

    /// action=observe payload (#1066): carrier-observed terminal facts only —
    /// this type carries no usefulness/failure-mode/plan-delta field, so
    /// observe structurally cannot write judgment.
    #[serde(default)]
    pub observe: Option<MirrorEvalObserveParams>,

    /// action=adjudicate payload (#1066): leader/independent-reviewer
    /// judgment for an already-registered run.
    #[serde(default)]
    pub adjudicate: Option<MirrorEvalAdjudicateParams>,

    /// action=get payload (#1066): resolve a run by `eval_run_id` or
    /// `native_child_id`.
    #[serde(default)]
    pub get: Option<MirrorEvalGetParams>,
}

/// #1066 `register`: records the parent contract, execution origin,
/// lifecycle owner, harness/native child id, and requested identity for a
/// harness-native subagent. Same `native_child_id` + same content replays
/// idempotently; same `native_child_id` + different content is an explicit
/// conflict (never silently overwritten).
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, JsonSchema)]
pub struct MirrorEvalRegisterParams {
    /// The frozen contract (issue/PR ref) the native subagent's work is
    /// under, e.g. `"kckylechen1/tachi#1066"`. Required.
    pub frozen_contract_ref: String,

    /// What kind of execution this is, e.g. `"host_native_subagent"`.
    /// Required.
    pub execution_origin: String,

    /// Who owns starting/stopping this worker, e.g. `"host"`. Tachi never
    /// claims it can wait, cancel, or close a host-owned worker — this field
    /// records that ownership explicitly. Required.
    pub lifecycle_owner: String,

    /// The host harness that launched the subagent, e.g.
    /// `"claude_code_task_tool"`.
    #[serde(default)]
    pub harness: Option<String>,

    /// The host-native child id, when the host exposes one. Absent this,
    /// registration can never be deduped — every call mints a fresh run.
    #[serde(default)]
    pub native_child_id: Option<String>,

    /// Requested identity — profile at spawn time.
    #[serde(default)]
    pub requested_profile: Option<String>,

    /// Requested identity — model at spawn time.
    #[serde(default)]
    pub requested_model: Option<String>,

    /// Requested identity — agent/vendor label at spawn time.
    #[serde(default)]
    pub requested_agent: Option<String>,
}

/// #1066 `observe`: carrier-observed terminal facts, duration/cost,
/// result/artifact references, and effective identity. Deliberately has NO
/// usefulness/failure_mode/plan_delta/evidence_usable field — those exist
/// only on [`MirrorEvalAdjudicateParams`]. Resolve the target run by
/// `eval_run_id` OR `native_child_id`.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, JsonSchema)]
pub struct MirrorEvalObserveParams {
    /// Resolve the target run by its `eval_run_id`.
    #[serde(default)]
    pub eval_run_id: Option<String>,

    /// Resolve the target run by its `native_child_id` (used when the caller
    /// does not have the `eval_run_id` handy).
    #[serde(default)]
    pub native_child_id: Option<String>,

    /// Carrier-observed terminal outcome, e.g. `"success"` / `"failure"` /
    /// `"partial"` / `"aborted"` / `"unknown"`. Required.
    pub terminal_outcome: String,

    #[serde(default)]
    pub duration_ms: Option<u64>,

    #[serde(default)]
    pub cost_tokens: Option<u64>,

    #[serde(default)]
    pub cost_usd: Option<f64>,

    /// Pointer to the result (PR/diff/artifact summary).
    #[serde(default)]
    pub result_ref: Option<String>,

    #[serde(default)]
    pub artifacts: Vec<String>,

    /// Carrier-observed (effective, not requested) model identity.
    #[serde(default)]
    pub effective_model: Option<String>,

    #[serde(default)]
    pub effective_backend: Option<String>,

    #[serde(default)]
    pub effective_harness: Option<String>,
}

/// #1066 `adjudicate`: leader/independent-reviewer judgment for an
/// already-registered, already-observed run. Append-only — a correction is a
/// new call with a fresh idempotency identity, never an edit of a prior
/// judgment.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, JsonSchema)]
pub struct MirrorEvalAdjudicateParams {
    #[serde(default)]
    pub eval_run_id: Option<String>,

    #[serde(default)]
    pub native_child_id: Option<String>,

    /// Who adjudicated (the leader/independent-reviewer seat identity).
    /// Required.
    pub actor: String,

    /// The reviewing engine's OWN effective model/lineage identity — the
    /// `verifier_engine_receipt` half of cross-model independence. Omit or
    /// leave unknown when the reviewer's own identity cannot be established;
    /// an unknown identity can never satisfy cross-model independence.
    #[serde(default)]
    pub verifier_model: Option<String>,

    /// Usefulness verdict, e.g. `"useful"` / `"partially_useful"` /
    /// `"not_useful"` / `"failed"`. Required.
    pub usefulness: String,

    #[serde(default)]
    pub failure_mode: Option<String>,

    #[serde(default)]
    pub first_review_findings: Vec<String>,

    #[serde(default)]
    pub plan_delta: Option<String>,

    #[serde(default)]
    pub next_prompt_delta: Option<String>,

    /// Whether this run's evidence is usable for routing/card aggregation.
    /// Required — no silent default: an omitted flag must not accidentally
    /// promote a row into aggregation.
    pub evidence_usable: bool,

    #[serde(default)]
    pub used_in_final_claim: bool,

    #[serde(default)]
    pub human_override: bool,

    /// Evidence reference backing this judgment (run id, review artifact).
    /// Required.
    pub evidence_ref: String,

    /// Idempotency identity for this specific judgment event. Replaying the
    /// same `event_key` with the same content is a no-op; a correction uses
    /// a NEW `event_key`. Defaults to a deterministic value derived from
    /// `eval_run_id` + `actor` + `usefulness` when omitted (single-shot
    /// callers do not need to invent one).
    #[serde(default)]
    pub event_key: Option<String>,
}

/// #1066 `get`: resolve a run by `eval_run_id` or `native_child_id`.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, JsonSchema)]
pub struct MirrorEvalGetParams {
    #[serde(default)]
    pub eval_run_id: Option<String>,

    #[serde(default)]
    pub native_child_id: Option<String>,
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

    /// tachi#1173 item 3: when true, restore the full per-row payload
    /// (identity_receipt/acpx/acpx_events) and skip folding terminal
    /// (completed/failed/canceled) rows into per-state count rows. Default
    /// (false/omitted): rows omit identity_receipt/acpx/acpx_events, and on
    /// the unfiltered view (`state_filter` omitted or "all") terminal rows
    /// are folded into count rows instead of listed individually. An
    /// explicit non-"all" `state_filter` is itself treated as an ask to see
    /// that state expanded and also disables folding, independent of this
    /// flag.
    #[serde(default)]
    pub verbose: Option<bool>,
}
