use super::dispatch::DispatchResult;
use std::time::Duration;
use tokio::process::Command;

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
            // drops the inner Child and triggers kill_on_drop.
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
    tachi_dispatch::tail_chars(text, max_chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_agent_subprocess_closes_child_stdin() {
        let cmd = Command::new("/bin/cat");

        let result = run_agent_subprocess(cmd, Duration::from_secs(5))
            .await
            .expect("stdin reader should observe EOF and exit");

        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.output, "");
    }
}
