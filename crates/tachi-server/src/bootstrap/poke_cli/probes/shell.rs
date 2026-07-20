use crate::tool_params::TachiShellParams;
use crate::MemoryServer;
use serde_json::{json, Value};
use std::path::PathBuf;

pub(in crate::bootstrap::poke_cli) async fn probe_shell_artifact(
    server: &MemoryServer,
) -> Result<Value, String> {
    let raw = crate::shell_ops::handle_tachi_shell(
        server,
        TachiShellParams {
            action: "plan".to_string(),
            format: Some("json".to_string()),
            flow_id: None,
            task: Some("Poke smoke shell artifact probe".to_string()),
            title: Some("poke shell artifact".to_string()),
            agent: None,
            profile: None,
            cwd: None,
            tool_profile: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
            async_dispatch: false,
            dispatch_reason: None,
            project: None,
            limit: None,
            notes: Some(
                "Poke probe: verify instruction/status/injected SOP artifacts.".to_string(),
            ),
            validation: vec!["echo poke-shell".to_string()],
            allowed_scope: vec!["sandbox".to_string()],
            slices: Vec::new(),
        },
    )
    .await?;
    let response: Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse shell response: {e}"))?;
    evaluate_shell_probe_response(response)
}

/// Pure evaluation of a `tachi_shell action=plan` response into the probe's
/// pass/fail verdict. Split out from `probe_shell_artifact` so the gating
/// logic (in particular the `failure_class` tolerance check) is unit
/// testable with crafted responses, without needing a live `MemoryServer`.
fn evaluate_shell_probe_response(response: Value) -> Result<Value, String> {
    let run_dir = response
        .get("run_dir")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| format!("shell response lacks run_dir: {response}"))?;
    let instruction = response
        .get("instruction_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| format!("shell response lacks instruction_path: {response}"))?;
    let status_path = run_dir.join("status.json");
    let injected = response
        .get("injected_skill")
        .cloned()
        .unwrap_or(Value::Null);
    let injected_path = injected
        .get("injected_path")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let injected_ok = injected_path.as_ref().is_some_and(|path| path.exists());
    let injected_required = injected
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let injected_failure_class = injected
        .get("failure_class")
        .and_then(Value::as_str)
        .map(str::to_string);
    let injected_warning = injected
        .get("warning")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // Hard gate: instruction + status must exist.
    if !instruction.exists() || !status_path.exists() {
        return Err(format!(
            "shell artifacts missing: instruction={} status={} response={response}",
            instruction.exists(),
            status_path.exists(),
        ));
    }
    // Injected SOP is tolerated as best-effort *only* when the underlying
    // injection result is explicitly classified as "host skill roots aren't
    // mounted on this runner" (`failure_class == "missing_source_roots"`,
    // see shell_ops::injection::InjectionResult). Any other failure (dir
    // create/read/write) is a real bug and must fail the probe loudly —
    // previously `injected_ok` was computed but never gated on, so every
    // injection failure silently passed (kckylechen1/tachi#1058).
    if injected_required && !injected_ok {
        let tolerated = injected_failure_class.as_deref() == Some("missing_source_roots");
        if !tolerated {
            return Err(format!(
                "shell injected skill failed with untolerated failure_class={injected_failure_class:?} warning={injected_warning:?} response={response}"
            ));
        }
    }
    Ok(json!({
        "name": "shell_artifact",
        "status": "passed",
        "expected": "shell stage writes instruction.md and status.json (injected SOP optional only when failure_class=missing_source_roots)",
        "observed": {
            "flow_id": response.get("flow_id").cloned().unwrap_or(Value::Null),
            "run_dir": run_dir.to_string_lossy(),
            "instruction_path": instruction.to_string_lossy(),
            "status_path": status_path.to_string_lossy(),
            "injected_ok": injected_ok,
            "injected_path": injected_path.map(|path| json!(path.to_string_lossy())).unwrap_or(Value::Null),
            "injected_failure_class": injected_failure_class,
            "injected_warning": if injected_warning.is_empty() { Value::Null } else { json!(injected_warning) },
        },
        "repro_steps": [
            "tachi_shell action=plan in isolated TACHI_RUN_ROOT",
            "check instruction.md and status.json"
        ],
    }))
}

#[cfg(test)]
mod tests {
    use super::evaluate_shell_probe_response;
    use serde_json::{json, Value};

    /// Builds a minimal, on-disk-backed response: real `instruction.md` and
    /// `status.json` files (so the hard gate passes), plus a caller-supplied
    /// `injected_skill` object. Returns the `TempDir` guard alongside the
    /// response so callers keep the backing files alive for the test's
    /// lifetime.
    fn response_with_injected(
        injected_skill: serde_json::Value,
    ) -> (tempfile::TempDir, serde_json::Value) {
        let temp = tempfile::tempdir().expect("temp run dir");
        let run_dir = temp.path().to_path_buf();
        let instruction_path = run_dir.join("instruction.md");
        std::fs::write(&instruction_path, "# instruction").expect("write instruction");
        std::fs::write(run_dir.join("status.json"), "{}").expect("write status");
        let response = json!({
            "flow_id": "flow_test",
            "run_dir": run_dir.to_string_lossy(),
            "instruction_path": instruction_path.to_string_lossy(),
            "injected_skill": injected_skill,
        });
        (temp, response)
    }

    #[test]
    fn tolerates_missing_source_roots_failure_class() {
        // The one failure mode CI runners without host skill roots are
        // expected to hit: injection didn't load, but it's explicitly
        // classified as "roots missing", so the probe must still pass.
        let (_guard, response) = response_with_injected(json!({
            "required": true,
            "loaded": false,
            "injected_path": Value::Null,
            "failure_class": "missing_source_roots",
            "warning": "meta skill file 'skill/x/SKILL.md' not found in any known root",
        }));
        let result = evaluate_shell_probe_response(response);
        assert!(
            result.is_ok(),
            "missing_source_roots must be tolerated: {result:?}"
        );
    }

    #[test]
    fn fails_on_untolerated_injection_failure() {
        // A real bug — e.g. the injected/ target directory wasn't
        // writable — must NOT be silently swallowed just because it's an
        // injection failure. Before the failure_class gate (tachi#1058),
        // `injected_ok` was computed but never checked, so this crafted
        // scenario passed the probe even though injection genuinely broke.
        let (_guard, response) = response_with_injected(json!({
            "required": true,
            "loaded": false,
            "injected_path": Value::Null,
            "failure_class": "write_failed",
            "warning": "write injected meta skill failed: Permission denied (os error 13)",
        }));
        let result = evaluate_shell_probe_response(response);
        assert!(
            result.is_err(),
            "non-missing_source_roots injection failure must fail the probe, got: {result:?}"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("write_failed"),
            "error should surface the failure_class: {err}"
        );
    }

    #[test]
    fn passes_when_injection_actually_loaded() {
        let injected_temp = tempfile::tempdir().expect("temp injected dir");
        let injected_path = injected_temp.path().join("superpowers-plan.md");
        std::fs::write(&injected_path, "# sop").expect("write injected sop");
        let (_guard, response) = response_with_injected(json!({
            "required": true,
            "loaded": true,
            "injected_path": injected_path.to_string_lossy(),
            "failure_class": Value::Null,
            "warning": Value::Null,
        }));
        let result = evaluate_shell_probe_response(response);
        assert!(result.is_ok(), "successful injection must pass: {result:?}");
    }
}
