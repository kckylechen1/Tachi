use super::dispatch_v2::append_trajectory_event;
use super::subprocess::resolve_permission_profile;
use crate::tool_params::TachiDispatchParams;
use chrono::Utc;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

const ACPX_EVENTS_FILE: &str = "acpx_events.jsonl";
const ACPX_NODE_REQUIREMENT: &str = ">=22.13.0";
const ACPX_NODE_MIN_VERSION: (u64, u64, u64) = (22, 13, 0);

#[derive(Debug, Clone)]
pub(super) struct AcpxCommandSpec {
    pub command: String,
    pub args: Vec<String>,
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum AcpxRunMode {
    Exec,
    Session,
}

#[derive(Debug, Clone)]
struct AcpxSession {
    name: String,
    source: &'static str,
}

#[derive(Debug, Clone)]
pub(super) struct AcpxEventSummary {
    pub events_file: PathBuf,
    pub mapped_events: usize,
    pub final_response: Option<String>,
}

pub(super) fn is_acpx_transport(transport: &str) -> bool {
    matches!(
        transport.trim().to_ascii_lowercase().as_str(),
        "acpx" | "acp"
    )
}

pub(super) fn prepare_acpx_prompt(prompt_file: &Path, prompt: &str) -> Result<PathBuf, String> {
    crate::utils::write_owner_only_file_atomic(prompt_file, prompt.as_bytes())
        .map_err(|err| format!("Failed to write acpx prompt file: {err}"))?;
    Ok(prompt_file.to_path_buf())
}

pub(super) fn build_acpx_command_spec(
    params: &TachiDispatchParams,
    agent: &str,
    prompt_file: &Path,
) -> Result<AcpxCommandSpec, String> {
    let command = std::env::var("TACHI_ACPX_COMMAND")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "acpx".to_string());
    let command = command.trim().to_string();
    if !crate::utils::is_trusted_command(&command) {
        return Err(format!(
            "acpx execution backend command '{}' is not trusted. Set TACHI_ACPX_COMMAND to a trusted binary such as acpx, npx, node, bun, or an approved absolute path.",
            command
        ));
    }
    if !command_available(&command) {
        return Err(format!(
            "acpx execution backend requested, but command '{}' was not found on PATH. Install acpx, or set TACHI_ACPX_COMMAND and TACHI_ACPX_ARGS, for example TACHI_ACPX_COMMAND=npx with TACHI_ACPX_ARGS='-y acpx@<tested-version>'.",
            command
        ));
    }
    let node_readiness = acpx_node_readiness(&command)?;

    let profile = resolve_permission_profile(params)?;
    let (permission_args, permission_label) = acpx_permission_args(profile)?;
    let cwd = params
        .cwd
        .clone()
        .unwrap_or_else(|| current_dir_string().unwrap_or_else(|| ".".to_string()));
    let acpx_agent = resolve_acpx_agent(agent)?;
    let run_mode = resolve_acpx_run_mode()?;
    let session = if run_mode == AcpxRunMode::Session {
        Some(resolve_acpx_session(params)?)
    } else {
        None
    };

    let mut base_args = env_args("TACHI_ACPX_ARGS");
    base_args.extend([
        "--cwd".to_string(),
        cwd.clone(),
        "--format".to_string(),
        "json".to_string(),
        "--json-strict".to_string(),
    ]);
    base_args.extend(permission_args);

    let mut args = base_args.clone();
    args.push(acpx_agent.clone());
    if run_mode == AcpxRunMode::Exec {
        args.push("exec".to_string());
    } else if let Some(session) = session.as_ref() {
        args.push("-s".to_string());
        args.push(session.name.clone());
    }
    args.push("--file".to_string());
    args.push(prompt_file.to_string_lossy().to_string());

    let session_name = session.as_ref().map(|session| session.name.clone());
    let session_source = session.as_ref().map(|session| session.source);
    let status_control = acpx_control_metadata(
        &command,
        &base_args,
        &acpx_agent,
        "status",
        session_name.as_deref(),
        run_mode,
    );
    let cancel_control = acpx_control_metadata(
        &command,
        &base_args,
        &acpx_agent,
        "cancel",
        session_name.as_deref(),
        run_mode,
    );

    Ok(AcpxCommandSpec {
        command,
        args,
        metadata: json!({
            "execution_backend": "acpx",
            "agent": acpx_agent,
            "mode": run_mode.as_str(),
            "session": session_name,
            "session_source": session_source,
            "cwd": cwd,
            "format": "json",
            "json_strict": true,
            "permissions": permission_label,
            "prompt_file": prompt_file.to_string_lossy(),
            "readiness": {
                "command_found": true,
                "node": node_readiness,
            },
            "controls": {
                "status": status_control,
                "cancel": cancel_control,
            },
        }),
    })
}

pub(super) fn build_acpx_command(spec: &AcpxCommandSpec) -> Command {
    let mut cmd = Command::new(&spec.command);
    for arg in &spec.args {
        cmd.arg(arg);
    }
    cmd
}

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
        let _ = crate::utils::write_owner_only_file_atomic(&artifact_path, &body);
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

pub(super) fn persist_acpx_events_and_map(
    workspace_dir: &Path,
    trajectory_path: &Path,
    dispatch_id: &str,
    agent: &str,
    output: &str,
) -> Result<AcpxEventSummary, String> {
    let events_file = workspace_dir.join(ACPX_EVENTS_FILE);
    let progress_path = workspace_dir.join("progress.jsonl");
    let mut raw_lines = Vec::new();
    let mut mapped_events = 0usize;
    let mut final_response = None;

    for (index, line) in output.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let event = serde_json::from_str::<Value>(trimmed).unwrap_or_else(|_| {
            json!({
                "event": "raw_text",
                "index": index,
                "text": trimmed,
            })
        });
        raw_lines.push(
            serde_json::to_string(&event)
                .map_err(|err| format!("Failed to serialize acpx event: {err}"))?,
        );
        if let Some(mapped) = map_acpx_event(dispatch_id, agent, &event) {
            let target_progress = mapped
                .get("tachi_target")
                .and_then(Value::as_str)
                .is_some_and(|target| target == "progress");
            if target_progress {
                append_trajectory_event(&progress_path, mapped.clone());
            } else {
                append_trajectory_event(trajectory_path, mapped.clone());
            }
            mapped_events += 1;
        }
        if final_response.is_none() && is_final_acpx_event(&event) {
            final_response = extract_event_text(&event);
        }
    }

    if raw_lines.is_empty() {
        raw_lines.push(
            serde_json::to_string(&json!({
                "event": "empty_output",
                "dispatch_id": dispatch_id,
            }))
            .map_err(|err| format!("Failed to serialize empty acpx event: {err}"))?,
        );
    }

    let raw_payload = format!("{}\n", raw_lines.join("\n"));
    crate::utils::write_owner_only_file_atomic(&events_file, raw_payload.as_bytes())
        .map_err(|err| format!("Failed to write {ACPX_EVENTS_FILE}: {err}"))?;
    append_trajectory_event(
        trajectory_path,
        json!({
            "event": "acpx_events_persisted",
            "dispatch_id": dispatch_id,
            "agent": agent,
            "events_file": events_file.to_string_lossy(),
            "raw_event_count": raw_lines.len(),
            "mapped_event_count": mapped_events,
            "timestamp": Utc::now().to_rfc3339(),
        }),
    );

    Ok(AcpxEventSummary {
        events_file,
        mapped_events,
        final_response,
    })
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

fn acpx_permission_args(profile: &str) -> Result<(Vec<String>, &'static str), String> {
    match profile {
        "default" => Ok((vec!["--approve-reads".to_string()], "approve-reads")),
        "allowlist" => Err(
            "permission_profile 'allowlist' is not supported by the acpx backend yet; use default read-approved posture."
                .to_string(),
        ),
        "full" => Err(
            "permission_profile 'full' is not supported by the acpx backend; Tachi never maps acpx to --approve-all."
                .to_string(),
        ),
        other => Err(format!(
            "unsupported permission_profile '{other}' for acpx backend"
        )),
    }
}

impl AcpxRunMode {
    fn as_str(self) -> &'static str {
        match self {
            AcpxRunMode::Exec => "exec",
            AcpxRunMode::Session => "session",
        }
    }
}

fn resolve_acpx_run_mode() -> Result<AcpxRunMode, String> {
    let raw = std::env::var("TACHI_ACPX_RUN_MODE")
        .ok()
        .or_else(|| std::env::var("TACHI_ACPX_SESSION_MODE").ok())
        .unwrap_or_else(|| "exec".to_string());
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "exec" | "one-shot" | "oneshot" | "one_shot" => Ok(AcpxRunMode::Exec),
        "session" | "persistent" | "named" => Ok(AcpxRunMode::Session),
        other => Err(format!(
            "unsupported acpx run mode '{other}'. Use TACHI_ACPX_RUN_MODE=exec or TACHI_ACPX_RUN_MODE=session."
        )),
    }
}

fn resolve_acpx_session(params: &TachiDispatchParams) -> Result<AcpxSession, String> {
    if let Ok(explicit) = std::env::var("TACHI_ACPX_SESSION") {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return Ok(AcpxSession {
                name: validate_acpx_session_name(explicit)?,
                source: "env:TACHI_ACPX_SESSION",
            });
        }
    }

    for (value, source) in [
        (params.profile.as_deref(), "dispatch_profile"),
        (params.stage.as_deref(), "stage"),
    ] {
        if let Some(session) = value.and_then(derive_acpx_session_from_card_hint) {
            return Ok(AcpxSession {
                name: session,
                source,
            });
        }
    }

    Ok(AcpxSession {
        name: "scv".to_string(),
        source: "default_builder",
    })
}

fn derive_acpx_session_from_card_hint(value: &str) -> Option<String> {
    let lower = value.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    if lower.contains("poke") || lower.contains("probe") || lower.contains("explore") {
        Some("poke".to_string())
    } else if lower.contains("raven")
        || lower.contains("review")
        || lower.contains("verify")
        || lower.contains("verifier")
    {
        Some("raven".to_string())
    } else if lower.contains("medic") || lower.contains("hotfix") {
        Some("scv-medic".to_string())
    } else if lower.contains("scv")
        || lower.contains("impl")
        || lower.contains("execute")
        || lower.contains("builder")
    {
        Some("scv".to_string())
    } else {
        None
    }
}

fn validate_acpx_session_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("acpx session name cannot be empty".to_string());
    }
    if trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        Ok(trimmed.to_string())
    } else {
        Err(format!(
            "invalid acpx session name '{trimmed}'. Use only ASCII letters, numbers, '-', '_', or '.'."
        ))
    }
}

fn resolve_acpx_agent(agent: &str) -> Result<String, String> {
    if let Ok(explicit) = std::env::var("TACHI_ACPX_AGENT") {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return Ok(explicit.to_string());
        }
    }
    match agent {
        "claude" | "codex" | "grok" | "kimi" => Ok(agent.to_string()),
        "custom" => Err(
            "harness_transport='acpx' with agent='custom' requires TACHI_ACPX_AGENT to name the ACP agent."
                .to_string(),
        ),
        other => Err(format!("unsupported acpx agent '{other}'")),
    }
}

fn env_args(name: &str) -> Vec<String> {
    std::env::var(name)
        .ok()
        .map(|value| {
            value
                .split_whitespace()
                .filter(|arg| !arg.trim().is_empty())
                .map(|arg| arg.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn current_dir_string() -> Option<String> {
    std::env::current_dir()
        .ok()
        .map(|path| path.to_string_lossy().to_string())
}

fn command_available(command: &str) -> bool {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return path.exists();
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(command).is_file())
}

fn acpx_node_readiness(command: &str) -> Result<Value, String> {
    if !should_check_node_for_acpx(command) {
        return Ok(json!({
            "checked": false,
            "reason": "custom acpx command; Node runtime check skipped",
            "required": ACPX_NODE_REQUIREMENT,
        }));
    }
    let output = std::process::Command::new("node")
        .arg("--version")
        .output()
        .map_err(|err| {
            format!(
                "acpx execution backend requires Node {ACPX_NODE_REQUIREMENT}, but `node --version` failed: {err}"
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "acpx execution backend requires Node {ACPX_NODE_REQUIREMENT}, but `node --version` exited with {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    let version_raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let version = parse_node_version(&version_raw).ok_or_else(|| {
        format!(
            "acpx execution backend requires Node {ACPX_NODE_REQUIREMENT}, but could not parse `node --version` output '{version_raw}'."
        )
    })?;
    if version < ACPX_NODE_MIN_VERSION {
        return Err(format!(
            "acpx execution backend requires Node {ACPX_NODE_REQUIREMENT}; found {version_raw}. Upgrade Node or set TACHI_ACPX_COMMAND to a compatible tested acpx wrapper."
        ));
    }
    Ok(json!({
        "checked": true,
        "ok": true,
        "required": ACPX_NODE_REQUIREMENT,
        "version": version_raw,
    }))
}

fn should_check_node_for_acpx(command: &str) -> bool {
    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| matches!(name, "acpx" | "npx" | "node"))
        .unwrap_or(false)
}

fn parse_node_version(raw: &str) -> Option<(u64, u64, u64)> {
    let trimmed = raw.trim().trim_start_matches('v');
    let mut parts = trimmed.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts
        .next()
        .unwrap_or("0")
        .split(|ch: char| !ch.is_ascii_digit())
        .next()
        .unwrap_or("0")
        .parse()
        .ok()?;
    Some((major, minor, patch))
}

fn acpx_control_metadata(
    command: &str,
    base_args: &[String],
    agent: &str,
    action: &str,
    session: Option<&str>,
    run_mode: AcpxRunMode,
) -> Value {
    if run_mode != AcpxRunMode::Session {
        return json!({
            "supported": false,
            "reason": "one-shot exec mode does not create or reuse an acpx session",
        });
    }
    let mut argv = Vec::with_capacity(base_args.len() + 5);
    argv.push(command.to_string());
    argv.extend(base_args.iter().cloned());
    argv.push(agent.to_string());
    argv.push(action.to_string());
    if let Some(session) = session {
        argv.push("-s".to_string());
        argv.push(session.to_string());
    }
    json!({
        "supported": true,
        "argv": argv,
    })
}

fn map_acpx_event(dispatch_id: &str, agent: &str, event: &Value) -> Option<Value> {
    let kind = acpx_event_kind(event);
    let lower = kind.to_ascii_lowercase();
    let mapped_event = if lower.contains("thinking") || lower.contains("message") {
        "acpx_message"
    } else if lower.contains("tool") {
        "acpx_tool_event"
    } else if lower.contains("diff") || lower.contains("edit") {
        "acpx_diff_event"
    } else if lower.contains("permission")
        || lower.contains("approval")
        || lower.contains("denial")
        || lower.contains("deny")
    {
        "acpx_permission_event"
    } else if lower.contains("final")
        || lower.contains("end_turn")
        || lower.contains("result")
        || lower.contains("complete")
    {
        "acpx_final_event"
    } else if lower.contains("cancel")
        || lower.contains("status")
        || lower.contains("dead")
        || lower.contains("error")
    {
        "acpx_lifecycle_event"
    } else {
        return None;
    };
    let target = if mapped_event == "acpx_message" {
        "progress"
    } else {
        "trajectory"
    };
    Some(json!({
        "event": mapped_event,
        "dispatch_id": dispatch_id,
        "agent": agent,
        "acpx_event": kind,
        "text": extract_event_text(event),
        "tachi_target": target,
        "timestamp": Utc::now().to_rfc3339(),
    }))
}

fn acpx_event_kind(event: &Value) -> String {
    ["event", "type", "kind", "name"]
        .iter()
        .find_map(|key| event.get(*key).and_then(Value::as_str))
        .unwrap_or("unknown")
        .to_string()
}

fn is_final_acpx_event(event: &Value) -> bool {
    let lower = acpx_event_kind(event).to_ascii_lowercase();
    lower.contains("final")
        || lower.contains("end_turn")
        || lower.contains("result")
        || lower.contains("complete")
}

fn extract_event_text(event: &Value) -> Option<String> {
    for key in [
        "final_response",
        "result",
        "message",
        "content",
        "text",
        "output",
    ] {
        if let Some(text) = event.get(key).and_then(Value::as_str) {
            if !text.trim().is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvRestore {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvRestore {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, old }
        }

        fn remove(key: &'static str) -> Self {
            let old = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, old }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            if let Some(old) = &self.old {
                std::env::set_var(self.key, old);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn params() -> TachiDispatchParams {
        TachiDispatchParams {
            agent: Some("codex".to_string()),
            profile: None,
            task: "noop".to_string(),
            cwd: Some("/tmp/project".to_string()),
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 5,
            permission_profile: None,
            allowed_tools: Vec::new(),
            max_turns: None,
            sandbox: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            command: Vec::new(),
            harness_transport: Some("acpx".to_string()),
            harness_server_url: None,
            project: None,
            stage: None,
            credential_profiles: Vec::new(),
            issue_ref: None,
            pr_ref: None,
            flow_id: None,
            tool_profile: None,
            auto_capability_bundle: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
        }
    }

    #[test]
    fn acpx_command_spec_uses_file_prompt_and_read_approved_posture() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _cmd = EnvRestore::set("TACHI_ACPX_COMMAND", "python3");
        let _args = EnvRestore::set("TACHI_ACPX_ARGS", "-m acpx");
        let _agent = EnvRestore::remove("TACHI_ACPX_AGENT");
        let _mode = EnvRestore::remove("TACHI_ACPX_RUN_MODE");
        let _legacy_mode = EnvRestore::remove("TACHI_ACPX_SESSION_MODE");
        let _session = EnvRestore::remove("TACHI_ACPX_SESSION");
        let params = params();
        let spec = build_acpx_command_spec(&params, "codex", Path::new("/tmp/run/prompt.md"))
            .expect("spec should build");

        assert_eq!(spec.command, "python3");
        assert!(spec.args.windows(2).any(|pair| pair == ["-m", "acpx"]));
        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["--cwd", "/tmp/project"]));
        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["--format", "json"]));
        assert!(spec.args.iter().any(|arg| arg == "--json-strict"));
        assert!(spec.args.iter().any(|arg| arg == "--approve-reads"));
        assert!(spec.args.windows(2).any(|pair| pair == ["codex", "exec"]));
        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["--file", "/tmp/run/prompt.md"]));
        assert_eq!(spec.metadata["permissions"], json!("approve-reads"));
        assert_eq!(spec.metadata["mode"], json!("exec"));
        assert_eq!(spec.metadata["session"], json!(null));
        assert_eq!(spec.metadata["readiness"]["node"]["checked"], json!(false));
        assert_eq!(
            spec.metadata["controls"]["status"]["supported"],
            json!(false)
        );
    }

    #[test]
    fn acpx_session_mode_maps_card_hint_to_named_session_and_controls() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _cmd = EnvRestore::set("TACHI_ACPX_COMMAND", "python3");
        let _args = EnvRestore::set("TACHI_ACPX_ARGS", "-m acpx");
        let _agent = EnvRestore::remove("TACHI_ACPX_AGENT");
        let _mode = EnvRestore::set("TACHI_ACPX_RUN_MODE", "session");
        let _legacy_mode = EnvRestore::remove("TACHI_ACPX_SESSION_MODE");
        let _session = EnvRestore::remove("TACHI_ACPX_SESSION");
        let mut params = params();
        params.stage = Some("review".to_string());
        let spec = build_acpx_command_spec(&params, "codex", Path::new("/tmp/run/prompt.md"))
            .expect("spec should build");

        assert!(spec.args.windows(2).any(|pair| pair == ["-s", "raven"]));
        assert!(!spec.args.windows(2).any(|pair| pair == ["codex", "exec"]));
        assert_eq!(spec.metadata["mode"], json!("session"));
        assert_eq!(spec.metadata["session"], json!("raven"));
        assert_eq!(spec.metadata["session_source"], json!("stage"));
        assert_eq!(
            spec.metadata["controls"]["status"]["supported"],
            json!(true)
        );
        assert!(spec.metadata["controls"]["status"]["argv"]
            .as_array()
            .expect("status argv")
            .iter()
            .any(|arg| arg.as_str() == Some("status")));
        assert!(spec.metadata["controls"]["cancel"]["argv"]
            .as_array()
            .expect("cancel argv")
            .iter()
            .any(|arg| arg.as_str() == Some("cancel")));
    }

    #[test]
    fn acpx_rejects_full_permission_profile() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _cmd = EnvRestore::set("TACHI_ACPX_COMMAND", "python3");
        let _mode = EnvRestore::remove("TACHI_ACPX_RUN_MODE");
        let _legacy_mode = EnvRestore::remove("TACHI_ACPX_SESSION_MODE");
        let _session = EnvRestore::remove("TACHI_ACPX_SESSION");
        let mut params = params();
        params.permission_profile = Some("full".to_string());
        let _allow = EnvRestore::set("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE", "true");
        let err = build_acpx_command_spec(&params, "codex", Path::new("/tmp/run/prompt.md"))
            .expect_err("full should not map to acpx approve-all");
        assert!(err.contains("never maps acpx to --approve-all"), "{err}");
    }

    #[test]
    fn acpx_event_mapping_persists_raw_events_and_extracts_final_response() {
        let dir = tempfile::tempdir().expect("tempdir");
        let trajectory = dir.path().join("trajectory.jsonl");
        crate::utils::write_owner_only_file_atomic(&trajectory, b"").expect("trajectory");
        let output = r#"{"type":"message","message":"working"}
{"event":"tool_call_start","tool_name":"bash"}
{"event":"end_turn","final_response":"done"}
"#;
        let summary =
            persist_acpx_events_and_map(dir.path(), &trajectory, "dispatch-1", "codex", output)
                .expect("events should persist");
        assert_eq!(summary.mapped_events, 3);
        assert_eq!(summary.final_response.as_deref(), Some("done"));
        assert!(summary.events_file.ends_with(ACPX_EVENTS_FILE));
        let raw = std::fs::read_to_string(summary.events_file).expect("raw events");
        assert!(raw.contains("tool_call_start"));
        let progress =
            std::fs::read_to_string(dir.path().join("progress.jsonl")).expect("progress events");
        assert!(progress.contains("acpx_message"));
        let trajectory = std::fs::read_to_string(trajectory).expect("trajectory events");
        assert!(trajectory.contains("acpx_tool_event"));
        assert!(trajectory.contains("acpx_final_event"));
    }
}
