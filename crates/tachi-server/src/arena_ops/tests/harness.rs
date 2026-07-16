use super::*;
use crate::test_support::EnvRestore;

const TERMINAL_DISPATCH_STATUS_ATTEMPTS: usize = 200;
const DISPATCH_STATUS_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);
const TEST_WATCHDOG_POLL_MILLIS: &str = "250";

// Same worst-case ceiling as before (40 * 100ms = 200 * 20ms = 4s); a finer
// poll interval lets the common fast-resolving case return sooner without
// weakening the timeout safety margin (issue #682 busy-wait sweep).
async fn wait_for_nonempty_file(path: &Path) -> String {
    for _ in 0..200 {
        if let Ok(raw) = std::fs::read_to_string(path) {
            if !raw.trim().is_empty() {
                return raw;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    std::fs::read_to_string(path).unwrap_or_default()
}

async fn wait_for_terminal_dispatch_status(run_dir: &Path) -> Value {
    let status_path = run_dir.join("status.json");
    for _ in 0..TERMINAL_DISPATCH_STATUS_ATTEMPTS {
        if let Ok(raw) = std::fs::read_to_string(&status_path) {
            if let Ok(status) = serde_json::from_str::<Value>(&raw) {
                if matches!(
                    status["state"].as_str(),
                    Some("TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED")
                ) {
                    return status;
                }
            }
        }
        tokio::time::sleep(DISPATCH_STATUS_POLL_INTERVAL).await;
    }
    panic!(
        "dispatch did not reach a terminal state: {}",
        status_path.display()
    );
}

#[tokio::test]
async fn arena_spawn_normalizes_golden_harness_lanes() {
    let _root = temp_arena_root();
    let server = server();
    let mut open = params("open");
    open.objective = Some("golden lanes".into());
    let opened: Value =
        serde_json::from_str(&handle_tachi_arena(&server, open).await.unwrap()).unwrap();
    let arena_id = opened["arena_id"].as_str().unwrap().to_string();

    let mut spawn = params("spawn");
    spawn.arena_id = Some(arena_id.clone());
    spawn.prompt = Some("brainstorm the design".into());
    spawn.harness = Some("gemini".into());
    let spawned: Value =
        serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
    assert_eq!(spawned["harness"], "gemini-advisor");
    assert_eq!(spawned["harness_lane"]["kind"], "advisor");
    assert_eq!(spawned["harness_lane"]["launch_mode"], "advisor_artifact");
    assert!(spawned["tracked_prompt"]
        .as_str()
        .unwrap()
        .contains("Advisor output is captured as an artifact"));
    let prompt = std::fs::read_to_string(spawned["prompt_path"].as_str().unwrap()).unwrap();
    assert!(prompt.contains("Gemini advisor"));
    assert!(prompt.contains("Advisor output is captured as an artifact"));

    let mut spawn = params("spawn");
    spawn.arena_id = Some(arena_id);
    spawn.prompt = Some("unknown harness stays document-only".into());
    spawn.harness = Some("experimental-harness".into());
    let spawned: Value =
        serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
    assert_eq!(spawned["harness"], "manual");
    assert_eq!(spawned["requested_harness"], "experimental-harness");
    assert!(spawned["harness_lane"]["command_hint"]
        .as_str()
        .unwrap()
        .contains("Unsupported harness hint"));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn arena_spawn_launches_opencode_dispatch_and_collects_result() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _root = temp_arena_root();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let fake_bin = tempfile::tempdir().expect("fake bin");
    let opencode_path = fake_bin.path().join("opencode");
    std::fs::write(
            &opencode_path,
            "#!/bin/sh\nprintf '%s\\n' 'Summary: fake opencode completed' 'Files changed: none' 'Commands run: fake opencode' 'Verification performed: fake smoke' 'Remaining risks or blockers: none'\nexit 7\n",
        )
        .expect("write fake opencode");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&opencode_path)
            .expect("fake opencode metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&opencode_path, perms).expect("chmod fake opencode");
    }
    let _home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _path = {
        let mut paths = vec![fake_bin.path().to_path_buf()];
        if let Some(existing) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&existing));
        }
        EnvRestore::set_os("PATH", &std::env::join_paths(paths).expect("join PATH"))
    };
    let _watchdog_poll =
        EnvRestore::set("TACHI_DISPATCH_WATCHDOG_POLL_MS", TEST_WATCHDOG_POLL_MILLIS);

    let server = server();
    let mut open = params("open");
    open.objective = Some("launch worker".into());
    let opened: Value =
        serde_json::from_str(&handle_tachi_arena(&server, open).await.unwrap()).unwrap();
    let arena_id = opened["arena_id"].as_str().unwrap().to_string();

    let mut spawn = params("spawn");
    spawn.arena_id = Some(arena_id.clone());
    spawn.prompt = Some("run a fake worker".into());
    spawn.harness = Some("opencode".into());
    spawn.role = Some("explore".into());
    spawn.launch = true;
    spawn.timeout_secs = Some(5);
    let spawned: Value =
        serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
    let mission_id = spawned["mission_id"].as_str().unwrap().to_string();
    let dispatch_id = spawned["status"]["dispatch_id"]
        .as_str()
        .expect("linked dispatch id")
        .to_string();
    let run_dir = PathBuf::from(
        spawned["launch"]["run_dir"]
            .as_str()
            .expect("linked run dir"),
    );
    let spawned_status = spawned["status"]
        .as_object()
        .expect("spawned status object");
    assert_eq!(spawned["status"]["dispatch_agent"], json!("opencode"));
    assert!(
        spawned_status.get("task").is_none(),
        "compact status should expose task_preview, not the full prompt: {spawned_status:#?}"
    );
    assert!(
        spawned_status.get("prompt_path").is_none()
            && spawned_status.get("result_path").is_none()
            && spawned_status.get("status_path").is_none()
            && spawned_status.get("run_dir").is_none(),
        "compact status should not duplicate artifact or run paths: {spawned_status:#?}"
    );
    assert_eq!(
        spawned["status"]["task_preview"],
        json!("run a fake worker")
    );
    assert!(
        !spawned_status.contains_key("dispatch_response"),
        "arena status must not persist full dispatch response: {spawned_status:#?}"
    );
    assert_eq!(
        spawned["status"]["dispatch_link"]["agent"],
        json!("opencode")
    );
    assert_eq!(spawned["status"]["dispatch_link"]["redacted"], json!(true));
    assert_eq!(
        spawned["status"]["dispatch_link"]["source"],
        json!("dispatch_response_summary")
    );
    assert!(spawned["status"]["dispatch_link"].get("run_dir").is_none());
    let dispatch_result = wait_for_nonempty_file(&run_dir.join("result.md")).await;
    assert!(dispatch_result.contains("fake opencode completed"));
    let terminal_status = wait_for_terminal_dispatch_status(&run_dir).await;
    assert_eq!(
        terminal_status["state"],
        json!("TASK_STATE_FAILED"),
        "fake opencode exits 7, so teardown must report failure: {terminal_status:#}"
    );

    let mut board = params("board");
    board.arena_id = Some(arena_id.clone());
    let board: Value =
        serde_json::from_str(&handle_tachi_arena(&server, board).await.unwrap()).unwrap();
    let mission = &board["result"]["missions"][0];
    assert_eq!(mission["dispatch_id"], json!(dispatch_id));
    assert_eq!(mission["dispatch_agent"], json!("opencode"));
    assert!(mission.get("task").is_none(), "{mission:#}");
    assert!(mission.get("prompt_path").is_none(), "{mission:#}");
    assert!(mission.get("result_path").is_none(), "{mission:#}");
    assert!(mission.get("run_dir").is_none(), "{mission:#}");
    assert_eq!(mission["linked_dispatch"]["agent"], json!("opencode"));
    assert_eq!(mission["linked_dispatch"]["redacted"], json!(true));
    assert_eq!(
        mission["linked_dispatch"]["source"],
        json!("dispatch_run_summary")
    );
    assert!(
        mission["linked_dispatch"].get("status").is_none(),
        "linked dispatch must be a redacted summary: {mission:#}"
    );
    assert!(mission["linked_dispatch"].get("run_dir").is_none());
    assert_eq!(
        mission["collection_state"],
        json!("pending_collect_from_dispatch")
    );

    let mut collect = params("collect");
    collect.arena_id = Some(arena_id);
    collect.mission_id = Some(mission_id);
    let collected: Value =
        serde_json::from_str(&handle_tachi_arena(&server, collect).await.unwrap()).unwrap();
    assert_eq!(collected["missions"][0]["state"], json!("collected"));
    assert_eq!(
        collected["missions"][0]["result_source"],
        json!("linked_dispatch_result")
    );
    assert_eq!(
        collected["missions"][0]["status"]["result_source"],
        json!("linked_dispatch_result")
    );
    assert_eq!(
        collected["missions"][0]["status"]["dispatch_agent"],
        json!("opencode")
    );
    assert!(collected["missions"][0]["status"].get("task").is_none());
    assert!(collected["missions"][0]["status"]
        .get("prompt_path")
        .is_none());
    assert!(collected["missions"][0]["status"]
        .get("result_path")
        .is_none());
    assert!(collected["missions"][0]["status"].get("run_dir").is_none());
    assert!(collected["missions"][0]["result"]
        .as_str()
        .unwrap()
        .contains("fake opencode completed"));
    assert_eq!(
        collected["missions"][0]["completion_draft"]["arguments"]["action"],
        json!("complete")
    );
    assert_eq!(
        collected["missions"][0]["completion_draft"]["arguments"]["agent"],
        json!("opencode")
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn arena_opencode_executor_fallback_uses_glm_registry_model() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _root = temp_arena_root();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let fake_bin = tempfile::tempdir().expect("fake bin");
    let opencode_path = fake_bin.path().join("opencode");
    let args_path = fake_bin.path().join("args.txt");
    std::fs::write(
        &opencode_path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nprintf '%s\\n' 'Summary: fake opencode completed'\nexit 0\n",
            args_path.display()
        ),
    )
    .expect("write fake opencode");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&opencode_path)
            .expect("fake opencode metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&opencode_path, perms).expect("chmod fake opencode");
    }
    let _home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _path = {
        let mut paths = vec![fake_bin.path().to_path_buf()];
        if let Some(existing) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&existing));
        }
        EnvRestore::set_os("PATH", &std::env::join_paths(paths).expect("join PATH"))
    };
    let _watchdog_poll =
        EnvRestore::set("TACHI_DISPATCH_WATCHDOG_POLL_MS", TEST_WATCHDOG_POLL_MILLIS);
    let _model = EnvRestore::set("TACHI_DISPATCH_GLM_CODING_MODEL", "zhipuai/glm-5.2-arena");

    let server = server();
    let mut open = params("open");
    open.objective = Some("launch registry worker".into());
    let opened: Value =
        serde_json::from_str(&handle_tachi_arena(&server, open).await.unwrap()).unwrap();
    let arena_id = opened["arena_id"].as_str().unwrap().to_string();

    let mut spawn = params("spawn");
    spawn.arena_id = Some(arena_id);
    spawn.prompt = Some("run a fake worker".into());
    spawn.harness = Some("opencode".into());
    spawn.role = Some("execute".into());
    spawn.launch = true;
    spawn.timeout_secs = Some(5);
    let spawned: Value =
        serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
    let run_dir = PathBuf::from(
        spawned["launch"]["run_dir"]
            .as_str()
            .expect("linked run dir"),
    );
    let dispatch_result = wait_for_nonempty_file(&run_dir.join("result.md")).await;
    assert!(dispatch_result.contains("fake opencode completed"));
    let terminal_status = wait_for_terminal_dispatch_status(&run_dir).await;
    assert_eq!(
        terminal_status["state"],
        json!("TASK_STATE_COMPLETED"),
        "successful fake opencode must finish teardown before the fixture drops: {terminal_status:#}"
    );

    let args = std::fs::read_to_string(args_path).expect("captured opencode args");
    assert!(args.contains("--model\nzhipuai/glm-5.2-arena"), "{args}");
}
