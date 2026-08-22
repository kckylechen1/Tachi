//! Internal Staff start/status contract — a thin semantic adapter onto the
//! canonical dispatch kernel.
//!
//! This is NOT a public MCP tool, NOT a new facade, and NOT a parallel
//! lifecycle. It maps a small model-decidable request into
//! a resolved typed assignment and enters the single canonical launch kernel;
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
//! `transport`, `credentials`, `sandbox`, or `allowed_tools`. Both
//! [`StaffStartRequest`] and [`tachi_params::TachiStaffParams`] enforce
//! `#[serde(deny_unknown_fields)]`: a request JSON that attempts to set any
//! hostile execution field fails deserialization loudly before reaching any
//! handler or creating any artifacts.

use crate::dispatch_ops::{
    canonical_dir_is_within, dispatch_runs_root, is_valid_dispatch_id, launch_staff_assignment,
};
use crate::MemoryServer;
use rmcp::schemars::JsonSchema;
// The `#[derive(JsonSchema)]` macro expands to reference `schemars::...`, so
// the crate must be in scope under that name.
use rmcp::schemars;

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
/// `#[serde(deny_unknown_fields)]` ensures that an inbound JSON carrying
/// `"cwd": "/evil"` or `"command": ["rm", "-rf"]` is rejected loudly at
/// deserialization — those names cannot leak into the mapped params.
pub(crate) type StaffStartRequest = tachi_params::StaffAssignmentRequest;

/// Read-only status probe. Only the canonical `dispatch_id` is accepted —
/// there is no Staff-local id namespace.
#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
pub(crate) struct StaffStatusRequest {
    pub dispatch_id: String,
}

#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
pub(crate) struct StaffCancelRequest {
    pub dispatch_id: String,
    pub expected_status_revision: u64,
}

pub(crate) async fn staff_cancel(
    server: &MemoryServer,
    request: StaffCancelRequest,
) -> Result<String, String> {
    crate::managed_run_control::request_managed_custom_cancel(
        server,
        &request.dispatch_id,
        request.expected_status_revision,
    )
    .await
}

/// Start a worker via the canonical dispatch kernel.
///
/// Resolves the semantic [`StaffAssignmentRequest`] and enters the single
/// canonical launch kernel. The `staffing_reason` field is required on the
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
    let (raw, _assignment, recommendation) = launch_staff_assignment(server, request).await?;

    record_route_decision_best_effort(server, &raw, recommendation.as_ref());

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
    resolved_recommendation: Option<&memcore::RouteRecommendationRow>,
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

    // Typed resolution validates this fact before acceptance and carries the
    // exact row here. Do not re-query a caller spelling after acceptance: a
    // delete or read failure in that interval cannot turn accepted advice into
    // an unadvised ledger row.
    let assignment_mode = if resolved_recommendation.is_some() {
        "advised"
    } else {
        "unadvised"
    };
    let override_flag = resolved_recommendation
        .as_ref()
        .map(|row| row.recommended_profile != selected_profile)
        .unwrap_or(false);
    let recommendation_id = resolved_recommendation.map(|row| row.recommendation_id.clone());

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

    // The shared status lock serializes this read/merge/write with terminal
    // status writers, so a fast child cannot be replaced by an older working
    // snapshot while evidence is stamped.
    if let Err(err) =
        crate::dispatch_ops::stamp_route_decision_id(&run_dir, &inserted.route_decision_id)
    {
        tracing::warn!(dispatch_id, error = %err, "tachi#1675 Seam B: failed to stamp route_decision_id into status.json");
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
pub(crate) mod tests {
    use super::*;
    use crate::tool_params::TachiDispatchReason;
    use serde_json::Value;

    fn write_fake_worker(bin_dir: &std::path::Path, exit_code: i32) {
        let worker = bin_dir.join("codex");
        std::fs::write(
            &worker,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  printf 'codex-cli 0.144.1\\n'\n  exit 0\nfi\nprintf 'staff fake worker\\n'\nexit {exit_code}\n"
            ),
        )
        .expect("write fake codex worker");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&worker)
                .expect("fake worker metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&worker, permissions).expect("make fake worker executable");
        }
    }

    #[cfg(unix)]
    fn write_hanging_managed_custom_worker(
        bin_dir: &std::path::Path,
        root_pid: &std::path::Path,
        descendant_pid: &std::path::Path,
    ) {
        let worker = bin_dir.join("opencode");
        std::fs::write(
            &worker,
            format!(
                "#!/bin/sh\ntrap '' TERM\nprintf '%s\\n' \"$$\" > '{}'\nsh -c 'trap \"\" TERM; sleep 60' &\nprintf '%s\\n' \"$!\" > '{}'\nwait\n",
                root_pid.display(),
                descendant_pid.display(),
            ),
        )
        .expect("write hanging managed custom worker");
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&worker)
            .expect("managed custom worker metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&worker, permissions)
            .expect("make managed custom worker executable");
    }

    #[cfg(unix)]
    async fn wait_for_managed_custom_processes(
        run_dir: &std::path::Path,
        root_pid: &std::path::Path,
        descendant_pid: &std::path::Path,
    ) {
        for _ in 0..360 {
            if root_pid.is_file() && descendant_pid.is_file() {
                return;
            }
            if let Some(status) = std::fs::read_to_string(run_dir.join("status.json"))
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            {
                if terminal_staff_state(&status) != "TASK_STATE_WORKING" {
                    let result = std::fs::read_to_string(run_dir.join("result.md"))
                        .unwrap_or_else(|error| format!("<result unavailable: {error}>"));
                    panic!(
                        "managed custom launch terminalized before its root fixture: status={status} result={result}"
                    );
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!(
            "managed custom fixture did not write root={} descendant={}",
            root_pid.display(),
            descendant_pid.display()
        );
    }

    #[cfg(unix)]
    async fn wait_for_test_process_exit(pid: libc::pid_t) -> bool {
        for _ in 0..360 {
            let absent = unsafe { libc::kill(pid, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if absent {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        false
    }

    async fn wait_for_staff_terminal(run_dir: &std::path::Path) -> (Value, String) {
        for _ in 0..360 {
            let status = std::fs::read_to_string(run_dir.join("status.json"))
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
            let result = std::fs::read_to_string(run_dir.join("result.md")).ok();
            if let (Some(status), Some(result)) = (status, result) {
                if status
                    .get("status")
                    .or_else(|| status.get("state"))
                    .or_else(|| status.get("task_state"))
                    .and_then(Value::as_str)
                    .is_some_and(|state| {
                        matches!(
                            state,
                            "completed"
                                | "failed"
                                | "timed_out"
                                | "cancelled"
                                | "TASK_STATE_COMPLETED"
                                | "TASK_STATE_FAILED"
                                | "TASK_STATE_CANCELED"
                        )
                    })
                {
                    return (status, result);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!(
            "staff launch did not reach a terminal canonical receipt: {}",
            run_dir.display()
        );
    }

    async fn wait_for_staff_cleanup(dispatch_id: &str) {
        for _ in 0..360 {
            if crate::dispatch_ops::background_dispatch_cleanup_complete(dispatch_id) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("Staff background cleanup did not finish for {dispatch_id}");
    }

    /// Keep the process-global fake-worker environment alive if assertions
    /// panic after acceptance. The E2Es use a two-thread runtime, so this
    /// bounded blocking Drop wait cannot starve the background dispatch.
    struct StaffCleanupGuard {
        accepted_response: String,
        armed: bool,
    }

    impl StaffCleanupGuard {
        fn arm(accepted_response: &str) -> Self {
            Self {
                accepted_response: accepted_response.to_string(),
                armed: true,
            }
        }

        fn disarm(&mut self) {
            self.armed = false;
        }
    }

    impl Drop for StaffCleanupGuard {
        fn drop(&mut self) {
            if !self.armed {
                return;
            }
            let dispatch_id = serde_json::from_str::<Value>(&self.accepted_response)
                .ok()
                .and_then(|response| response["dispatch_id"].as_str().map(str::to_string));
            let Some(dispatch_id) = dispatch_id else {
                return;
            };
            for _ in 0..360 {
                if crate::dispatch_ops::background_dispatch_cleanup_complete(&dispatch_id) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            eprintln!("Staff cleanup guard timed out for {dispatch_id}");
        }
    }

    fn terminal_staff_state(status: &Value) -> &str {
        status
            .get("status")
            .or_else(|| status.get("state"))
            .or_else(|| status.get("task_state"))
            .and_then(Value::as_str)
            .expect("terminal canonical receipt state")
    }

    fn staff_request(project: &str) -> StaffStartRequest {
        StaffStartRequest {
            task: "prove the canonical Staff launch lifecycle".to_string(),
            staffing_reason: TachiDispatchReason::DurableCrossSession,
            profile: Some("codex_55_review".to_string()),
            worker: Some("codex".to_string()),
            project: Some(project.to_string()),
            stage: None,
            execution_level: None,
            issue_ref: Some("kckylechen1/tachi#1814".to_string()),
            pr_ref: None,
            flow_id: Some("flow_1814_staff_e2e".to_string()),
            completion_predicate: None,
            recommendation_ref: None,
        }
    }

    /// End-to-end discriminator for #1814: Staff must reach the one canonical
    /// background launcher, rather than returning a pending-only receipt.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)] // serializes process-global fake-worker environment through terminal cleanup
    async fn staff_start_launches_fake_worker_through_canonical_receipt_lifecycle() {
        let _environment = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp tachi home");
        let temp_runs = tempfile::tempdir().expect("temp canonical run root");
        let temp_bin = tempfile::tempdir().expect("temp fake worker bin");
        write_fake_worker(temp_bin.path(), 0);
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let joined_path = std::env::join_paths(
            std::iter::once(temp_bin.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
        )
        .expect("join fake-worker PATH");
        let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
        let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", temp_runs.path());
        let _path = crate::test_support::EnvRestore::set_os("PATH", &joined_path);
        let server = test_server();
        let recommendation = server
            .with_global_store(|store| {
                memcore::insert_route_recommendation(
                    store.connection(),
                    &memcore::NewRouteRecommendation {
                        recommendation_id: "rec-staff-e2e".to_string(),
                        task_type: Some("implementation".to_string()),
                        risk: "low".to_string(),
                        candidates: serde_json::json!([{"profile": "codex_55_review"}]),
                        recommended_profile: Some("codex_55_review".to_string()),
                        policy_source_revision: Some("staff-e2e".to_string()),
                        rows_considered: 1,
                        occurred_at: memcore::now_utc_iso(),
                    },
                )
                .map_err(|error| error.to_string())
            })
            .expect("seed recommendation fact");
        let mut request = staff_request("tachi");
        request.recommendation_ref = Some(recommendation.recommendation_id.clone());

        let raw = staff_start(&server, request)
            .await
            .expect("Staff start should be accepted before background execution");
        let mut cleanup_guard = StaffCleanupGuard::arm(&raw);
        let response: Value = serde_json::from_str(&raw).expect("canonical response JSON");
        let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");
        let run_dir = dispatch_runs_root().join(dispatch_id);
        let (status, result) = wait_for_staff_terminal(&run_dir).await;
        wait_for_staff_cleanup(dispatch_id).await;
        cleanup_guard.disarm();

        assert_eq!(
            terminal_staff_state(&status),
            "TASK_STATE_COMPLETED",
            "fake worker terminal receipt"
        );
        assert_eq!(
            status["project"], "tachi",
            "Staff project linkage survives launch"
        );
        assert_eq!(
            status["result_written"], true,
            "terminal outcome retains result receipt"
        );
        assert_eq!(
            status["run_dir"],
            run_dir.to_string_lossy().as_ref(),
            "recovery retains the canonical run directory"
        );
        assert!(
            result.contains("staff fake worker"),
            "canonical result.md: {result}"
        );

        let trajectory = std::fs::read_to_string(run_dir.join("trajectory.jsonl"))
            .expect("canonical trajectory");
        let events = trajectory
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("trajectory JSON"))
            .map(|event| event["event"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>();
        let received = events
            .iter()
            .position(|event| event == "dispatch_received")
            .expect("receipt-first event");
        let started = events
            .iter()
            .position(|event| event == "execute_started")
            .expect("real execution event");
        let finished = events
            .iter()
            .position(|event| event == "subprocess_finished")
            .expect("subprocess terminal event");
        assert!(
            received < started && started < finished,
            "canonical trajectory ordering: {events:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| *event == "dispatch_received")
                .count(),
            1,
            "no second lifecycle"
        );
        let decision = server
            .with_global_store_read(|store| {
                memcore::get_route_decision_by_dispatch_id(store.connection(), dispatch_id)
                    .map_err(|error| error.to_string())
            })
            .expect("read route decision")
            .expect("exactly one route decision for accepted Staff start");
        assert_eq!(
            route_decisions_count(&server),
            1,
            "one acceptance creates one decision"
        );
        assert_eq!(decision.assignment_mode, "advised");
        assert_eq!(
            decision.recommendation_id.as_deref(),
            Some(recommendation.recommendation_id.as_str())
        );
        assert_eq!(
            status["route_decision_id"].as_str(),
            Some(decision.route_decision_id.as_str()),
            "the evidence stamp must retain the fast child's terminal receipt"
        );
    }

    /// The facade must install the in-memory cancellation owner before its
    /// accepted custom launch reaches the hanging root. This drives the real
    /// `tachi_staff start -> status -> cancel` handlers and proves confirmation
    /// follows process-group reaping, not merely a request acknowledgement.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)] // serializes process-global fake-worker environment through terminal cleanup
    async fn staff_cancel_managed_custom_start_status_cancel_e2e() {
        let _environment = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp tachi home");
        let temp_runs = tempfile::tempdir().expect("temp canonical run root");
        let temp_bin = tempfile::tempdir().expect("temp fake worker bin");
        let root_pid = temp_bin.path().join("root.pid");
        let descendant_pid = temp_bin.path().join("descendant.pid");
        write_hanging_managed_custom_worker(temp_bin.path(), &root_pid, &descendant_pid);
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let joined_path = std::env::join_paths(
            std::iter::once(temp_bin.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
        )
        .expect("join fake-worker PATH");
        let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
        let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", temp_runs.path());
        let _path = crate::test_support::EnvRestore::set_os("PATH", &joined_path);
        let _transport = crate::test_support::EnvRestore::set("TACHI_OPENCODE_TRANSPORT", "cli");
        let _v2 = crate::test_support::EnvRestore::set("DISPATCH_V2_ENABLED", "false");
        let _review = crate::test_support::EnvRestore::set("DISPATCH_V2_PLAN_REVIEW", "false");
        let server = test_server();
        let mut request = staff_request("tachi");
        request.profile = Some("glm_impl".to_string());
        request.worker = Some("custom".to_string());
        request.issue_ref = Some("kckylechen1/tachi#1825".to_string());
        request.flow_id = Some("flow_1825_managed_cancel_e2e".to_string());

        let raw = staff_start(&server, request)
            .await
            .expect("managed custom Staff start is accepted");
        let mut cleanup_guard = StaffCleanupGuard::arm(&raw);
        let accepted: Value = serde_json::from_str(&raw).expect("accepted response JSON");
        let dispatch_id = accepted["dispatch_id"].as_str().expect("dispatch id");
        let run_dir = dispatch_runs_root().join(dispatch_id);
        wait_for_managed_custom_processes(&run_dir, &root_pid, &descendant_pid).await;

        let accepted_status: Value = serde_json::from_str(
            &staff_status(
                &server,
                StaffStatusRequest {
                    dispatch_id: dispatch_id.to_string(),
                },
            )
            .await
            .expect("actual Staff status after accepted start"),
        )
        .expect("accepted canonical status JSON");
        let accepted_revision = accepted_status["status_revision"]
            .as_u64()
            .expect("accepted managed custom status revision");
        assert_eq!(accepted_status["state"], "TASK_STATE_WORKING");
        assert_eq!(
            accepted_status["execution_classification"],
            "managed_custom"
        );

        let cancel: Value = serde_json::from_str(
            &staff_cancel(
                &server,
                StaffCancelRequest {
                    dispatch_id: dispatch_id.to_string(),
                    expected_status_revision: accepted_revision,
                },
            )
            .await
            .expect("actual Staff cancel response"),
        )
        .expect("cancel response JSON");
        assert_eq!(cancel["receipt"], "cancellation_confirmed");
        assert_eq!(cancel["expected_status_revision"], accepted_revision);
        assert_eq!(cancel["termination_proof"], "unix_process_group_absent");

        let (terminal, _result) = wait_for_staff_terminal(&run_dir).await;
        wait_for_staff_cleanup(dispatch_id).await;
        cleanup_guard.disarm();
        assert_eq!(terminal_staff_state(&terminal), "TASK_STATE_CANCELED");
        assert_eq!(
            terminal["cancellation"]["receipt"],
            "cancellation_confirmed"
        );
        assert_eq!(
            terminal["cancellation"]["expected_status_revision"], accepted_revision,
            "request receipt precedes the confirmation written from the same checked revision"
        );
        assert_eq!(
            terminal["cancellation"]["observed_status_revision"],
            accepted_revision + 1,
            "the requested receipt must advance before cancellation is confirmed"
        );
        assert_eq!(
            cancel["observed_status_revision"],
            accepted_revision + 2,
            "the confirmation receipt must advance after the requested receipt"
        );
        assert!(
            terminal["status_revision"]
                .as_u64()
                .expect("terminal revision")
                >= accepted_revision + 2,
            "terminal persistence must not regress the confirmed revision"
        );
        let trajectory = std::fs::read_to_string(run_dir.join("trajectory.jsonl"))
            .expect("canonical trajectory");
        for event in [
            "dispatch_received",
            "execute_started",
            "subprocess_finished",
        ] {
            assert!(
                trajectory.contains(&format!(r#""event":"{event}""#)),
                "trajectory missing {event}: {trajectory}"
            );
        }
        let root: libc::pid_t = std::fs::read_to_string(&root_pid)
            .expect("root pid")
            .trim()
            .parse()
            .expect("numeric root pid");
        let descendant: libc::pid_t = std::fs::read_to_string(&descendant_pid)
            .expect("descendant pid")
            .trim()
            .parse()
            .expect("numeric descendant pid");
        assert!(
            wait_for_test_process_exit(root).await,
            "managed root must be absent"
        );
        assert!(
            wait_for_test_process_exit(descendant).await,
            "managed descendant must be absent"
        );
    }

    /// A child spawn failure is asynchronous: Staff receives the canonical
    /// acceptance first, then the sole run transitions to failed with its
    /// canonical result and cleanup-owned terminal receipt.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)] // serializes process-global fake-worker environment through terminal cleanup
    async fn staff_start_child_failure_is_asynchronous_and_uses_one_lifecycle() {
        let _environment = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp tachi home");
        let temp_runs = tempfile::tempdir().expect("temp canonical run root");
        let temp_bin = tempfile::tempdir().expect("temp fake worker bin");
        write_fake_worker(temp_bin.path(), 17);
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let joined_path = std::env::join_paths(
            std::iter::once(temp_bin.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
        )
        .expect("join fake-worker PATH");
        let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
        let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", temp_runs.path());
        let _path = crate::test_support::EnvRestore::set_os("PATH", &joined_path);
        let server = test_server();

        let raw = staff_start(&server, staff_request("tachi"))
            .await
            .expect("spawn failure remains asynchronously accepted");
        let mut cleanup_guard = StaffCleanupGuard::arm(&raw);
        let response: Value = serde_json::from_str(&raw).expect("canonical response JSON");
        let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");
        let run_dir = dispatch_runs_root().join(dispatch_id);
        let (status, result) = wait_for_staff_terminal(&run_dir).await;
        wait_for_staff_cleanup(dispatch_id).await;
        cleanup_guard.disarm();

        assert_eq!(
            terminal_staff_state(&status),
            "TASK_STATE_FAILED",
            "failed child updates canonical receipt"
        );
        assert!(
            result.contains("staff fake worker"),
            "failed canonical result.md: {result}"
        );
        let trajectory = std::fs::read_to_string(run_dir.join("trajectory.jsonl"))
            .expect("canonical trajectory");
        assert_eq!(
            trajectory
                .matches("\"event\":\"dispatch_received\"")
                .count(),
            1,
            "no second receipt lifecycle"
        );
        assert_eq!(
            trajectory
                .matches("\"event\":\"subprocess_finished\"")
                .count(),
            1,
            "one terminal child record"
        );
        assert_eq!(
            std::fs::read_dir(dispatch_runs_root())
                .expect("run root")
                .count(),
            1,
            "cleanup remains owned by the one canonical run"
        );
    }

    /// Discrimination test: a `StaffStartRequest` JSON that OMITS
    /// `staffing_reason` is REJECTED at deserialization — the field is
    /// required (no `#[serde(default)]`). This is the facade-level admission
    /// gate. A request missing the reason cannot reach `staff_start`, so no
    /// worker is launched, no run directory is created.
    ///
    /// RED-before-fix proof: before `staffing_reason` was added as a required
    /// field, this deserialization SUCCEEDED (all fields were optional and the
    /// reason was a free-string marker that the retired compatibility bridge
    /// dropped. After the
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

    /// The typed request retains its required admission reason without a
    /// Staff-facing projection into launch mechanics.
    #[test]
    fn typed_request_carries_staffing_reason_without_flat_projection() {
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
        assert_eq!(
            request.staffing_reason,
            TachiDispatchReason::CrossDeviceRemote,
            "the typed request must retain its required admission reason"
        );
        assert_eq!(request.task, "prove reason is carried through");
        assert_eq!(request.worker.as_deref(), Some("codex"));
        assert_eq!(request.profile.as_deref(), Some("codex_55_review"));
        assert_eq!(request.project.as_deref(), Some("tachi"));
        assert_eq!(request.stage.as_deref(), Some("execute"));
        assert_eq!(request.issue_ref.as_deref(), Some("o/r#42"));
        assert_eq!(request.pr_ref.as_deref(), Some("o/r#43"));
        assert_eq!(request.flow_id.as_deref(), Some("flow_xyz"));
    }

    #[test]
    fn staff_production_surface_rejects_flat_dispatch_facade_mutants() {
        let source = include_str!("mod.rs");
        for forbidden in [
            ["TachiDispatch", "Params"].concat(),
            ["into_dispatch", "_params"].concat(),
            ["into_", "params"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "Staff production code must not mention {forbidden}"
            );
            assert!(
                format!("{source}\n{forbidden}").contains(&forbidden),
                "the Staff flat-facade detector must reject a deliberate mutant"
            );
        }
    }

    /// Boundary test: a `StaffStartRequest` JSON that attempts to set
    /// execution-shaped fields fails deserialization loudly (deny_unknown_fields).
    #[test]
    fn staff_start_request_has_no_execution_fields() {
        let hostile = serde_json::json!({
            "task": "prove the boundary",
            "staffing_reason": "native_subagent_unavailable",
            "worker": "claude",
            // ── hostile / out-of-boundary fields: must fail loudly ──────────
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
        let err = serde_json::from_value::<StaffStartRequest>(hostile).unwrap_err();
        assert!(
            err.to_string().contains("unknown field"),
            "hostile execution fields must fail deserialization loudly: {err}"
        );

        let valid = serde_json::json!({
            "task": "prove the boundary",
            "staffing_reason": "native_subagent_unavailable",
            "worker": "claude",
            "profile": "codex_55_review",
            "project": "tachi",
            "stage": "execute",
            "flow_id": "flow-123",
        });
        let request: StaffStartRequest = serde_json::from_value(valid).expect("parses");

        // Semantic intent stays in the typed request.
        assert_eq!(request.task, "prove the boundary");
        assert_eq!(request.worker.as_deref(), Some("claude"));
        assert_eq!(request.profile.as_deref(), Some("codex_55_review"));
        assert_eq!(request.project.as_deref(), Some("tachi"));
        assert_eq!(request.stage.as_deref(), Some("execute"));
        assert_eq!(request.flow_id.as_deref(), Some("flow-123"));
        assert_eq!(
            request.staffing_reason,
            TachiDispatchReason::NativeSubagentUnavailable
        );
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
        let err = serde_json::from_value::<StaffStartRequest>(raw).unwrap_err();
        assert!(
            err.to_string().contains("unknown field"),
            "caller-forged dispatch_id must fail deserialization loudly: {err}"
        );
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

    pub(crate) fn test_server() -> MemoryServer {
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
        let recommendation = server
            .with_global_store(|store| {
                memcore::insert_route_recommendation(
                    store.connection(),
                    &memcore::NewRouteRecommendation {
                        recommendation_id: "rec-match".to_string(),
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

        record_route_decision_best_effort(&server, &raw, Some(&recommendation));

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

    /// #1814: route evidence consumes the pre-acceptance recommendation fact.
    /// Deleting its source row after resolution must not cause a second query
    /// to downgrade a valid accepted decision to `unadvised`.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn record_route_decision_uses_carried_recommendation_after_source_delete() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp home");
        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
        let server = test_server();

        let recommendation = server
            .with_global_store(|store| {
                memcore::insert_route_recommendation(
                    store.connection(),
                    &memcore::NewRouteRecommendation {
                        recommendation_id: "rec-carried-after-delete".to_string(),
                        task_type: Some("fix_request".to_string()),
                        risk: "low".to_string(),
                        candidates: serde_json::json!([{"profile": "wizard_sonnet"}]),
                        recommended_profile: Some("wizard_sonnet".to_string()),
                        policy_source_revision: Some("rev-carried".to_string()),
                        rows_considered: 1,
                        occurred_at: memcore::now_utc_iso(),
                    },
                )
                .map_err(|error| error.to_string())
            })
            .expect("seed carried recommendation");
        server
            .with_global_store(|store| {
                store
                    .connection()
                    .execute(
                        "DELETE FROM route_recommendations WHERE recommendation_id = ?1",
                        [&recommendation.recommendation_id],
                    )
                    .map_err(|error| error.to_string())
            })
            .expect("delete recommendation after typed resolution");
        let dispatch_id = "20260810T000006Z-claude-ffffffff";
        seed_status_json(dispatch_id, "env-1", "dev", "claude-sonnet-5");
        let raw = fake_raw_response(dispatch_id, "wizard_sonnet");

        record_route_decision_best_effort(&server, &raw, Some(&recommendation));

        let row = server
            .with_global_store_read(|store| {
                memcore::get_route_decision_by_dispatch_id(store.connection(), dispatch_id)
                    .map_err(|e| e.to_string())
            })
            .unwrap()
            .expect("route decision lands from carried fact");
        assert_eq!(row.assignment_mode, "advised");
        assert_eq!(
            row.recommendation_id.as_deref(),
            Some(recommendation.recommendation_id.as_str())
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

        let recommendation = server
            .with_global_store(|store| {
                memcore::insert_route_recommendation(
                    store.connection(),
                    &memcore::NewRouteRecommendation {
                        recommendation_id: "rec-diverge".to_string(),
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

        record_route_decision_best_effort(&server, &raw, Some(&recommendation));

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

    /// Typed Staff resolution validates recommendation ownership before the
    /// canonical receipt/evidence lifecycle. A stale reference must therefore
    /// leave neither a run artifact nor a route_decisions projection behind.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn staff_start_refuses_unknown_recommendation_before_artifacts_or_evidence() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp home");
        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
        let server = test_server();
        let before_runs = std::fs::read_dir(dispatch_runs_root())
            .map(|entries| entries.count())
            .unwrap_or(0);

        let err = staff_start(
            &server,
            StaffStartRequest {
                task: "refuse stale recommendation".to_string(),
                staffing_reason: TachiDispatchReason::ExplicitUserRequest,
                profile: None,
                worker: Some("codex".to_string()),
                project: Some("named-project".to_string()),
                stage: None,
                execution_level: None,
                issue_ref: None,
                pr_ref: None,
                flow_id: None,
                completion_predicate: None,
                recommendation_ref: Some("missing-recommendation".to_string()),
            },
        )
        .await
        .expect_err("unknown recommendation must fail before acceptance");
        assert!(err.contains("Unknown or stale recommendation_ref"));
        assert_eq!(
            std::fs::read_dir(dispatch_runs_root())
                .map(|entries| entries.count())
                .unwrap_or(0),
            before_runs,
            "refusal must create zero canonical run artifacts"
        );
        let route_rows: i64 = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row("SELECT COUNT(*) FROM route_decisions", [], |row| row.get(0))
                    .map_err(|error| error.to_string())
            })
            .expect("count route evidence");
        assert_eq!(route_rows, 0, "refusal must write zero route evidence rows");
    }

    /// Authority refusal is before claim persistence as well as before the
    /// canonical receipt lifecycle. The linked issue/flow must not become an
    /// active claim when the selected provider cannot honor the contract.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn staff_start_authority_refusal_leaves_zero_claim_workspace_or_evidence() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp home");
        let temp_runs = tempfile::tempdir().expect("temp canonical run root");
        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
        let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", temp_runs.path());
        let server = test_server();
        let before_claims: i64 = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row("SELECT COUNT(*) FROM session_claims", [], |row| row.get(0))
                    .map_err(|error| error.to_string())
            })
            .expect("count claims before refusal");

        let mut request = staff_request("tachi");
        request.profile = Some("deepseek_explore".to_string());
        request.worker = Some("custom".to_string());
        request.issue_ref = Some("kckylechen1/tachi#1814-authority".to_string());
        request.flow_id = Some("flow_1814_authority_refusal".to_string());
        let error = staff_start(&server, request).await.expect_err(
            "uncertified shell-capable read-only authority must refuse before acceptance",
        );
        assert!(error.contains("not kill-test certified"), "{error}");
        assert!(error.contains("fail-closed"), "{error}");

        let after_claims: i64 = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row("SELECT COUNT(*) FROM session_claims", [], |row| row.get(0))
                    .map_err(|error| error.to_string())
            })
            .expect("count claims after refusal");
        assert_eq!(
            after_claims, before_claims,
            "authority refusal creates zero claims"
        );
        assert_eq!(
            std::fs::read_dir(temp_runs.path())
                .expect("read isolated run root")
                .count(),
            0,
            "authority refusal creates zero workspaces/artifacts"
        );
        assert_eq!(
            route_decisions_count(&server),
            0,
            "authority refusal creates zero evidence"
        );
    }
}
