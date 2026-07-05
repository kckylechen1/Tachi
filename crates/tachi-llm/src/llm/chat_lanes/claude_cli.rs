use std::time::{Duration, Instant};

use super::super::provider_health::{
    ChatLane, ClaudeCliFailure, ClaudeCliFailureKind, ClaudeCliSkip, CLAUDE_CLI_FAILURE_COOLDOWN,
};

impl super::super::LlmClient {
    pub(in crate::llm) fn claude_cli_skip_at(&self, now: Instant) -> Option<ClaudeCliSkip> {
        let failure = self
            .claude_cli_failure
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let failure = failure?;
        let elapsed = now.saturating_duration_since(failure.failed_at);
        if elapsed >= CLAUDE_CLI_FAILURE_COOLDOWN {
            return None;
        }
        Some(ClaudeCliSkip {
            kind: failure.kind,
            remaining: CLAUDE_CLI_FAILURE_COOLDOWN.saturating_sub(elapsed),
        })
    }

    fn claude_cli_skip(&self) -> Option<ClaudeCliSkip> {
        self.claude_cli_skip_at(Instant::now())
    }

    pub(in crate::llm) fn record_claude_cli_success(&self) {
        self.claude_cli_failure
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }

    pub(in crate::llm) fn record_claude_cli_failure_at(&self, error: &str, failed_at: Instant) {
        let Some(kind) = ClaudeCliFailureKind::from_error(error) else {
            return;
        };
        *self
            .claude_cli_failure
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some(ClaudeCliFailure { kind, failed_at });
    }

    fn record_claude_cli_failure(&self, error: &str) {
        self.record_claude_cli_failure_at(error, Instant::now());
    }

    pub async fn call_reasoning_llm(
        &self,
        system: &str,
        user: &str,
        model: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, String> {
        if let Some(skip) = self.claude_cli_skip() {
            tracing::debug!(
                "skipping claude-cli reasoning after recent {} failure; retry in {}s",
                skip.kind.as_str(),
                skip.remaining.as_secs().max(1),
            );
        } else {
            // Try Claude Code CLI first for higher-quality reasoning.
            match Self::call_claude_cli(system, user).await {
                Ok(response) => {
                    self.record_claude_cli_success();
                    tracing::info!(
                        "reasoning via claude-cli succeeded ({} chars)",
                        response.len()
                    );
                    return Ok(response);
                }
                Err(e) => {
                    self.record_claude_cli_failure(&e);
                    tracing::warn!("claude-cli reasoning failed, falling back to lane LLM: {e}");
                }
            }
        }
        self.call_lane_llm(
            ChatLane::Reasoning,
            system,
            user,
            model,
            temperature,
            max_tokens,
        )
        .await
    }

    async fn call_claude_cli(system: &str, user: &str) -> Result<String, String> {
        use std::process::Stdio;
        use tokio::io::AsyncWriteExt;
        use tokio::process::Command;

        let prompt = format!("<system>\n{system}\n</system>\n\n{user}");
        let mut child = Command::new("claude")
            .arg("-p")
            .arg("--output-format")
            .arg("text")
            .arg("--max-turns")
            .arg("1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("claude cli spawn failed: {e}"))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .map_err(|e| format!("claude cli stdin write failed: {e}"))?;
        } else {
            return Err("claude cli stdin unavailable".to_string());
        }

        // Add timeout protection (5 minutes) to prevent indefinite blocking
        let output = tokio::time::timeout(Duration::from_secs(300), child.wait_with_output())
            .await
            .map_err(|_| "claude cli timeout after 5 minutes".to_string())?
            .map_err(|e| format!("claude cli failed: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "claude cli exited {}: {}",
                output.status.code().unwrap_or(-1),
                stderr.chars().take(500).collect::<String>()
            ));
        }

        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() {
            return Err("claude cli returned empty output".to_string());
        }
        Ok(text)
    }
}
