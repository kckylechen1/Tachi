use super::binary::resolve_claude_binary;
use super::cleanup::cleanup_runs_dir;
use super::envelope::parse_claude_json_envelope;
use super::files::{
    sanitize_label, write_owner_only_file_blocking, write_run_file_blocking,
    write_run_status_file_blocking,
};
use super::*;

impl super::ClaudePool {
    /// Construct a pool with `max_concurrent` permits writing into
    /// `<tachi_home>/foundry-runs/`. Honours `CLAUDE_POOL_TIMEOUT_SECS`
    /// and `CLAUDE_BIN` env overrides.
    pub fn new(max_concurrent: usize) -> Self {
        Self::new_in_app_home(max_concurrent, crate::path_utils::tachi_home())
    }

    pub(crate) fn new_in_app_home(max_concurrent: usize, app_home: impl Into<PathBuf>) -> Self {
        let runs_dir = app_home.into().join("foundry-runs");
        if let Err(error) = std::fs::create_dir_all(&runs_dir) {
            tracing::warn!(
                runs_dir = %runs_dir.display(),
                error = %error,
                "failed to create ClaudePool foundry-runs directory"
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(error) =
                std::fs::set_permissions(&runs_dir, std::fs::Permissions::from_mode(0o700))
            {
                tracing::warn!(
                    runs_dir = %runs_dir.display(),
                    error = %error,
                    "failed to restrict ClaudePool foundry-runs permissions"
                );
            }
        }

        let timeout_secs = std::env::var("CLAUDE_POOL_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        let binary = resolve_claude_binary();

        Self {
            sem: Arc::new(Semaphore::new(max_concurrent.max(1))),
            runs_dir,
            timeout: Duration::from_secs(timeout_secs),
            binary,
        }
    }

    pub fn runs_dir(&self) -> &Path {
        &self.runs_dir
    }

    /// Invoke Claude CLI with `prompt` (which already includes any system
    /// preamble). `label` is a short ASCII slug used in the per-call dir
    /// name; non-ASCII / unsafe chars are replaced with `_`.
    pub async fn call(&self, label: &str, prompt: &str) -> Result<ClaudeCallOutcome, String> {
        let permit = self
            .sem
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| format!("claude_pool semaphore closed: {e}"))?;

        let ts = Utc::now().format("%Y%m%dT%H%M%S%.3f").to_string();
        let safe_label = sanitize_label(label);
        let run_dir = self.runs_dir.join(format!("{safe_label}-{ts}"));
        if let Err(e) = tokio::fs::create_dir_all(&run_dir).await {
            return Err(format!(
                "claude_pool create run dir {}: {e}",
                run_dir.display()
            ));
        }

        let prompt_path = run_dir.join("prompt.md");
        if let Err(e) =
            write_owner_only_file_blocking(prompt_path.clone(), prompt.as_bytes().to_vec()).await
        {
            return Err(format!("claude_pool write {}: {e}", prompt_path.display()));
        }

        let started_at = Utc::now().to_rfc3339();
        let started = Instant::now();
        let result = self.run_claude_cli(prompt).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let finished_at = Utc::now().to_rfc3339();

        // Drop permit before fs writes — file I/O shouldn't hold a CLI slot.
        drop(permit);

        match result {
            Ok(text) => {
                if let Err(err) =
                    write_run_file_blocking(run_dir.join("result.md"), text.clone()).await
                {
                    tracing::warn!("claude_pool failed to write result.md: {err}");
                }
                if let Err(err) = write_run_status_file_blocking(
                    run_dir.clone(),
                    json!({
                        "status": "success",
                        "started_at": started_at,
                        "finished_at": finished_at,
                        "elapsed_ms": elapsed_ms,
                        "label": label,
                        "bytes": text.len(),
                    }),
                )
                .await
                {
                    tracing::warn!("claude_pool failed to write status.json: {err}");
                }
                Ok(ClaudeCallOutcome { text })
            }
            Err(err) => {
                if let Err(write_err) =
                    write_run_file_blocking(run_dir.join("result.md"), err.clone()).await
                {
                    tracing::warn!("claude_pool failed to write error result.md: {write_err}");
                }
                if let Err(write_err) = write_run_status_file_blocking(
                    run_dir.clone(),
                    json!({
                        "status": "failed",
                        "started_at": started_at,
                        "finished_at": finished_at,
                        "elapsed_ms": elapsed_ms,
                        "label": label,
                        "error": err,
                    }),
                )
                .await
                {
                    tracing::warn!("claude_pool failed to write failed status.json: {write_err}");
                }
                Err(err)
            }
        }
    }

    /// Spawn `claude -p --output-format json [--dangerously-skip-permissions]`
    /// with the prompt on stdin. Honors `TACHI_CLAUDE_SKIP_PERMISSIONS`:
    /// `true`/`1` enables `--dangerously-skip-permissions`; any other value,
    /// disables it. When unset, non-interactive callers default to
    /// `--dangerously-skip-permissions`; interactive callers keep prompts enabled.
    pub(super) async fn run_claude_cli(&self, prompt: &str) -> Result<String, String> {
        let skip_perms = std::env::var("TACHI_CLAUDE_SKIP_PERMISSIONS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or_else(|_| !std::io::IsTerminal::is_terminal(&std::io::stdin()));
        let binary = self.binary.as_ref().map_err(|err| err.clone())?;
        let mut cmd = Command::new(binary);
        cmd.arg("-p").arg("--output-format").arg("json");
        if skip_perms {
            cmd.arg("--dangerously-skip-permissions");
        }
        cmd.kill_on_drop(true);
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("claude cli spawn failed ({binary}): {e}"))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .map_err(|e| format!("claude cli stdin write: {e}"))?;
            stdin
                .shutdown()
                .await
                .map_err(|e| format!("claude cli stdin close: {e}"))?;
        }

        let output = match timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Err(format!("claude cli wait failed: {e}")),
            Err(_) => {
                return Err(format!(
                    "claude cli timed out after {}s",
                    self.timeout.as_secs()
                ));
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "claude cli exited {}: {}",
                output.status.code().unwrap_or(-1),
                stderr.chars().take(800).collect::<String>()
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let text = parse_claude_json_envelope(&stdout)?;
        if text.trim().is_empty() {
            return Err("claude cli returned empty result".to_string());
        }
        Ok(text)
    }

    /// Walk `runs_dir` and remove directories older than the retention
    /// policy (7d success / 30d failed). Returns `(removed, scanned)`.
    pub fn cleanup_expired(&self) -> (usize, usize) {
        cleanup_runs_dir(&self.runs_dir, SystemTime::now())
    }
}
