use super::*;

use super::dispatch::DispatchResult;

// ─── Agent subprocess builders ───────────────────────────────────────────────

/// Resolve the effective permission profile.
///
/// Defaults to `"default"` (Claude prompts for confirmations; Codex uses the
/// configured sandbox). Callers must explicitly pass `permission_profile:
/// "full"` to opt into `--dangerously-skip-permissions` /
/// `--dangerously-bypass-approvals-and-sandbox`. This is a deliberate safe
/// default: the previous `"full"` default gave any caller of `tachi_dispatch`
/// unsandboxed autonomous execution.
pub(super) fn resolve_permission_profile(params: &TachiDispatchParams) -> &str {
    params.permission_profile.as_deref().unwrap_or("default")
}

pub(super) fn build_claude_command(
    params: &TachiDispatchParams,
    prompt: &str,
    mcp_config_path: Option<&PathBuf>,
) -> Command {
    let mut cmd = Command::new("claude");
    cmd.arg("-p"); // print mode
    cmd.arg("--output-format").arg("json");

    // Permission profile
    let profile = resolve_permission_profile(params);
    match profile {
        "full" => {
            cmd.arg("--dangerously-skip-permissions");
        }
        "allowlist" if !params.allowed_tools.is_empty() => {
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
    cmd
}

pub(super) fn build_codex_command(
    params: &TachiDispatchParams,
    prompt: &str,
    _mcp_config_path: Option<&PathBuf>,
) -> Command {
    let mut cmd = Command::new("codex");
    cmd.arg("exec"); // non-interactive subcommand

    // Permission profile
    let profile = resolve_permission_profile(params);
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
    cmd
}

pub(super) fn build_grok_command(
    params: &TachiDispatchParams,
    prompt: &str,
    mcp_config_path: Option<&PathBuf>,
) -> Command {
    let mut cmd = Command::new("grok");
    cmd.arg("-p").arg(prompt);
    cmd.arg("--output-format").arg("json");

    let profile = resolve_permission_profile(params);
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
        cmd.arg("--cwd").arg(cwd);
    }
    cmd
}

pub(super) fn build_kimi_command(params: &TachiDispatchParams, prompt: &str) -> Command {
    let mut cmd = Command::new("kimi");
    cmd.arg("-p").arg(prompt);
    cmd.arg("--output-format").arg("json");

    let profile = resolve_permission_profile(params);
    if profile == "full" {
        cmd.arg("-y");
    }

    if let Some(ref model) = params.model {
        cmd.arg("-m").arg(model);
    }

    if let Some(ref cwd) = params.cwd {
        cmd.current_dir(cwd);
    }
    cmd
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
            "Command '{}' is not in the trusted allowlist. Allowed: npx, node, bun, deno, python3, python, uv, cargo, rustup, docker, podman, tachi, or paths under /opt/homebrew/, /usr/local/bin/, ~/.cargo/bin/, ~/.local/bin/",
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
