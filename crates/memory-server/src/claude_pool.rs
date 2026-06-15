//! Bounded Claude Code CLI pool used by Foundry batch distill, dispatch V2
//! plan stage, Hub skill evolve, and Hub security scan.
//!
//! Each call:
//!   * acquires a semaphore permit (cap concurrent CLI invocations);
//!   * writes `prompt.md` / `result.md` / `status.json` under
//!     `~/.tachi/foundry-runs/<label>-<UTCts>/`;
//!   * spawns `claude -p --output-format json` by default; adds
//!     `--dangerously-skip-permissions` only when
//!     `TACHI_CLAUDE_SKIP_PERMISSIONS=true` (or `1`) is set. Daemon-driven
//!     callers (Foundry distill, Hub evolve) should export that variable
//!     explicitly. Leave it unset for interactive use so Claude's permission
//!     prompts are preserved;
//!   * enforces a wall-clock timeout (default 180s, env
//!     `CLAUDE_POOL_TIMEOUT_SECS`) and kills timed-out children on drop.
//!
//! The pool also exposes `cleanup_expired` which removes run directories
//! older than the policy (7d success / 30d failed) so the foundry runs
//! folder doesn't grow without bound.
//!
//! Callers use `pool_call_with_fallback` for a Claude CLI → raw API
//! degradation path. Each caller is expected to handle its own LLM-fallback
//! policy when `call()` returns Err.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::utils::{write_owner_only_file, write_owner_only_file_atomic};
use chrono::Utc;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::time::timeout;

/// Default bounded concurrency for Claude CLI invocations.
pub const DEFAULT_MAX_CONCURRENT: usize = 2;
/// Default per-call wall-clock budget.
pub const DEFAULT_TIMEOUT_SECS: u64 = 180;
/// Cleanup retention for successful runs.
pub const SUCCESS_RETENTION_DAYS: u64 = 7;
/// Cleanup retention for failed runs (kept longer for post-mortem).
pub const FAILED_RETENTION_DAYS: u64 = 30;

pub struct ClaudePool {
    sem: Arc<Semaphore>,
    runs_dir: PathBuf,
    timeout: Duration,
    binary: Result<String, String>,
}

#[derive(Debug)]
pub struct ClaudeCallOutcome {
    pub text: String,
}

impl ClaudePool {
    /// Construct a pool with `max_concurrent` permits writing into
    /// `<home>/.tachi/foundry-runs/`. Honours `CLAUDE_POOL_TIMEOUT_SECS`
    /// and `CLAUDE_BIN` env overrides.
    pub fn new(max_concurrent: usize) -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let runs_dir = home.join(".tachi").join("foundry-runs");
        let _ = std::fs::create_dir_all(&runs_dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&runs_dir, std::fs::Permissions::from_mode(0o700));
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
        if let Err(e) = write_owner_only_file(&prompt_path, prompt.as_bytes()) {
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
                if let Err(err) = write_run_file(&run_dir.join("result.md"), &text) {
                    tracing::warn!("claude_pool failed to write result.md: {err}");
                }
                if let Err(err) = crate::utils::write_run_status_file(
                    &run_dir,
                    &json!({
                        "status": "success",
                        "started_at": started_at,
                        "finished_at": finished_at,
                        "elapsed_ms": elapsed_ms,
                        "label": label,
                        "bytes": text.len(),
                    }),
                ) {
                    tracing::warn!("claude_pool failed to write status.json: {err}");
                }
                Ok(ClaudeCallOutcome { text })
            }
            Err(err) => {
                if let Err(write_err) = write_run_file(&run_dir.join("result.md"), &err) {
                    tracing::warn!("claude_pool failed to write error result.md: {write_err}");
                }
                if let Err(write_err) = crate::utils::write_run_status_file(
                    &run_dir,
                    &json!({
                        "status": "failed",
                        "started_at": started_at,
                        "finished_at": finished_at,
                        "elapsed_ms": elapsed_ms,
                        "label": label,
                        "error": err,
                    }),
                ) {
                    tracing::warn!("claude_pool failed to write failed status.json: {write_err}");
                }
                Err(err)
            }
        }
    }

    /// Spawn `claude -p --output-format json [--dangerously-skip-permissions]`
    /// with the prompt on stdin. Honors `TACHI_CLAUDE_SKIP_PERMISSIONS`:
    /// `true`/`1` enables `--dangerously-skip-permissions`; any other value,
    /// or an unset variable, leaves interactive permission prompts enabled.
    /// Non-interactive callers must opt in explicitly.
    async fn run_claude_cli(&self, prompt: &str) -> Result<String, String> {
        let skip_perms = std::env::var("TACHI_CLAUDE_SKIP_PERMISSIONS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
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

/// Combine a `system` preamble and `user` payload into a single prompt the
/// Claude CLI can consume on stdin. Mirrors the OpenAI-compatible chat
/// shape used by the raw-API lanes so behaviour stays comparable.
fn format_pool_prompt(system: &str, user: &str) -> String {
    let sys = system.trim();
    let usr = user.trim();
    if sys.is_empty() {
        usr.to_string()
    } else {
        format!("<system>\n{sys}\n</system>\n\n{usr}")
    }
}

/// Source label returned by [`pool_call_with_fallback`] indicating which
/// backend actually produced the response. Useful for audit/log output so
/// operators can see when the Claude pool degraded to the raw-API lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolCallSource {
    ClaudeCli,
    RawApiFallback,
}

impl PoolCallSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            PoolCallSource::ClaudeCli => "claude_cli",
            PoolCallSource::RawApiFallback => "raw_api_fallback",
        }
    }
}

/// Try `pool.call(system+user, label)` first; on Err, invoke `fallback`
/// (the existing raw-API LLM helper closure). Returns the produced text
/// together with which backend supplied it so callers can log/audit.
///
/// The pool failure is logged to stderr so operators notice when the
/// Claude CLI lane keeps degrading.
pub async fn pool_call_with_fallback<F, Fut>(
    pool: &ClaudePool,
    system: &str,
    user: &str,
    label: &str,
    fallback: F,
) -> Result<(String, PoolCallSource), String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let combined = format_pool_prompt(system, user);
    match pool.call(label, &combined).await {
        Ok(outcome) => Ok((outcome.text, PoolCallSource::ClaudeCli)),
        Err(pool_err) => {
            eprintln!("[claude_pool:{label}] degraded to raw_api fallback: {pool_err}");
            let text = fallback().await?;
            Ok((text, PoolCallSource::RawApiFallback))
        }
    }
}

fn sanitize_label(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        "run".to_string()
    } else {
        trimmed.chars().take(48).collect()
    }
}

fn write_run_file(path: &Path, body: &str) -> Result<(), String> {
    write_owner_only_file_atomic(path, body.as_bytes())
}

/// Extract the `result` field from Claude CLI's JSON envelope. Tolerates
/// either a JSON object or raw text falling back to the whole stdout.
fn parse_claude_json_envelope(stdout: &str) -> Result<String, String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err("claude cli returned empty stdout".to_string());
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(s) = value.get("result").and_then(|v| v.as_str()) {
            return Ok(s.to_string());
        }
        // Some claude CLI versions wrap text in `content` arrays.
        if let Some(s) = value.get("text").and_then(|v| v.as_str()) {
            return Ok(s.to_string());
        }
        if let Some(arr) = value.get("content").and_then(|v| v.as_array()) {
            let mut buf = String::new();
            for item in arr {
                if let Some(s) = item.get("text").and_then(|v| v.as_str()) {
                    buf.push_str(s);
                }
            }
            if !buf.is_empty() {
                return Ok(buf);
            }
        }
    }
    // Non-JSON output: hand back as-is (caller will reject empty later).
    Ok(trimmed.to_string())
}

fn cleanup_runs_dir(root: &Path, now: SystemTime) -> (usize, usize) {
    cleanup_runs_dir_recursive(root, now, 0)
}

fn cleanup_runs_dir_recursive(root: &Path, now: SystemTime, depth: usize) -> (usize, usize) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return (0, 0);
    };

    let success_max = Duration::from_secs(SUCCESS_RETENTION_DAYS * 86_400);
    let failed_max = Duration::from_secs(FAILED_RETENTION_DAYS * 86_400);

    let mut removed = 0usize;
    let mut scanned = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let status_path = path.join("status.json");
        let manifest_path = path.join("source_manifest.json");
        if !status_path.exists() && !manifest_path.exists() {
            let child_has_dirs = std::fs::read_dir(&path)
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .any(|e| e.path().is_dir());
            if child_has_dirs && depth < 8 {
                let (r, s) = cleanup_runs_dir_recursive(&path, now, depth + 1);
                removed += r;
                scanned += s;
                continue;
            }
        }

        scanned += 1;

        let (failed, modified) = if manifest_path.exists() {
            let mtime = std::fs::metadata(&manifest_path)
                .and_then(|m| m.modified())
                .ok();
            (false, mtime)
        } else if let Ok(raw) = std::fs::read_to_string(&status_path) {
            let failed = serde_json::from_str::<Value>(&raw)
                .ok()
                .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(str::to_string))
                .map(|s| s != "success")
                .unwrap_or(true);
            let mtime = std::fs::metadata(&status_path)
                .and_then(|m| m.modified())
                .ok();
            (failed, mtime)
        } else {
            let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            (true, mtime)
        };

        let Some(modified) = modified else { continue };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        let max = if failed { failed_max } else { success_max };
        if age > max {
            if std::fs::remove_dir_all(&path).is_ok() {
                removed += 1;
            }
        }
    }
    (removed, scanned)
}

fn resolve_claude_binary() -> Result<String, String> {
    match std::env::var("CLAUDE_BIN") {
        Ok(raw) => validate_claude_binary_override(&raw),
        Err(_) => Ok("claude".to_string()),
    }
}

fn validate_claude_binary_override(raw: &str) -> Result<String, String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err("CLAUDE_BIN is empty".to_string());
    }
    if value.chars().any(char::is_whitespace) {
        return Err("CLAUDE_BIN must be a single executable path without arguments".to_string());
    }
    if value == "claude" {
        return Ok(value.to_string());
    }

    let path = Path::new(value);
    if !path.is_absolute() {
        return Err("CLAUDE_BIN must be 'claude' or an absolute path".to_string());
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("CLAUDE_BIN must not contain parent directory components".to_string());
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "CLAUDE_BIN must include a valid executable name".to_string())?;
    if file_name == "claude" || file_name.starts_with("claude-") {
        let metadata = std::fs::metadata(path)
            .map_err(|_| "CLAUDE_BIN must point to an existing executable".to_string())?;
        if !metadata.is_file() || !is_executable(&metadata) {
            return Err("CLAUDE_BIN must point to an executable file".to_string());
        }
        Ok(value.to_string())
    } else {
        Err("CLAUDE_BIN executable name must be 'claude' or start with 'claude-'".to_string())
    }
}

fn is_executable(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_label_replaces_unsafe_chars() {
        assert_eq!(sanitize_label("ok-label_1"), "ok-label_1");
        assert_eq!(sanitize_label("a/b c"), "a_b_c");
        assert_eq!(sanitize_label(""), "run");
        // Trim leading/trailing underscores.
        assert_eq!(sanitize_label("///"), "run");
        // Long labels capped at 48 chars.
        let long = "x".repeat(80);
        assert_eq!(sanitize_label(&long).len(), 48);
    }

    #[test]
    fn claude_binary_override_accepts_only_claude_executables() {
        let tmp = tempfile::tempdir().unwrap();
        let claude_path = tmp.path().join("claude");
        std::fs::write(&claude_path, "#!/bin/sh\nexit 0\n").unwrap();
        let claude_beta_path = tmp.path().join("claude-beta");
        std::fs::write(&claude_beta_path, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::set_permissions(&claude_beta_path, std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }

        assert_eq!(validate_claude_binary_override("claude").unwrap(), "claude");
        assert_eq!(
            validate_claude_binary_override(claude_path.to_str().unwrap()).unwrap(),
            claude_path.to_str().unwrap()
        );
        assert_eq!(
            validate_claude_binary_override(claude_beta_path.to_str().unwrap()).unwrap(),
            claude_beta_path.to_str().unwrap()
        );

        for bad in [
            "",
            "claude --dangerously-skip-permissions",
            "./claude",
            "/tmp/not-claude",
            "/tmp/../tmp/claude",
            "/nonexistent/claude",
        ] {
            assert!(
                validate_claude_binary_override(bad).is_err(),
                "expected invalid override: {bad}"
            );
        }
    }

    #[test]
    fn resolve_claude_binary_reports_invalid_override() {
        let prev = std::env::var("CLAUDE_BIN").ok();
        std::env::set_var("CLAUDE_BIN", "/nonexistent/claude");

        let err = resolve_claude_binary().expect_err("invalid override should be surfaced");
        assert!(
            err.contains("existing executable"),
            "unexpected error: {err}"
        );

        match prev {
            Some(v) => std::env::set_var("CLAUDE_BIN", v),
            None => std::env::remove_var("CLAUDE_BIN"),
        }
    }

    #[test]
    fn parse_claude_envelope_extracts_result() {
        let stdout = r#"{"result":"hello world","cost_usd":0.01}"#;
        assert_eq!(parse_claude_json_envelope(stdout).unwrap(), "hello world");
    }

    #[test]
    fn parse_claude_envelope_falls_back_to_raw_text() {
        let stdout = "raw text output";
        assert_eq!(
            parse_claude_json_envelope(stdout).unwrap(),
            "raw text output"
        );
    }

    #[test]
    fn parse_claude_envelope_handles_content_array() {
        let stdout = r#"{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}"#;
        assert_eq!(parse_claude_json_envelope(stdout).unwrap(), "ab");
    }

    #[test]
    fn parse_claude_envelope_rejects_empty() {
        assert!(parse_claude_json_envelope("   ").is_err());
    }

    #[test]
    fn cleanup_removes_old_failed_dirs_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Fresh success — should be kept.
        let fresh = root.join("fresh-1");
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::write(fresh.join("status.json"), r#"{"status":"success"}"#).unwrap();

        // "Old" failed — same on-disk mtime, but we pretend `now` is 60 days
        // in the future so the dir's age exceeds the failed-retention cap.
        let old_failed = root.join("old-failed");
        std::fs::create_dir_all(&old_failed).unwrap();
        std::fs::write(old_failed.join("status.json"), r#"{"status":"failed"}"#).unwrap();

        // "now" = real now + 60 days → all entries appear 60 days old.
        let future_now = SystemTime::now() + Duration::from_secs(60 * 86_400);
        let (removed, scanned) = cleanup_runs_dir(root, future_now);
        // Both are 60 days old: failed (30d cap) removed, success (7d cap) also removed.
        assert_eq!(scanned, 2);
        assert_eq!(removed, 2);
        assert!(!fresh.exists());
        assert!(!old_failed.exists());

        // Second scenario: pretend now is 10 days in the future. Failed
        // (30d cap) stays; success (7d cap) is removed.
        let fresh2 = root.join("fresh-2");
        std::fs::create_dir_all(&fresh2).unwrap();
        std::fs::write(fresh2.join("status.json"), r#"{"status":"success"}"#).unwrap();
        let failed2 = root.join("failed-2");
        std::fs::create_dir_all(&failed2).unwrap();
        std::fs::write(failed2.join("status.json"), r#"{"status":"failed"}"#).unwrap();

        let near_future = SystemTime::now() + Duration::from_secs(10 * 86_400);
        let (removed, scanned) = cleanup_runs_dir(root, near_future);
        assert_eq!(scanned, 2);
        assert_eq!(removed, 1, "only the success dir should be over its 7d cap");
        assert!(!fresh2.exists());
        assert!(failed2.exists());
    }

    #[test]
    fn format_pool_prompt_combines_system_and_user() {
        let out = format_pool_prompt("be precise", "the question");
        assert!(out.contains("be precise"));
        assert!(out.contains("the question"));
        assert!(out.contains("<system>"));
        assert!(out.contains("</system>"));
    }

    #[test]
    fn format_pool_prompt_skips_separator_when_system_empty() {
        let out = format_pool_prompt("   ", "just user");
        assert_eq!(out, "just user");
    }

    #[tokio::test]
    async fn pool_call_with_fallback_invokes_fallback_when_pool_errors() {
        // CLAUDE_BIN points at a binary that cannot exist → pool errors → fallback fires.
        let prev = std::env::var("CLAUDE_BIN").ok();
        std::env::set_var("CLAUDE_BIN", "/nonexistent/__tachi_test_no_such_claude__");

        let tmp = tempfile::tempdir().unwrap();
        let pool = ClaudePool {
            sem: Arc::new(Semaphore::new(1)),
            runs_dir: tmp.path().to_path_buf(),
            timeout: Duration::from_secs(5),
            binary: Ok("/nonexistent/__tachi_test_no_such_claude__".to_string()),
        };

        let (text, source) = pool_call_with_fallback(&pool, "sys", "usr", "unit-test", || async {
            Ok::<_, String>("fallback-text".to_string())
        })
        .await
        .expect("fallback path should succeed");

        assert_eq!(text, "fallback-text");
        assert_eq!(source, PoolCallSource::RawApiFallback);
        assert_eq!(source.as_str(), "raw_api_fallback");

        // Restore env
        match prev {
            Some(v) => std::env::set_var("CLAUDE_BIN", v),
            None => std::env::remove_var("CLAUDE_BIN"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_claude_cli_kills_timed_out_child() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_claude = tmp.path().join("claude");
        let sentinel = tmp.path().join("sentinel");
        std::fs::write(
            &fake_claude,
            "#!/bin/sh\nsleep 1\nprintf done > \"$SENTINEL_FILE\"\nprintf '{\"result\":\"late\"}\\n'\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let previous_sentinel = std::env::var("SENTINEL_FILE").ok();
        std::env::set_var("SENTINEL_FILE", &sentinel);
        let pool = ClaudePool {
            sem: Arc::new(Semaphore::new(1)),
            runs_dir: tmp.path().to_path_buf(),
            timeout: Duration::from_millis(50),
            binary: Ok(fake_claude.to_string_lossy().to_string()),
        };

        let err = pool
            .run_claude_cli("prompt from stdin")
            .await
            .expect_err("fake claude should time out");
        assert!(err.contains("timed out"), "unexpected error: {err}");
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        assert!(
            !sentinel.exists(),
            "timed-out claude child should be killed before it continues work"
        );

        match previous_sentinel {
            Some(value) => std::env::set_var("SENTINEL_FILE", value),
            None => std::env::remove_var("SENTINEL_FILE"),
        }
    }

    #[test]
    fn skip_permissions_defaults_to_false_when_env_unset() {
        // clear the env var so .unwrap_or(false) fires.
        let prev = std::env::var("TACHI_CLAUDE_SKIP_PERMISSIONS").ok();
        std::env::remove_var("TACHI_CLAUDE_SKIP_PERMISSIONS");
        let skip = std::env::var("TACHI_CLAUDE_SKIP_PERMISSIONS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        assert!(
            !skip,
            "default should be false (preserve interactive prompts)"
        );
        match prev {
            Some(v) => std::env::set_var("TACHI_CLAUDE_SKIP_PERMISSIONS", v),
            None => std::env::remove_var("TACHI_CLAUDE_SKIP_PERMISSIONS"),
        }
    }

    #[test]
    fn skip_permissions_is_false_when_env_is_false() {
        let prev = std::env::var("TACHI_CLAUDE_SKIP_PERMISSIONS").ok();
        std::env::set_var("TACHI_CLAUDE_SKIP_PERMISSIONS", "false");
        let skip = std::env::var("TACHI_CLAUDE_SKIP_PERMISSIONS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        assert!(!skip, "explicit false should disable skip");
        match prev {
            Some(v) => std::env::set_var("TACHI_CLAUDE_SKIP_PERMISSIONS", v),
            None => std::env::remove_var("TACHI_CLAUDE_SKIP_PERMISSIONS"),
        }
    }

    #[tokio::test]
    async fn pool_call_with_fallback_propagates_fallback_error() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = ClaudePool {
            sem: Arc::new(Semaphore::new(1)),
            runs_dir: tmp.path().to_path_buf(),
            timeout: Duration::from_secs(5),
            binary: Ok("/nonexistent/__tachi_test_no_such_claude_2__".to_string()),
        };

        let err = pool_call_with_fallback(&pool, "sys", "usr", "unit-test-err", || async {
            Err::<String, _>("raw api also down".to_string())
        })
        .await
        .expect_err("fallback error should surface");
        assert!(err.contains("raw api also down"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn prompt_file_is_owner_readable_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let pool = ClaudePool {
            sem: Arc::new(Semaphore::new(1)),
            runs_dir: tmp.path().to_path_buf(),
            timeout: Duration::from_secs(1),
            binary: Ok("/nonexistent/__tachi_test_no_such_claude_prompt__".to_string()),
        };

        let _ = pool
            .call("perm-test", "secret prompt body")
            .await
            .expect_err("binary missing so call fails after writing prompt");

        let prompt_file = std::fs::read_dir(tmp.path())
            .unwrap()
            .flat_map(|e| e.ok())
            .find(|e| e.file_type().unwrap().is_dir())
            .map(|e| e.path().join("prompt.md"))
            .expect("run dir with prompt.md should exist");

        let meta = std::fs::metadata(&prompt_file).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "prompt.md should be owner-readable only");
        let contents = std::fs::read_to_string(&prompt_file).unwrap();
        assert!(contents.contains("secret prompt body"));
    }

    #[cfg(unix)]
    #[test]
    fn foundry_runs_dir_is_created_with_0o700() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tmp = tempfile::tempdir().expect("temp home");
        let original_home = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());

        let _pool = ClaudePool::new(1);
        let runs_dir = tmp.path().join(".tachi").join("foundry-runs");
        assert!(runs_dir.exists(), "foundry-runs dir should be created");
        let mode = std::fs::metadata(&runs_dir)
            .expect("foundry-runs metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "foundry-runs dir should be restricted to owner"
        );

        if let Some(value) = original_home {
            std::env::set_var("HOME", value);
        } else {
            std::env::remove_var("HOME");
        }
    }
}
