use super::{action_inventory, string_enum_schema, TachiDispatchReason};
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

fn tachi_verify_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &super::action_enums::TachiVerifyAction::all_wire_strings(),
        "Required Tachi verification ledger action (start/record/status/board/run).",
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
    /// Action: start / record / status / board / run (F4 typed enum).
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
    ///
    /// NEVER persisted from caller input: `tachi_verify start/record` strips
    /// caller-authored authority fields at write (ledger::base_item), so this
    /// field only round-trips through params — it is not durable evidence.
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_i64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_integer_from_string_or_number_schema")]
    pub exit_code: Option<i64>,

    /// Path to a durable log/artifact for this verification run.
    ///
    /// NEVER persisted from caller input (same strip rule as `exit_code`);
    /// the server-executed `action=run` path writes its own server-observed
    /// log path into the ledger item and receipt.
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

    /// action=run only (#1454): closed-set verification kind the server
    /// executes — version-sync, clippy, fmt, audit, nextest, portable-contract,
    /// doc (the full ci.yml rust-job surface; see
    /// `tachi-server::verify_ops::MERGE_REQUIRED_RUN_KINDS`). No caller-supplied
    /// argv is ever accepted; the server maps kind → its own command table.
    #[serde(default)]
    pub check_kind: Option<String>,

    /// action=run only (#1454): per-run timeout in seconds. Default 1800;
    /// capped at 3600 (provisional, dispatch clause 9). Server-enforced via
    /// kill-on-timeout; never a caller-negotiated relaxation of the cap.
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_u64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_integer_from_string_or_number_schema")]
    pub timeout_secs: Option<u64>,
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
#[serde(deny_unknown_fields)]
pub struct TachiStaffParams {
    /// Action: "start" | "status" | "cancel"
    #[schemars(schema_with = "tachi_staff_action_schema")]
    pub action: String,

    /// Response shape: default JSON for agent automation; pass "markdown" for human-readable text.
    #[serde(default)]
    pub format: Option<String>,

    /// Existing canonical dispatch_id for `action='status'`. Required for
    /// status; ignored for start.
    #[serde(default)]
    pub dispatch_id: Option<String>,

    // Canonical receipt revision required by action=cancel.
    #[serde(default)]
    pub expected_status_revision: Option<u64>,

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

    /// tachi#1675 PR1 Seam B: the `recommendation_id` of a persisted internal
    /// route-recommendation fact, when this start was placed on that advice.
    /// Optional and start-only — absence is itself evidence (`assignment_mode`
    /// records `unadvised`, never a fabricated advisory). Typed Staff
    /// resolution validates it before acceptance: an unknown or stale
    /// reference is refused with zero claim, workspace artifact, or
    /// route-decision evidence.
    #[serde(default)]
    pub recommendation_ref: Option<String>,
}

impl TachiStaffParams {
    pub fn cancel_request(&self) -> Result<(String, u64), String> {
        let dispatch_id = self
            .dispatch_id
            .clone()
            .ok_or_else(|| "tachi_staff: action='cancel' requires a `dispatch_id`".to_string())?;
        let expected_status_revision = self.expected_status_revision.ok_or_else(|| {
            "tachi_staff: action='cancel' requires `expected_status_revision`".to_string()
        })?;
        for (name, present) in [
            ("task", self.task.is_some()),
            ("staffing_reason", self.staffing_reason.is_some()),
            ("profile", self.profile.is_some()),
            ("worker", self.worker.is_some()),
            ("project", self.project.is_some()),
            ("stage", self.stage.is_some()),
            ("issue_ref", self.issue_ref.is_some()),
            ("pr_ref", self.pr_ref.is_some()),
            ("flow_id", self.flow_id.is_some()),
            ("recommendation_ref", self.recommendation_ref.is_some()),
        ] {
            if present {
                return Err(format!(
                    "tachi_staff: action='cancel' rejects start-only field `{name}`"
                ));
            }
        }
        Ok((dispatch_id, expected_status_revision))
    }

    pub fn to_assignment_request(&self) -> Result<crate::facade::StaffAssignmentRequest, String> {
        if self.dispatch_id.is_some() {
            return Err(
                "dispatch_id is minted by the kernel and cannot be specified when action='start'"
                    .to_string(),
            );
        }
        if self.expected_status_revision.is_some() {
            return Err(
                "expected_status_revision is accepted only when action='cancel'".to_string(),
            );
        }
        let staffing_reason = self
            .staffing_reason
            .ok_or_else(|| "staffing_reason is required when action='start'".to_string())?;
        let task = self
            .task
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| "task is required when action='start'".to_string())?
            .to_string();

        Ok(crate::facade::StaffAssignmentRequest {
            staffing_reason,
            task,
            profile: self.profile.clone(),
            worker: self.worker.clone(),
            stage: self.stage.clone(),
            execution_level: None,
            issue_ref: self.issue_ref.clone(),
            pr_ref: self.pr_ref.clone(),
            flow_id: self.flow_id.clone(),
            project: self.project.clone(),
            completion_predicate: None,
            recommendation_ref: self.recommendation_ref.clone(),
        })
    }
}

// ─── Internal: orchestrator (persistent hard_state TODO / handoff) ─────────

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct TachiOrchestratorParams {
    /// todo_list | todo_update | handoff_write | handoff_read | recovery_briefing
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

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct TachiAgentEvalParams {
    /// aggregate | aggregate_live | telemetry | perf | register | observe |
    /// adjudicate | get | route_projection | attach_session | get_attachment.
    /// aggregate replays a local JSONL
    /// fixture only when TACHI_AGENT_EVAL_ALLOW_FIXTURE=1 is set.
    /// register/observe/adjudicate/get (#1066) are the mirror eval intake for
    /// harness-native subagents — work Tachi did not dispatch and only
    /// observes. route_projection (#1675) is the ledger-backed routing
    /// projection that runs in parallel with the `/eval`-memory path.
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

    /// action=route_projection payload (tachi#1675 PR2 / design D6 phase 1):
    /// the ledger-backed routing projection. It runs in PARALLEL with the
    /// `/eval`-memory path — `recommend` is untouched — and every response
    /// declares `evidence_source` so a consumer can tell the two apart. The
    /// payload is optional: with no payload the projection answers for an
    /// unclassified task over the default window.
    #[serde(default)]
    pub projection: Option<RouteProjectionParams>,

    // #1733 generic host-owned ACP attachment admission. These fields remain
    // flat for compatibility with the existing single-facade parameter
    // contract; action-specific presence is enforced by the server handler.
    #[serde(default)]
    #[schemars(
        description = "[action=attach_session|get_attachment] Admitted host connection identity."
    )]
    pub host_identity: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=attach_session] Stable admitted AgentIdentity id.")]
    pub agent_identity_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=attach_session|get_attachment] Existing WorkClaim id.")]
    pub work_claim_id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=attach_session] Exact WorkClaim transition revision (compare-and-swap)."
    )]
    pub expected_transition_revision: Option<i64>,
    #[serde(default)]
    #[schemars(description = "[action=attach_session|get_attachment] ACP protocol version.")]
    pub protocol_version: Option<i64>,
    #[serde(default)]
    #[schemars(
        description = "[action=attach_session|get_attachment] Opaque adapter-owned connection identity."
    )]
    pub adapter_connection_identity: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=attach_session|get_attachment] Opaque remote ACP session id."
    )]
    pub remote_session_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=attach_session] Frozen assignment/contract digest.")]
    pub contract_digest: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=attach_session] Closed ACP session capabilities: observe, wait, prompt, cancel, resume, load, events, artifacts."
    )]
    pub session_capabilities: Vec<String>,
    #[serde(default)]
    #[schemars(description = "[action=attach_session] Requested policy tool profile.")]
    pub tool_profile: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=attach_session] Requested policy capability class.")]
    pub capability_class: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=attach_session] Attachment idempotency key.")]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=attach_session] Durable admission receipt reference.")]
    pub admission_receipt_ref: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=get_attachment] Stable attachment id.")]
    pub attachment_id: Option<String>,
}

/// tachi#1675 PR2 `route_projection`: what task the projection is being
/// asked about, and how far back to look. The candidate set itself is NOT a
/// parameter — it comes from the existing admission/required/blocked risk
/// classifier, which prunes before any scoring so a historical score can
/// never resurrect a candidate the current rules removed.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, JsonSchema)]
pub struct RouteProjectionParams {
    /// Free-text task description, classified exactly the way
    /// `tachi_orchestrator(recommend)` classifies it.
    #[serde(default)]
    pub task: Option<String>,

    /// Explicit task type (e.g. `fix_request`), when the caller already knows
    /// it. Omitted, it is derived from `task`.
    #[serde(default)]
    pub task_type: Option<String>,

    /// Risk override (`low`/`medium`/`high`/`critical`), same semantics as
    /// the recommendation path.
    #[serde(default)]
    pub risk: Option<String>,

    /// Files in scope, used by the risk classifier.
    #[serde(default)]
    pub file_paths: Option<Vec<String>>,

    /// Evidence window in days (default 30, capped at 365). Rows older than
    /// the window are counted as excluded, never silently dropped.
    #[serde(default)]
    pub window_days: Option<u32>,
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

    /// tachi#1675 PR1 D3: optional structured rubric block. When present, an
    /// `eval_rubric_scores` row is written alongside the free-text verdict
    /// above (the rubric is a companion, never a replacement — the verdict
    /// stays the #1035 CHECK-constraint backbone). Omitted entirely, no
    /// rubric row is written and this event's `excluded_reason` at the
    /// projection layer is `unstructured_verdict`.
    #[serde(default)]
    pub rubric: Option<EvalRubricParams>,
}

/// tachi#1675 PR1 D3: structured judgment alongside the free-text `verdict`.
/// Six ordinal dimensions (closed vocabulary `pass`/`concern`/`fail`/
/// `not_assessed` — never a float; floats invite averaging into the
/// forbidden one-dimensional reputation score) plus a confidence label.
/// `adjudicator_vendor` is optional — when omitted the writer derives it from
/// this same call's `verifier_model` (the existing cross-model-independence
/// primitive); `adjudicator_actor` is not a separate field here — it is the
/// enclosing call's own `actor`, the same identity `dispatch_adjudications`/
/// `mirror_eval_adjudications` already record as the adjudicating actor.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, JsonSchema)]
pub struct EvalRubricParams {
    /// `pass` | `concern` | `fail` | `not_assessed`.
    pub contract_correctness: String,
    /// `pass` | `concern` | `fail` | `not_assessed`.
    pub evidence_quality: String,
    /// `pass` | `concern` | `fail` | `not_assessed`.
    pub safety: String,
    /// `pass` | `concern` | `fail` | `not_assessed`.
    pub scope_discipline: String,
    /// `pass` | `concern` | `fail` | `not_assessed`.
    pub intervention_burden: String,
    /// `pass` | `concern` | `fail` | `not_assessed`.
    pub completion_integrity: String,
    /// `low` | `medium` | `high`.
    pub adjudication_confidence: String,
    /// The adjudicator's OWN vendor/lineage identity. Optional — falls back
    /// to `lineage_of(verifier_model)` (this call's existing field) when
    /// omitted, so a caller that already sets `verifier_model` does not have
    /// to restate it.
    #[serde(default)]
    pub adjudicator_vendor: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tachi_staff_params_rejects_hostile_execution_fields() {
        // Check each model-facing execution field separately. A combined
        // payload may stop at its first unknown key and mask a widened schema.
        for (field, value) in [
            ("command", serde_json::json!(["sh", "-c", "echo pwned"])),
            ("cwd", serde_json::json!("/etc")),
            ("env", serde_json::json!({"SECRET": "pwned"})),
            ("env_vars", serde_json::json!({"SECRET": "pwned"})),
            ("credentials", serde_json::json!(["admin"])),
            ("credential_profiles", serde_json::json!(["admin"])),
            ("allowed_tools", serde_json::json!(["Bash"])),
            ("tools", serde_json::json!(["Bash"])),
            ("sandbox", serde_json::json!("danger-full-access")),
            ("harness_transport", serde_json::json!("cli")),
            (
                "harness_server_url",
                serde_json::json!("http://127.0.0.1:4321"),
            ),
            ("timeout_secs", serde_json::json!(1)),
            ("pid", serde_json::json!(1234)),
            ("process_id", serde_json::json!("forged-process")),
            ("exit_code", serde_json::json!(0)),
            ("result", serde_json::json!("forged result")),
            ("result_written", serde_json::json!(true)),
        ] {
            let mut hostile = serde_json::json!({
                "action": "start",
                "task": "Do work",
                "staffing_reason": "explicit_user_request",
            });
            hostile[field] = value;
            let err = serde_json::from_value::<TachiStaffParams>(hostile).unwrap_err();
            assert!(
                err.to_string().contains("unknown field") && err.to_string().contains(field),
                "tachi_staff must reject model-facing '{field}' before any artifact: {err}"
            );
        }
    }

    #[test]
    fn tachi_staff_params_to_assignment_request_maps_cleanly() {
        let raw = serde_json::json!({
            "action": "start",
            "task": "Clean mapping",
            "staffing_reason": "durable_cross_session",
            "worker": "claude",
            "profile": "claude_plan",
            "stage": "plan",
            "project": "tachi",
            "issue_ref": "kckylechen1/tachi#1692",
            "pr_ref": "kckylechen1/tachi#1812",
            "flow_id": "flow-c5",
            "recommendation_ref": "rec-999",
        });
        let params: TachiStaffParams = serde_json::from_value(raw).expect("deserializes");
        let req = params.to_assignment_request().expect("maps to request");
        assert_eq!(req.task, "Clean mapping");
        assert_eq!(
            req.staffing_reason,
            crate::facade::TachiDispatchReason::DurableCrossSession
        );
        assert_eq!(req.worker.as_deref(), Some("claude"));
        assert_eq!(req.profile.as_deref(), Some("claude_plan"));
        assert_eq!(req.stage.as_deref(), Some("plan"));
        assert_eq!(req.project.as_deref(), Some("tachi"));
        assert_eq!(req.issue_ref.as_deref(), Some("kckylechen1/tachi#1692"));
        assert_eq!(req.pr_ref.as_deref(), Some("kckylechen1/tachi#1812"));
        assert_eq!(req.flow_id.as_deref(), Some("flow-c5"));
        assert_eq!(req.recommendation_ref.as_deref(), Some("rec-999"));
    }

    #[test]
    fn tachi_staff_params_rejects_forged_dispatch_id_on_start() {
        let raw = serde_json::json!({
            "action": "start",
            "task": "Forged dispatch_id",
            "staffing_reason": "explicit_user_request",
            "dispatch_id": "forged-id-123",
        });
        let params: TachiStaffParams = serde_json::from_value(raw).expect("deserializes");
        let err = params.to_assignment_request().unwrap_err();
        assert!(
            err.contains("dispatch_id is minted by the kernel"),
            "action='start' must reject caller-supplied dispatch_id: {err}"
        );
    }
}

#[cfg(test)]
mod issue_1825_cancel_tests {
    use super::TachiStaffParams;

    #[test]
    fn tachi_staff_cancel_schema_and_effect_contract() {
        let accepted: TachiStaffParams = serde_json::from_value(serde_json::json!({
            "action": "cancel",
            "dispatch_id": "20260823T010101Z-custom-deadbeef",
            "expected_status_revision": 7,
            "format": "json",
        }))
        .expect("the frozen cancel fields deserialize");
        assert_eq!(
            accepted.cancel_request().expect("cancel fields validate"),
            ("20260823T010101Z-custom-deadbeef".to_string(), 7)
        );
        for field in [
            "task",
            "staffing_reason",
            "profile",
            "worker",
            "project",
            "stage",
            "issue_ref",
            "pr_ref",
            "flow_id",
            "recommendation_ref",
        ] {
            let mut raw = serde_json::json!({
                "action": "cancel",
                "dispatch_id": "20260823T010101Z-custom-deadbeef",
                "expected_status_revision": 7,
            });
            raw[field] = match field {
                "staffing_reason" => serde_json::json!("explicit_user_request"),
                "stage" => serde_json::json!("plan"),
                _ => serde_json::json!("forged-authority"),
            };
            let params: TachiStaffParams = serde_json::from_value(raw)
                .expect("flat schema admits the field for action validation");
            assert!(
                params.cancel_request().is_err(),
                "cancel must reject {field}"
            );
        }
        let schema = serde_json::to_value(rmcp::schemars::schema_for!(TachiStaffParams))
            .expect("Staff schema serializes");
        let properties = schema["properties"]
            .as_object()
            .expect("Staff schema properties");
        for field in [
            "pid",
            "pgid",
            "command",
            "cwd",
            "env",
            "credentials",
            "signal",
        ] {
            let mut raw = serde_json::json!({
                "action": "cancel",
                "dispatch_id": "20260823T010101Z-custom-deadbeef",
                "expected_status_revision": 7,
            });
            raw[field] = serde_json::json!("hostile-process-authority");
            let error = serde_json::from_value::<TachiStaffParams>(raw)
                .expect_err("control field must fail Staff deserialization");
            assert!(
                error.to_string().contains("unknown field") && error.to_string().contains(field),
                "Staff deserialization must reject control field {field}: {error}"
            );
            assert!(
                !properties.contains_key(field),
                "Staff's generated MCP schema must not advertise control field {field}"
            );
        }
    }
}
