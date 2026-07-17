use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::super::launcher::resolve_permission_profile;
use super::session::{native_acp_session_key, resolve_native_acp_session};
use super::{NativeAcpRunMode, NativeAcpRunSpec, ACP_STREAM_FILE};
use crate::tool_params::TachiDispatchParams;

/// Single-sourced from `tachi_dispatch::transport_kind` (#894 S2d) — same
/// rationale as `is_acpx_transport`.
pub(in crate::dispatch_ops) fn is_native_acp_transport(transport: &str) -> bool {
    tachi_dispatch::transport_kind(transport) == tachi_dispatch::TransportKind::AcpNative
}

pub(in crate::dispatch_ops) fn build_native_acp_run_spec(
    home: &Path,
    params: &TachiDispatchParams,
    agent: &str,
    prompt: &str,
) -> Result<NativeAcpRunSpec, String> {
    // Defense-in-depth (#894 S0 round 2): runs BEFORE command resolution/
    // trust/availability preflight below. The primary fix is hoisting this
    // same check to the dispatch entry point (`dispatch.rs::handle_tachi_dispatch`,
    // before Stage 1/ClaudePool and before any builder runs at all); this
    // builder-local copy stays as a second, independent gate in case a caller
    // reaches this function through a path that bypassed the entry check.
    tachi_dispatch::reject_unsupported_sandbox("acp-native", params.sandbox.as_deref())?;

    let (command, args, command_source) = resolve_native_acp_command(params, agent)?;
    if !crate::utils::is_trusted_command(&command) {
        return Err(format!(
            "native ACP command '{}' is not trusted. Use agent='custom' with a trusted command, or set TACHI_ACP_NATIVE_COMMAND to a trusted binary.",
            command
        ));
    }
    if !command_available(&command) {
        return Err(format!(
            "native ACP transport requested, but command '{}' was not found on PATH",
            command
        ));
    }

    let cwd = params
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let cwd = absolutize_cwd(&cwd);
    let mode = resolve_native_acp_run_mode()?;
    let permission_label = native_acp_permission_label(resolve_permission_profile(params)?)?;
    let session = if mode == NativeAcpRunMode::Session {
        Some(resolve_native_acp_session(params)?)
    } else {
        None
    };
    let session_key = native_acp_session_key(agent, &command, &args, &cwd, session.as_ref());
    let (session_record_path, session_distill_path) = if mode == NativeAcpRunMode::Session {
        let record_id = crate::utils::stable_hash(
            &serde_json::to_string(&session_key)
                .map_err(|err| format!("serialize native ACP session key: {err}"))?,
        );
        let base = home.join("sessions").join("acp");
        (
            Some(base.join(format!("{record_id}.json"))),
            Some(base.join(format!("{record_id}.md"))),
        )
    } else {
        (None, None)
    };

    let session_name = session.as_ref().map(|session| session.name.clone());
    let session_source = session.as_ref().map(|session| session.source);
    let metadata = json!({
        "execution_backend": "acp_native",
        "agent": agent,
        "command": command,
        "args": args,
        "command_source": command_source,
        "mode": mode.as_str(),
        "session": session_name,
        "session_source": session_source,
        "session_key": session_key,
        "session_record": session_record_path.as_ref().map(|path| path.to_string_lossy().to_string()),
        "session_distill": session_distill_path.as_ref().map(|path| path.to_string_lossy().to_string()),
        "cwd": cwd.to_string_lossy(),
        "permissions": permission_label,
        "client_capabilities": {
            "fs": {
                "readTextFile": false,
                "writeTextFile": false
            },
            "terminal": false
        },
        "raw_stream_file": ACP_STREAM_FILE,
    });

    Ok(NativeAcpRunSpec {
        command,
        args,
        cwd,
        prompt: prompt.to_string(),
        mode,
        permission_label: permission_label.to_string(),
        session,
        session_record_path,
        session_distill_path,
        metadata,
        env: HashMap::new(),
    })
}

impl NativeAcpRunMode {
    fn as_str(self) -> &'static str {
        match self {
            NativeAcpRunMode::OneShot => "oneshot",
            NativeAcpRunMode::Session => "session",
        }
    }
}

fn resolve_native_acp_command(
    params: &TachiDispatchParams,
    agent: &str,
) -> Result<(String, Vec<String>, &'static str), String> {
    if agent == "custom" && !params.command.is_empty() {
        let command = params.command[0].trim().to_string();
        let args = params.command[1..].to_vec();
        if command.is_empty() {
            return Err("agent='custom' native ACP command cannot be empty".to_string());
        }
        return Ok((command, args, "dispatch.command"));
    }

    let command = std::env::var("TACHI_ACP_NATIVE_COMMAND")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "harness_transport='acp-native' requires TACHI_ACP_NATIVE_COMMAND, or agent='custom' with a command array naming an ACP stdio adapter"
                .to_string()
        })?;
    Ok((command, env_args("TACHI_ACP_NATIVE_ARGS"), "env"))
}

fn resolve_native_acp_run_mode() -> Result<NativeAcpRunMode, String> {
    let raw = std::env::var("TACHI_ACP_NATIVE_RUN_MODE")
        .ok()
        .or_else(|| std::env::var("TACHI_ACP_RUN_MODE").ok())
        .unwrap_or_else(|| "session".to_string());
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "session" | "persistent" | "named" => Ok(NativeAcpRunMode::Session),
        "exec" | "oneshot" | "one-shot" | "one_shot" => Ok(NativeAcpRunMode::OneShot),
        other => Err(format!(
            "unsupported native ACP run mode '{other}'. Use TACHI_ACP_NATIVE_RUN_MODE=session or oneshot."
        )),
    }
}

fn native_acp_permission_label(profile: &str) -> Result<&'static str, String> {
    match profile {
        "default" => Ok("approve-reads"),
        "allowlist" => Err(
            "permission_profile 'allowlist' is not supported by native ACP yet; use default read-approved posture."
                .to_string(),
        ),
        "full" => Err(
            "permission_profile 'full' is not supported by native ACP; Tachi does not map ACP to approve-all yet."
                .to_string(),
        ),
        other => Err(format!(
            "unsupported permission_profile '{other}' for native ACP backend"
        )),
    }
}

fn env_args(name: &str) -> Vec<String> {
    std::env::var(name)
        .ok()
        .map(|value| {
            value
                .split_whitespace()
                .filter(|arg| !arg.trim().is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
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

fn absolutize_cwd(cwd: &Path) -> PathBuf {
    if cwd.is_absolute() {
        return cwd.to_path_buf();
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> TachiDispatchParams {
        TachiDispatchParams {
            agent: Some("custom".to_string()),
            profile: None,
            task: "noop".to_string(),
            execution_level: None,
            cwd: Some("/tmp/project".to_string()),
            env_id: None,
            unmanaged_cwd: None,
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 5,
            permission_profile: None,
            allowed_tools: Vec::new(),
            completion_predicate: None,
            max_turns: None,
            sandbox: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            command: vec!["python3".to_string()],
            harness_transport: Some("acp-native".to_string()),
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
            verbose: None,
            inject_card: None,
        }
    }

    #[test]
    fn native_acp_fails_closed_on_sandbox_request() {
        // Native ACP has no `--sandbox`-equivalent knob; a caller-supplied
        // sandbox request must fail closed with a receipt naming the backend
        // and requested level, never be silently dropped (#894 S0).
        let mut params = params();
        params.sandbox = Some("workspace-write".to_string());

        let err = build_native_acp_run_spec(
            Path::new("/tmp/tachi-home-test"),
            &params,
            "custom",
            "hello",
        )
        .expect_err("native ACP has no sandbox concept and must fail closed");
        assert!(
            err.contains("acp-native") && err.contains("workspace-write"),
            "receipt must name backend + requested level: {err}"
        );
        assert!(err.contains("fail-closed"), "{err}");
    }

    #[test]
    fn native_acp_builds_spec_without_sandbox() {
        let params = params();
        let spec = build_native_acp_run_spec(
            Path::new("/tmp/tachi-home-test"),
            &params,
            "custom",
            "hello",
        )
        .expect("no sandbox requested should build cleanly");
        assert_eq!(spec.command, "python3");
    }
}
