//! kckylechen1/tachi#971 — dispatches must not be invisible to board/status
//! pollers until the V2 plan stage (up to 180s LLM call) completes, and a
//! plan-stage failure must not leave an orphaned kanban row.
//!
//! #1274 moved V2 planning to provider-only HTTP. These tests install a local
//! OpenAI-compatible reasoning provider so success and failure are independent
//! of ambient credentials, provider health, and network access. The ordering
//! test's provider handler waits on a test-controlled sentinel before replying,
//! preserving the pre-plan observation barrier.
//!
//! Each test isolates `TACHI_HOME` to a fresh temp dir and holds
//! `global_test_lock()` because `DISPATCH_V2_ENABLED` / `TACHI_HOME` are
//! process-global env vars.

use super::super::make_server;
use super::{
    dispatch_params, wait_for_dispatch_result, DISPATCH_TEST_WAIT_ATTEMPTS,
    DISPATCH_TEST_WAIT_INTERVAL,
};
use crate::test_support::EnvRestore;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use tachi_llm::{
    llm::{ChatLaneConfig, ProviderRuntimeConfig},
    LlmClient, ProviderSecret, RerankConfig, RerankProviderKind,
};

const MOCK_PLAN: &str = "## Goal\nboard-first test.\n\n## Steps\n1. inspect\n\n## Files\n- src/lib.rs\n\n## Validation\n- cargo test\n";

#[derive(Clone)]
enum MockProviderMode {
    Success,
    SuccessAfter(std::path::PathBuf),
    Failure,
}

struct MockProvider {
    llm: LlmClient,
    server_task: tokio::task::JoinHandle<()>,
}

impl Drop for MockProvider {
    fn drop(&mut self) {
        self.server_task.abort();
    }
}

impl MockProvider {
    async fn start(mode: MockProviderMode) -> Self {
        let app = Router::new()
            .route("/chat/completions", post(mock_chat_completions))
            .with_state(mode);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock reasoning provider");
        let port = listener
            .local_addr()
            .expect("mock reasoning provider address")
            .port();
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mock reasoning provider");
        });

        let unused_lane = || ChatLaneConfig {
            base_url: "https://unused.test/v1/chat/completions".to_string(),
            model: "unused".to_string(),
            api_key_envs: vec!["UNUSED_API_KEY"],
        };
        let config = ProviderRuntimeConfig {
            extract: unused_lane(),
            summary: unused_lane(),
            reasoning: ChatLaneConfig {
                base_url: format!("http://127.0.0.1:{port}/chat/completions"),
                model: "mock-board-first-reasoning".to_string(),
                api_key_envs: vec!["BOARD_FIRST_REASONING_API_KEY"],
            },
            distill: unused_lane(),
            rerank: RerankConfig {
                provider: RerankProviderKind::Voyage,
                local_endpoint: None,
            },
        };
        let llm = LlmClient::new_with_config(config, None).expect("initialize mock LLM client");
        llm.set_provider_secret_pool(
            "BOARD_FIRST_REASONING_API_KEY",
            vec![ProviderSecret {
                key_id: "board-first-test-key".to_string(),
                value: "test-key".to_string(),
            }],
        );

        Self { llm, server_task }
    }
}

async fn mock_chat_completions(State(mode): State<MockProviderMode>) -> Response {
    match mode {
        MockProviderMode::Failure => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"message": "synthetic plan failure"}})),
        )
            .into_response(),
        MockProviderMode::Success => mock_plan_response(),
        MockProviderMode::SuccessAfter(release_path) => {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
            while !release_path.is_file() {
                if tokio::time::Instant::now() >= deadline {
                    return (
                        StatusCode::GATEWAY_TIMEOUT,
                        Json(
                            json!({"error": {"message": "timed out waiting for release sentinel"}}),
                        ),
                    )
                        .into_response();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            mock_plan_response()
        }
    }
}

fn mock_plan_response() -> Response {
    Json(json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": MOCK_PLAN
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "total_tokens": 2
        },
        "model": "mock-board-first-reasoning"
    }))
    .into_response()
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
/// Wait for the receipt itself, while the mock provider remains blocked on its
/// sentinel, so this test observes the required ordering rather than a
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
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _v2_review = EnvRestore::set("DISPATCH_V2_PLAN_REVIEW", "false");
    let mock_provider = MockProvider::start(MockProviderMode::Failure).await;

    let run_root = temp_home.path().join("runs");
    let mut server = make_server();
    server.replace_llm(mock_provider.llm.clone());

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
/// BEFORE the blocked mock-provider plan stage completes — proving BOARD-FIRST /
/// RECEIPT-FIRST ordering is observable, not just eventually-true.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn successful_dispatch_seeds_status_and_kanban_before_plan_completes() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let temp_home = tempfile::tempdir().expect("temp tachi home");
    // #971 review-fix (F4): the HTTP handler blocks on this sentinel file.
    // The test releases it only after the pre-plan assertions below have
    // passed, so the observation window is deterministic.
    let release_path = temp_home.path().join("release-plan-stage");

    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _v2_review = EnvRestore::set("DISPATCH_V2_PLAN_REVIEW", "false");
    let mock_provider =
        MockProvider::start(MockProviderMode::SuccessAfter(release_path.clone())).await;

    let run_root = temp_home.path().join("runs");
    let mut server = make_server();
    server.replace_llm(mock_provider.llm.clone());
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
    // the mock provider is blocked on `release_path`, which this test has not
    // created yet, so the plan stage cannot have completed.
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
    // release the mock HTTP handler so it can return the plan. The plan stage
    // cannot resolve before this point, by construction, not by luck.
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
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _v2_review = EnvRestore::set("DISPATCH_V2_PLAN_REVIEW", "true");
    let mock_provider = MockProvider::start(MockProviderMode::Success).await;

    let run_root = temp_home.path().join("runs");
    let mut server = make_server();
    server.replace_llm(mock_provider.llm.clone());

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
