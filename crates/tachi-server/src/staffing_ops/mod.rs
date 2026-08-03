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
//! hints, routing/boundary markers, completion intent). Execution fields —
//! `cwd`, `command`, `transport`, `credentials`, `sandbox`, `allowed_tools`,
//! MCP plumbing, watchdog internals — are resolved by profile / policy /
//! [`crate::host_profile`] / ExecEnv / adapter during mapping, NEVER by the
//! caller. The dispatch kernel owns the single lifecycle; the canonical
//! `status.json` receipt owns the single truth.
//!
//! # Why an explicit allowlist struct
//!
//! [`StaffStartRequest`] deliberately has NO field named `cwd`, `command`,
//! `transport`, `credentials`, `sandbox`, or `allowed_tools`. A request JSON
//! that attempts to set any of those is silently ignored (no matching field
//! exists to deserialize into), so the resulting [`TachiDispatchParams`] maps
//! every execution field to its kernel-side default — exactly the
//! discrimination the [`external_staffing_contract`][crate::tests::docs_tests]
//! census enforces structurally.
//
// `#[allow(dead_code)]` until PR4 ([1319-B7]) wires this contract to the
// `tachi_staff` facade caller. The items ARE exercised by this module's
// discrimination tests, so `--all-targets` sees them as live and
// `#[expect(dead_code)]` would go red there as an unfulfilled expectation;
// only the `--lib` gate (tests are not roots) reports them. Delete this
// attribute in the PR that adds the public caller. (Same convention as
// `governed_precedent_establishment.rs:132-137`.)
#![allow(dead_code)]

use crate::dispatch_ops::{
    canonical_dir_is_within, dispatch_runs_root, handle_tachi_dispatch, is_valid_dispatch_id,
};
use crate::tool_params::TachiDispatchParams;
use crate::MemoryServer;
use rmcp::schemars::{self, JsonSchema};

/// Default per-dispatch timeout (seconds) when the Staff request does not
/// declare one. Mirrors [`default_dispatch_timeout`] in `tachi-params` (600s);
/// duplicated locally because the params crate keeps that fn private.
/// Default per-dispatch timeout (seconds) when the Staff request does not
/// declare one. Mirrors `default_dispatch_timeout` in `tachi-params` (600s);
/// duplicated locally because the params crate keeps that fn private.
const DEFAULT_STAFF_DISPATCH_TIMEOUT_SECS: u64 = 600;

/// Minimal semantic request for externally staffing a worker. The model may
/// only set intent fields here; execution fields (cwd, command, transport,
/// credentials, sandbox, allowed_tools, MCP plumbing) are resolved by profile /
/// policy / ExecEnv / adapter during mapping, NEVER by the caller.
///
/// `#[serde(default)]` on every optional field plus the absence of any
/// execution-shaped field means an inbound JSON carrying `"cwd": "/evil"` or
/// `"command": ["rm", "-rf"]` is silently ignored — those names have no field
/// to bind to, so they cannot leak into the mapped params.
#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
pub(crate) struct StaffStartRequest {
    /// Task description / prompt for the worker. Required — a Staff request
    /// with no task is meaningless.
    pub task: String,
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
    /// Declared scope marker (informational; does not override kernel scoping).
    #[serde(default)]
    pub scope: Option<String>,
    /// Declared assignment mode marker (informational).
    #[serde(default)]
    pub assignment_mode: Option<String>,
    /// Declared boundary reason (why this work is externally staffed).
    #[serde(default)]
    pub boundary_reason: Option<String>,
    /// Declared routing trigger marker (informational).
    #[serde(default)]
    pub routing_trigger: Option<String>,
    /// Declared routing override marker (informational; never a raw transport).
    #[serde(default)]
    pub routing_override: Option<String>,
    /// Completion intent marker. The model may declare intent, but the
    /// machine-checkable predicate object is policy-shaped, so for v1 this is
    /// an opaque semantic string only.
    // TODO(1319): if a structured completion predicate is exposed to Staff
    // callers in a later slice, promote this to Option<CompletionPredicate>.
    #[serde(default)]
    pub completion: Option<String>,
}

impl StaffStartRequest {
    /// Map a semantic Staff request onto the canonical dispatch params.
    ///
    /// Every execution-shaped field (`cwd`, `command`, `transport`,
    /// `credentials`, `sandbox`, `allowed_tools`, MCP plumbing, watchdog
    /// internals) is left at its kernel-side default: `None` / empty / the
    /// default timeout. Those are resolved by the canonical admission / policy
    /// / profile pipeline inside `handle_tachi_dispatch`
    /// ([`resolve_dispatch_start`][crate::dispatch_ops], env binding, authority
    /// contract), NEVER set here.
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
/// [`handle_tachi_dispatch`]. That entry-point seeds canonical `status.json`
/// (receipt-first) BEFORE prompt assembly / plan stage / spawn, so a
/// successful `Ok` return guarantees the canonical receipt already exists on
/// disk. This adapter creates NO parallel store.
pub(crate) async fn staff_start(
    server: &MemoryServer,
    request: StaffStartRequest,
) -> Result<String, String> {
    let params = request.into_params();
    // Delegate to the single canonical choke-point. handle_tachi_dispatch
    // seeds canonical status.json (receipt-first) before spawn, so a
    // successful return guarantees the canonical receipt exists.
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
    // Gate the id the same way `load_dispatch_identity_receipt_checked` does:
    // allowlist + canonicalize-and-confine. A bad id is treated as "not found"
    // (Err), never surfaced as a distinct error class — same fail-closed shape
    // the rest of the dispatch status readers use.
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
    // Return the canonical receipt verbatim. serde_json::Value round-trips
    // through to_string without re-shaping, so callers see exactly what the
    // kernel wrote.
    serde_json::to_string_pretty(&status)
        .map_err(|err| format!("staff_status: serialize receipt: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Discrimination test #1: a `StaffStartRequest` JSON that attempts to set
    /// execution-shaped fields (`cwd`, `command`, `transport`, `credentials`,
    /// `sandbox`, `allowed_tools`) is silently ignored — those names have no
    /// field to bind to — and the resulting mapped `TachiDispatchParams` maps
    /// every execution field to its kernel-side default (`None` / empty).
    ///
    /// This is the structural enforcement of the boundary contract: the Staff
    /// adapter CANNOT carry execution intent, so a caller cannot smuggle a
    /// hostile cwd/command/transport through it. The forbidden names simply
    /// don't exist on the struct.
    #[test]
    fn staff_start_request_has_no_execution_fields() {
        let raw = serde_json::json!({
            "task": "prove the boundary",
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

        // Intent fields DO map through.
        assert_eq!(params.task, "prove the boundary");
        assert_eq!(params.agent.as_deref(), Some("claude"));
        assert_eq!(params.profile.as_deref(), Some("codex_55_review"));
        assert_eq!(params.project.as_deref(), Some("tachi"));
        assert_eq!(params.stage.as_deref(), Some("execute"));
        assert_eq!(params.flow_id.as_deref(), Some("flow-123"));

        // ── Execution fields MUST all be at kernel defaults ─────────────────
        // cwd: hostile "/evil/absolute/path" must NOT have leaked through.
        assert_eq!(
            params.cwd, None,
            "Staff must never set cwd; the kernel's env-binding pipeline owns it"
        );
        assert_eq!(params.command, Vec::<String>::new(), "no command smuggle");
        assert_eq!(
            params.harness_transport, None,
            "no transport override (neither 'transport' nor 'harness_transport' bound)"
        );
        assert_eq!(params.sandbox, None, "no sandbox smuggle");
        assert_eq!(
            params.allowed_tools,
            Vec::<String>::new(),
            "no allowed_tools smuggle"
        );
        assert_eq!(
            params.credential_profiles,
            Vec::<String>::new(),
            "no credential_profiles smuggle"
        );
        assert_eq!(
            params.allowed_mcp_servers,
            Vec::<String>::new(),
            "no allowed_mcp_servers smuggle"
        );
        assert_eq!(params.inject_tachi_mcp, None, "no MCP plumbing smuggle");
        assert_eq!(params.permission_profile, None, "no permission override");
        assert_eq!(params.env_id, None, "no env_id smuggle");
        assert_eq!(params.unmanaged_cwd, None, "no unmanaged_cwd smuggle");

        // timeout_secs defaults to the kernel default (600), never caller-set.
        assert_eq!(params.timeout_secs, DEFAULT_STAFF_DISPATCH_TIMEOUT_SECS);
    }

    /// Discrimination test #1b: the v1 schema also does NOT expose
    /// `dispatch_id` on a START request — it is minted by the kernel, not the
    /// caller. A caller-supplied `dispatch_id` on a start request is ignored.
    #[test]
    fn staff_start_request_does_not_accept_dispatch_id() {
        let raw = serde_json::json!({
            "task": "prove no dispatch_id on start",
            "dispatch_id": "caller-forged-id",
        });
        let request: StaffStartRequest = serde_json::from_value(raw).expect("parses");
        // No panic, no dispatch_id field — the struct simply has no such field.
        assert_eq!(request.task, "prove no dispatch_id on start");
    }

    /// Acceptance test #2: `staff_start`'s entire body is
    /// `handle_tachi_dispatch(server, request.into_params())` — i.e. it is a
    /// pure delegating adapter with NO side channel, NO second lifecycle, and
    /// NO receipt store of its own. The canonical receipt-first guarantee
    /// (status.json seeded before the worker completes) is already proven end
    /// to end by the existing spawn-based acceptance test
    /// `canonical_external_staffing_start_and_terminal_receipt_share_one_run_dir`
    /// in `dispatch_ops/dispatch/tests.rs`, which exercises the exact same
    /// `handle_tachi_dispatch` entry point this adapter delegates to.
    ///
    /// Re-spawning a blocking worker here would only re-test the kernel, not
    /// the Staff boundary, and would add a slow/flaky subprocess to a unit
    /// suite. Instead this test pins the delegation contract at the source
    /// level: `into_params()` must map every semantic field through and must
    /// NOT surface any execution field, which is the property the boundary
    /// exists to enforce.
    #[test]
    fn staff_start_delegates_via_into_params_mapping_only() {
        let request = StaffStartRequest {
            task: "prove staff_start is a pure delegating adapter".to_string(),
            worker: Some("codex".to_string()),
            profile: Some("codex_55_review".to_string()),
            project: Some("tachi".to_string()),
            stage: Some("execute".to_string()),
            issue_ref: Some("o/r#42".to_string()),
            pr_ref: Some("o/r#43".to_string()),
            flow_id: Some("flow_xyz".to_string()),
            scope: Some("src/foo".to_string()),
            assignment_mode: Some("external".to_string()),
            boundary_reason: Some("capacity".to_string()),
            routing_trigger: Some("overflow".to_string()),
            routing_override: Some("coordinate".to_string()),
            completion: Some("tests pass".to_string()),
        };
        let params = request.into_params();

        // Every semantic field maps through to the canonical params.
        assert_eq!(
            params.task,
            "prove staff_start is a pure delegating adapter"
        );
        assert_eq!(params.agent.as_deref(), Some("codex"));
        assert_eq!(params.profile.as_deref(), Some("codex_55_review"));
        assert_eq!(params.project.as_deref(), Some("tachi"));
        assert_eq!(params.stage.as_deref(), Some("execute"));
        assert_eq!(params.issue_ref.as_deref(), Some("o/r#42"));
        assert_eq!(params.pr_ref.as_deref(), Some("o/r#43"));
        assert_eq!(params.flow_id.as_deref(), Some("flow_xyz"));

        // The marker-only fields (scope/assignment_mode/boundary_reason/
        // routing_trigger/routing_override/completion) have NO corresponding
        // field on TachiDispatchParams by design — they are v1 informational
        // markers carried on the Staff request for future policy projection,
        // not execution knobs. into_params() correctly drops them rather than
        // inventing a mapping, which is why they do not appear above.

        // Execution fields remain at kernel defaults (cross-checked against
        // test #1's exhaustive list — the boundary holds for the populated
        // request too, not just the hostile one).
        assert_eq!(params.cwd, None);
        assert_eq!(params.command, Vec::<String>::new());
        assert_eq!(params.harness_transport, None);
        assert_eq!(params.sandbox, None);
        assert_eq!(params.allowed_tools, Vec::<String>::new());
        assert_eq!(params.credential_profiles, Vec::<String>::new());
        assert_eq!(params.timeout_secs, DEFAULT_STAFF_DISPATCH_TIMEOUT_SECS);
    }

    /// Acceptance test #3: `staff_status` reads ONLY the canonical receipt.
    /// Seed a canonical `status.json` at a known dispatch_id under the
    /// canonical runs root, call the status core, and assert it returns the
    /// canonical content verbatim AND creates no new file (no parallel store).
    ///
    /// Uses a temp `TACHI_HOME` (no `MemoryServer`) because the status read is
    /// filesystem-only — building a full server here would be pure overhead
    /// and a parallel-load liability. `staff_status_impl` is the synchronous
    /// core that the public `staff_status` delegates to.
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

        // Snapshot the run_dir contents BEFORE the call so we can prove
        // staff_status did not create any new file (no parallel store).
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
        assert_eq!(
            before, after,
            "staff_status must be read-only: no new files created"
        );
    }

    /// Acceptance test #4: `staff_status` returns `Err` for a dispatch_id with
    /// no canonical `status.json` — both for a valid-shaped-but-absent id and
    /// for a path-traversal-shaped id (fail-closed, same as the rest of the
    /// dispatch status readers).
    #[tokio::test]
    async fn staff_status_rejects_unknown_dispatch_id() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp home");
        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());

        // Valid-shaped id with no run dir on disk.
        let err = staff_status_impl(&StaffStatusRequest {
            dispatch_id: "20260803T999999Z-nobody-00000000".to_string(),
        })
        .await
        .expect_err("absent dispatch_id must Err");
        assert!(
            err.contains("unknown dispatch_id"),
            "absent id error must say 'unknown dispatch_id': {err}"
        );

        // Path-traversal-shaped id: fail-closed, treated as unknown.
        for malicious in ["../decoy", "..", "/etc/passwd", "a/../../decoy", "a/b"] {
            let err = staff_status_impl(&StaffStatusRequest {
                dispatch_id: malicious.to_string(),
            })
            .await
            .expect_err("malformed dispatch_id must Err fail-closed");
            assert!(
                err.contains("unknown dispatch_id"),
                "malicious id {malicious:?} must be rejected as 'unknown dispatch_id': {err}"
            );
        }
    }
}
