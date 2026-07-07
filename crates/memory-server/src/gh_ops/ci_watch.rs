//! Background CI check-state watcher (#605).
//!
//! The watch/poller twin of `tachi-await`. A daemon-resident background loop
//! periodically polls GitHub check state for tracked open PRs (PRs linked to
//! non-terminal Tachi flows) and records each transition into the flow's
//! `check_state.json` ledger via #816's `ingest_check_state_transition`. A red
//! check becomes a ledger event a subscriber (`tachi-await`) can observe.
//!
//! Design constraints (see issue #605):
//! - **Observes + records only.** No webhook server, no repair dispatch, no
//!   auto-merge. The watcher never mutates GitHub state.
//! - **Resilient.** A poll failure for one PR must NOT crash the loop or skip
//!   other PRs. Errors are logged and the loop continues.
//! - **Rate-limit aware.** `GhError::RateLimited` causes the whole cycle to be
//!   skipped (not retried in a tight loop) so the API is never hammered.
//! - **Testable without real GitHub.** The loop takes a `CheckStateReader`
//!   trait object (#816) and a discovery closure, so a `FixedCheckReader` test
//!   double + a synthetic run directory drive every code path.
//!
//! The watcher reuses #816's `CheckStateReader` blanket impl on `GhClient` and
//! #816's `ingest_check_state_transition` recorder — it does NOT implement a
//! second GitHub checks reader or a second recorder.

use super::*;
use crate::shell_ops::shell_runs_root;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use super::safe_merge::{
    gh_client_for_server, ingest_check_state_transition, CheckStateIngestRequest, CheckStateReader,
    SelectedGhClient,
};

/// Default poll interval (60s). Override with `TACHI_CI_WATCH_INTERVAL_SECS`.
pub(crate) const DEFAULT_CI_WATCH_INTERVAL_SECS: u64 = 60;

/// Minimum poll interval clamp. Prevents a misconfigured env var from turning
/// the watcher into an API-abuse tight loop.
const MIN_CI_WATCH_INTERVAL_SECS: u64 = 15;

/// A PR the watcher should poll. Discovered from a flow's `status.json`
/// `github` block: a non-terminal flow with a linked `pr_number` + `repo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WatchTarget {
    pub flow_id: String,
    pub repo: String,
    pub pr_number: u64,
    /// `owner/repo#N` display form, used as `pr_ref` in the ingest request.
    pub pr_ref: Option<String>,
    /// HEAD branch name (e.g. `feat/x`), surfaced as `head_ref` in the artifact.
    pub head_ref: Option<String>,
    /// The head SHA the flow expects to be current. When the observed SHA
    /// diverges, the recorder emits a `Stale` transition. `None` disables
    /// staleness detection for this target (no expected SHA on record).
    pub expected_head_sha: Option<String>,
}

/// Result of polling a single target. Aggregated into a cycle summary so the
/// loop log line is a single structured record, not one line per PR.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct PollCycleSummary {
    pub polled: usize,
    pub transitions: usize,
    pub errors: usize,
    pub rate_limited: bool,
    pub skipped: usize,
}

/// Discover tracked open PRs by scanning flow run directories for a
/// `status.json` carrying a `github.pr_number` + `github.repo` linkage, where
/// the flow is not terminal (no `close_loop.json` and the PR is not yet
/// merged).
///
/// "Tracked open PR" = a PR linked to a non-terminal flow. Discovery is
/// intentionally filesystem-based and cheap: it reads `status.json` only (no
/// GitHub I/O), so it can run every cycle without costing API budget. A flow
/// becomes terminal (and drops out of the watch set) once `close_loop.json`
/// appears or `github.merge_state == "merged"`.
pub(crate) fn discover_watch_targets(runs_root: &Path) -> Vec<WatchTarget> {
    let Ok(entries) = std::fs::read_dir(runs_root) else {
        return Vec::new();
    };
    let mut targets = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(flow_id) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        // Only flows whose id validates — a stray directory must never become a
        // watch target (it could be an attacker-planted path).
        if crate::shell_ops::validate_flow_id(&flow_id).is_err() {
            continue;
        }
        // Terminal flows are no longer tracked: closure already wrote back the
        // issue/PR, so polling would only re-record a state the operator has
        // already acted on.
        if dir.join("close_loop.json").exists() {
            continue;
        }
        let status_path = dir.join("status.json");
        let Ok(raw) = std::fs::read_to_string(&status_path) else {
            continue;
        };
        let Ok(status) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(github) = status.get("github") else {
            continue;
        };
        // A merged PR means the flow is effectively terminal even without a
        // close_loop artifact — stop watching it.
        if github
            .get("merge_state")
            .and_then(Value::as_str)
            .is_some_and(|state| state == "merged")
        {
            continue;
        }
        let Some(repo) = github
            .get("repo")
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty())
        else {
            continue;
        };
        let Some(pr_number) = github.get("pr_number").and_then(Value::as_u64) else {
            continue;
        };
        let pr_ref = github
            .get("pr_url")
            .and_then(Value::as_str)
            .map(|_| format!("{repo}#{pr_number}"));
        let head_ref = github
            .pointer("/pr/head_ref")
            .and_then(Value::as_str)
            .map(str::to_string);
        // The expected head SHA is whatever the flow last recorded for this PR.
        // We read it from the existing check_state artifact (if any) so a
        // re-poll after a force-push can detect the move as `Stale`.
        let expected_head_sha = read_expected_head_sha(&dir);
        targets.push(WatchTarget {
            flow_id,
            repo: repo.to_string(),
            pr_number,
            pr_ref,
            head_ref,
            expected_head_sha,
        });
    }
    targets
}

/// Read the last-recorded `expected_head_sha` for a flow from its
/// `check_state.json`, so a subsequent poll can classify a moved head as
/// `Stale`. Returns `None` when no artifact exists yet (first poll).
fn read_expected_head_sha(run_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(run_dir.join("check_state.json")).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    value
        .get("transition")
        .and_then(|t| t.get("expected_head_sha"))
        .and_then(Value::as_str)
        .filter(|sha| !sha.is_empty())
        .map(str::to_string)
        .or_else(|| {
            value
                .get("pr")
                .and_then(|p| p.get("head_sha"))
                .and_then(Value::as_str)
                .filter(|sha| !sha.is_empty())
                .map(str::to_string)
        })
}

/// Poll a single watch target: read check state via the injected reader and
/// record the transition into the flow's `check_state.json` ledger.
///
/// Returns `Err(GhError::RateLimited)` ONLY when the reader was rate-limited,
/// so the caller can skip the rest of the cycle. Any other reader error is
/// converted by `ingest_check_state_transition` into a `reader_error` ledger
/// state (a recorded transition, not a propagated failure) — this is the
/// degraded-input marker from #816 that keeps a permissive `Ready` from being
/// mistaken for "all checks confirmed green".
///
/// Rate-limit detection: #816's recorder converts any reader error into a
/// `reader_error` ledger state with a `read_error` message. When that message
/// indicates a GitHub rate-limit (`GhError::RateLimited`'s Display form), the
/// watcher treats the whole cycle as throttled and short-circuits — recording
/// one `reader_error` transition for the first target, then skipping the rest,
/// rather than hammering the API with N more doomed reads.
pub(crate) async fn poll_one_pr<R: CheckStateReader + ?Sized>(
    reader: &R,
    target: &WatchTarget,
) -> Result<bool, GhError> {
    let request = CheckStateIngestRequest {
        flow_id: &target.flow_id,
        repo: &target.repo,
        pr_number: target.pr_number,
        pr_ref: target.pr_ref.as_deref(),
        head_ref: target.head_ref.as_deref(),
        expected_head_sha: target.expected_head_sha.as_deref(),
        source: "ci_state.watch",
    };
    match ingest_check_state_transition(reader, &request).await {
        Ok(result) => {
            // If the recorder flagged a degraded read AND the underlying error
            // was a rate-limit, surface it so the cycle short-circuits. The
            // transition was still recorded (so the operator sees the
            // reader_error state), but we stop polling further targets this
            // cycle to respect the rate limit.
            if result.reader_error && result.artifact.persisted {
                if let Some(read_error) = read_recorded_error(&target.flow_id) {
                    if is_rate_limit_error(&read_error) {
                        return Err(GhError::RateLimited(
                            "ci watch cycle rate-limited; skipping remaining targets".to_string(),
                        ));
                    }
                }
            }
            Ok(result.changed)
        }
        Err(err) => {
            // A recorder error (e.g. run-dir write failure) is logged but NOT
            // propagated — one target's filesystem hiccup must not abort the
            // cycle for the others.
            eprintln!(
                "[ci-watch] failed to record transition for {} ({}#{}): {err}",
                target.flow_id, target.repo, target.pr_number
            );
            Ok(false)
        }
    }
}

/// Read the `transition.read_error` string from a flow's check_state artifact,
/// if one was recorded. Used to distinguish a rate-limit from other reader
/// errors after the recorder has already converted the error into a ledger
/// state.
fn read_recorded_error(flow_id: &str) -> Option<String> {
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).ok()?;
    let raw = std::fs::read_to_string(run_dir.join("check_state.json")).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    value
        .get("transition")
        .and_then(|t| t.get("read_error"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Whether a recorded `read_error` message represents a GitHub rate-limit.
/// Matches the `GhError::RateLimited` Display form (`"rate limited: ..."`).
fn is_rate_limit_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("rate limited") || lower.contains("rate limit")
}

/// Run a single poll cycle over the discovered targets. Exposed (and tested)
/// separately from the long-running loop so a test can assert cycle semantics
/// without waiting on a timer.
///
/// `discover` is injected so tests can supply a synthetic target list without
/// touching the filesystem.
pub(crate) async fn run_poll_cycle<R, F>(reader: &R, discover: F) -> PollCycleSummary
where
    R: CheckStateReader + ?Sized,
    F: FnOnce() -> Vec<WatchTarget>,
{
    let targets = discover();
    let mut summary = PollCycleSummary {
        polled: 0,
        transitions: 0,
        errors: 0,
        rate_limited: false,
        skipped: targets.len(),
    };
    for target in &targets {
        match poll_one_pr(reader, target).await {
            Ok(changed) => {
                summary.polled += 1;
                summary.skipped -= 1;
                if changed {
                    summary.transitions += 1;
                }
            }
            Err(GhError::RateLimited(msg)) => {
                // The recorder already ran for this target and persisted a
                // `reader_error` transition (the rate-limit was detected AFTER
                // ingest, not before), so count it as polled+transitioned.
                // Then short-circuit the rest of the cycle (the remaining
                // targets would just pile on more 403s). Do NOT crash — the
                // loop backs off and retries next interval.
                summary.polled += 1;
                summary.skipped -= 1;
                summary.transitions += 1;
                summary.rate_limited = true;
                eprintln!("[ci-watch] rate-limited mid-cycle; {msg}");
                break;
            }
            Err(err) => {
                // Any other reader error was already converted to a ledger
                // state by the recorder; count it and continue.
                summary.polled += 1;
                summary.skipped -= 1;
                summary.errors += 1;
                eprintln!(
                    "[ci-watch] poll error for {} ({}#{}): {err}",
                    target.flow_id, target.repo, target.pr_number
                );
            }
        }
    }
    summary
}

/// Resolve the poll interval from `TACHI_CI_WATCH_INTERVAL_SECS`, clamped to
/// the minimum. Returns `None` when the watcher is disabled (`0`).
pub(crate) fn resolve_watch_interval() -> Option<Duration> {
    let secs = std::env::var("TACHI_CI_WATCH_INTERVAL_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_CI_WATCH_INTERVAL_SECS);
    if secs == 0 {
        return None;
    }
    Some(Duration::from_secs(secs.max(MIN_CI_WATCH_INTERVAL_SECS)))
}

/// Whether the CI watcher should run. Default ON for the daemon, but MUST be
/// disableable for tests / CLI-only invocations. Gated on
/// `TACHI_CI_WATCH=0`/`false`/`off` OR `TACHI_DAEMON` being unset (a stdio
/// invocation has no business polling GitHub).
pub(crate) fn ci_watch_enabled() -> bool {
    if let Ok(raw) = std::env::var("TACHI_CI_WATCH") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "off" | "no" | "disable" | "disabled" => return false,
            "1" | "true" | "on" | "yes" | "enable" | "enabled" => return true,
            _ => {}
        }
    }
    // Default: enabled only when running as the daemon. A stdio/CLI invocation
    // is short-lived and has no long-lived process to host the loop.
    std::env::var("TACHI_DAEMON")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// A `'static` check-state reader backed by a daemon-owned `MemoryServer`.
///
/// `gh_client_for_server` returns a `SelectedGhClient<'_>` that borrows the
/// server (the CLI variant holds `&MemoryServer`), which cannot satisfy the
/// `'static` bound a detached background task needs. This adapter owns an
/// `Arc<MemoryServer>` and rebuilds the client fresh on every call — cheap,
/// since `CliGhClient` is just a borrowed handle, and `HttpGhClient` is a
/// configured `reqwest::Client` + token. A transient transport build failure
/// is surfaced as `GhError::Sanitized` so the recorder records a
/// `reader_error` ledger state rather than crashing the loop.
pub(crate) struct DaemonCheckStateReader {
    server: Arc<MemoryServer>,
}

impl DaemonCheckStateReader {
    pub(crate) fn new(server: Arc<MemoryServer>) -> Self {
        Self { server }
    }
}

/// Build the daemon's check-state reader as a boxed trait object, so callers
/// (the bootstrap wiring) never need to name the `CheckStateReader` trait by
/// path — they just hand the `Arc` to `spawn_ci_watch`.
pub(crate) fn daemon_ci_reader(server: Arc<MemoryServer>) -> Arc<dyn CheckStateReader> {
    Arc::new(DaemonCheckStateReader::new(server))
}

#[async_trait]
impl CheckStateReader for DaemonCheckStateReader {
    async fn read_check_state(
        &self,
        repo: &str,
        pr_number: u64,
    ) -> Result<CheckStateRead, GhError> {
        // Build the selected client for this poll. `gh_client_for_server`
        // decides CLI vs HTTP from `TACHI_GH_TRANSPORT`; both implement
        // `GhClient`, and the blanket impl gives `GhClient -> CheckStateReader`.
        let client = gh_client_for_server(&self.server)
            .map_err(|err| GhError::Sanitized(format!("ci-watch client build: {err}")))?;
        match client {
            SelectedGhClient::Cli(cli) => {
                CheckStateReader::read_check_state(&cli, repo, pr_number).await
            }
            SelectedGhClient::Http(http) => {
                CheckStateReader::read_check_state(&http, repo, pr_number).await
            }
        }
    }
}

/// Spawn the background CI watcher loop. Matches the architecture of the other
/// background loops in `bootstrap/serve/background.rs`: a `tokio::spawn`'d task
/// driven by a `CancellationToken`, gated on config, returning a `JoinHandle`
/// the caller holds for the daemon lifetime.
///
/// The reader is boxed (`Arc<dyn CheckStateReader>`) so the real `GhClient`
/// (cli or http transport) can be injected at spawn time, while tests inject a
/// `FixedCheckReader`.
pub(crate) fn spawn_ci_watch(
    reader: Arc<dyn CheckStateReader>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let Some(interval) = resolve_watch_interval() else {
        eprintln!("[ci-watch] disabled (TACHI_CI_WATCH_INTERVAL_SECS=0)");
        return tokio::spawn(async {});
    };
    if !ci_watch_enabled() {
        eprintln!("[ci-watch] disabled (TACHI_CI_WATCH=off or not a daemon)");
        return tokio::spawn(async {});
    }
    eprintln!(
        "[ci-watch] enabled (interval={}s, min={}s)",
        interval.as_secs(),
        MIN_CI_WATCH_INTERVAL_SECS
    );
    tokio::spawn(async move {
        // Consume the immediate first tick so we wait a full cadence before
        // the first poll (matches the WAL-checkpoint / distill pattern).
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = ticker.tick() => {
                    let runs_root = shell_runs_root();
                    let summary = run_poll_cycle(reader.as_ref(), || {
                        discover_watch_targets(&runs_root)
                    }).await;
                    eprintln!(
                        "[ci-watch] cycle: polled={} transitions={} errors={} rate_limited={} skipped={}",
                        summary.polled, summary.transitions, summary.errors,
                        summary.rate_limited, summary.skipped
                    );
                }
            }
        }
    })
}

#[cfg(test)]
mod tests;
