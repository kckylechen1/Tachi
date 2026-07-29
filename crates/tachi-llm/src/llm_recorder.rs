//! Bounded LLM-call recorder for foundry runs.
//!
//! Each recorded call:
//!   * acquires a semaphore permit (caps concurrent invocations);
//!   * writes `prompt.md` / `result.md` / `status.json` under
//!     `<tachi_home>/foundry-runs/<label>-<UTCts>/`;
//!   * runs the caller-supplied provider executor (a `tachi-llm` chat-lane
//!     closure — no subprocess spawn);
//!   * writes the outcome (`result.md` + `status.json`) regardless of
//!     success/failure, so a failed call leaves a full post-mortem trail.
//!
//! ## History
//!
//! This module is what survived the ClaudePool decommission (#1261, step
//! 3/3). The pre-#1261 `claude_pool` module combined three concerns: (1)
//! this run-directory recording, (2) a `claude` CLI subprocess spawn path,
//! and (3) a CLI→provider fallback orchestrator. Steps 1 and 2 of the
//! decommission flipped the rollout default and removed every CLI fallback
//! branch from the five live call sites; this step 3 deletes the CLI spawn
//! path and the fallback orchestrator entirely, leaving only the recording
//! concern — which is executor-agnostic by construction (the caller supplies
//! the closure). The `claude_pool` name no longer fits and the module is
//! renamed to `llm_recorder` to reflect what it actually does.
//!
//! `foundry-runs` consumers today: `foundry_runtime_ops::daily_distill`,
//! `dispatch_ops::dispatch_v2` (plan stage), `hub_ops::security_scan`,
//! `hub_ops::register` (skill analysis), `hub_ops::evolve`. The
//! `status_ops::ledger` distill marker at `foundry-runs/.last_distill_run`
//! is independent of this recorder (it is written by the distill runner,
//! not by a recorded call).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use chrono::Utc;
use serde_json::json;
use tokio::sync::Semaphore;

use crate::{Generated, PersistedModelInvocationReceiptV1};

mod cleanup;
mod files;

/// Cleanup retention for successful runs.
pub const SUCCESS_RETENTION_DAYS: u64 = 7;
/// Cleanup retention for failed runs (kept longer for post-mortem).
pub const FAILED_RETENTION_DAYS: u64 = 30;
/// Default bounded concurrency for recorded LLM calls. Mirrors the
/// pre-#1261 `ClaudePool::DEFAULT_MAX_CONCURRENT` value so the foundry-runs
/// folder's concurrency footprint is unchanged by the decommission.
pub const DEFAULT_MAX_CONCURRENT: usize = 2;

/// Outcome of a recorded LLM call. The recorder writes the full prompt +
/// result + status trail to disk; this struct is the in-memory handle the
/// caller uses to consume the produced text.
#[derive(Debug)]
pub struct RecordedCallOutcome {
    pub text: String,
}

/// Text plus the closed, persisted-safe receipt for the engine that produced
/// it. The recorder's filesystem elapsed time stays in `status.json` and must
/// never replace `invocation.latency_ms()`, which belongs to the serving engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCallWithReceipt {
    pub text: String,
    pub invocation: PersistedModelInvocationReceiptV1,
}

/// Bounded recorder for LLM calls, writing the `prompt.md` /
/// `result.md` / `status.json` artifact triple per call under
/// `<runs_dir>/<label>-<UTCts>/`. The executor is caller-supplied — this
/// type never spawns a subprocess.
pub struct LlmCallRecorder {
    sem: Arc<Semaphore>,
    runs_dir: PathBuf,
}

impl LlmCallRecorder {
    /// Construct a recorder with `max_concurrent` permits writing into
    /// `<app_home>/foundry-runs/`. Creates the directory (mode 0o700 on
    /// Unix) if it does not exist.
    ///
    /// #1096 leaf-2a lineage: `tachi-server`'s `MemoryServer::new` already
    /// resolves its own home once at construction (`MemoryServer::home_dir`)
    /// — pass that value straight through here instead of re-deriving it.
    pub fn new_in_app_home(max_concurrent: usize, app_home: impl Into<PathBuf>) -> Self {
        let runs_dir = app_home.into().join("foundry-runs");
        if let Err(error) = std::fs::create_dir_all(&runs_dir) {
            tracing::warn!(
                runs_dir = %runs_dir.display(),
                error = %error,
                "failed to create llm recorder foundry-runs directory"
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
                    "failed to restrict llm recorder foundry-runs permissions"
                );
            }
        }

        Self {
            sem: Arc::new(Semaphore::new(max_concurrent.max(1))),
            runs_dir,
        }
    }

    pub fn runs_dir(&self) -> &Path {
        &self.runs_dir
    }

    /// Record a provider-executor call: acquire a bounded-concurrency
    /// permit, create `<runs_dir>/<label>-<ts>/`, write `prompt.md`, run
    /// `executor`, then write `result.md` + `status.json` regardless of
    /// outcome. `prompt` is recorded verbatim to `prompt.md`; `executor` is
    /// what actually produces the completion (a `tachi-llm` chat-lane call
    /// or equivalent closure).
    pub async fn record_call<F, Fut>(
        &self,
        label: &str,
        prompt: &str,
        executor: F,
    ) -> Result<RecordedCallOutcome, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<String, String>>,
    {
        let permit = self
            .sem
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| format!("llm recorder semaphore closed: {e}"))?;

        let ts = Utc::now().format("%Y%m%dT%H%M%S%.3f").to_string();
        let safe_label = files::sanitize_label(label);
        let run_dir = self.runs_dir.join(format!("{safe_label}-{ts}"));
        if let Err(e) = tokio::fs::create_dir_all(&run_dir).await {
            return Err(format!(
                "llm recorder create run dir {}: {e}",
                run_dir.display()
            ));
        }

        let prompt_path = run_dir.join("prompt.md");
        if let Err(e) =
            files::write_owner_only_file_blocking(prompt_path.clone(), prompt.as_bytes().to_vec())
                .await
        {
            return Err(format!("llm recorder write {}: {e}", prompt_path.display()));
        }

        let started_at = Utc::now().to_rfc3339();
        let started = Instant::now();
        let result = executor().await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let finished_at = Utc::now().to_rfc3339();

        // Drop permit before fs writes — file I/O shouldn't hold a call slot.
        drop(permit);

        match result {
            Ok(text) => {
                if let Err(err) =
                    files::write_run_file_blocking(run_dir.join("result.md"), text.clone()).await
                {
                    tracing::warn!("llm recorder failed to write result.md: {err}");
                }
                if let Err(err) = files::write_run_status_file_blocking(
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
                    tracing::warn!("llm recorder failed to write status.json: {err}");
                }
                Ok(RecordedCallOutcome { text })
            }
            Err(err) => {
                if let Err(write_err) =
                    files::write_run_file_blocking(run_dir.join("result.md"), err.clone()).await
                {
                    tracing::warn!("llm recorder failed to write error result.md: {write_err}");
                }
                if let Err(write_err) = files::write_run_status_file_blocking(
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
                    tracing::warn!("llm recorder failed to write failed status.json: {write_err}");
                }
                Err(err)
            }
        }
    }

    /// Receipt-preserving sibling to [`Self::record_call`]. It retains the
    /// recorder's status/result artifacts while returning the original
    /// provider/CLI invocation receipt unchanged for a durable producer to
    /// attach at its own primary write boundary.
    pub async fn record_call_with_receipt<F, Fut, O>(
        &self,
        label: &str,
        prompt: &str,
        executor: F,
    ) -> Result<RecordedCallWithReceipt, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<O, String>>,
        O: Into<Generated<String>>,
    {
        let permit = self
            .sem
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| format!("llm recorder semaphore closed: {e}"))?;

        let ts = Utc::now().format("%Y%m%dT%H%M%S%.3f").to_string();
        let safe_label = files::sanitize_label(label);
        let run_dir = self.runs_dir.join(format!("{safe_label}-{ts}"));
        if let Err(e) = tokio::fs::create_dir_all(&run_dir).await {
            return Err(format!(
                "llm recorder create run dir {}: {e}",
                run_dir.display()
            ));
        }

        let prompt_path = run_dir.join("prompt.md");
        if let Err(e) =
            files::write_owner_only_file_blocking(prompt_path.clone(), prompt.as_bytes().to_vec())
                .await
        {
            return Err(format!("llm recorder write {}: {e}", prompt_path.display()));
        }

        let started_at = Utc::now().to_rfc3339();
        let started = Instant::now();
        let result = executor().await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let finished_at = Utc::now().to_rfc3339();

        // Drop permit before filesystem writes — file I/O shouldn't hold a
        // provider call slot.
        drop(permit);

        match result {
            Ok(result) => {
                let Generated { value, invocation } = result.into();
                if let Err(err) =
                    files::write_run_file_blocking(run_dir.join("result.md"), value.clone()).await
                {
                    tracing::warn!("llm recorder failed to write result.md: {err}");
                }
                if let Err(err) = files::write_run_status_file_blocking(
                    run_dir.clone(),
                    json!({
                        "status": "success",
                        "started_at": started_at,
                        "finished_at": finished_at,
                        "elapsed_ms": elapsed_ms,
                        "label": label,
                        "bytes": value.len(),
                        "model_invocation": invocation,
                    }),
                )
                .await
                {
                    tracing::warn!("llm recorder failed to write status.json: {err}");
                }
                Ok(RecordedCallWithReceipt {
                    text: value,
                    invocation,
                })
            }
            Err(err) => {
                if let Err(write_err) =
                    files::write_run_file_blocking(run_dir.join("result.md"), err.clone()).await
                {
                    tracing::warn!("llm recorder failed to write error result.md: {write_err}");
                }
                if let Err(write_err) = files::write_run_status_file_blocking(
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
                    tracing::warn!("llm recorder failed to write failed status.json: {write_err}");
                }
                Err(err)
            }
        }
    }

    /// Walk `runs_dir` and remove directories older than the retention
    /// policy (7d success / 30d failed). Returns `(removed, scanned)`.
    pub fn cleanup_expired(&self) -> (usize, usize) {
        cleanup::cleanup_runs_dir(&self.runs_dir, SystemTime::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use super::cleanup::cleanup_runs_dir;

    /// The foundry-runs directory must be created with mode 0o700 on Unix
    /// so prompt text (which may contain sensitive context) is not
    /// world-readable. Pre-#1261 this was a `ClaudePool` invariant; the
    /// decommission must not relax it.
    #[cfg(unix)]
    #[test]
    fn foundry_runs_dir_is_created_with_0o700() {
        use std::os::unix::fs::PermissionsExt;
        let app_home = tempfile::tempdir().expect("temp app home");
        let _recorder = LlmCallRecorder::new_in_app_home(2, app_home.path());
        let runs_dir = app_home.path().join("foundry-runs");
        assert!(runs_dir.exists(), "foundry-runs dir should be created");
        let mode = std::fs::metadata(&runs_dir)
            .expect("runs_dir metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "foundry-runs must be 0o700, got 0o{mode:o}");
    }

    /// Retention sweep keeps recent failed dirs and removes expired ones.
    /// Migrated verbatim from the pre-#1261 `claude_pool::tests` — the
    /// cleanup policy is a foundry-runs contract, not a CLI-pool one. Uses
    /// the same "pretend `now` is N days in the future" trick so no
    /// filesystem mtime manipulation is needed.
    #[test]
    fn cleanup_removes_old_failed_dirs_only() {
        let tmp = tempfile::tempdir().expect("temp runs root");
        let root = tmp.path();

        // Fresh success — should be kept at the 10-day mark (under 7d cap is
        // removed, but we assert the SECOND scenario's invariant below).
        let fresh = root.join("fresh-1");
        std::fs::create_dir_all(&fresh).expect("create fresh");
        std::fs::write(fresh.join("status.json"), r#"{"status":"success"}"#).expect("write status");

        let old_failed = root.join("old-failed");
        std::fs::create_dir_all(&old_failed).expect("create old failed");
        std::fs::write(old_failed.join("status.json"), r#"{"status":"failed"}"#)
            .expect("write status");

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
        std::fs::create_dir_all(&fresh2).expect("create fresh2");
        std::fs::write(fresh2.join("status.json"), r#"{"status":"success"}"#)
            .expect("write status");
        let failed2 = root.join("failed-2");
        std::fs::create_dir_all(&failed2).expect("create failed2");
        std::fs::write(failed2.join("status.json"), r#"{"status":"failed"}"#)
            .expect("write status");

        let near_future = SystemTime::now() + Duration::from_secs(10 * 86_400);
        let (removed, scanned) = cleanup_runs_dir(root, near_future);
        assert_eq!(scanned, 2);
        assert_eq!(removed, 1, "only the success dir should be over its 7d cap");
        assert!(!fresh2.exists());
        assert!(failed2.exists());
    }

    #[tokio::test]
    async fn record_call_with_receipt_preserves_provider_latency_not_filesystem_elapsed() {
        let app_home = tempfile::tempdir().expect("temp app home");
        let recorder = LlmCallRecorder::new_in_app_home(1, app_home.path());
        let invocation = PersistedModelInvocationReceiptV1::claude_cli_reasoning(1);

        let recorded = recorder
            .record_call_with_receipt("receipt-latency", "safe prompt", move || {
                let invocation = invocation.clone();
                async move {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    Ok::<_, String>(Generated {
                        value: "provider result".to_string(),
                        invocation,
                    })
                }
            })
            .await
            .expect("recorded invocation");

        assert_eq!(recorded.text, "provider result");
        assert_eq!(recorded.invocation.latency_ms(), Some(1));
        let run_dir = std::fs::read_dir(recorder.runs_dir())
            .expect("runs directory")
            .next()
            .expect("one recorded run")
            .expect("run entry")
            .path();
        let status: serde_json::Value = serde_json::from_slice(
            &std::fs::read(run_dir.join("status.json")).expect("read status"),
        )
        .expect("parse status");
        assert!(
            status["elapsed_ms"].as_u64().expect("elapsed ms") >= 10,
            "filesystem/run elapsed evidence must remain distinct from provider latency"
        );
        assert_eq!(status["model_invocation"]["latency_ms"], 1);
        assert_eq!(status["model_invocation"]["schema"], "model-invocation-v1");
    }

    #[tokio::test]
    async fn legacy_record_call_remains_text_only_compatible() {
        let app_home = tempfile::tempdir().expect("temp app home");
        let recorder = LlmCallRecorder::new_in_app_home(1, app_home.path());

        let recorded = recorder
            .record_call("legacy-text", "legacy prompt", || async {
                Ok::<_, String>("legacy result".to_string())
            })
            .await
            .expect("legacy record call");
        assert_eq!(recorded.text, "legacy result");
        let run_dir = std::fs::read_dir(recorder.runs_dir())
            .expect("runs directory")
            .next()
            .expect("one recorded run")
            .expect("run entry")
            .path();
        let status: serde_json::Value = serde_json::from_slice(
            &std::fs::read(run_dir.join("status.json")).expect("read status"),
        )
        .expect("parse status");
        assert!(
            status.get("model_invocation").is_none(),
            "text-only adapter must preserve its legacy status shape"
        );
    }
}
