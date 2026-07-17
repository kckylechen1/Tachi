use chrono::Utc;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

use crate::dispatch_ops::dispatch_v2::append_trajectory_event;

use super::spec::command_available;

pub(crate) async fn run_acpx_control_from_status(
    run_dir: &Path,
    status: &Value,
    control: &str,
    timeout: Duration,
) -> Result<Value, String> {
    if !matches!(control, "status" | "cancel") {
        return Err(format!("unsupported acpx control '{control}'"));
    }
    let dispatch_id = status
        .get("dispatch_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if status.get("execution_backend").and_then(Value::as_str) != Some("acpx") {
        return Err(format!(
            "dispatch '{dispatch_id}' is not using the acpx execution backend"
        ));
    }
    let control_meta = status
        .get("acpx")
        .and_then(|acpx| acpx.get("controls"))
        .and_then(|controls| controls.get(control))
        .ok_or_else(|| {
            format!("dispatch '{dispatch_id}' has no acpx {control} control metadata")
        })?;
    if !control_meta
        .get("supported")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let reason = control_meta
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("acpx control is not supported for this dispatch");
        return Err(format!(
            "dispatch '{dispatch_id}' cannot run acpx {control}: {reason}"
        ));
    }
    let argv = control_meta
        .get("argv")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("dispatch '{dispatch_id}' acpx {control} metadata lacks argv"))?
        .iter()
        .map(|arg| {
            arg.as_str().map(str::to_string).ok_or_else(|| {
                format!("dispatch '{dispatch_id}' acpx {control} argv contains a non-string value")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if argv.is_empty() {
        return Err(format!(
            "dispatch '{dispatch_id}' acpx {control} argv is empty"
        ));
    }
    let command = &argv[0];
    if !crate::utils::is_trusted_command(command) {
        return Err(format!(
            "dispatch '{dispatch_id}' acpx {control} command '{}' is not trusted",
            command
        ));
    }
    if !command_available(command) {
        return Err(format!(
            "dispatch '{dispatch_id}' acpx {control} command '{}' was not found on PATH",
            command
        ));
    }

    let mut cmd = Command::new(command);
    for arg in &argv[1..] {
        cmd.arg(arg);
    }
    let output = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| {
            format!(
                "dispatch '{dispatch_id}' acpx {control} timed out after {}s",
                timeout.as_secs()
            )
        })?
        .map_err(|err| format!("dispatch '{dispatch_id}' acpx {control} failed to spawn: {err}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let parsed_stdout = parse_acpx_control_stdout(&stdout);
    let result = json!({
        "dispatch_id": dispatch_id,
        "execution_backend": "acpx",
        "control": control,
        "success": output.status.success(),
        "exit_code": output.status.code(),
        "stdout_json": parsed_stdout,
        "stdout_tail": tail_control_text(&stdout, 4000),
        "stderr_tail": tail_control_text(&stderr, 2000),
        "argv": argv,
        "timestamp": Utc::now().to_rfc3339(),
    });
    let artifact_path = run_dir.join(format!("acpx_{control}.json"));
    if let Ok(body) = serde_json::to_vec_pretty(&result) {
        if let Err(error) = crate::utils::write_owner_only_file_atomic(&artifact_path, &body) {
            tracing::warn!(error = %error, path = %artifact_path.display(), "failed to write acpx control artifact");
        }
    }
    append_trajectory_event(
        &run_dir.join("trajectory.jsonl"),
        json!({
            "event": "acpx_control_invoked",
            "dispatch_id": dispatch_id,
            "control": control,
            "success": output.status.success(),
            "exit_code": output.status.code(),
            "artifact": artifact_path.to_string_lossy(),
            "timestamp": Utc::now().to_rfc3339(),
        }),
    );

    let mut result = result;
    if let Some(obj) = result.as_object_mut() {
        obj.insert(
            "artifact".to_string(),
            Value::String(artifact_path.to_string_lossy().to_string()),
        );
    }
    Ok(result)
}

fn parse_acpx_control_stdout(stdout: &str) -> Value {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Value::Null;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return value;
    }
    let values = trimmed
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .collect::<Vec<_>>();
    if values.is_empty() {
        Value::Null
    } else {
        Value::Array(values)
    }
}

fn tail_control_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.trim().to_string();
    }
    let mut chars = text.chars().rev().take(max_chars).collect::<Vec<_>>();
    chars.reverse();
    chars.into_iter().collect::<String>().trim().to_string()
}
