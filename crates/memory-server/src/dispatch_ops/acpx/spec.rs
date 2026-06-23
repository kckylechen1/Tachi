use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::process::Command;

use crate::dispatch_ops::subprocess::resolve_permission_profile;
use crate::tool_params::TachiDispatchParams;

use super::types::{
    AcpxCommandSpec, AcpxRunMode, AcpxSession, ACPX_NODE_MIN_VERSION, ACPX_NODE_REQUIREMENT,
};

pub(in crate::dispatch_ops) fn is_acpx_transport(transport: &str) -> bool {
    matches!(
        transport.trim().to_ascii_lowercase().as_str(),
        "acpx" | "acp"
    )
}

pub(in crate::dispatch_ops) fn prepare_acpx_prompt(
    prompt_file: &Path,
    prompt: &str,
) -> Result<PathBuf, String> {
    crate::utils::write_owner_only_file_atomic(prompt_file, prompt.as_bytes())
        .map_err(|err| format!("Failed to write acpx prompt file: {err}"))?;
    Ok(prompt_file.to_path_buf())
}

pub(in crate::dispatch_ops) fn build_acpx_command_spec(
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

pub(in crate::dispatch_ops) fn build_acpx_command(spec: &AcpxCommandSpec) -> Command {
    let mut cmd = Command::new(&spec.command);
    for arg in &spec.args {
        cmd.arg(arg);
    }
    cmd
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

pub(super) fn command_available(command: &str) -> bool {
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
