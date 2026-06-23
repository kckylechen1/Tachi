use super::dispatch::DispatchResult;
use crate::tool_params::TachiDispatchParams;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

// ─── Agent subprocess builders ───────────────────────────────────────────────

/// Resolve the effective permission profile.
///
/// Defaults to `"default"` (Claude prompts for confirmations; Codex uses the
/// configured sandbox). The `"allowlist"` profile is also accepted when
/// `allowed_tools` is non-empty. The `"full"` profile is rejected unless the
/// administrator explicitly opts in via `TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE=true`.
pub(super) fn resolve_permission_profile(params: &TachiDispatchParams) -> Result<&str, String> {
    let profile = params.permission_profile.as_deref().unwrap_or("default");
    match profile {
        "default" => Ok("default"),
        "allowlist" if !params.allowed_tools.is_empty() => Ok("allowlist"),
        "allowlist" => Err(
            "permission_profile 'allowlist' requires at least one allowed_tool".to_string(),
        ),
        "full" => {
            let allowed = std::env::var("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if allowed {
                Ok("full")
            } else {
                Err(
                    "permission_profile 'full' requires explicit opt-in via TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE=true"
                        .to_string(),
                )
            }
        }
        other => Err(format!(
            "unsupported permission_profile '{other}'; allowed: default, allowlist, full (with opt-in)"
        )),
    }
}

fn reject_non_claude_allowlist(agent: &str, profile: &str) -> Result<(), String> {
    if profile == "allowlist" {
        return Err(format!(
            "permission_profile 'allowlist' is only supported for agent 'claude', not '{agent}'"
        ));
    }
    Ok(())
}

pub(super) fn build_claude_command(
    params: &TachiDispatchParams,
    prompt: &str,
    mcp_config_path: Option<&PathBuf>,
) -> Result<Command, String> {
    let mut cmd = Command::new("claude");
    cmd.arg("-p"); // print mode
    cmd.arg("--output-format").arg("json");

    // Permission profile
    let profile = resolve_permission_profile(params)?;
    match profile {
        "full" => {
            cmd.arg("--dangerously-skip-permissions");
        }
        "allowlist" => {
            for tool in &params.allowed_tools {
                cmd.arg("--allowedTools").arg(tool);
            }
        }
        _ => {} // "default" — no permission flags
    }

    // Max turns
    if let Some(turns) = params.max_turns {
        cmd.arg("--max-turns").arg(turns.to_string());
    }

    // Model override
    if let Some(ref model) = params.model {
        cmd.arg("--model").arg(model);
    }

    // Prompt MUST come before --mcp-config because --mcp-config <configs...>
    // is a varadic arg that swallows all subsequent positional args.
    cmd.arg(prompt);

    // MCP config injection (after prompt to avoid swallowing)
    if let Some(path) = mcp_config_path {
        cmd.arg("--mcp-config").arg(path);
    }

    if let Some(ref cwd) = params.cwd {
        cmd.current_dir(cwd);
    }
    Ok(cmd)
}

pub(super) fn build_codex_command(
    params: &TachiDispatchParams,
    prompt: &str,
    _mcp_config_path: Option<&PathBuf>,
) -> Result<Command, String> {
    let mut cmd = Command::new("codex");
    cmd.arg("exec"); // non-interactive subcommand

    // Permission profile
    let profile = resolve_permission_profile(params)?;
    reject_non_claude_allowlist("codex", profile)?;
    if profile == "full" {
        cmd.arg("--dangerously-bypass-approvals-and-sandbox");
    } else {
        let sandbox = params.sandbox.as_deref().unwrap_or("workspace-write");
        cmd.arg("--sandbox").arg(sandbox);
    }

    cmd.arg("--json");

    if let Some(turns) = params.max_turns {
        // Codex doesn't have a direct --max-turns; use -c config override
        cmd.arg("-c").arg(format!("max_turns={turns}"));
    }

    if let Some(ref model) = params.model {
        cmd.arg("-m").arg(model);
    }

    cmd.arg(prompt);

    if let Some(ref cwd) = params.cwd {
        cmd.arg("-C").arg(cwd);
    }
    Ok(cmd)
}

pub(super) fn build_grok_command(
    params: &TachiDispatchParams,
    prompt: &str,
    mcp_config_path: Option<&PathBuf>,
) -> Result<Command, String> {
    let mut cmd = Command::new("grok");
    cmd.arg("-p").arg(prompt);
    cmd.arg("--output-format").arg("json");

    let profile = resolve_permission_profile(params)?;
    reject_non_claude_allowlist("grok", profile)?;
    if profile == "full" {
        cmd.arg("--permission-mode").arg("bypassPermissions");
    }

    if let Some(turns) = params.max_turns {
        cmd.arg("--max-turns").arg(turns.to_string());
    }

    if let Some(ref model) = params.model {
        cmd.arg("-m").arg(model);
    }

    if let Some(path) = mcp_config_path {
        cmd.arg("--mcp-config").arg(path);
    }

    if let Some(ref cwd) = params.cwd {
        cmd.current_dir(std::path::Path::new(cwd));
    }
    Ok(cmd)
}

pub(super) fn build_kimi_command(
    params: &TachiDispatchParams,
    prompt: &str,
) -> Result<Command, String> {
    let profile = resolve_permission_profile(params)?;
    reject_non_claude_allowlist("kimi", profile)?;
    let mut cmd = Command::new("kimi");
    for arg in kimi_command_args(params, prompt, profile) {
        cmd.arg(arg);
    }

    if let Some(ref cwd) = params.cwd {
        cmd.current_dir(cwd);
    }
    Ok(cmd)
}

fn kimi_command_args(params: &TachiDispatchParams, prompt: &str, profile: &str) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        prompt.to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
    ];
    if profile == "full" {
        args.push("-y".to_string());
    }

    if let Some(ref model) = params.model {
        args.push("-m".to_string());
        args.push(model.clone());
    }

    args
}

pub(super) fn build_custom_command(
    params: &TachiDispatchParams,
    prompt: &str,
) -> Result<Command, String> {
    if params.command.is_empty() {
        return Err("agent='custom' requires a non-empty 'command' array".to_string());
    }
    let binary = &params.command[0];
    if !crate::utils::is_trusted_command(binary) {
        return Err(format!(
            "Command '{}' is not in the trusted allowlist. Allowed: npx, node, bun, deno, python3, python, uv, cargo, rustup, docker, podman, tachi, opencode, acpx, or paths under /opt/homebrew/, /usr/local/bin/, ~/.cargo/bin/, ~/.local/bin/",
            binary
        ));
    }
    let mut cmd = Command::new(binary);
    for arg in &params.command[1..] {
        cmd.arg(arg);
    }
    cmd.arg(prompt);
    if let Some(ref cwd) = params.cwd {
        cmd.current_dir(cwd);
    }
    Ok(cmd)
}

// ─── Execute subprocess ──────────────────────────────────────────────────────

pub(super) async fn run_agent_subprocess(
    mut cmd: Command,
    timeout: Duration,
) -> Result<DispatchResult, String> {
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    // If this task is dropped (daemon shutdown, spawning task cancelled, ...),
    // tokio kills the child via SIGKILL instead of leaving it orphaned.
    cmd.kill_on_drop(true);

    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn agent process: {e}"))?;

    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(res) => res.map_err(|e| format!("Agent process error: {e}"))?,
        Err(_) => {
            // Timeout fired. `wait_with_output` consumed `child`, but the
            // tokio::time::timeout cancellation path drops the future, which
            // drops the inner Child — that triggers kill_on_drop and sends
            // SIGKILL. To be explicit and avoid any future tokio behavior
            // change, also flag the error clearly so the watchdog treats
            // this as a failure rather than a crash.
            return Err(format!(
                "Agent process timed out after {}s (killed)",
                timeout.as_secs()
            ));
        }
    };

    let exit_code = output.status.code();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let output_text = if stdout.is_empty() && !stderr.is_empty() {
        stderr
    } else if !stderr.is_empty() {
        format!("{}\n\n--- stderr ---\n{}", stdout, stderr)
    } else {
        stdout
    };

    Ok(DispatchResult {
        output: output_text,
        exit_code,
    })
}

pub(super) fn tail_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars().rev().take(max_chars).collect::<Vec<_>>();
    chars.reverse();
    chars.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dispatch_params(agent: &str) -> TachiDispatchParams {
        TachiDispatchParams {
            agent: Some(agent.to_string()),
            profile: None,
            task: "noop".to_string(),
            cwd: None,
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
            harness_transport: None,
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
    fn kimi_command_uses_supported_stream_json_output() {
        let params = dispatch_params("kimi");
        let args = kimi_command_args(&params, "hello", "default");

        assert!(args.windows(2).any(|pair| pair == ["-p", "hello"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--output-format", "stream-json"]));
        assert!(
            !args
                .windows(2)
                .any(|pair| pair == ["--output-format", "json"]),
            "Kimi Code supports text/stream-json, not json: {args:?}"
        );
    }

    #[test]
    fn full_permission_profile_requires_opt_in_env() {
        let mut params = dispatch_params("claude");
        params.permission_profile = Some("full".to_string());

        let prev = std::env::var("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE").ok();
        std::env::remove_var("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE");

        assert!(
            resolve_permission_profile(&params).is_err(),
            "'full' should be rejected without opt-in env var"
        );
        assert!(
            build_claude_command(&params, "hello", None).is_err(),
            "command build should propagate full-profile rejection"
        );

        std::env::set_var("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE", "true");
        assert_eq!(
            resolve_permission_profile(&params).expect("opt-in should allow full"),
            "full"
        );
        assert!(
            build_claude_command(&params, "hello", None).is_ok(),
            "build should succeed after opt-in"
        );

        match prev {
            Some(v) => std::env::set_var("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE", v),
            None => std::env::remove_var("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE"),
        }
    }

    #[test]
    fn allowlist_permission_profile_rejected_for_non_claude_agents() {
        let mut params = dispatch_params("codex");
        params.permission_profile = Some("allowlist".to_string());
        params.allowed_tools = vec!["Read".to_string()];
        assert!(
            build_codex_command(&params, "hello", None).is_err(),
            "codex should reject allowlist profile"
        );

        params = dispatch_params("grok");
        params.permission_profile = Some("allowlist".to_string());
        params.allowed_tools = vec!["Read".to_string()];
        assert!(
            build_grok_command(&params, "hello", None).is_err(),
            "grok should reject allowlist profile"
        );

        params = dispatch_params("kimi");
        params.permission_profile = Some("allowlist".to_string());
        params.allowed_tools = vec!["Read".to_string()];
        assert!(
            build_kimi_command(&params, "hello").is_err(),
            "kimi should reject allowlist profile"
        );
    }

    #[tokio::test]
    async fn run_agent_subprocess_closes_child_stdin() {
        let mut cmd = Command::new("python3");
        cmd.arg("-c")
            .arg("import sys; data = sys.stdin.read(); print('stdin-eof:' + data)");

        let result = run_agent_subprocess(cmd, Duration::from_secs(5))
            .await
            .expect("stdin reader should observe EOF and exit");

        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.output.trim(), "stdin-eof:");
    }
}
