//! Internal Staff start/status contract — a thin semantic adapter onto the
//! canonical dispatch kernel.
//!
//! This is NOT a public MCP tool, NOT a new facade, and NOT a parallel
//! lifecycle. It maps a small model-decidable request into
//! [`TachiDispatchParams`](crate::tool_params::TachiDispatchParams) and delegates
//! to the single canonical choke-point [`crate::dispatch_ops::handle_tachi_dispatch`];
//! it creates no second result/status store.
//!
//! # Boundary contract
//!
//! The model may only set *intent* fields here (task, worker/profile/project
//! hints, issue/pr/flow refs) plus a **required typed `staffing_reason`**.
//! Execution fields — `cwd`, `command`, `transport`, `credentials`, `sandbox`,
//! `allowed_tools`, MCP plumbing, watchdog internals — are resolved by profile /
//! policy / [`crate::host_profile`] / ExecEnv / adapter during mapping, NEVER by
//! the caller. The dispatch kernel owns the single lifecycle; the canonical
//! `status.json` receipt owns the single truth.
//!
//! # Admission gate (#1319 review blocker)
//!
//! `staffing_reason` is **required** (non-optional). A `tachi_staff(start)`
//! request that omits it is rejected at deserialization before `staff_start`
//! runs, and the kernel's defense-in-depth check (inside
//! [`handle_tachi_dispatch`], before any run artifact) fails closed if a
//! well-typed caller somehow reaches it without one. This restores the
//! native-first admission that the retired `tachi_task(dispatch)`
//! `require_tachi_dispatch_reason` gate enforced: external staffing is an
//! *exception*, admitted only for a named, receipt-stamped reason, never the
//! default delegation path.
//!
//! # Why an explicit allowlist struct
//!
//! [`StaffStartRequest`] deliberately has NO field named `cwd`, `command`,
//! `transport`, `credentials`, `sandbox`, or `allowed_tools`. A request JSON
//! that attempts to set any of those is silently ignored (no matching field
//! exists to deserialize into), so the resulting [`TachiDispatchParams`] maps
//! every execution field to its kernel-side default.
//
// `#[allow(dead_code)]` until [1319-B7] wires the `tachi_staff` facade caller.
// The items ARE exercised by this module's discrimination tests, so
// `--all-targets` sees them as live and `#[expect(dead_code)]` would go red
// there as an unfulfilled expectation; only the `--lib` gate reports them.
// Delete this attribute in the PR that adds the public caller. (Same
// convention as `governed_precedent_establishment.rs:132-137`.)
#![allow(dead_code)]

use crate::dispatch_ops::{
    canonical_dir_is_within, dispatch_runs_root, handle_tachi_dispatch, is_valid_dispatch_id,
};
use crate::tool_params::{TachiDispatchParams, TachiDispatchReason};
use crate::MemoryServer;
use rmcp::schemars::JsonSchema;
// The `#[derive(JsonSchema)]` macro expands to reference `schemars::...`, so
// the crate must be in scope under that name.
use rmcp::schemars;

/// Default per-dispatch timeout (seconds) when the Staff request does not
/// declare one. Mirrors `default_dispatch_timeout` in `tachi-params` (600s);
/// duplicated locally because the params crate keeps that fn private.
const DEFAULT_STAFF_DISPATCH_TIMEOUT_SECS: u64 = 600;

/// Minimal semantic request for externally staffing a worker. The model may
/// only set intent fields here; execution fields (cwd, command, transport,
/// credentials, sandbox, allowed_tools, MCP plumbing) are resolved by profile /
/// policy / ExecEnv / adapter during mapping, NEVER by the caller.
///
/// `staffing_reason` is **required** and typed: it is the admission contract.
/// A missing or free-form reason is rejected (serde fails on a missing required
/// field; an unknown variant fails schema deserialization). The reason is
/// stamped into the canonical receipt so staffing is auditable.
///
/// `#[serde(default)]` on the optional fields plus the absence of any
/// execution-shaped field means an inbound JSON carrying `"cwd": "/evil"` or
/// `"command": ["rm", "-rf"]` is silently ignored — those names have no field
/// to bind to, so they cannot leak into the mapped params.
#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
pub(crate) struct StaffStartRequest {
    /// Task description / prompt for the worker. Required — a Staff request
    /// with no task is meaningless.
    pub task: String,
    /// REQUIRED typed reason execution is leaving the host harness. Admission
    /// fails closed without it (see the module-level admission-gate docs).
    /// Reuses [`TachiDispatchReason`] so the vocabulary cannot drift from the
    /// retired `tachi_task(dispatch)` gate.
    pub staffing_reason: TachiDispatchReason,
    /// Semantic dispatch profile hint — resolved through the existing profile
    /// pipeline, not a raw agent/transport override.
    #[serde(default)]
    pub profile: Option<String>,
    /// Worker/agent backend hint (e.g. "claude", "codex", "custom"). Resolved
    /// to the canonical `agent` field; never a transport override.
    #[serde(default)]
    pub worker: Option<String>,
    /// Optional named project DB for context search.
    #[serde(default)]
    pub project: Option<String>,
    /// Dispatch stage: "plan" | "execute" | "auto".
    #[serde(default)]
    pub stage: Option<String>,
    /// GitHub issue reference bound to this dispatch.
    #[serde(default)]
    pub issue_ref: Option<String>,
    /// GitHub PR reference bound to this dispatch.
    #[serde(default)]
    pub pr_ref: Option<String>,
    /// Tachi flow id for feature-scoped briefing/dispatch/eval linkage.
    #[serde(default)]
    pub flow_id: Option<String>,
}

impl StaffStartRequest {
    /// Map a semantic Staff request onto the canonical dispatch params.
    ///
    /// Every execution-shaped field (`cwd`, `command`, `transport`,
    /// `credentials`, `sandbox`, `allowed_tools`, MCP plumbing, watchdog
    /// internals) is left at its kernel-side default: `None` / empty / the
    /// default timeout. Those are resolved by the canonical admission / policy
    /// / profile pipeline inside `handle_tachi_dispatch`, NEVER set here.
    ///
    /// `staffing_reason` IS mapped through (not dropped) — it carries the
    /// admission contract into `TachiDispatchParams` so the kernel's
    /// defense-in-depth check and the receipt stamp both see it.
    ///
    /// NOTE: `TachiDispatchParams` does NOT derive `Default` (it has 30+ fields
    /// with non-trivial serde attributes), so this method enumerates every
    /// field explicitly — same pattern the dispatch tests use for their
    /// `test_dispatch_params` helper. Any new field added to
    /// `TachiDispatchParams` will surface as a compile error here, forcing an
    /// explicit decision about whether Staff should expose it (default: no).
    fn into_params(self) -> TachiDispatchParams {
        TachiDispatchParams {
            task: self.task,
            // #1319 admission contract: carry the typed reason into the kernel.
            staffing_reason: self.staffing_reason,
            agent: self.worker,
            profile: self.profile,
            project: self.project,
            stage: self.stage,
            issue_ref: self.issue_ref,
            pr_ref: self.pr_ref,
            flow_id: self.flow_id,
            // ── Execution fields: intentionally left at kernel defaults ──────
            cwd: None,
            env_id: None,
            unmanaged_cwd: None,
            execution_level: None,
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
            completion_predicate: None,
            max_turns: None,
            timeout_secs: DEFAULT_STAFF_DISPATCH_TIMEOUT_SECS,
            auto_capability_bundle: None,
            verbose: None,
            inject_card: None,
        }
    }
}

/// Read-only status probe. Only the canonical `dispatch_id` is accepted —
/// there is no Staff-local id namespace.
#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
pub(crate) struct StaffStatusRequest {
    pub dispatch_id: String,
}

/// Start a worker via the canonical dispatch kernel.
///
/// Maps the semantic [`StaffStartRequest`] into [`TachiDispatchParams`] and
/// delegates to the single canonical choke-point
/// [`handle_tachi_dispatch`]. The `staffing_reason` field is required on the
/// request struct, so a caller that omits it is rejected at deserialization
/// (the field has no `#[serde(default)]`). This is the facade-level admission
/// gate; the kernel additionally fail-closes inside `handle_tachi_dispatch`
/// before any run artifact is created. That entry-point seeds canonical
/// `status.json` (receipt-first) BEFORE prompt assembly / plan stage / spawn,
/// so a successful `Ok` return guarantees the canonical receipt already exists
/// on disk. This adapter creates NO parallel store.
pub(crate) async fn staff_start(
    server: &MemoryServer,
    request: StaffStartRequest,
) -> Result<String, String> {
    // Facade-level admission: `staffing_reason` is a non-optional field, so a
    // missing reason is rejected by serde before this fn runs. No additional
    // runtime check is needed here — the struct's type IS the gate. The
    // kernel-side defense-in-depth check inside handle_tachi_dispatch catches
    // any future caller that reaches it without going through this struct.
    let params = request.into_params();
    handle_tachi_dispatch(server, params).await
}

/// Read a worker's canonical status receipt.
///
/// Reads ONLY the canonical `status.json` that `handle_tachi_dispatch` (and
/// thus [`staff_start`]) writes at `<runs_root>/<dispatch_id>/status.json`.
/// No second result/status store is created or consulted. The dispatch_id is
/// gated by the same path-traversal allowlist every other status reader uses
/// ([`is_valid_dispatch_id`] + [`canonical_dir_is_within`]), so a malformed or
/// escaping id behaves identically to "not found" — fail-closed.
///
/// Returns the canonical status JSON verbatim (the receipt owns the truth); a
/// future slice may project a trimmed view, but v1 returns the whole blob so
/// there is exactly one status shape to reason about.
pub(crate) async fn staff_status(
    _server: &MemoryServer,
    request: StaffStatusRequest,
) -> Result<String, String> {
    staff_status_impl(&request).await
}

/// Synchronous core of [`staff_status`], split out so tests can exercise the
/// canonical-receipt read without constructing a full `MemoryServer` (the
/// status read touches only the filesystem, never the server). The public
/// `staff_status` keeps the `&MemoryServer` parameter for facade-call
/// symmetry with `staff_start`, even though the status path does not use it
/// today — a future slice that projects status through server-held policy
/// will need it.
async fn staff_status_impl(request: &StaffStatusRequest) -> Result<String, String> {
    if !is_valid_dispatch_id(&request.dispatch_id) {
        return Err(format!(
            "staff_status: unknown dispatch_id {:?}",
            request.dispatch_id
        ));
    }
    let runs_root = dispatch_runs_root();
    let run_dir = runs_root.join(&request.dispatch_id);
    if !run_dir.is_dir() || !canonical_dir_is_within(&run_dir, &runs_root) {
        return Err(format!(
            "staff_status: unknown dispatch_id {:?}",
            request.dispatch_id
        ));
    }
    let status_path = run_dir.join("status.json");
    let Some(status) = crate::task_lifecycle::read_json_file(&status_path)
        .map_err(|err| format!("staff_status: read {}: {err}", status_path.display()))?
    else {
        return Err(format!(
            "staff_status: unknown dispatch_id {:?}",
            request.dispatch_id
        ));
    };
    serde_json::to_string_pretty(&status)
        .map_err(|err| format!("staff_status: serialize receipt: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Discrimination test: a `StaffStartRequest` JSON that OMITS
    /// `staffing_reason` is REJECTED at deserialization — the field is
    /// required (no `#[serde(default)]`). This is the facade-level admission
    /// gate. A request missing the reason cannot reach `staff_start`, so no
    /// worker is launched, no run directory is created.
    ///
    /// RED-before-fix proof: before `staffing_reason` was added as a required
    /// field, this deserialization SUCCEEDED (all fields were optional and the
    /// reason was a free-string marker that into_params dropped). After the
    /// fix, it fails — the gate is structural.
    #[test]
    fn staff_start_request_without_reason_is_rejected_at_deserialize() {
        let raw = serde_json::json!({
            "task": "launch a worker with no stated reason",
            "worker": "claude",
        });
        let err = serde_json::from_value::<StaffStartRequest>(raw)
            .expect_err("missing staffing_reason must be rejected at deserialization");
        let msg = err.to_string();
        assert!(
            msg.contains("staffing_reason") || msg.contains("missing field"),
            "error must name the missing staffing_reason field: {msg}"
        );
    }

    /// Discrimination test: `staffing_reason` is a CLOSED typed contract, not a
    /// free-form string. An unknown variant is rejected at deserialization;
    /// an allowlisted reason is admitted. Mirrors the existing
    /// `dispatch_reason_is_allowlisted_not_free_form` test for tachi_task.
    #[test]
    fn staffing_reason_is_typed_not_free_form() {
        let admitted: StaffStartRequest = serde_json::from_value(serde_json::json!({
            "task": "prove typed reason",
            "staffing_reason": "durable_cross_session",
        }))
        .expect("allowlisted reason deserializes");
        assert_eq!(
            admitted.staffing_reason,
            TachiDispatchReason::DurableCrossSession
        );

        let err = serde_json::from_value::<StaffStartRequest>(serde_json::json!({
            "task": "prove free-form rejected",
            "staffing_reason": "want_parallelism",
        }))
        .expect_err("free-form reasons must fail schema deserialization");
        assert!(
            err.to_string().contains("unknown variant"),
            "free-form reason must be rejected as unknown variant: {err}"
        );
    }

    /// Discrimination test: `into_params()` carries `staffing_reason` THROUGH
    /// to `TachiDispatchParams.staffing_reason` (it is NOT dropped like the
    /// retired free-string markers were). This is the contract that makes the
    /// reason reach the kernel gate and the receipt stamp.
    #[test]
    fn into_params_carries_staffing_reason_through() {
        let request = StaffStartRequest {
            task: "prove reason is carried through".to_string(),
            staffing_reason: TachiDispatchReason::CrossDeviceRemote,
            profile: Some("codex_55_review".to_string()),
            worker: Some("codex".to_string()),
            project: Some("tachi".to_string()),
            stage: Some("execute".to_string()),
            issue_ref: Some("o/r#42".to_string()),
            pr_ref: Some("o/r#43".to_string()),
            flow_id: Some("flow_xyz".to_string()),
        };
        let params = request.into_params();
        assert_eq!(
            params.staffing_reason,
            TachiDispatchReason::CrossDeviceRemote,
            "into_params must carry staffing_reason through to the kernel, not drop it"
        );
        // Semantic fields still map through.
        assert_eq!(params.task, "prove reason is carried through");
        assert_eq!(params.agent.as_deref(), Some("codex"));
        assert_eq!(params.profile.as_deref(), Some("codex_55_review"));
        assert_eq!(params.project.as_deref(), Some("tachi"));
        assert_eq!(params.stage.as_deref(), Some("execute"));
        assert_eq!(params.issue_ref.as_deref(), Some("o/r#42"));
        assert_eq!(params.pr_ref.as_deref(), Some("o/r#43"));
        assert_eq!(params.flow_id.as_deref(), Some("flow_xyz"));
    }

    /// Boundary test: a `StaffStartRequest` JSON that attempts to set
    /// execution-shaped fields is silently ignored, and the resulting mapped
    /// `TachiDispatchParams` maps every execution field to its kernel-side
    /// default. The forbidden names simply don't exist on the struct.
    #[test]
    fn staff_start_request_has_no_execution_fields() {
        let raw = serde_json::json!({
            "task": "prove the boundary",
            "staffing_reason": "native_subagent_unavailable",
            "worker": "claude",
            "profile": "codex_55_review",
            "project": "tachi",
            "stage": "execute",
            "flow_id": "flow-123",
            // ── hostile / out-of-boundary fields: must be ignored ──────────
            "cwd": "/evil/absolute/path",
            "command": ["rm", "-rf", "/"],
            "transport": "acpx",
            "harness_transport": "acpx",
            "credentials": ["superuser"],
            "credential_profiles": ["superuser"],
            "sandbox": "danger-full-access",
            "allowed_tools": ["Bash(rm*)"],
            "allowed_mcp_servers": ["evil-mcp"],
            "inject_tachi_mcp": true,
            "permission_profile": "full",
            "env_id": "lease-evil",
            "unmanaged_cwd": true,
        });
        let request: StaffStartRequest = serde_json::from_value(raw).expect("parses");
        let params = request.into_params();

        // Intent fields + reason DO map through.
        assert_eq!(params.task, "prove the boundary");
        assert_eq!(params.agent.as_deref(), Some("claude"));
        assert_eq!(params.profile.as_deref(), Some("codex_55_review"));
        assert_eq!(params.project.as_deref(), Some("tachi"));
        assert_eq!(params.stage.as_deref(), Some("execute"));
        assert_eq!(params.flow_id.as_deref(), Some("flow-123"));
        assert_eq!(
            params.staffing_reason,
            TachiDispatchReason::NativeSubagentUnavailable
        );

        // ── Execution fields MUST all be at kernel defaults ─────────────────
        assert_eq!(params.cwd, None, "Staff must never set cwd");
        assert_eq!(params.command, Vec::<String>::new(), "no command smuggle");
        assert_eq!(params.harness_transport, None, "no transport override");
        assert_eq!(params.sandbox, None, "no sandbox smuggle");
        assert_eq!(params.allowed_tools, Vec::<String>::new());
        assert_eq!(params.credential_profiles, Vec::<String>::new());
        assert_eq!(params.allowed_mcp_servers, Vec::<String>::new());
        assert_eq!(params.inject_tachi_mcp, None);
        assert_eq!(params.permission_profile, None);
        assert_eq!(params.env_id, None);
        assert_eq!(params.unmanaged_cwd, None);
        assert_eq!(params.timeout_secs, DEFAULT_STAFF_DISPATCH_TIMEOUT_SECS);
    }

    /// Discrimination test: the v1 schema does NOT expose `dispatch_id` on a
    /// START request — it is minted by the kernel, not the caller.
    #[test]
    fn staff_start_request_does_not_accept_dispatch_id() {
        let raw = serde_json::json!({
            "task": "prove no dispatch_id on start",
            "staffing_reason": "explicit_user_request",
            "dispatch_id": "caller-forged-id",
        });
        let request: StaffStartRequest = serde_json::from_value(raw).expect("parses");
        assert_eq!(request.task, "prove no dispatch_id on start");
    }

    /// `staff_status` reads ONLY the canonical receipt. Seeds a canonical
    /// `status.json` at a known dispatch_id under the canonical runs root,
    /// calls the status core, asserts it returns the canonical content verbatim
    /// AND creates no new file (no parallel store).
    #[tokio::test]
    async fn staff_status_reads_canonical_receipt_only() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp home");
        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());

        let dispatch_id = "20260803T101010Z-claude-deadbeef";
        let runs_root = dispatch_runs_root();
        let run_dir = runs_root.join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("create canonical run dir");
        let canonical_receipt = serde_json::json!({
            "dispatch_id": dispatch_id,
            "state": "TASK_STATE_WORKING",
            "agent": "claude",
            "task": "prove staff_status reads canonical receipt",
            "run_dir": run_dir.to_string_lossy(),
            "custom_receipt_field": "must round-trip verbatim",
        });
        std::fs::write(run_dir.join("status.json"), canonical_receipt.to_string())
            .expect("seed canonical status.json");

        let before: Vec<std::path::PathBuf> = std::fs::read_dir(&run_dir)
            .expect("read run dir before")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();

        let body = staff_status_impl(&StaffStatusRequest {
            dispatch_id: dispatch_id.to_string(),
        })
        .await
        .expect("staff_status returns canonical receipt");

        let returned: serde_json::Value = serde_json::from_str(&body).expect("returned JSON");
        assert_eq!(returned, canonical_receipt, "verbatim canonical receipt");

        let after: Vec<std::path::PathBuf> = std::fs::read_dir(&run_dir)
            .expect("read run dir after")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        assert_eq!(before, after, "staff_status must be read-only");
    }

    /// `staff_status` returns `Err` for an unknown / malformed dispatch_id
    /// (fail-closed, uniform error — no oracle for a prober).
    #[tokio::test]
    async fn staff_status_rejects_unknown_dispatch_id() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp home");
        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());

        let err = staff_status_impl(&StaffStatusRequest {
            dispatch_id: "20260803T999999Z-nobody-00000000".to_string(),
        })
        .await
        .expect_err("absent dispatch_id must Err");
        assert!(err.contains("unknown dispatch_id"), "{err}");

        for malicious in ["../decoy", "..", "/etc/passwd", "a/../../decoy", "a/b"] {
            let err = staff_status_impl(&StaffStatusRequest {
                dispatch_id: malicious.to_string(),
            })
            .await
            .expect_err("malformed dispatch_id must Err fail-closed");
            assert!(
                err.contains("unknown dispatch_id"),
                "malicious id {malicious:?} rejected as unknown: {err}"
            );
        }
    }
}
