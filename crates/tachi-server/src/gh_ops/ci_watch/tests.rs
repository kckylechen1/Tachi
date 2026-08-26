use super::*;
use crate::gh_ops::safe_merge::{
    ingest_check_state_transition, CheckStateIngestRequest, CheckStateLedgerState, CheckStateRead,
};
use async_trait::async_trait;
use serde_json::json;
use std::time::Duration;
use tachi_gh_safe_merge::{CheckRun, GhError};

/// A reader whose response can vary per call, so a single test can simulate a
/// pending→failed transition across two cycles.
struct ScriptedReader {
    responses: std::sync::Mutex<std::collections::VecDeque<Result<CheckStateRead, GhError>>>,
}

impl ScriptedReader {
    fn new(responses: Vec<Result<CheckStateRead, GhError>>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into()),
        }
    }
}

#[async_trait]
impl CheckStateReader for ScriptedReader {
    async fn read_check_state(
        &self,
        _repo: &str,
        _pr_number: u64,
        _expected_head_sha: Option<&str>,
    ) -> Result<CheckStateRead, GhError> {
        self.responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .unwrap_or_else(|| {
                Ok(CheckStateRead {
                    checks: Vec::new(),
                    observed_head_sha: None,
                })
            })
    }
}

/// A reader that records how many times it was called, used to prove a
/// rate-limit short-circuits the cycle (remaining targets are NOT polled).
struct CountingReader {
    calls: std::sync::Mutex<Vec<(String, u64)>>,
    result: Result<CheckStateRead, GhError>,
}

impl CountingReader {
    fn new(result: Result<CheckStateRead, GhError>) -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            result,
        }
    }
    fn calls(&self) -> usize {
        self.calls.lock().expect("calls lock").len()
    }
}

#[async_trait]
impl CheckStateReader for CountingReader {
    async fn read_check_state(
        &self,
        repo: &str,
        pr_number: u64,
        _expected_head_sha: Option<&str>,
    ) -> Result<CheckStateRead, GhError> {
        self.calls
            .lock()
            .expect("calls lock")
            .push((repo.to_string(), pr_number));
        self.result.clone()
    }
}

/// A reader whose call count is observable through a shared `AtomicUsize`,
/// decoupled from the `Arc<dyn CheckStateReader>` handed to the spawned task.
/// The backoff test drives `spawn_ci_watch` (which owns the reader as a trait
/// object) but still needs to read the live call count from outside the loop —
/// `Arc<dyn Trait>` cannot be downcast to `dyn Any`, so the count is shared via
/// this side-channel atomic instead.
struct SharedCountingReader {
    counter: Arc<std::sync::atomic::AtomicUsize>,
    result: Result<CheckStateRead, GhError>,
}

impl SharedCountingReader {
    fn new(
        counter: Arc<std::sync::atomic::AtomicUsize>,
        result: Result<CheckStateRead, GhError>,
    ) -> Self {
        Self { counter, result }
    }
}

#[async_trait]
impl CheckStateReader for SharedCountingReader {
    async fn read_check_state(
        &self,
        _repo: &str,
        _pr_number: u64,
        _expected_head_sha: Option<&str>,
    ) -> Result<CheckStateRead, GhError> {
        self.counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.result.clone()
    }
}

fn check(name: &str, status: &str, conclusion: Option<&str>) -> CheckRun {
    CheckRun {
        name: name.to_string(),
        status: status.to_string(),
        conclusion: conclusion.map(str::to_string),
    }
}

fn fixture_target(flow: &str) -> WatchTarget {
    WatchTarget {
        flow_id: flow.to_string(),
        repo: "o/r".to_string(),
        pr_number: 42,
        pr_ref: Some("o/r#42".to_string()),
        head_ref: Some("feat/watch".to_string()),
        expected_head_sha: Some("head-1".to_string()),
    }
}

/// Acquire the process-global run-root env lock (same one the check-state
/// artifact tests use) and point `TACHI_RUN_ROOT` at a temp dir. Returns the
/// guard plus the original env value so the caller restores it. The guard is
/// `&'static`-bound because the lock is a `static` Mutex — no transmute.
fn lock_run_root(
    tmp: &tempfile::TempDir,
) -> (
    std::sync::MutexGuard<'static, ()>,
    Option<std::ffi::OsString>,
) {
    let guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());
    (guard, original)
}

/// **Red/green test (a).** Proves the watcher records a red-check transition
/// into a flow's `check_state` artifact.
///
/// Structural discrimination: before this lane, no `ci_watch` module existed,
/// so `run_poll_cycle` + `poll_one_pr` could not be called — there was no
/// background loop to invoke `ingest_check_state_transition` for a watched PR.
/// Concretely, a flow's `check_state.json` would never be written by a poller.
/// Here the cycle ingests a pending-then-failed sequence and we assert the
/// artifact's `transition.state` advances `pending → failed` with `changed:
/// true`, which is impossible without the watcher driving the recorder.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn ci_watch_records_red_check_transition_into_flow_artifact() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_guard, original) = lock_run_root(&tmp);
    let target = fixture_target("flow_watch-red-check");

    // Cycle 1: pending check.
    let pending_reader = ScriptedReader::new(vec![Ok(CheckStateRead {
        checks: vec![check("ci", "in_progress", None)],
        observed_head_sha: Some("head-1".to_string()),
    })]);
    let first = run_poll_cycle(&pending_reader, || vec![target.clone()]).await;
    assert_eq!(first.polled, 1);
    assert_eq!(first.transitions, 1);
    assert_eq!(first.errors, 0);
    assert!(!first.rate_limited);

    // Cycle 2: check went red.
    let failed_reader = ScriptedReader::new(vec![Ok(CheckStateRead {
        checks: vec![check("ci", "completed", Some("failure"))],
        observed_head_sha: Some("head-1".to_string()),
    })]);
    let second = run_poll_cycle(&failed_reader, || vec![target.clone()]).await;
    assert_eq!(second.transitions, 1);

    // The red-check transition was persisted to the flow's check_state.json.
    let run_dir = tmp.path().join(&target.flow_id);
    let artifact: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("check_state.json")).expect("artifact written"),
    )
    .expect("artifact is json");
    assert_eq!(artifact["transition"]["previous_state"], json!("pending"));
    assert_eq!(artifact["transition"]["state"], json!("failed"));
    assert_eq!(artifact["transition"]["changed"], json!(true));
    assert_eq!(artifact["aggregate"]["conclusion"], json!("failure"));
    assert_eq!(artifact["failed_checks_recorded_only"], json!(true));
    assert_eq!(artifact["repair_attempted"], json!(false));
    assert_eq!(artifact["merge_attempted"], json!(false));

    restore_run_root(original);
}

/// **Resilience test (b).** Proves a poll failure for one PR does NOT stop the
/// loop or skip other PRs. A reader that errors for the first PR but succeeds
/// for the second must still record the second PR's transition.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn ci_watch_continues_after_one_pr_poll_failure() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_guard, original) = lock_run_root(&tmp);
    let target_a = WatchTarget {
        flow_id: "flow_watch-error-pr".to_string(),
        repo: "o/r".to_string(),
        pr_number: 10,
        pr_ref: Some("o/r#10".to_string()),
        head_ref: None,
        expected_head_sha: None,
    };
    let target_b = WatchTarget {
        flow_id: "flow_watch-ok-pr".to_string(),
        repo: "o/r".to_string(),
        pr_number: 11,
        pr_ref: Some("o/r#11".to_string()),
        head_ref: None,
        expected_head_sha: None,
    };

    // Reader: first call errors (target A), second call succeeds (target B).
    // poll_one_pr probes then records; the recorder converts the error into a
    // reader_error ledger state (returns Ok(false)), so the loop continues.
    let reader = ScriptedReader::new(vec![
        Err(GhError::Sanitized("network blip".to_string())),
        Ok(CheckStateRead {
            checks: vec![check("ci", "completed", Some("success"))],
            observed_head_sha: None,
        }),
    ]);

    let summary = run_poll_cycle(&reader, || vec![target_a.clone(), target_b.clone()]).await;
    // Both targets were polled (the error became a recorded reader_error state,
    // not a propagated failure that aborts the cycle).
    assert_eq!(summary.polled, 2);
    assert_eq!(
        summary.errors, 0,
        "reader_error is a recorded state, not a counted error"
    );
    assert!(!summary.rate_limited);
    assert_eq!(summary.skipped, 0);

    // Target A: reader_error transition recorded.
    let a_artifact: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(&target_a.flow_id).join("check_state.json"))
            .expect("A artifact"),
    )
    .expect("A json");
    assert_eq!(a_artifact["transition"]["state"], json!("reader_error"));

    // Target B: passed transition recorded.
    let b_artifact: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(&target_b.flow_id).join("check_state.json"))
            .expect("B artifact"),
    )
    .expect("B json");
    assert_eq!(b_artifact["transition"]["state"], json!("passed"));

    restore_run_root(original);
}

/// **Rate-limit test (c).** Proves a `GhError::RateLimited` causes a skip, not
/// a crash. When the first target is rate-limited, the cycle short-circuits
/// (the remaining targets are NOT polled that cycle) and returns
/// `rate_limited: true`. The loop itself does not panic.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn ci_watch_rate_limit_skips_cycle_without_crashing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_guard, original) = lock_run_root(&tmp);
    let target_a = WatchTarget {
        flow_id: "flow_watch-rl-a".to_string(),
        repo: "o/r".to_string(),
        pr_number: 20,
        pr_ref: Some("o/r#20".to_string()),
        head_ref: None,
        expected_head_sha: None,
    };
    let target_b = WatchTarget {
        flow_id: "flow_watch-rl-b".to_string(),
        repo: "o/r".to_string(),
        pr_number: 21,
        pr_ref: Some("o/r#21".to_string()),
        head_ref: None,
        expected_head_sha: None,
    };

    // Reader: rate-limited on every call.
    let reader = CountingReader::new(Err(GhError::RateLimited(
        "secondary rate limit".to_string(),
    )));

    let summary = run_poll_cycle(&reader, || vec![target_a.clone(), target_b.clone()]).await;
    // Rate-limited mid-cycle: the cycle is flagged and short-circuited.
    assert!(summary.rate_limited);
    // Only the first target was read (1 read), the second was NOT polled.
    assert_eq!(reader.calls(), 1);
    // Target A was polled (the recorder ran and recorded a reader_error state
    // for it before the rate-limit was detected and surfaced).
    assert_eq!(summary.polled, 1);
    assert_eq!(summary.skipped, 1); // target B was skipped

    // Target A: the rate-limit was recorded as a reader_error transition.
    let a_artifact: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(&target_a.flow_id).join("check_state.json"))
            .expect("A artifact"),
    )
    .expect("A json");
    assert_eq!(a_artifact["transition"]["state"], json!("reader_error"));
    assert!(
        a_artifact["transition"]["read_error"]
            .as_str()
            .is_some_and(|e| e.to_ascii_lowercase().contains("rate limit")),
        "read_error should mention rate limit: {:?}",
        a_artifact["transition"]["read_error"]
    );
    // Target B: NOT polled — no artifact written (cycle short-circuited).
    assert!(!tmp
        .path()
        .join(&target_b.flow_id)
        .join("check_state.json")
        .exists());

    // Typed-field discrimination (finding #2b): the rate-limit signal must be a
    // `bool` set from `matches!(err, GhError::RateLimited(_))`, NOT derived by
    // substring-matching the recorded `read_error`. Drive the recorder directly
    // against both a RateLimited and a Sanitized error whose message
    // DELIBERATELY contains "rate limit" to prove the typed match is what
    // governs the flag (a Sanitized error mentioning "rate limit" must NOT set
    // it, and a RateLimited must — regardless of message wording).
    let typed_request = CheckStateIngestRequest {
        flow_id: "flow_watch-rl-typed",
        repo: "o/r",
        pr_number: 99,
        pr_ref: Some("o/r#99"),
        head_ref: None,
        expected_head_sha: None,
        source: "ci_state.watch",
    };
    let rate_limited_result = ingest_check_state_transition(
        &CountingReader::new(Err(GhError::RateLimited("403 too many".to_string()))),
        &typed_request,
    )
    .await
    .expect("ingest rate-limited");
    assert!(
        rate_limited_result.rate_limited,
        "RateLimited variant must set rate_limited=true (typed match, not string)"
    );
    // A Sanitized error that happens to mention "rate limit" must NOT trip the
    // typed flag — this is exactly the false-positive the substring match risked.
    let sanitized_luring = ingest_check_state_transition(
        &CountingReader::new(Err(GhError::Sanitized(
            "upstream returned 'rate limit exceeded' as a generic error".to_string(),
        ))),
        &CheckStateIngestRequest {
            flow_id: "flow_watch-rl-luring",
            repo: "o/r",
            pr_number: 100,
            pr_ref: Some("o/r#100"),
            head_ref: None,
            expected_head_sha: None,
            source: "ci_state.watch",
        },
    )
    .await
    .expect("ingest sanitized");
    assert!(
        !sanitized_luring.rate_limited,
        "Sanitized error (even one mentioning 'rate limit') must NOT set rate_limited=true"
    );

    restore_run_root(original);
}

/// Discovery: a flow with a linked PR (github.repo + github.pr_number) and no
/// close_loop.json is tracked; a terminal flow (close_loop.json present OR
/// merge_state=merged) is skipped.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn ci_watch_discovery_tracks_open_prs_and_skips_terminal_flows() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_guard, original) = lock_run_root(&tmp);
    let root = tmp.path();

    // Tracked: open PR, not terminal.
    let tracked = root.join("flow_watch-tracked");
    std::fs::create_dir_all(&tracked).unwrap();
    std::fs::write(
        tracked.join("status.json"),
        json!({
            "github": {
                "repo": "o/r",
                "pr_number": 42,
                "pr_url": "https://github.com/o/r/pull/42",
                "merge_state": "pending",
            }
        })
        .to_string(),
    )
    .unwrap();

    // Terminal via close_loop.json.
    let closed = root.join("flow_watch-closed");
    std::fs::create_dir_all(&closed).unwrap();
    std::fs::write(
        closed.join("status.json"),
        json!({ "github": { "repo": "o/r", "pr_number": 43 } }).to_string(),
    )
    .unwrap();
    std::fs::write(closed.join("close_loop.json"), "{}").unwrap();

    // Terminal via merged PR.
    let merged = root.join("flow_watch-merged");
    std::fs::create_dir_all(&merged).unwrap();
    std::fs::write(
        merged.join("status.json"),
        json!({ "github": { "repo": "o/r", "pr_number": 44, "merge_state": "merged" } })
            .to_string(),
    )
    .unwrap();

    // No github block: skipped.
    let nopr = root.join("flow_watch-no-pr");
    std::fs::create_dir_all(&nopr).unwrap();
    std::fs::write(nopr.join("status.json"), json!({}).to_string()).unwrap();

    // Invalid flow id (doesn't start with flow_): skipped for safety.
    let bogus = root.join("not-a-flow");
    std::fs::create_dir_all(&bogus).unwrap();
    std::fs::write(
        bogus.join("status.json"),
        json!({ "github": { "repo": "o/r", "pr_number": 99 } }).to_string(),
    )
    .unwrap();

    let targets = discover_watch_targets(root);
    assert_eq!(
        targets.len(),
        1,
        "only the tracked open PR should be watched"
    );
    assert_eq!(targets[0].flow_id, "flow_watch-tracked");
    assert_eq!(targets[0].repo, "o/r");
    assert_eq!(targets[0].pr_number, 42);
    assert_eq!(targets[0].pr_ref.as_deref(), Some("o/r#42"));

    restore_run_root(original);
}

/// Discovery picks up the expected_head_sha from a pre-existing
/// check_state.json so a subsequent poll can detect a moved head as Stale.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn ci_watch_discovery_reads_expected_head_sha_from_prior_artifact() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_guard, original) = lock_run_root(&tmp);
    let root = tmp.path();

    let flow = root.join("flow_watch-prior-sha");
    std::fs::create_dir_all(&flow).unwrap();
    std::fs::write(
        flow.join("status.json"),
        json!({ "github": { "repo": "o/r", "pr_number": 42 } }).to_string(),
    )
    .unwrap();
    // A prior cycle recorded an expected head SHA.
    std::fs::write(
        flow.join("check_state.json"),
        json!({
            "schema": "tachi.github.check_state.v1",
            "transition": { "expected_head_sha": "head-v1" }
        })
        .to_string(),
    )
    .unwrap();

    let targets = discover_watch_targets(root);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].expected_head_sha.as_deref(), Some("head-v1"));

    restore_run_root(original);
}

/// `Stale` is now reachable in production: when the observed head diverges
/// from the expected head, the recorder emits a `stale` transition. This is
/// the #816 gap this lane closes (the blanket impl now populates
/// observed_head_sha).
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn ci_watch_records_stale_when_observed_head_diverges() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_guard, original) = lock_run_root(&tmp);
    let target = WatchTarget {
        flow_id: "flow_watch-stale".to_string(),
        repo: "o/r".to_string(),
        pr_number: 42,
        pr_ref: Some("o/r#42".to_string()),
        head_ref: None,
        expected_head_sha: Some("expected-head".to_string()),
    };
    // Reader observes checks passing BUT on a different head SHA → Stale.
    let reader = ScriptedReader::new(vec![Ok(CheckStateRead {
        checks: vec![check("ci", "completed", Some("success"))],
        observed_head_sha: Some("observed-different-head".to_string()),
    })]);
    let summary = run_poll_cycle(&reader, || vec![target.clone()]).await;
    assert_eq!(summary.transitions, 1);

    let artifact: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(&target.flow_id).join("check_state.json"))
            .expect("artifact"),
    )
    .expect("json");
    assert_eq!(artifact["transition"]["state"], json!("stale"));
    assert_eq!(
        artifact["transition"]["expected_head_sha"],
        json!("expected-head")
    );
    assert_eq!(
        artifact["transition"]["observed_head_sha"],
        json!("observed-different-head")
    );

    restore_run_root(original);
}

/// The blanket `CheckStateReader` impl on `GhClient` now populates
/// `observed_head_sha` from `pr_view` (the #816 gap fix). Drive it through the
/// real `MockGhClient` to prove the production reader path reaches Stale.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn ci_watch_blanket_reader_populates_observed_head_sha_from_pr_view() {
    use tachi_gh_safe_merge::{
        ChecksState, Mergeable, MockGhClient, PrLifecycleState, PrState, ReviewDecision,
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_guard, original) = lock_run_root(&tmp);
    let ready = PrState {
        number: 42,
        state: PrLifecycleState::Open,
        mergeable: Mergeable::Mergeable,
        review_decision: Some(ReviewDecision::Approved),
        checks: ChecksState::Success,
        is_draft: false,
        head_sha: "live-head".to_string(),
        head_ref: Some("feat/live".to_string()),
        linked_issue_refs: Vec::new(),
        closing_issue_labels: Vec::new(),
        head_consistent: None,
    };
    let client = MockGhClient::new().with_pr("o/r", ready).with_checks(
        "o/r",
        42,
        vec![CheckRun {
            name: "ci".to_string(),
            status: "completed".to_string(),
            conclusion: Some("success".to_string()),
        }],
    );

    let target = WatchTarget {
        flow_id: "flow_watch-blanket".to_string(),
        repo: "o/r".to_string(),
        pr_number: 42,
        pr_ref: Some("o/r#42".to_string()),
        head_ref: None,
        // Expected head diverges from the live head the mock reports → Stale.
        expected_head_sha: Some("stale-expected".to_string()),
    };
    let summary = run_poll_cycle(&client, || vec![target.clone()]).await;
    assert_eq!(summary.transitions, 1);

    let artifact: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(&target.flow_id).join("check_state.json"))
            .expect("artifact"),
    )
    .expect("json");
    assert_eq!(artifact["transition"]["state"], json!("stale"));
    assert_eq!(
        artifact["transition"]["observed_head_sha"],
        json!("live-head")
    );

    restore_run_root(original);
}

/// Config: `TACHI_CI_WATCH_INTERVAL_SECS=0` disables the watcher (returns
/// None), and a value below the min is clamped up.
#[test]
fn ci_watch_interval_resolves_with_min_clamp_and_disable() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let saved = std::env::var("TACHI_CI_WATCH_INTERVAL_SECS").ok();
    // Disabled.
    std::env::set_var("TACHI_CI_WATCH_INTERVAL_SECS", "0");
    assert!(resolve_watch_interval().is_none());
    // Below min → clamped to MIN_CI_WATCH_INTERVAL_SECS.
    std::env::set_var("TACHI_CI_WATCH_INTERVAL_SECS", "1");
    let interval = resolve_watch_interval().expect("clamped interval");
    assert_eq!(interval.as_secs(), MIN_CI_WATCH_INTERVAL_SECS);
    // Explicit value honored when above min.
    std::env::set_var("TACHI_CI_WATCH_INTERVAL_SECS", "120");
    assert_eq!(resolve_watch_interval().unwrap().as_secs(), 120);
    // Default when unset.
    std::env::remove_var("TACHI_CI_WATCH_INTERVAL_SECS");
    assert_eq!(
        resolve_watch_interval().unwrap().as_secs(),
        DEFAULT_CI_WATCH_INTERVAL_SECS
    );

    if let Some(v) = saved {
        std::env::set_var("TACHI_CI_WATCH_INTERVAL_SECS", v);
    } else {
        std::env::remove_var("TACHI_CI_WATCH_INTERVAL_SECS");
    }
}

/// Config: the watcher is enabled by default only when `TACHI_DAEMON` is set,
/// but `TACHI_CI_WATCH=on` forces it on regardless.
#[test]
fn ci_watch_enabled_gating() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let saved_watch = std::env::var("TACHI_CI_WATCH").ok();
    let saved_daemon = std::env::var("TACHI_DAEMON").ok();

    // No daemon flag → disabled by default.
    std::env::remove_var("TACHI_CI_WATCH");
    std::env::remove_var("TACHI_DAEMON");
    assert!(!ci_watch_enabled());

    // Daemon set → enabled by default.
    std::env::set_var("TACHI_DAEMON", "1");
    assert!(ci_watch_enabled());

    // Explicit disable wins even when daemon is set.
    std::env::set_var("TACHI_CI_WATCH", "off");
    assert!(!ci_watch_enabled());

    // Explicit enable wins even without daemon (for tests / forced runs).
    std::env::remove_var("TACHI_DAEMON");
    std::env::set_var("TACHI_CI_WATCH", "on");
    assert!(ci_watch_enabled());

    // Restore.
    match saved_watch {
        Some(v) => std::env::set_var("TACHI_CI_WATCH", v),
        None => std::env::remove_var("TACHI_CI_WATCH"),
    }
    match saved_daemon {
        Some(v) => std::env::set_var("TACHI_DAEMON", v),
        None => std::env::remove_var("TACHI_DAEMON"),
    }
}

/// Spawned task exits cleanly on shutdown (matches the background-loop test
/// pattern in `bootstrap/serve/background.rs`).
#[tokio::test]
async fn ci_watch_spawn_exits_on_shutdown_cancel() {
    let reader: Arc<dyn CheckStateReader> = Arc::new(ScriptedReader::new(vec![]));
    let shutdown = CancellationToken::new();
    // Force-enable so spawn doesn't no-op.
    let saved_watch = std::env::var("TACHI_CI_WATCH").ok();
    let saved_interval = std::env::var("TACHI_CI_WATCH_INTERVAL_SECS").ok();
    std::env::set_var("TACHI_CI_WATCH", "on");
    std::env::set_var("TACHI_CI_WATCH_INTERVAL_SECS", "15");
    let handle = spawn_ci_watch(reader, shutdown.clone());
    shutdown.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("ci watch task did not exit within 2s after shutdown")
        .expect("task panicked");
    match saved_watch {
        Some(v) => std::env::set_var("TACHI_CI_WATCH", v),
        None => std::env::remove_var("TACHI_CI_WATCH"),
    }
    match saved_interval {
        Some(v) => std::env::set_var("TACHI_CI_WATCH_INTERVAL_SECS", v),
        None => std::env::remove_var("TACHI_CI_WATCH_INTERVAL_SECS"),
    }
}

/// **pr_view gate test (finding #4).** Proves the blanket
/// `CheckStateReader` impl only calls `pr_view` when there is an expected head
/// SHA to compare against (i.e. a prior artifact exists). On the FIRST poll of
/// a new flow (`expected_head_sha` is None) `pr_view` is NOT called at all —
/// halving per-PR API cost — and `Stale` stays unreachable (nothing to be
/// stale against). On the SECOND poll (expected SHA now present from the first
/// cycle's artifact), `pr_view` IS called so a moved head can be detected.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn ci_watch_pr_view_gated_on_expected_head_sha() {
    use crate::gh_ops::safe_merge::CheckStateReader;
    use tachi_gh_safe_merge::{
        ChecksState, Mergeable, MockGhClient, PrLifecycleState, PrState, ReviewDecision,
    };

    let tmp = tempfile::tempdir().expect("tempdir");
    let (_guard, original) = lock_run_root(&tmp);

    let ready = PrState {
        number: 42,
        state: PrLifecycleState::Open,
        mergeable: Mergeable::Mergeable,
        review_decision: Some(ReviewDecision::Approved),
        checks: ChecksState::Success,
        is_draft: false,
        head_sha: "live-head".to_string(),
        head_ref: Some("feat/live".to_string()),
        linked_issue_refs: Vec::new(),
        closing_issue_labels: Vec::new(),
        head_consistent: None,
    };
    let client = MockGhClient::new().with_pr("o/r", ready).with_checks(
        "o/r",
        42,
        vec![CheckRun {
            name: "ci".to_string(),
            status: "completed".to_string(),
            conclusion: Some("success".to_string()),
        }],
    );

    // FIRST poll of a brand-new flow: no prior artifact → expected_head_sha is
    // None → the blanket impl MUST skip pr_view.
    let first = CheckStateReader::read_check_state(&client, "o/r", 42, None)
        .await
        .expect("first read");
    assert!(
        client.pr_view_calls().is_empty(),
        "first poll (expected_head_sha=None) must NOT call pr_view, but got {:?}",
        client.pr_view_calls()
    );
    // observed_head_sha stays None on a gated read — Stale not reachable, correct.
    assert_eq!(first.observed_head_sha, None);
    assert!(!first.checks.is_empty(), "checks_list was still called");

    // SECOND poll: now there IS an expected SHA to compare against → pr_view
    // IS called so a moved head can be detected as Stale.
    let second = CheckStateReader::read_check_state(&client, "o/r", 42, Some("expected-head"))
        .await
        .expect("second read");
    assert_eq!(
        client.pr_view_calls().len(),
        1,
        "second poll (expected_head_sha=Some) MUST call pr_view exactly once"
    );
    // observed_head_sha is now populated from pr_view.
    assert_eq!(second.observed_head_sha.as_deref(), Some("live-head"));

    restore_run_root(original);
}

/// **Backoff test (finding #2).** Proves that after a rate-limited cycle the
/// watcher skips ticks (the reader's call count stays flat across the backoff
/// window) instead of resuming at the fixed cadence. Uses `tokio::time::pause()`
/// so the real wall-clock sleeps are virtualized and the test is fast.
///
/// Strategy: drive `spawn_ci_watch` with a rate-limiting reader; assert that
/// across a backoff window (>= base interval * 2^1) the reader call count does
/// not increase, then after the backoff delay elapses a second cycle runs.
#[allow(clippy::await_holding_lock)]
#[tokio::test(start_paused = true)]
async fn ci_watch_backoff_skips_ticks_after_rate_limit() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_run_guard, original_run) = lock_run_root(&tmp);
    // Seed a tracked flow so discovery returns a target every cycle.
    let flow_dir = tmp.path().join("flow_watch-backoff");
    std::fs::create_dir_all(&flow_dir).unwrap();
    std::fs::write(
        flow_dir.join("status.json"),
        json!({ "github": { "repo": "o/r", "pr_number": 7, "pr_url": "https://github.com/o/r/pull/7" } }).to_string(),
    )
    .unwrap();

    // Reader: rate-limits on EVERY call, recording each call through a shared
    // atomic so the test can observe the live count without downcasting the
    // trait object the spawned task owns.
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reader: Arc<dyn CheckStateReader> = Arc::new(SharedCountingReader::new(
        counter.clone(),
        Err(GhError::RateLimited("secondary rate limit".to_string())),
    ));
    let calls = || counter.load(std::sync::atomic::Ordering::SeqCst);

    let saved_watch = std::env::var("TACHI_CI_WATCH").ok();
    let saved_interval = std::env::var("TACHI_CI_WATCH_INTERVAL_SECS").ok();
    std::env::set_var("TACHI_CI_WATCH", "on");
    // Min clamp is 15s; with time paused this is virtualized. Backoff after one
    // rate-limited cycle = 15s * 2^1 = 30s before the next poll.
    std::env::set_var("TACHI_CI_WATCH_INTERVAL_SECS", "15");

    let shutdown = CancellationToken::new();
    let handle = spawn_ci_watch(reader.clone(), shutdown.clone());

    // Let the initial base-interval sleep (15s) + first poll cycle run.
    tokio::time::sleep(Duration::from_secs(16)).await;
    let calls_after_first = calls();
    assert_eq!(
        calls_after_first, 1,
        "exactly one poll cycle (one reader call) should have run after the base interval"
    );

    // Advance partway through the backoff window (after a rate-limited cycle the
    // next delay is 15s * 2^1 = 30s). 20s in is within the backoff window → the
    // reader call count must stay flat (no second cycle yet).
    tokio::time::sleep(Duration::from_secs(20)).await;
    assert_eq!(
        calls(),
        calls_after_first,
        "reader call count must stay flat across the backoff window (no re-poll until the backoff delay elapses)"
    );

    // Advance past the full backoff delay (total elapsed from first poll now
    // exceeds 30s) → a second poll cycle runs.
    tokio::time::sleep(Duration::from_secs(20)).await;
    assert_eq!(
        calls(),
        calls_after_first + 1,
        "after the backoff delay elapses, the watcher must resume and run a second cycle"
    );

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("ci watch task did not exit within 5s after shutdown")
        .expect("task panicked");

    match saved_watch {
        Some(v) => std::env::set_var("TACHI_CI_WATCH", v),
        None => std::env::remove_var("TACHI_CI_WATCH"),
    }
    match saved_interval {
        Some(v) => std::env::set_var("TACHI_CI_WATCH_INTERVAL_SECS", v),
        None => std::env::remove_var("TACHI_CI_WATCH_INTERVAL_SECS"),
    }
    restore_run_root(original_run);
}

/// **Backoff curve unit test (finding #2).** Pins the exponential backoff
/// formula `min(base * 2^n, cap=30min)` and its reset-on-clean semantics, so a
/// future edit can't silently weaken the curve.
#[test]
fn ci_watch_backoff_delay_doubles_per_consecutive_rate_limit_and_caps() {
    let base = Duration::from_secs(60);
    // Clean cycle → base cadence.
    assert_eq!(backoff_delay(base, 0), base);
    // 1 consecutive rate-limit → 2x.
    assert_eq!(backoff_delay(base, 1), Duration::from_secs(120));
    // 2 → 4x.
    assert_eq!(backoff_delay(base, 2), Duration::from_secs(240));
    // 3 → 8x.
    assert_eq!(backoff_delay(base, 3), Duration::from_secs(480));
    // Cap kicks in: 2^5 * 60s = 1920s, 2^6 * 60s = 3840s > 1800s cap.
    assert_eq!(
        backoff_delay(base, 6),
        RATE_LIMIT_BACKOFF_CAP,
        "must be clamped to the 30 min cap"
    );
    // Even extreme counts never exceed the cap and never overflow.
    assert_eq!(backoff_delay(base, u32::MAX), RATE_LIMIT_BACKOFF_CAP);
}

fn restore_run_root(original: Option<std::ffi::OsString>) {
    match original {
        Some(v) => std::env::set_var("TACHI_RUN_ROOT", v),
        None => std::env::remove_var("TACHI_RUN_ROOT"),
    }
}

/// Silence unused-symbol warnings for items only referenced by assertions in
/// the blanket-impl test above (kept import-explicit for readability).
#[allow(dead_code)]
fn _silence_imports(_: CheckStateLedgerState) {}
