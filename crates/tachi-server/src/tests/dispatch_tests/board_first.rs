//! kckylechen1/tachi#971 — dispatches must not be invisible to board/status
//! pollers until the V2 plan stage (up to 180s LLM call) completes, and a
//! plan-stage failure must not leave an orphaned kanban row.
//!
//! These tests drive the real V2 plan stage against a fake `claude` binary
//! (`CLAUDE_BIN` env override, same mechanism `v2_smoke.rs` uses) so no
//! network call / real Claude Code CLI is required. Each test isolates
//! `TACHI_HOME` to a fresh temp dir and holds `global_test_lock()` because
//! `CLAUDE_BIN` / `DISPATCH_V2_ENABLED` / `TACHI_HOME` are process-global env
//! vars.

use super::super::make_server;
use super::{
    dispatch_params, wait_for_dispatch_result, DISPATCH_TEST_WAIT_ATTEMPTS,
    DISPATCH_TEST_WAIT_INTERVAL,
};
use crate::test_support::EnvRestore;
use serde_json::{json, Value};

/// Write a fake `claude` CLI at `path` that either succeeds with a
/// minimal-but-valid plan envelope, or exits non-zero to force
/// `ClaudePool::call` into its `Err` branch (mirrors `v2_smoke.rs`'s fixture,
/// with an added failure mode).
///
/// #971 review-fix (F4): `Success` no longer takes a fixed wall-clock sleep.
/// A fixed sleep is a CI flake trap — under load, polling can be delayed
/// past the sleep window, so the pre-plan assertions can race a plan stage
/// that already completed. Instead the fake binary spin-waits on a sentinel
/// file (`release_path`) that the test creates only AFTER its pre-plan
/// assertions have passed, making the pre-plan observation window
/// test-controlled rather than timing-dependent. `poll_timeout_secs` is a
/// generous backstop so a test bug (never creating the sentinel) fails fast
/// instead of hanging forever.
///
/// #971 review-fix (F4, second pass): `release_path` is derived from the
/// test's isolated `TACHI_HOME`, which itself derives from the process
/// `TMPDIR` — a directory this test does not control the naming of. The
/// generated `RELEASE=...` assignment must therefore be a single-quoted
/// shell literal (with embedded `'` escaped as `'\''`): double quotes stop
/// spaces but still expand `$()`/backticks under a hostile `TMPDIR`.
fn write_fake_claude_binary(path: &std::path::Path, mode: FakeClaudeMode) {
    use std::io::Write;
    let script = match mode {
        FakeClaudeMode::Success {
            release_path,
            poll_timeout_secs,
        } => format!(
            "#!/usr/bin/env bash\nset -e\nRELEASE='{release}'\nDEADLINE=$(( $(date +%s) + {timeout} ))\nwhile [ ! -f \"$RELEASE\" ]; do\n  if [ \"$(date +%s)\" -ge \"$DEADLINE\" ]; then\n    echo 'fake claude: timed out waiting for release sentinel' 1>&2\n    exit 1\n  fi\n  sleep 0.02\ndone\ncat <<'JSON'\n{{\"result\":\"## Goal\\nboard-first test.\\n\\n## Steps\\n1. inspect\\n\\n## Files\\n- src/lib.rs\\n\\n## Validation\\n- cargo test\\n\"}}\nJSON\n",
            release = release_path.display().to_string().replace('\'', "'\\''"),
            timeout = poll_timeout_secs,
        ),
        FakeClaudeMode::Fail => "#!/usr/bin/env bash\necho 'synthetic plan failure' 1>&2\nexit 1\n"
            .to_string(),
    };
    let mut f = std::fs::File::create(path).expect("create fake claude binary");
    f.write_all(script.as_bytes())
        .expect("write fake claude binary");
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }
}

enum FakeClaudeMode {
    Success {
        release_path: std::path::PathBuf,
        poll_timeout_secs: u64,
    },
    Fail,
}

/// Wait until exactly one subdirectory exists under `run_root` and return
/// its path. Mirrors `dispatch_ops::dispatch::tests::single_run_dir`, which
/// is `pub(super)`-scoped to that module and not reachable from here.
///
/// Excludes `.dispatch-dedupe` (see `dispatch_ops::dispatch::dedupe::
/// dispatch_dedupe_root`), which lives as a sibling directory directly under
/// `run_root` and is created by `reserve_global_dispatch_slot` once a
/// dispatch reaches its final "spawn background task + return" step. For a
/// fast V1 dispatch, `handle_tachi_dispatch` can already be back from that
/// `.await` (dedupe dir included) before this helper's first poll runs, so
/// counting *all* directories under `run_root` — including the dedupe
/// lock dir — makes the "exactly one dir" check permanently false for V1
/// and only accidentally true for the slower V2 fixtures here (which are
/// polled while still mid-plan-stage, before step 8 creates the dedupe
/// dir). Filter by name instead of relying on that timing.
async fn wait_for_single_run_dir(run_root: &std::path::Path) -> std::path::PathBuf {
    for _ in 0..DISPATCH_TEST_WAIT_ATTEMPTS {
        if let Ok(entries) = std::fs::read_dir(run_root) {
            let dirs: Vec<_> = entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .filter(|p| p.file_name().and_then(|n| n.to_str()) != Some(".dispatch-dedupe"))
                .collect();
            if dirs.len() == 1 {
                return dirs.into_iter().next().expect("one run dir");
            }
        }
        tokio::time::sleep(DISPATCH_TEST_WAIT_INTERVAL).await;
    }
    panic!("no run dir appeared under {}", run_root.display());
}

fn read_status_json(run_dir: &std::path::Path) -> Option<Value> {
    std::fs::read_to_string(run_dir.join("status.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
}

/// A run directory is created before its receipt is atomically published.
/// Wait for the receipt itself, while the fake plan stage remains blocked on
/// its sentinel, so this test observes the required ordering rather than a
/// directory-creation race.
async fn wait_for_status_json(run_dir: &std::path::Path) -> Value {
    for _ in 0..DISPATCH_TEST_WAIT_ATTEMPTS {
        if let Some(status) = read_status_json(run_dir) {
            return status;
        }
        tokio::time::sleep(DISPATCH_TEST_WAIT_INTERVAL).await;
    }
    panic!(
        "status.json did not appear under {} before the blocked plan stage timed out",
        run_dir.display()
    );
}

/// (5a) A V2 plan-stage FAILURE must leave BOTH a terminal status.json
/// (exit_code set, plan_review_status="failed") AND a kanban row in a
/// terminal state (TASK_STATE_FAILED) — no orphaned "planning"/WORKING row.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn plan_stage_failure_closes_both_status_and_kanban_row() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let fake_claude = temp_home.path().join("claude-fail");
    write_fake_claude_binary(&fake_claude, FakeClaudeMode::Fail);

    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _claude_bin = EnvRestore::set_path("CLAUDE_BIN", &fake_claude);
    let _skip_perms = EnvRestore::set("TACHI_CLAUDE_SKIP_PERMISSIONS", "true");
    let _v2_review = EnvRestore::set("DISPATCH_V2_PLAN_REVIEW", "false");

    let run_root = temp_home.path().join("runs");
    let server = make_server();

    let mut params = dispatch_params(Some("claude"), "plan stage failure should close kanban");
    params.stage = Some("auto".to_string());

    let err = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect_err("plan-stage failure must surface as an Err to the caller");
    assert!(
        err.contains("dispatch v2 stage1 (plan) failed"),
        "unexpected error: {err}"
    );

    let run_dir = wait_for_single_run_dir(&run_root).await;

    // status.json must be terminal (exit_code set, plan_review_status=failed).
    let status = read_status_json(&run_dir).expect("status.json written");
    assert_eq!(status["plan_review_status"], json!("failed"), "{status:#}");
    assert_eq!(status["exit_code"], json!(1), "{status:#}");

    // The kanban row (created BOARD-FIRST, before the plan stage ran) must
    // now be terminal, not left in TASK_STATE_WORKING.
    let dispatch_id = run_dir
        .file_name()
        .and_then(|n| n.to_str())
        .expect("dispatch id from run dir name")
        .to_string();
    let kanban_state = crate::dispatch_ops::get_kanban_state(&server, &dispatch_id).await;
    assert_eq!(
        kanban_state.as_deref(),
        Some("TASK_STATE_FAILED"),
        "kanban row must be closed after plan-stage failure, not left orphaned in TASK_STATE_WORKING"
    );
}

/// (5b) A successful V2 dispatch must have BOTH status.json AND the kanban
/// row present with pre-plan content (dispatch accepted, no plan yet)
/// BEFORE the (slow, faked) plan stage completes — proving BOARD-FIRST /
/// RECEIPT-FIRST ordering is observable, not just eventually-true.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn successful_dispatch_seeds_status_and_kanban_before_plan_completes() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let fake_claude = temp_home.path().join("claude-slow-success");
    // #971 review-fix (F4): the fake binary blocks on this sentinel file
    // instead of a fixed wall-clock sleep — the test releases it only after
    // the pre-plan assertions below have already passed, so the pre-plan
    // observation window is deterministic, not a race against a timer.
    let release_path = temp_home.path().join("release-plan-stage");
    write_fake_claude_binary(
        &fake_claude,
        FakeClaudeMode::Success {
            release_path: release_path.clone(),
            poll_timeout_secs: 60,
        },
    );

    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _claude_bin = EnvRestore::set_path("CLAUDE_BIN", &fake_claude);
    let _skip_perms = EnvRestore::set("TACHI_CLAUDE_SKIP_PERMISSIONS", "true");
    let _v2_review = EnvRestore::set("DISPATCH_V2_PLAN_REVIEW", "false");

    let run_root = temp_home.path().join("runs");
    let server = make_server();
    let server_for_task = (*server).clone();

    let mut params = dispatch_params(Some("custom"), "board-first ordering smoke");
    params.stage = Some("auto".to_string());
    // Stage-2 execute uses a no-op command; the plan stage (Stage 1) is the
    // slow part under test.
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];

    let dispatch_task = tokio::spawn(async move {
        crate::dispatch_ops::handle_tachi_dispatch(&server_for_task, params).await
    });

    // Run dir + status.json must appear almost immediately (receipt-first) —
    // the fake plan-stage binary is blocked on `release_path`, which this
    // test has not created yet, so it cannot have completed.
    let run_dir = wait_for_single_run_dir(&run_root).await;
    let dispatch_id = run_dir
        .file_name()
        .and_then(|n| n.to_str())
        .expect("dispatch id from run dir name")
        .to_string();

    let early_status = wait_for_status_json(&run_dir).await;
    assert_eq!(early_status["v2"], json!(true), "{early_status:#}");
    assert!(
        early_status["plan_generated_at"].is_null(),
        "plan has not generated yet: {early_status:#}"
    );
    assert!(
        early_status["exit_code"].is_null(),
        "dispatch must still be in-flight: {early_status:#}"
    );

    // The kanban row must also exist pre-plan-completion (BOARD-FIRST).
    let mut early_kanban_state = None;
    for _ in 0..DISPATCH_TEST_WAIT_ATTEMPTS {
        early_kanban_state = crate::dispatch_ops::get_kanban_state(&server, &dispatch_id).await;
        if early_kanban_state.is_some() {
            break;
        }
        tokio::time::sleep(DISPATCH_TEST_WAIT_INTERVAL).await;
    }
    assert_eq!(
        early_kanban_state.as_deref(),
        Some("TASK_STATE_WORKING"),
        "kanban row must exist (BOARD-FIRST) before the V2 plan stage completes"
    );

    // dispatch_received must be the (or one of the) earliest trajectory
    // events, written before plan_generated.
    let trajectory = std::fs::read_to_string(run_dir.join("trajectory.jsonl")).unwrap_or_default();
    assert!(
        trajectory.contains("\"event\":\"dispatch_received\""),
        "receipt-first trajectory event missing: {trajectory}"
    );

    // #971 review-fix (F4): all pre-plan assertions above have now passed —
    // release the fake plan-stage binary so it can complete. This is the
    // deterministic barrier replacing the old fixed 2s sleep: the plan
    // stage cannot resolve before this point, by construction, not by luck.
    std::fs::write(&release_path, b"go").expect("write release sentinel");

    // Now let the dispatch actually finish and sanity-check the final state.
    let raw = dispatch_task
        .await
        .expect("dispatch task should not panic")
        .expect("v2 dispatch should eventually succeed");
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(response["v2"], json!(true), "{response:#}");
}

/// (5b2) #971 review-fix (F2, second pass): a plan-review early response
/// must project `TASK_STATE_INPUT_REQUIRED` — not `TASK_STATE_PENDING_REVIEW`
/// — in THREE places, and all three must agree:
///   1. the synchronous dispatch response's `task.status.state`,
///   2. the kanban row (`get_kanban_state`), and
///   3. the run's `status.json` (`status_state()`'s existing
///      `plan_review_status == "pending_review"` -> `TASK_STATE_INPUT_REQUIRED`
///      mapping).
/// `TASK_STATE_INPUT_REQUIRED` (unlike the old `PENDING_REVIEW`) is also in
/// `kanban::KANBAN_DISPATCH_NON_TERMINAL_STATES`, so an abandoned row is
/// reapable by `gc_expired_kanban_cards` instead of pinned forever — this
/// test only asserts the vocabulary is consistent; GC aging itself is
/// covered at the `kanban::gc` unit level, not re-driven end-to-end here.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn plan_review_pending_response_projects_input_required_kanban_state() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let fake_claude = temp_home.path().join("claude-pending-review");
    let release_path = temp_home.path().join("release-plan-stage");
    write_fake_claude_binary(
        &fake_claude,
        FakeClaudeMode::Success {
            release_path: release_path.clone(),
            poll_timeout_secs: 60,
        },
    );
    // Nothing blocks on the sentinel pre-plan in this test — release it
    // immediately so the plan stage can complete and hand back the
    // pending-review early response.
    std::fs::write(&release_path, b"go").expect("write release sentinel");

    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _claude_bin = EnvRestore::set_path("CLAUDE_BIN", &fake_claude);
    let _skip_perms = EnvRestore::set("TACHI_CLAUDE_SKIP_PERMISSIONS", "true");
    let _v2_review = EnvRestore::set("DISPATCH_V2_PLAN_REVIEW", "true");

    let run_root = temp_home.path().join("runs");
    let server = make_server();

    let mut params = dispatch_params(Some("claude"), "plan review pending state projection");
    params.stage = Some("auto".to_string());

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("plan-review early response is Ok, not Err");
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");

    // (1) synchronous response
    assert_eq!(
        response["task"]["status"]["state"],
        json!("TASK_STATE_INPUT_REQUIRED"),
        "early response must not report the retired TASK_STATE_PENDING_REVIEW vocabulary: {response:#}"
    );
    assert_eq!(
        response["plan_review_status"],
        json!("pending_review"),
        "{response:#}"
    );

    let run_dir = wait_for_single_run_dir(&run_root).await;
    let dispatch_id = run_dir
        .file_name()
        .and_then(|n| n.to_str())
        .expect("dispatch id from run dir name")
        .to_string();

    // (2) kanban row
    let kanban_state = crate::dispatch_ops::get_kanban_state(&server, &dispatch_id).await;
    assert_eq!(
        kanban_state.as_deref(),
        Some("TASK_STATE_INPUT_REQUIRED"),
        "kanban row must project TASK_STATE_INPUT_REQUIRED for a pending plan review, matching status.json"
    );

    // (3) status.json carries the same `plan_review_status: "pending_review"`
    // fact that `board::status::status_state()` maps to
    // `TASK_STATE_INPUT_REQUIRED` for status.json/board readers (see that
    // function's `plan_review_status == "pending_review"` branch) — so this
    // is the same vocabulary as the kanban row asserted above, not a
    // separately-drifting one.
    let status = read_status_json(&run_dir).expect("status.json written");
    assert_eq!(
        status["plan_review_status"],
        json!("pending_review"),
        "{status:#}"
    );
}

/// (5c) V1 (non-V2) dispatch is unaffected by the RECEIPT-FIRST /
/// BOARD-FIRST reorder: status.json + kanban row both land with V1's
/// `plan_review_status: "n/a"` and the dispatch still succeeds end to end.
/// Broad V1 coverage already exists (e.g.
/// `workflow_artifacts::closure_dispatch_markers::dispatch_board`); this
/// test is narrowly scoped to the ordering claim itself.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn v1_dispatch_status_and_kanban_unaffected_by_reorder() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let run_root = temp_home.path().join("runs");
    let server = make_server();

    let mut params = dispatch_params(Some("custom"), "v1 dispatch unaffected by reorder");
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print('ok')".to_string(),
    ];
    // No `stage`, no DISPATCH_V2_ENABLED — V1 path.

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("v1 dispatch should start");
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(response["v2"], json!(false), "{response:#}");
    let dispatch_id = response["dispatch_id"]
        .as_str()
        .expect("dispatch_id")
        .to_string();

    let run_dir = wait_for_single_run_dir(&run_root).await;
    let status = read_status_json(&run_dir).expect("status.json written");
    assert_eq!(status["plan_review_status"], json!("n/a"), "{status:#}");

    let kanban_state = crate::dispatch_ops::get_kanban_state(&server, &dispatch_id).await;
    assert!(
        kanban_state.is_some(),
        "kanban row must exist for a V1 dispatch too"
    );

    let _ = wait_for_dispatch_result(&run_dir).await;
}
