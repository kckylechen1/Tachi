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
/// Rate-limit detection is **typed**: #816's recorder matches on the
/// `GhError::RateLimited` variant and sets `result.rate_limited` directly —
/// the watcher reads that boolean from the in-memory `CheckStateIngestResult`
/// instead of re-reading JSON + substring-matching the `read_error` string.
/// String-matching was a ban-risk: a non-rate-limit error whose message
/// contained "rate limit" would falsely trip the backoff, and a genuine
/// rate-limit whose Display form ever changed would silently miss it.
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
            // Typed rate-limit signal: read the boolean set by the recorder from
            // a `matches!(err, GhError::RateLimited(_))` match, NOT from
            // re-reading JSON + substring-matching. The transition was still
            // recorded (so the operator sees the reader_error state), but we
            // stop polling further targets this cycle to respect the rate
            // limit.
            if result.rate_limited && result.artifact.persisted {
                return Err(GhError::RateLimited(
                    "ci watch cycle rate-limited; skipping remaining targets".to_string(),
                ));
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
        expected_head_sha: Option<&str>,
    ) -> Result<CheckStateRead, GhError> {
        // Build the selected client for this poll. `gh_client_for_server`
        // decides CLI vs HTTP from `TACHI_GH_TRANSPORT`; both implement
        // `GhClient`, and the blanket impl gives `GhClient -> CheckStateReader`.
        let client = gh_client_for_server(&self.server)
            .map_err(|err| GhError::Sanitized(format!("ci-watch client build: {err}")))?;
        match client {
            SelectedGhClient::Cli(cli) => {
                CheckStateReader::read_check_state(&cli, repo, pr_number, expected_head_sha).await
            }
            SelectedGhClient::Http(http) => {
                CheckStateReader::read_check_state(&http, repo, pr_number, expected_head_sha).await
            }
        }
    }
}

/// Cap for the exponential backoff after a rate-limited cycle. GitHub's
/// secondary rate-limit window is short (~minutes), so 30 min is a generous
/// ceiling that still bounds worst-case latency before the watcher retries.
const RATE_LIMIT_BACKOFF_CAP: Duration = Duration::from_secs(30 * 60);

/// Compute the next poll delay, applying an exponential backoff when the
/// previous cycle was rate-limited. `consecutive_rate_limits` counts how many
/// cycles in a row hit a rate-limit; the delay is
/// `min(base_interval * 2^consecutive_rate_limits, RATE_LIMIT_BACKOFF_CAP)`.
/// A clean cycle resets the counter to 0 (no extra delay). Exposed as a free
/// fn so the backoff curve is unit-testable without a running loop.
pub(crate) fn backoff_delay(base_interval: Duration, consecutive_rate_limits: u32) -> Duration {
    if consecutive_rate_limits == 0 {
        return base_interval;
    }
    // `2^consecutive_rate_limits` saturates at u32::MAX, but the cap makes the
    // overflow unreachable well before that (2^11 * 60s > 30min).
    let multiplier = 2u32.saturating_pow(consecutive_rate_limits);
    let scaled = base_interval.saturating_mul(multiplier);
    scaled.min(RATE_LIMIT_BACKOFF_CAP)
}

/// Spawn the background CI watcher loop. Matches the architecture of the other
/// background loops in `bootstrap/serve/background.rs`: a `tokio::spawn`'d task
/// driven by a `CancellationToken`, gated on config, returning a `JoinHandle`
/// the caller holds for the daemon lifetime.
///
/// The reader is boxed (`Arc<dyn CheckStateReader>`) so the real `GhClient`
/// (cli or http transport) can be injected at spawn time, while tests inject a
/// `FixedCheckReader`.
///
/// **Rate-limit backoff.** A `tokio::time::interval` would resume at the fixed
/// cadence every cycle, hammering GitHub after a 403. Instead the loop is a
/// manual sleep: the next delay is `backoff_delay(base, consecutive_rate_limits)`,
/// doubling per consecutive rate-limited cycle (capped at 30 min) and resetting
/// to the base cadence on a clean (non-rate-limited) cycle. This is the ban-risk
/// mitigation for GitHub's secondary rate-limit.
pub(crate) fn spawn_ci_watch(
    reader: Arc<dyn CheckStateReader>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let Some(base_interval) = resolve_watch_interval() else {
        eprintln!("[ci-watch] disabled (TACHI_CI_WATCH_INTERVAL_SECS=0)");
        return tokio::spawn(async {});
    };
    if !ci_watch_enabled() {
        eprintln!("[ci-watch] disabled (TACHI_CI_WATCH=off or not a daemon)");
        return tokio::spawn(async {});
    }
    eprintln!(
        "[ci-watch] enabled (interval={}s, min={}s, backoff_cap={}s)",
        base_interval.as_secs(),
        MIN_CI_WATCH_INTERVAL_SECS,
        RATE_LIMIT_BACKOFF_CAP.as_secs()
    );
    tokio::spawn(async move {
        // Backoff state: counts consecutive rate-limited cycles. Reset to 0 on
        // any clean (non-rate-limited) cycle. Drives the exponential delay via
        // `backoff_delay`.
        let mut consecutive_rate_limits: u32 = 0;
        // Wait a full base cadence before the first poll (matches the
        // WAL-checkpoint / distill "no immediate first tick" pattern).
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(base_interval) => {}
        }
        loop {
            let runs_root = shell_runs_root();
            let summary =
                run_poll_cycle(reader.as_ref(), || discover_watch_targets(&runs_root)).await;
            eprintln!(
                "[ci-watch] cycle: polled={} transitions={} errors={} rate_limited={} skipped={}",
                summary.polled,
                summary.transitions,
                summary.errors,
                summary.rate_limited,
                summary.skipped
            );
            // Backoff bookkeeping: increment on a rate-limited cycle, reset on
            // a clean one. A cycle with no targets (skipped == len, polled 0)
            // counts as clean — there was no rate-limit signal.
            if summary.rate_limited {
                consecutive_rate_limits = consecutive_rate_limits.saturating_add(1);
                eprintln!(
                    "[ci-watch] rate-limited; backing off (consecutive={}, next_delay={}s)",
                    consecutive_rate_limits,
                    backoff_delay(base_interval, consecutive_rate_limits).as_secs()
                );
            } else {
                consecutive_rate_limits = 0;
            }
            let delay = backoff_delay(base_interval, consecutive_rate_limits);
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(delay) => {}
            }
        }
    })
}

#[cfg(test)]
mod tests;
