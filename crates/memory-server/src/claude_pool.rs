//! Bounded Claude Code CLI pool used by Foundry batch distill, dispatch V2
//! plan stage, Hub skill evolve, and Hub security scan.
//!
//! Each call:
//!   * acquires a semaphore permit (cap concurrent CLI invocations);
//!   * writes `prompt.md` / `result.md` / `status.json` under
//!     `~/.tachi/foundry-runs/<label>-<UTCts>/`;
//!   * spawns `claude -p --output-format json --dangerously-skip-permissions`
//!     with the prompt on stdin — this is the intentional default because
//!     Claude CLI's interactive permission prompts would hang daemon-driven
//!     callers (Foundry distill, Hub evolve). Set
//!     `TACHI_CLAUDE_SKIP_PERMISSIONS=false` to restore prompts for
//!     non-daemon use;
//!   * enforces a wall-clock timeout (default 180s, env
//!     `CLAUDE_POOL_TIMEOUT_SECS`).
//!
//! The pool also exposes `cleanup_expired` which removes run directories
//! older than the policy (7d success / 30d failed) so the foundry runs
//! folder doesn't grow without bound.
//!
//! Callers use `pool_call_with_fallback` for a Claude CLI → raw API
//! degradation path. Each caller is expected to handle its own LLM-fallback
//! policy when `call()` returns Err.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

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
    binary: String,
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

        let timeout_secs = std::env::var("CLAUDE_POOL_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        let binary = std::env::var("CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());

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
        if let Err(e) = std::fs::create_dir_all(&run_dir) {
            return Err(format!(
                "claude_pool create run dir {}: {e}",
                run_dir.display()
            ));
        }

        let prompt_path = run_dir.join("prompt.md");
        if let Err(e) = std::fs::write(&prompt_path, prompt) {
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
                let _ = std::fs::write(run_dir.join("result.md"), &text);
                let _ = write_status(
                    &run_dir,
                    &json!({
                        "status": "success",
                        "started_at": started_at,
                        "finished_at": finished_at,
                        "elapsed_ms": elapsed_ms,
                        "label": label,
                        "bytes": text.len(),
                    }),
                );
                Ok(ClaudeCallOutcome { text })
            }
            Err(err) => {
                let _ = std::fs::write(run_dir.join("result.md"), &err);
                let _ = write_status(
                    &run_dir,
                    &json!({
                        "status": "failed",
                        "started_at": started_at,
                        "finished_at": finished_at,
                        "elapsed_ms": elapsed_ms,
                        "label": label,
                        "error": err,
                    }),
                );
                Err(err)
            }
        }
    }

    /// Spawn `claude -p --output-format json [--dangerously-skip-permissions]`
    /// with the prompt on stdin. Honors `TACHI_CLAUDE_SKIP_PERMISSIONS`:
    /// true (default, daemon-safe) | false (restore interactive prompts).
    /// The default is true because this pool serves non-interactive callers
    /// (Foundry distill, dispatch V2 plan, Hub evolve/security scan) where
    /// a permission prompt would hang indefinitely.
    async fn run_claude_cli(&self, prompt: &str) -> Result<String, String> {
        let skip_perms = std::env::var("TACHI_CLAUDE_SKIP_PERMISSIONS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let mut cmd = Command::new(&self.binary);
        cmd.arg("-p").arg("--output-format").arg("json");
        if skip_perms {
            cmd.arg("--dangerously-skip-permissions");
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("claude cli spawn failed ({}): {e}", self.binary))?;

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
pub fn format_pool_prompt(system: &str, user: &str) -> String {
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

fn write_status(run_dir: &Path, value: &Value) -> std::io::Result<()> {
    let path = run_dir.join("status.json");
    let body = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    std::fs::write(path, body)
}

/// Extract the `result` field from Claude CLI's JSON envelope. Tolerates
/// either a JSON object or raw text falling back to the whole stdout.
pub fn parse_claude_json_envelope(stdout: &str) -> Result<String, String> {
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
            binary: "/nonexistent/__tachi_test_no_such_claude__".to_string(),
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

    #[test]
    fn skip_permissions_defaults_to_true_when_env_unset() {
        // clear the env var so .unwrap_or(true) fires.
        let prev = std::env::var("TACHI_CLAUDE_SKIP_PERMISSIONS").ok();
        std::env::remove_var("TACHI_CLAUDE_SKIP_PERMISSIONS");
        let skip = std::env::var("TACHI_CLAUDE_SKIP_PERMISSIONS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        assert!(skip, "default should be true (daemon-safe)");
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
            binary: "/nonexistent/__tachi_test_no_such_claude_2__".to_string(),
        };

        let err = pool_call_with_fallback(&pool, "sys", "usr", "unit-test-err", || async {
            Err::<String, _>("raw api also down".to_string())
        })
        .await
        .expect_err("fallback error should surface");
        assert!(err.contains("raw api also down"));
    }
}
