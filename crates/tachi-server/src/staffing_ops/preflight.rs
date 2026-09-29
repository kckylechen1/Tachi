//! Read-only Staff capability preflight (`tachi_staff(action='preflight')`).
//!
//! Advisory host/version/transport projection only. It never launches, admits,
//! authenticates, or cancels; the only external action is a bounded, read-only
//! `<backend> --version` process probe (offloaded to the blocking pool). It
//! creates no lease, run directory, credential material, or `status.json`, and
//! it reads no credential/config content.
//!
//! Facts stay separate: `discovered`, `authentication_observation`,
//! `tool_execution_verification`, `cancel_cleanup_verification`, and
//! `certification`. `launch_admission` is always `not_evaluated` — a passing
//! qualification predicate is not launch readiness, and an unknown stays unknown.

use serde_json::{json, Value};
use tachi_dispatch::{
    normalize_dispatch_agent_name, probe_backend_version, provider_has_sandbox_primitive,
    qualify_provider, resolve_dispatch_profile, versions_match, CertificationReceipt,
    TransportKind, WorkspaceAuthority, PROVIDER_QUALIFICATIONS, RECEIPTS,
};
use tachi_params::TachiStaffParams;

/// The only `backend x transport` pair the qualification table covers today.
const PREFLIGHT_TRANSPORT: TransportKind = TransportKind::Cli;
/// The one lane the qualification table's codex row scopes.
const SCOPED_LANE: &str = "shell-capable read-only";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReceiptApplicability {
    version: &'static str,
    os_family: &'static str,
}

/// Applicability of one historical receipt to the installed version and current
/// host OS family. Exact host coverage is never asserted: the runtime
/// qualification path has no arch / current-OS-version match (PR1950's separate
/// host-match repair is out of scope here).
fn receipt_applicability(
    receipt: &CertificationReceipt,
    installed: Option<&str>,
    current_os: &str,
) -> ReceiptApplicability {
    ReceiptApplicability {
        version: match installed {
            None => "unknown",
            Some(actual) if versions_match(actual, receipt.vendor_version) => "match",
            Some(_) => "mismatch",
        },
        os_family: if current_os.eq_ignore_ascii_case(receipt.host_os) {
            "match"
        } else {
            "mismatch"
        },
    }
}

fn qualification_predicate(
    sandbox_primitive: bool,
    version_qualified: bool,
    applicability: Option<&ReceiptApplicability>,
) -> &'static str {
    if !sandbox_primitive {
        "not_applicable"
    } else if version_qualified
        && applicability.is_some_and(|scope| scope.version == "match" && scope.os_family == "match")
    {
        "pass"
    } else {
        "refused"
    }
}

/// Refuse fields that are meaningless or misleading for a read-only preflight,
/// before any probe or I/O: `TachiStaffParams` is the shared flat struct, so a
/// caller could otherwise pass launch/cancel intent that preflight never reads.
/// Accepted: `worker` (required), `profile`, `project` (server may inject), `format`.
fn reject_irrelevant_fields(params: &TachiStaffParams) -> Result<(), String> {
    if params.dispatch_id.is_some() {
        return Err(
            "tachi_staff: action='preflight' does not accept `dispatch_id`; use action='status'. \
             Refused before any probe."
                .to_string(),
        );
    }
    if params.expected_status_revision.is_some() {
        return Err(
            "tachi_staff: action='preflight' does not accept `expected_status_revision` and never \
             cancels. Refused before any probe."
                .to_string(),
        );
    }
    for (name, present) in [
        ("task", params.task.is_some()),
        ("staffing_reason", params.staffing_reason.is_some()),
        ("stage", params.stage.is_some()),
        ("issue_ref", params.issue_ref.is_some()),
        ("pr_ref", params.pr_ref.is_some()),
        ("flow_id", params.flow_id.is_some()),
        ("recommendation_ref", params.recommendation_ref.is_some()),
        ("declared_file_scope", params.declared_file_scope.is_some()),
    ] {
        if present {
            return Err(format!(
                "tachi_staff: action='preflight' is read-only and does not accept start-only field \
                 `{name}`; remove it or use action='start'. Refused before any probe."
            ));
        }
    }
    Ok(())
}

/// Production boundary for `tachi_staff(action='preflight')`. Every refusal path
/// returns `Err` before the version probe runs.
pub(crate) async fn staff_preflight(params: &TachiStaffParams) -> Result<String, String> {
    reject_irrelevant_fields(params)?;

    let requested_worker = params
        .worker
        .as_deref()
        .map(str::trim)
        .filter(|worker| !worker.is_empty())
        .ok_or_else(|| {
            "tachi_staff: action='preflight' requires a `worker` (e.g. \"codex\").".to_string()
        })?;
    let Some(backend) = normalize_dispatch_agent_name(requested_worker) else {
        return Err(format!(
            "tachi_staff: action='preflight' unknown worker '{requested_worker}'; known workers: \
             claude, codex, grok, kimi, opencode, custom. Refused before any probe."
        ));
    };

    // A requested profile is validated (unknown -> refuse; backend mismatch ->
    // refuse) but never applied here: profile-owned launch defaults belong to
    // action='start'. This keeps the start semantics unchanged.
    let profile_section = match params
        .profile
        .as_deref()
        .map(str::trim)
        .filter(|profile| !profile.is_empty())
    {
        None => json!({
            "requested": Value::Null,
            "note": "no profile requested; capability is reported for the worker backend only",
        }),
        Some(raw) => match resolve_dispatch_profile(raw) {
            None => {
                return Err(format!(
                    "tachi_staff: action='preflight' unknown dispatch profile '{raw}'; omit it or \
                     use a supported profile. Refused before any probe."
                ));
            }
            Some(profile) if !profile.backend.eq_ignore_ascii_case(&backend) => {
                return Err(format!(
                    "tachi_staff: action='preflight' profile '{}' targets backend '{}' but worker \
                     is '{}'; preflight reports one backend. Pass a profile for this backend or omit \
                     `profile`. Refused before any probe.",
                    profile.name, profile.backend, backend
                ));
            }
            Some(profile) => json!({
                "requested": raw,
                "resolved": profile.name,
                "backend": profile.backend,
                "runtime_tool_profile": profile.tool_profile,
                "note": "resolvable; profile-owned launch defaults are applied by action='start'",
            }),
        },
    };

    // Bounded read-only `<backend> --version`, offloaded so the sync probe never
    // blocks the async runtime. A failed join degrades to "unknown".
    let probe_target = backend.clone();
    let installed = match tokio::task::spawn_blocking(move || probe_backend_version(&probe_target))
        .await
    {
        Ok(version) => version,
        Err(error) => {
            tracing::warn!(%error, "staff preflight: version probe task failed; reporting unknown");
            None
        }
    };

    let sandbox_primitive = provider_has_sandbox_primitive(&backend, PREFLIGHT_TRANSPORT);
    let qualification = qualify_provider(
        PROVIDER_QUALIFICATIONS,
        &backend,
        PREFLIGHT_TRANSPORT,
        installed.as_deref(),
        WorkspaceAuthority::ReadOnly,
    );
    let current_os = std::env::consts::OS;
    let receipt = RECEIPTS.iter().copied().find(|receipt| {
        receipt.backend.eq_ignore_ascii_case(&backend) && receipt.transport == PREFLIGHT_TRANSPORT
    });
    let applicability =
        receipt.map(|receipt| receipt_applicability(receipt, installed.as_deref(), current_os));
    let qualification_predicate = qualification_predicate(
        sandbox_primitive,
        qualification.is_ok(),
        applicability.as_ref(),
    );
    let qualification_reason = match &qualification {
        Ok(_) if qualification_predicate == "pass" => format!(
            "version and OS family apply to the historical {SCOPED_LANE} receipt; exact host coverage and launch admission remain unverified"
        ),
        Ok(_) => "the historical receipt does not apply to this version and OS family".to_string(),
        Err(reason) => reason.clone(),
    };

    let mut missing_prerequisites: Vec<String> = Vec::new();
    let mut next_steps: Vec<String> = vec![
        "preflight is advisory; to run a worker use tachi_staff(action='start') and let \
         profile/policy resolve grant, credentials, and exec-env."
            .to_string(),
    ];

    let certification = match receipt {
        None => {
            missing_prerequisites.push(format!(
                "no executed kill-test receipt for '{backend}' over the cli transport, so the \
                 {SCOPED_LANE} lane has no qualification entry; other lanes/authorities are governed \
                 by their own gates, not by this receipt"
            ));
            next_steps.push(format!(
                "if the {SCOPED_LANE} lane is required, certification needs an actually executed \
                 kill-test witness plus independent review before any receipt is cited; an installed \
                 CLI alone does not justify issuing one"
            ));
            json!({
                "historical_receipt": Value::Null,
                "lane": SCOPED_LANE,
                "version_applicability": "not_applicable",
                "os_applicability": "not_applicable",
                "precise_host_coverage": "unknown",
                "qualification_predicate": qualification_predicate,
                "qualification_reason": qualification_reason,
            })
        }
        Some(receipt) => {
            let applicability = receipt_applicability(receipt, installed.as_deref(), current_os);
            if installed.is_none() {
                missing_prerequisites.push(format!(
                    "the installed '{backend}' version could not be determined (not on PATH or \
                     `--version` failed); the lane predicate cannot be scoped to this binary"
                ));
            } else if applicability.version == "mismatch" {
                missing_prerequisites.push(format!(
                    "no receipt covering {backend} {}, which is installed (the receipt covers {})",
                    installed.as_deref().unwrap_or("<unknown>"),
                    receipt.vendor_version
                ));
            }
            if applicability.os_family == "mismatch" {
                missing_prerequisites.push(format!(
                    "the receipt was executed on host OS family '{}', not '{}'",
                    receipt.host_os, current_os
                ));
            }
            if qualification_predicate == "pass" {
                missing_prerequisites.push(
                    "precise host coverage (arch / current OS build) is unrecorded, so the lane \
                     predicate reflects version + OS family applicability only"
                        .to_string(),
                );
            }
            json!({
                "historical_receipt": {
                    "id": receipt.id,
                    "source_file": receipt.source_file,
                    "vendor_binary": receipt.vendor_binary,
                    "vendor_version": receipt.vendor_version,
                    "host_os": receipt.host_os,
                    "host_os_version": receipt.host_os_version,
                    "transport": "cli",
                    "result": receipt.result.as_str(),
                    "kill_test": receipt.kill_test,
                    "covers": receipt.covers.iter().map(|level| level.as_str()).collect::<Vec<_>>(),
                    "matrix_len": receipt.matrix.len(),
                },
                "lane": SCOPED_LANE,
                "version_applicability": applicability.version,
                "os_applicability": applicability.os_family,
                "precise_host_coverage": "unknown",
                "qualification_predicate": qualification_predicate,
                "qualification_reason": qualification_reason,
            })
        }
    };

    // Auth, execution, and cancellation have no canonical observation reachable
    // here; each stays unknown with its concrete missing prerequisite stated once
    // (in its own section, not duplicated into `missing_prerequisites`).
    next_steps.push(
        "authenticate locally with your own CLI login; preflight does not read credential/config \
         content"
            .to_string(),
    );
    next_steps.push(
        "record a bounded managed run through the canonical kernel to obtain a tool-execution witness"
            .to_string(),
    );
    next_steps.push(
        "record a bounded managed cancellation through the canonical kernel to obtain a cancellation \
         witness"
            .to_string(),
    );

    let output = json!({
        "action": "preflight",
        "status": "advisory",
        "launch_admission": "not_evaluated",
        "launch_admission_reason": "preflight resolves no grant, authentication, exec-env, or profile \
                                    authority; it never authorizes or refuses a launch",
        "worker": { "requested": requested_worker, "backend": backend, "transport": "cli" },
        "host": { "os": current_os, "arch": std::env::consts::ARCH, "os_build": "not_observed" },
        "discovered": {
            "status": if installed.is_some() { "version_observed" } else { "version_unknown" },
            "version": installed,
            "transport": "cli",
            "sandbox_primitive": sandbox_primitive,
            "probe": format!("{backend} --version (bounded, read-only)"),
        },
        "authentication_observation": {
            "status": "unknown",
            "missing_prerequisite": "a canonical authentication observation for this host/backend; \
                                     preflight reads no credentials and runs no login/canary",
        },
        "tool_execution_verification": {
            "status": "unknown",
            "missing_prerequisite": "a canonical tool-execution witness; installed/logged-in/\
                                     process-exit-0 is not execution evidence",
        },
        "cancel_cleanup_verification": {
            "status": "unknown",
            "missing_prerequisite": "a canonical managed-cancellation witness; a sandbox \
                                     mutation-denial receipt does not attest cancel/cleanup",
        },
        "certification": certification,
        "profile": profile_section,
        "missing_prerequisites": missing_prerequisites,
        "next_steps": next_steps,
    });

    serde_json::to_string(&output).map_err(|err| format!("tachi_staff: serialize preflight: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(raw: serde_json::Value) -> TachiStaffParams {
        serde_json::from_value(raw).expect("preflight params deserialize")
    }

    #[test]
    fn receipt_applicability_matches_version_numerically_and_os_family_only() {
        let receipt = RECEIPTS
            .iter()
            .copied()
            .find(|receipt| receipt.backend == "codex")
            .expect("codex receipt ships");

        let matched = receipt_applicability(receipt, Some("0.144.1"), receipt.host_os);
        assert_eq!(matched.version, "match");
        assert_eq!(matched.os_family, "match");
        assert_eq!(qualification_predicate(true, true, Some(&matched)), "pass");
        let other_os = receipt_applicability(receipt, Some("0.144.1"), "linux");
        assert_eq!(
            qualification_predicate(true, true, Some(&other_os)),
            "refused",
            "a matching version cannot certify another OS"
        );
        assert_eq!(qualification_predicate(true, true, None), "refused");
        assert_eq!(
            receipt_applicability(receipt, Some("0.1.0"), receipt.host_os).version,
            "mismatch"
        );
        assert_eq!(
            receipt_applicability(receipt, None, receipt.host_os).version,
            "unknown"
        );
        assert_eq!(
            receipt_applicability(receipt, Some("0.144.1"), "linux").os_family,
            "mismatch"
        );
    }

    #[tokio::test]
    async fn preflight_requires_worker_and_refuses_unknown_worker_before_probe() {
        let err = staff_preflight(&params(json!({"action": "preflight"})))
            .await
            .expect_err("worker is required");
        assert!(err.contains("requires a `worker`"), "{err}");

        let err = staff_preflight(&params(
            json!({"action": "preflight", "worker": "not-a-worker"}),
        ))
        .await
        .expect_err("unknown worker is refused");
        assert!(
            err.contains("unknown worker") && err.contains("Refused before any probe"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn preflight_rejects_status_cancel_and_start_only_fields_before_probe() {
        for (field, value) in [
            ("dispatch_id", json!("20260823T010101Z-custom-deadbeef")),
            ("expected_status_revision", json!(7)),
            ("task", json!("do work")),
            ("staffing_reason", json!("explicit_user_request")),
            ("stage", json!("plan")),
            ("issue_ref", json!("owner/repo#1")),
            ("pr_ref", json!("owner/repo#2")),
            ("flow_id", json!("flow-1")),
            ("recommendation_ref", json!("rec-1")),
            ("declared_file_scope", json!(["src/lib.rs"])),
        ] {
            let mut raw = json!({"action": "preflight", "worker": "codex"});
            raw[field] = value;
            let err = staff_preflight(&params(raw))
                .await
                .expect_err("irrelevant field is refused");
            assert!(
                err.contains("Refused before any probe"),
                "field {field}: {err}"
            );
        }
    }

    #[tokio::test]
    async fn preflight_refuses_unknown_or_mismatched_profile_before_probe() {
        let err = staff_preflight(&params(json!({
            "action": "preflight", "worker": "codex", "profile": "definitely_not_a_profile"
        })))
        .await
        .expect_err("unknown profile is refused");
        assert!(err.contains("unknown dispatch profile"), "{err}");

        let err = staff_preflight(&params(json!({
            "action": "preflight", "worker": "codex", "profile": "claude_plan"
        })))
        .await
        .expect_err("profile targeting a different backend is refused");
        assert!(err.contains("targets backend 'claude'"), "{err}");
        assert!(err.contains("Refused before any probe"), "{err}");
    }

    #[cfg(unix)]
    fn write_fake_cli(dir: &std::path::Path, name: &str, version_line: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!("#!/bin/sh\nprintf '{version_line}\\n'\nexit 0\n"),
        )
        .expect("write fake CLI");
        let mut permissions = std::fs::metadata(&path)
            .expect("fake CLI metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("make fake CLI executable");
    }

    #[cfg(unix)]
    struct PreflightRun {
        result: Result<String, String>,
        runs_root_before: Vec<std::path::PathBuf>,
        runs_root_after: Vec<std::path::PathBuf>,
    }

    #[cfg(unix)]
    fn run_preflight_with_path(bin_dir: &std::path::Path, raw: serde_json::Value) -> PreflightRun {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp home");
        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
        let _path = crate::test_support::EnvRestore::set_path("PATH", bin_dir);
        let runs_root = crate::dispatch_ops::dispatch_runs_root();
        let before = list_dir(&runs_root);
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(staff_preflight(&params(raw)));
        let after = list_dir(&runs_root);
        PreflightRun {
            result,
            runs_root_before: before,
            runs_root_after: after,
        }
    }

    #[cfg(unix)]
    fn list_dir(path: &std::path::Path) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(path)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[cfg(unix)]
    #[test]
    fn preflight_version_missing_and_mismatch_scope_the_lane_without_admission() {
        let empty = tempfile::tempdir().expect("empty bin dir");
        let value: Value = serde_json::from_str(
            &run_preflight_with_path(
                empty.path(),
                json!({"action": "preflight", "worker": "codex"}),
            )
            .result
            .expect("missing version is still Ok"),
        )
        .expect("preflight JSON");
        assert_eq!(value["discovered"]["status"], json!("version_unknown"));
        assert_eq!(
            value["certification"]["qualification_predicate"],
            json!("refused")
        );
        assert!(value.get("read_only_gate").is_none());
        assert_eq!(value["launch_admission"], json!("not_evaluated"));

        let old = tempfile::tempdir().expect("old version bin dir");
        write_fake_cli(old.path(), "codex", "codex-cli 0.1.0");
        let value: Value = serde_json::from_str(
            &run_preflight_with_path(
                old.path(),
                json!({"action": "preflight", "worker": "codex"}),
            )
            .result
            .expect("mismatch is still Ok"),
        )
        .expect("preflight JSON");
        assert_eq!(
            value["certification"]["version_applicability"],
            json!("mismatch")
        );
        assert_eq!(
            value["certification"]["qualification_predicate"],
            json!("refused")
        );
    }

    #[cfg(unix)]
    #[test]
    fn preflight_version_and_os_match_still_reports_admission_not_evaluated() {
        let bin = tempfile::tempdir().expect("bin dir");
        write_fake_cli(bin.path(), "codex", "codex-cli 0.144.1");
        let run = run_preflight_with_path(
            bin.path(),
            json!({"action": "preflight", "worker": "codex"}),
        );
        assert_eq!(
            run.runs_root_before, run.runs_root_after,
            "a read-only preflight must add no run artifact"
        );
        let value: Value = serde_json::from_str(&run.result.expect("matching version is Ok"))
            .expect("preflight JSON");
        assert_eq!(
            value["certification"]["version_applicability"],
            json!("match")
        );
        assert_eq!(
            value["certification"]["os_applicability"],
            json!(if std::env::consts::OS.eq_ignore_ascii_case("macos") {
                "match"
            } else {
                "mismatch"
            })
        );
        assert_eq!(
            value["certification"]["precise_host_coverage"],
            json!("unknown")
        );
        assert_eq!(
            value["launch_admission"],
            json!("not_evaluated"),
            "a passing lane predicate must never become a launch-admission verdict"
        );
        assert!(value.get("read_only_gate").is_none());
        assert_eq!(
            value["tool_execution_verification"]["status"],
            json!("unknown")
        );
        assert_eq!(
            value["cancel_cleanup_verification"]["status"],
            json!("unknown")
        );
    }

    #[cfg(unix)]
    #[test]
    fn preflight_without_receipt_does_not_blanket_refuse_launches() {
        let bin = tempfile::tempdir().expect("bin dir");
        write_fake_cli(bin.path(), "claude", "claude 1.2.3");
        let value: Value = serde_json::from_str(
            &run_preflight_with_path(
                bin.path(),
                json!({"action": "preflight", "worker": "claude"}),
            )
            .result
            .expect("no-receipt backend is still Ok"),
        )
        .expect("preflight JSON");
        assert_eq!(
            value["certification"]["qualification_predicate"],
            json!("not_applicable")
        );
        assert_eq!(value["certification"]["lane"], json!(SCOPED_LANE));
        assert_eq!(
            value["tool_execution_verification"]["status"],
            json!("unknown")
        );
        let body = serde_json::to_string(&value).expect("serialize");
        assert!(
            !body.contains("managed launch"),
            "preflight must not claim all managed launches are refused: {body}"
        );
        assert!(
            value["missing_prerequisites"]
                .as_array()
                .expect("missing prerequisites")
                .iter()
                .any(|entry| entry
                    .as_str()
                    .is_some_and(|text| text.contains("other lanes/authorities"))),
            "certification gap must be scoped to the lane: {value}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preflight_creates_no_run_artifacts_and_leaks_no_secret_environment() {
        let _canary =
            crate::test_support::EnvRestore::set("TACHI_PREFLIGHT_CANARY_SECRET", "s3cr3t-canary");
        let bin = tempfile::tempdir().expect("bin dir");
        write_fake_cli(bin.path(), "codex", "codex-cli 0.144.1");
        let run = run_preflight_with_path(
            bin.path(),
            json!({"action": "preflight", "worker": "codex"}),
        );
        assert!(
            !run.result.expect("preflight Ok").contains("s3cr3t-canary"),
            "preflight must not echo environment secrets"
        );
        assert_eq!(
            run.runs_root_before, run.runs_root_after,
            "a read-only preflight must create no lease/run artifact"
        );
    }
}
