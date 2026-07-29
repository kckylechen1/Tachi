use std::time::{Duration, Instant};

use super::super::provider_health::{
    ChatLane, ClaudeCliFailure, ClaudeCliFailureKind, ClaudeCliSkip, ModelInvocationLaneV1,
    PersistedModelInvocationReceiptV1, CLAUDE_CLI_FAILURE_COOLDOWN,
};

/// #1071 fix-round (checkpoints 5/6): honest engine-receipt signal for
/// `ask`'s reasoning-lane synthesis. `used_fallback` is `true` whenever the
/// answer did NOT come from the higher-quality Claude CLI path (either the
/// CLI was skipped due to a recent-failure cooldown, or it failed and the
/// HTTP reasoning lane served the request instead) — the prior
/// `call_reasoning_llm` unconditionally reported `fallback: false` on any
/// `Ok(_)`, which was true for the CLI path but dishonest for a lane
/// fallback. `truncated` is `true` when the lane response's
/// `finish_reason == "length"` (the Claude CLI path has no equivalent
/// finish-reason signal to check, so `truncated` is always `false` there —
/// a documented scope gap, not a fabricated guarantee).
pub struct ReasoningOutcome {
    pub text: String,
    pub used_fallback: bool,
    pub truncated: bool,
    /// The closed, persisted-safe receipt for the engine that actually served
    /// this reasoning call. Claude CLI intentionally reports unknown model,
    /// token, and completion fields; an HTTP fallback carries the HTTP
    /// provider's actual receipt plus a fixed fallback marker.
    pub invocation: PersistedModelInvocationReceiptV1,
}

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
        self.call_reasoning_llm_with_receipt(system, user, model, temperature, max_tokens)
            .await
            .map(|outcome| outcome.text)
    }

    /// Same reasoning-capability call as [`Self::call_reasoning_llm`], but
    /// exposes whether the answer came from a fallback path and whether the
    /// provider truncated its response — see [`ReasoningOutcome`]'s doc
    /// comment for why (#1071 fix-round checkpoints 5/6). Added as a sibling
    /// rather than widening `call_reasoning_llm`'s existing
    /// `Result<String, String>` signature, which 3 other callers
    /// (`daily_pipeline::routing`/`health`, `continuity_ops::pipeline`) use
    /// and none of them need this receipt.
    pub async fn call_reasoning_llm_with_receipt(
        &self,
        system: &str,
        user: &str,
        model: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<ReasoningOutcome, String> {
        if let Some(skip) = self.claude_cli_skip() {
            tracing::debug!(
                "skipping claude-cli reasoning after recent {} failure; retry in {}s",
                skip.kind.as_str(),
                skip.remaining.as_secs().max(1),
            );
        } else {
            // Try Claude Code CLI first for higher-quality reasoning.
            let cli_started = Instant::now();
            match Self::call_claude_cli(system, user).await {
                Ok(response) => {
                    self.record_claude_cli_success();
                    tracing::info!(
                        "reasoning via claude-cli succeeded ({} chars)",
                        response.len()
                    );
                    return Ok(ReasoningOutcome {
                        text: response,
                        used_fallback: false,
                        truncated: false,
                        invocation: PersistedModelInvocationReceiptV1::claude_cli_reasoning(
                            cli_started.elapsed().as_millis(),
                        ),
                    });
                }
                Err(e) => {
                    self.record_claude_cli_failure(&e);
                    tracing::warn!("claude-cli reasoning failed, falling back to lane LLM: {e}");
                }
            }
        }
        let outcome = self
            .call_lane_llm(
                ChatLane::Reasoning,
                system,
                user,
                model,
                temperature,
                max_tokens,
            )
            .await?
            .into_generated(ModelInvocationLaneV1::Reasoning);
        let mut invocation = outcome.invocation;
        invocation.mark_claude_cli_to_provider_http_fallback();
        Ok(ReasoningOutcome {
            text: outcome.value,
            used_fallback: true,
            truncated: matches!(
                invocation.completion_status,
                super::super::provider_health::CompletionStatusV1::Truncated
            ),
            invocation,
        })
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
