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
            project: None,
            state_filter: None,
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
    let injected_path = response
        .get("injected_skill")
        .and_then(|value| value.get("injected_path"))
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let injected_ok = injected_path.as_ref().is_some_and(|path| path.exists());
    if !instruction.exists() || !status_path.exists() || !injected_ok {
        return Err(format!(
            "shell artifacts missing: instruction={} status={} injected_ok={} response={response}",
            instruction.exists(),
            status_path.exists(),
            injected_ok
        ));
    }
    Ok(json!({
        "name": "shell_artifact",
        "status": "passed",
        "expected": "shell stage writes instruction.md, status.json, and injected SOP artifact",
        "observed": {
            "flow_id": response.get("flow_id").cloned().unwrap_or(Value::Null),
            "run_dir": run_dir.to_string_lossy(),
            "instruction_path": instruction.to_string_lossy(),
            "status_path": status_path.to_string_lossy(),
            "injected_path": injected_path.map(|path| json!(path.to_string_lossy())).unwrap_or(Value::Null),
        },
        "repro_steps": [
            "tachi_shell action=plan in isolated TACHI_RUN_ROOT",
            "check instruction.md/status.json/injected SOP artifact"
        ],
    }))
}
