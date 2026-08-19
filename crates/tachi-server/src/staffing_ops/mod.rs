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

use crate::dispatch_ops::{
    canonical_dir_is_within, dispatch_runs_root, handle_tachi_dispatch, is_valid_dispatch_id,
};
use crate::MemoryServer;
use rmcp::schemars::JsonSchema;
// The `#[derive(JsonSchema)]` macro expands to reference `schemars::...`, so
// the crate must be in scope under that name.
use rmcp::schemars;
use serde_json::Value;

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
pub(crate) type StaffStartRequest = tachi_params::StaffAssignmentRequest;

/// Read-only status probe. Only the canonical `dispatch_id` is accepted —
/// there is no Staff-local id namespace.
#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
pub(crate) struct StaffStatusRequest {
    pub dispatch_id: String,
}

/// Start a worker via the canonical dispatch kernel.
///
/// Maps the semantic [`StaffAssignmentRequest`] into [`TachiDispatchParams`] and
/// delegates to the single canonical choke-point
/// [`handle_tachi_dispatch`]. The `staffing_reason` field is required on the
/// request struct, so a caller that omits it is rejected at deserialization
/// (the field has no `#[serde(default)]`). This is the facade-level admission
/// gate; the kernel additionally fail-closes inside `handle_tachi_dispatch`
/// before any run artifact is created. That entry-point seeds canonical
/// `status.json` (receipt-first) BEFORE prompt assembly / plan stage / spawn,
/// so a successful `Ok` return guarantees the canonical receipt already exists
/// on disk. This adapter creates NO parallel store.
///
/// tachi#1675 PR1 Seam B: on a successful acceptance, records a
/// `route_decisions` row (idempotent on `dispatch_id`). This is an
/// ACKNOWLEDGED DUAL WRITE, not a transaction with `status.json` — the
/// canonical receipt is already durable (written inside
/// `handle_tachi_dispatch`, which only returns `Ok` after that) by the time
/// this insert runs; a crash between the two leaves no `route_decisions` row,
/// and the honest floor for a missing row is `assignment_mode = 'unadvised'`
/// at the projection layer — this function never fabricates one. Best-effort:
/// a failure recording the decision row does NOT fail the dispatch itself
/// (the worker is already running) — it is logged and swallowed, mirroring
/// `claims_ops::auto_register_or_heartbeat_claim`'s "never fails the primary
/// action" posture for parallel ledger writes.
pub(crate) async fn staff_start(
    server: &MemoryServer,
    request: StaffStartRequest,
) -> Result<String, String> {
    // Facade-level admission: `staffing_reason` is a non-optional field, so a
    // missing reason is rejected by serde before this fn runs. No additional
    // runtime check is needed here — the struct's type IS the gate. The
    // kernel-side defense-in-depth check inside handle_tachi_dispatch catches
    // any future caller that reaches it without going through this struct.
    let recommendation_ref = request.recommendation_ref.clone();
    let params = request.into_dispatch_params();
    let raw = handle_tachi_dispatch(server, params).await?;

    record_route_decision_best_effort(server, &raw, recommendation_ref.as_deref());

    Ok(raw)
}

/// tachi#1675 PR1 Seam B implementation. Parses the dispatch response for
/// `dispatch_id`/`selected_profile`, re-reads the just-written canonical
/// `status.json` for `env_id`/`host_profile`/`authority`/`identity_receipt`
/// (all stamped there by `handle_tachi_dispatch` before it returned), and
/// inserts the acceptance-moment `route_decisions` row. Every failure mode
/// here (malformed response, missing receipt, DB error) is swallowed after a
/// trace log — recording routing evidence must never retroactively fail an
/// already-accepted, already-running dispatch.
fn record_route_decision_best_effort(
    server: &MemoryServer,
    raw_response: &str,
    recommendation_ref: Option<&str>,
) {
    let response: serde_json::Value = match serde_json::from_str(raw_response) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(error = %err, "tachi#1675 Seam B: dispatch response not JSON, skipping route_decisions write");
            return;
        }
    };
    let Some(dispatch_id) = response.get("dispatch_id").and_then(|v| v.as_str()) else {
        tracing::warn!("tachi#1675 Seam B: dispatch response missing dispatch_id, skipping route_decisions write");
        return;
    };
    let selected_profile = response
        .get("selected_profile")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let run_dir = dispatch_runs_root().join(dispatch_id);
    let status = match crate::task_lifecycle::read_json_file(&run_dir.join("status.json")) {
        Ok(Some(v)) => v,
        Ok(None) => {
            tracing::warn!(
                dispatch_id,
                "tachi#1675 Seam B: status.json missing, skipping route_decisions write"
            );
            return;
        }
        Err(err) => {
            tracing::warn!(dispatch_id, error = %err, "tachi#1675 Seam B: failed reading status.json, skipping route_decisions write");
            return;
        }
    };
    let env_id = status
        .get("env_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let host_profile = status
        .get("host_profile")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let selected_model = status
        .pointer("/identity_receipt/planned/model")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let contract_hash = status
        .get("authority")
        .map(crate::tune_ops::route_policy::content_digest_hex);

    // tachi#1675 BUG-10: `assignment_mode` and `recommendation_id` must
    // reflect a recommendation the DB actually resolved, never the mere
    // presence of a caller-supplied ref. A caller can pass any string
    // (stale, typo'd, forged) as `recommendation_ref`; recording 'advised'
    // for a ref that doesn't resolve would be the ledger fabricating advice
    // that was never given. `resolved_recommendation` is the ONE lookup
    // whose outcome both `assignment_mode` and `override_flag` are derived
    // from — a miss (not found OR a query error) degrades to the honest
    // 'unadvised' floor with `recommendation_id` stored NULL, exactly the
    // same shape as no ref ever being supplied.
    let resolved_recommendation = recommendation_ref.and_then(|rec_id| {
        match server.with_global_store_read(|store| {
            memcore::get_route_recommendation(store.connection(), rec_id)
                .map_err(|e| e.to_string())
        }) {
            Ok(Some(row)) => Some(row),
            Ok(None) => {
                tracing::warn!(
                    dispatch_id,
                    recommendation_ref = rec_id,
                    "tachi#1675 Seam B: recommendation_ref does not resolve to a route_recommendations \
                     row; recording assignment_mode='unadvised' rather than fabricating advice"
                );
                None
            }
            Err(err) => {
                tracing::warn!(
                    dispatch_id,
                    recommendation_ref = rec_id,
                    error = %err,
                    "tachi#1675 Seam B: recommendation lookup failed; recording assignment_mode='unadvised' \
                     rather than fabricating advice"
                );
                None
            }
        }
    });
    let assignment_mode = if resolved_recommendation.is_some() {
        "advised"
    } else {
        "unadvised"
    };
    let override_flag = resolved_recommendation
        .as_ref()
        .map(|row| row.recommended_profile != selected_profile)
        .unwrap_or(false);
    let recommendation_id = resolved_recommendation.map(|row| row.recommendation_id);

    let route_decision_id = uuid::Uuid::new_v4().to_string();
    let new_decision = memcore::NewRouteDecision {
        route_decision_id: route_decision_id.clone(),
        dispatch_id: dispatch_id.to_string(),
        recommendation_id,
        selected_profile,
        selected_model,
        assignment_mode: assignment_mode.to_string(),
        override_flag,
        contract_hash,
        env_id,
        host_profile,
        // #1239 not yet wired — nullable pending v21 claim identity (spec
        // correction 1).
        work_claim_id: None,
        occurred_at: memcore::now_utc_iso(),
    };
    let inserted = server.with_global_store(|store| {
        memcore::insert_route_decision_idempotent(store.connection(), &new_decision)
            .map_err(|e| e.to_string())
    });
    let inserted = match inserted {
        Ok(row) => row,
        Err(err) => {
            tracing::warn!(dispatch_id, error = %err, "tachi#1675 Seam B: failed to record route_decisions row");
            return;
        }
    };

    // Stamp the (possibly pre-existing, on an idempotent replay)
    // `route_decision_id` back into status.json — convenience only, the DB
    // row above is the queryable authority. This is a ONE-TIME explicit
    // patch, not a `write_status_json` call: no later writer may emit this
    // key at all (see the `write_status_json` preserve-list in
    // `dispatch_ops::dispatch_v2`, which carries it forward automatically
    // once present — emitting `route_decision_id: null` there would erase
    // it).
    if let Ok(Value::Object(mut obj)) =
        crate::task_lifecycle::read_json_file(&run_dir.join("status.json"))
            .map(|v| v.unwrap_or(Value::Null))
    {
        obj.insert(
            "route_decision_id".to_string(),
            Value::String(inserted.route_decision_id),
        );
        let body = serde_json::to_string_pretty(&Value::Object(obj)).unwrap_or_default();
        if !body.is_empty() {
            if let Err(err) = crate::utils::write_owner_only_file_atomic(
                &run_dir.join("status.json"),
                body.as_bytes(),
            ) {
                tracing::warn!(dispatch_id, error = %err, "tachi#1675 Seam B: failed to stamp route_decision_id into status.json");
            }
        }
    }
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
    use crate::tool_params::TachiDispatchReason;

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
            execution_level: None,
            issue_ref: Some("o/r#42".to_string()),
            pr_ref: Some("o/r#43".to_string()),
            flow_id: Some("flow_xyz".to_string()),
            completion_predicate: None,
            recommendation_ref: Some("rec-xyz".to_string()),
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
        assert_eq!(params.timeout_secs, 600);
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
    #[allow(clippy::await_holding_lock)]
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
    #[allow(clippy::await_holding_lock)]
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

    // ─── tachi#1675 PR1 Seam B: record_route_decision_best_effort ──────────

    fn test_server() -> MemoryServer {
        let db_path = crate::utils::test_fixture_path(format!(
            "staffing-seam-b-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        MemoryServer::new(db_path, None).expect("test memory server")
    }

    /// Seed a fake `<runs_root>/<dispatch_id>/status.json` the same shape
    /// `dispatch_ops::dispatch::write_status_json` produces — this test
    /// module intentionally drives `record_route_decision_best_effort`
    /// directly rather than a full `staff_start()` (which spawns a real
    /// execution backend) so it stays a fast, hermetic unit test.
    fn seed_status_json(dispatch_id: &str, env_id: &str, host_profile: &str, model: &str) {
        let run_dir = dispatch_runs_root().join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("create run dir");
        let status = serde_json::json!({
            "dispatch_id": dispatch_id,
            "env_id": env_id,
            "host_profile": host_profile,
            "authority": {"level": "workspace-write", "enforced_by": "codex-cli"},
            "identity_receipt": {"planned": {"model": model}},
        });
        std::fs::write(run_dir.join("status.json"), status.to_string()).expect("seed status.json");
    }

    fn fake_raw_response(dispatch_id: &str, selected_profile: &str) -> String {
        serde_json::json!({
            "dispatch_id": dispatch_id,
            "selected_profile": selected_profile,
        })
        .to_string()
    }

    fn route_decisions_count(server: &MemoryServer) -> i64 {
        server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row("SELECT COUNT(*) FROM route_decisions", [], |r| r.get(0))
                    .map_err(|e| e.to_string())
            })
            .unwrap()
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn record_route_decision_writes_unadvised_row_with_env_and_host_profile() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp home");
        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
        let server = test_server();

        let dispatch_id = "20260810T000001Z-claude-aaaaaaaa";
        seed_status_json(dispatch_id, "env-42", "dev", "claude-sonnet-5");
        let raw = fake_raw_response(dispatch_id, "wizard_sonnet");

        record_route_decision_best_effort(&server, &raw, None);

        let row = server
            .with_global_store_read(|store| {
                memcore::get_route_decision_by_dispatch_id(store.connection(), dispatch_id)
                    .map_err(|e| e.to_string())
            })
            .unwrap()
            .expect("route_decisions row present");
        assert_eq!(row.assignment_mode, "unadvised");
        assert_eq!(row.env_id.as_deref(), Some("env-42"));
        assert_eq!(row.host_profile.as_deref(), Some("dev"));
        assert_eq!(row.selected_profile.as_deref(), Some("wizard_sonnet"));
        assert_eq!(row.selected_model.as_deref(), Some("claude-sonnet-5"));
        assert!(row.contract_hash.is_some());
        assert!(!row.override_flag);
        assert!(row.recommendation_id.is_none());

        // route_decision_id round-trips back into status.json.
        let status_path = dispatch_runs_root().join(dispatch_id).join("status.json");
        let status: Value =
            serde_json::from_str(&std::fs::read_to_string(&status_path).unwrap()).unwrap();
        assert_eq!(
            status["route_decision_id"].as_str(),
            Some(row.route_decision_id.as_str())
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn record_route_decision_replay_is_zero_write_idempotent() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp home");
        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
        let server = test_server();

        let dispatch_id = "20260810T000002Z-claude-bbbbbbbb";
        seed_status_json(dispatch_id, "env-1", "dev", "claude-sonnet-5");
        let raw = fake_raw_response(dispatch_id, "wizard_sonnet");

        record_route_decision_best_effort(&server, &raw, None);
        let before = route_decisions_count(&server);
        record_route_decision_best_effort(&server, &raw, None);
        let after = route_decisions_count(&server);

        assert_eq!(before, 1);
        assert_eq!(after, 1, "replayed acceptance is a zero-write no-op");
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn record_route_decision_advised_when_recommendation_ref_present() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp home");
        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
        let server = test_server();

        // Seed a recommendation row whose recommended_profile MATCHES the
        // eventual selection -> override_flag must be false.
        let recommendation_id = "rec-match".to_string();
        server
            .with_global_store(|store| {
                memcore::insert_route_recommendation(
                    store.connection(),
                    &memcore::NewRouteRecommendation {
                        recommendation_id: recommendation_id.clone(),
                        task_type: Some("fix_request".to_string()),
                        risk: "low".to_string(),
                        candidates: serde_json::json!([{"profile": "wizard_sonnet"}]),
                        recommended_profile: Some("wizard_sonnet".to_string()),
                        policy_source_revision: Some("rev-1".to_string()),
                        rows_considered: 3,
                        occurred_at: memcore::now_utc_iso(),
                    },
                )
                .map_err(|e| e.to_string())
            })
            .unwrap();

        let dispatch_id = "20260810T000003Z-claude-cccccccc";
        seed_status_json(dispatch_id, "env-1", "dev", "claude-sonnet-5");
        let raw = fake_raw_response(dispatch_id, "wizard_sonnet");

        record_route_decision_best_effort(&server, &raw, Some(&recommendation_id));

        let row = server
            .with_global_store_read(|store| {
                memcore::get_route_decision_by_dispatch_id(store.connection(), dispatch_id)
                    .map_err(|e| e.to_string())
            })
            .unwrap()
            .unwrap();
        assert_eq!(row.assignment_mode, "advised");
        assert_eq!(row.recommendation_id.as_deref(), Some("rec-match"));
        assert!(
            !row.override_flag,
            "selected_profile matches the recommendation -> no override"
        );
    }

    /// BUG-10: a `recommendation_ref` that does NOT resolve to any
    /// `route_recommendations` row (stale, typo'd, forged — no row is ever
    /// seeded here) must NOT be recorded as 'advised'. The ledger must never
    /// fabricate advice that was never actually given: the honest floor for
    /// an unresolved ref is identical to no ref at all — 'unadvised' with
    /// `recommendation_id` stored NULL.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn record_route_decision_unadvised_when_recommendation_ref_does_not_resolve() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp home");
        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
        let server = test_server();

        // Deliberately NO route_recommendations row seeded for this id.
        let dispatch_id = "20260810T000006Z-claude-ffffffff";
        seed_status_json(dispatch_id, "env-1", "dev", "claude-sonnet-5");
        let raw = fake_raw_response(dispatch_id, "wizard_sonnet");

        record_route_decision_best_effort(&server, &raw, Some("rec-does-not-exist"));

        let row = server
            .with_global_store_read(|store| {
                memcore::get_route_decision_by_dispatch_id(store.connection(), dispatch_id)
                    .map_err(|e| e.to_string())
            })
            .unwrap()
            .expect("route_decisions row still lands even when the ref is unresolved");
        assert_eq!(
            row.assignment_mode, "unadvised",
            "an unresolved recommendation_ref must never be recorded as advised"
        );
        assert!(
            row.recommendation_id.is_none(),
            "recommendation_id must be NULL, not the unresolved ref string"
        );
        assert!(!row.override_flag);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn record_route_decision_override_flag_when_selection_diverges_from_recommendation() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp home");
        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
        let server = test_server();

        let recommendation_id = "rec-diverge".to_string();
        server
            .with_global_store(|store| {
                memcore::insert_route_recommendation(
                    store.connection(),
                    &memcore::NewRouteRecommendation {
                        recommendation_id: recommendation_id.clone(),
                        task_type: Some("fix_request".to_string()),
                        risk: "low".to_string(),
                        candidates: serde_json::json!([{"profile": "codex_55_review"}]),
                        recommended_profile: Some("codex_55_review".to_string()),
                        policy_source_revision: Some("rev-1".to_string()),
                        rows_considered: 3,
                        occurred_at: memcore::now_utc_iso(),
                    },
                )
                .map_err(|e| e.to_string())
            })
            .unwrap();

        let dispatch_id = "20260810T000004Z-claude-dddddddd";
        seed_status_json(dispatch_id, "env-1", "dev", "claude-sonnet-5");
        // Caller actually got routed to a DIFFERENT profile than advised.
        let raw = fake_raw_response(dispatch_id, "wizard_sonnet");

        record_route_decision_best_effort(&server, &raw, Some(&recommendation_id));

        let row = server
            .with_global_store_read(|store| {
                memcore::get_route_decision_by_dispatch_id(store.connection(), dispatch_id)
                    .map_err(|e| e.to_string())
            })
            .unwrap()
            .unwrap();
        assert!(
            row.override_flag,
            "selected profile diverges from the recommendation's advice"
        );
    }

    /// Negative test: the Seam B write path never touches session_claims or
    /// agent_identities.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn record_route_decision_never_touches_session_or_identity_tables() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp home");
        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
        let server = test_server();

        let count_of = |table: &str| -> i64 {
            server
                .with_global_store_read(|store| {
                    store
                        .connection()
                        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                        .map_err(|e| e.to_string())
                })
                .unwrap()
        };
        let before_claims = count_of("session_claims");
        let before_identities = count_of("agent_identities");

        let dispatch_id = "20260810T000005Z-claude-eeeeeeee";
        seed_status_json(dispatch_id, "env-1", "dev", "claude-sonnet-5");
        let raw = fake_raw_response(dispatch_id, "wizard_sonnet");
        record_route_decision_best_effort(&server, &raw, None);

        assert_eq!(before_claims, count_of("session_claims"));
        assert_eq!(before_identities, count_of("agent_identities"));
    }
}
