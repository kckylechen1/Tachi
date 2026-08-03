use super::*;

#[tokio::test]
async fn arena_spawn_collect_reads_tracked_documents() {
    let _root = temp_arena_root();
    let server = server();
    // [1319-D1] open/close were removed; spawn auto-provisions the arena dir.
    let mut spawn = params("spawn");
    spawn.arena_id = Some("arena_20260606T000000Z_test_collect".to_string());
    spawn.title = Some("Arena Test".into());
    spawn.objective = Some("coordinate tracked workers".into());
    spawn.prompt = Some("inspect the code".into());
    spawn.harness = Some("codex".into());
    spawn.role = Some("explore".into());
    spawn.skills = vec!["skill:waza-check".into()];
    let raw = handle_tachi_arena(&server, spawn).await.unwrap();
    let spawned: Value = serde_json::from_str(&raw).unwrap();
    let arena_id = spawned["arena_id"].as_str().unwrap().to_string();
    let mission_id = spawned["mission_id"].as_str().unwrap().to_string();
    let mission_dir = PathBuf::from(spawned["mission_dir"].as_str().unwrap());
    assert!(mission_dir.join("prompt.md").exists());
    assert!(spawned["tracked_prompt"]
        .as_str()
        .unwrap()
        .contains("write a completion report"));
    std::fs::write(
        mission_dir.join("plan.md"),
        "Plan: inspect files before reporting.\n",
    )
    .unwrap();
    std::fs::write(
            mission_dir.join("result.md"),
            "Summary: done\nFiles changed: none\nCommands run: none\nVerification performed: read-only\nRemaining risks or blockers: none\n",
        )
        .unwrap();

    let mut collect = params("collect");
    collect.arena_id = Some(arena_id.clone());
    collect.mission_id = Some(mission_id.clone());
    let raw = handle_tachi_arena(&server, collect).await.unwrap();
    let collected: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(collected["missions"][0]["state"], "collected");
    assert_eq!(collected["missions"][0]["result_written"], true);
    assert_eq!(collected["missions"][0]["result_source"], "mission_result");
    assert_eq!(
        collected["missions"][0]["status"]["result_source"],
        "mission_result"
    );
    // [1319-D1] collect no longer copies a linked dispatch result; the canonical
    // result.md stays the single source of truth. canonical_result_ref is null
    // when a real mission result is present.
    assert_eq!(
        collected["missions"][0]["canonical_result_ref"],
        Value::Null
    );
}

#[tokio::test]
async fn arena_board_reads_external_plan_and_result_writes() {
    let _root = temp_arena_root();
    let server = server();
    let mut spawn = params("spawn");
    let arena_id = "arena_20260606T000000Z_test_board".to_string();
    spawn.arena_id = Some(arena_id.clone());
    spawn.objective = Some("refresh board".into());
    spawn.prompt = Some("write files externally".into());
    let spawned: Value =
        serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
    let mission_dir = PathBuf::from(spawned["mission_dir"].as_str().unwrap());
    std::fs::write(mission_dir.join("plan.md"), "Plan: external write\n").unwrap();
    std::fs::write(mission_dir.join("result.md"), "Summary: external result\n").unwrap();

    let mut board = params("board");
    board.arena_id = Some(arena_id.clone());
    let board: Value =
        serde_json::from_str(&handle_tachi_arena(&server, board).await.unwrap()).unwrap();
    let mission = &board["result"]["missions"][0];
    assert_eq!(mission["plan_written"], true);
    assert_eq!(mission["result_written"], true);

    let mut board_all = params("board");
    board_all.arena_id = None;
    let board: Value =
        serde_json::from_str(&handle_tachi_arena(&server, board_all).await.unwrap()).unwrap();
    let arena = &board["arenas"][0];
    assert_eq!(arena["mission_count"], json!(1));
    assert_eq!(arena["active_missions"], json!(1));
    assert_eq!(arena["pending_collect"], json!(1));
    // [1319-D1] board.json is no longer written by refresh_board; the board path
    // is still surfaced as a legacy pointer but the file need not exist.
    assert!(arena["board_path"]
        .as_str()
        .unwrap()
        .ends_with("board.json"));
}

#[tokio::test]
async fn arena_collect_marks_corrupt_result_as_read_error_instead_of_pending() {
    let _root = temp_arena_root();
    let server = server();
    let arena_id = "arena_20260606T000000Z_test_corrupt".to_string();
    let mut spawn = params("spawn");
    spawn.arena_id = Some(arena_id.clone());
    spawn.prompt = Some("write corrupt result".into());
    let spawned: Value =
        serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
    let mission_id = spawned["mission_id"].as_str().unwrap().to_string();
    let mission_dir = PathBuf::from(spawned["mission_dir"].as_str().unwrap());

    std::fs::write(mission_dir.join("plan.md"), "Plan: write broken bytes\n").unwrap();
    std::fs::write(mission_dir.join("result.md"), vec![0xff, 0xfe, b'x']).unwrap();

    let mut collect = params("collect");
    collect.arena_id = Some(arena_id.clone());
    collect.mission_id = Some(mission_id.clone());
    let collected: Value =
        serde_json::from_str(&handle_tachi_arena(&server, collect).await.unwrap()).unwrap();
    let mission = &collected["missions"][0];
    assert_eq!(
        mission["state"],
        json!("artifact_read_error"),
        "{mission:#}"
    );
    assert_eq!(mission["status"]["state"], json!("artifact_read_error"));
    assert_eq!(mission["result_source"], json!("result_read_error"));
    assert_eq!(
        mission["status"]["result_source"],
        json!("result_read_error")
    );
    assert_eq!(mission["result_written"], json!(true), "{mission:#}");
    assert_ne!(mission["state"], json!("pending_result"));
    assert!(mission["completion_draft"].is_null(), "{mission:#}");
    let read_error = mission["artifact_read_error"]
        .as_str()
        .expect("read error should be surfaced");
    assert!(read_error.contains("arena mission result"), "{read_error}");
    assert!(
        read_error.contains("refusing descriptor-bound text read"),
        "{read_error}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn arena_collect_refuses_mission_result_leaf_symlink() {
    let _root = temp_arena_root();
    let server = server();
    let arena_id = "arena_20260606T000000Z_test_leaf_link".to_string();
    let mut spawn = params("spawn");
    spawn.arena_id = Some(arena_id.clone());
    spawn.prompt = Some("write a result".into());
    let spawned: Value =
        serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
    let mission_id = spawned["mission_id"].as_str().unwrap().to_string();
    let mission_dir = PathBuf::from(spawned["mission_dir"].as_str().unwrap());
    let outside = tempfile::tempdir().expect("outside result");
    let outside_result = outside.path().join("result.md");
    std::fs::write(&outside_result, "outside result").unwrap();
    std::os::unix::fs::symlink(&outside_result, mission_dir.join("result.md")).unwrap();

    let mut collect = params("collect");
    collect.arena_id = Some(arena_id);
    collect.mission_id = Some(mission_id);
    let error = handle_tachi_arena(&server, collect)
        .await
        .expect_err("mission result leaf symlink must be a loud refusal");
    assert!(error.contains("refusing descriptor-bound read"), "{error}");
}

#[cfg(unix)]
#[tokio::test]
async fn arena_board_refuses_mission_parent_symlink() {
    let _root = temp_arena_root();
    let server = server();
    let arena_id = "arena_20260606T000000Z_test_parent_link".to_string();
    let mut spawn = params("spawn");
    spawn.arena_id = Some(arena_id.clone());
    spawn.prompt = Some("write a result".into());
    let spawned: Value =
        serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
    let mission_dir = PathBuf::from(spawned["mission_dir"].as_str().unwrap());
    let outside = tempfile::tempdir().expect("outside mission");
    let status = std::fs::read(mission_dir.join("status.json")).unwrap();
    std::fs::remove_dir_all(&mission_dir).unwrap();
    std::fs::write(outside.path().join("status.json"), status).unwrap();
    std::fs::write(outside.path().join("plan.md"), "outside plan").unwrap();
    std::fs::write(outside.path().join("result.md"), "outside result").unwrap();
    std::os::unix::fs::symlink(outside.path(), &mission_dir).unwrap();

    let mut board = params("board");
    board.arena_id = Some(arena_id);
    let error = handle_tachi_arena(&server, board)
        .await
        .expect_err("mission parent symlink must be a loud refusal");
    assert!(error.contains("outside containment root"), "{error}");
}

#[cfg(unix)]
#[tokio::test]
async fn arena_mission_result_read_keeps_opened_file_across_replacement_race() {
    let _root = temp_arena_root();
    let server = server();
    let arena_id = "arena_20260606T000000Z_test_race".to_string();
    let mut spawn = params("spawn");
    spawn.arena_id = Some(arena_id.clone());
    spawn.prompt = Some("write a result".into());
    let spawned: Value =
        serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
    let mission_id = spawned["mission_id"].as_str().unwrap().to_string();
    let mission_dir = PathBuf::from(spawned["mission_dir"].as_str().unwrap());
    let result_path = mission_dir.join("result.md");
    let outside = tempfile::tempdir().expect("outside result");
    let outside_result = outside.path().join("result.md");
    std::fs::write(&result_path, "inside result").unwrap();
    std::fs::write(&outside_result, "outside result").unwrap();
    crate::dispatch_ops::install_secure_read_hook(
        crate::dispatch_ops::SecureReadHookStage::AfterOpen,
        result_path,
        move |opened| {
            std::fs::remove_file(opened).unwrap();
            std::os::unix::fs::symlink(&outside_result, opened).unwrap();
        },
    );

    let read = crate::arena_ops::state::read_mission_result(&arena_id, &mission_id);
    let crate::arena_ops::state::ArenaArtifactRead::Present(result) = read else {
        panic!("mission result should be read from the original descriptor");
    };
    assert_eq!(result, "inside result");
    assert_ne!(result, "outside result");
}

#[cfg(unix)]
#[tokio::test]
async fn arena_collect_enforces_mission_result_named_byte_limit() {
    let _root = temp_arena_root();
    let server = server();
    let arena_id = "arena_20260606T000000Z_test_limit".to_string();
    let mut spawn = params("spawn");
    spawn.arena_id = Some(arena_id.clone());
    spawn.prompt = Some("write a bounded result".into());
    let spawned: Value =
        serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
    let mission_id = spawned["mission_id"].as_str().unwrap().to_string();
    let result_path = PathBuf::from(spawned["result_path"].as_str().unwrap());
    let limit = crate::arena_ops::state::ARENA_MISSION_RESULT_MAX_BYTES;

    std::fs::write(&result_path, vec![b'x'; limit]).unwrap();
    let mut exact_collect = params("collect");
    exact_collect.arena_id = Some(arena_id.clone());
    exact_collect.mission_id = Some(mission_id.clone());
    let exact: Value = serde_json::from_str(
        &handle_tachi_arena(&server, exact_collect)
            .await
            .expect("exact mission result limit must collect"),
    )
    .unwrap();
    assert_eq!(exact["missions"][0]["state"], json!("collected"));
    assert_eq!(
        exact["missions"][0]["result"].as_str().map(str::len),
        Some(limit)
    );

    std::fs::write(&result_path, vec![b'x'; limit + 1]).unwrap();
    let mut over_collect = params("collect");
    over_collect.arena_id = Some(arena_id);
    over_collect.mission_id = Some(mission_id);
    let over: Value = serde_json::from_str(
        &handle_tachi_arena(&server, over_collect)
            .await
            .expect("oversize mission result is reported in mission state"),
    )
    .unwrap();
    assert_eq!(over["missions"][0]["state"], json!("artifact_read_error"));
    assert!(over["missions"][0]["artifact_read_error"]
        .as_str()
        .is_some_and(|error| error.contains("named limit")));
}

#[cfg(unix)]
#[test]
fn arena_collect_refuses_linked_run_dir_symlink_escape() {
    struct TachiHomeRestore(Option<std::ffi::OsString>);
    impl Drop for TachiHomeRestore {
        fn drop(&mut self) {
            // SAFETY: this test holds global_test_lock for its lifetime.
            unsafe {
                match self.0.as_ref() {
                    Some(value) => std::env::set_var("TACHI_HOME", value),
                    None => std::env::remove_var("TACHI_HOME"),
                }
            }
        }
    }

    let _run_lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let original_home = TachiHomeRestore(std::env::var_os("TACHI_HOME"));
    let tachi_home = tempfile::tempdir().expect("temporary tachi home");
    // SAFETY: serialized by global_test_lock and restored on drop.
    unsafe {
        std::env::set_var("TACHI_HOME", tachi_home.path());
    }
    let _arena_root = temp_arena_root();
    let server = server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread test runtime");

    runtime.block_on(async {
        let arena_id = "arena_20260606T000000Z_test_linked_escape".to_string();
        let mut spawn = params("spawn");
        spawn.arena_id = Some(arena_id.clone());
        spawn.prompt = Some("collect linked result".into());
        let spawned: Value =
            serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
        let mission_id = spawned["mission_id"].as_str().unwrap().to_string();
        let mission_dir = PathBuf::from(spawned["mission_dir"].as_str().unwrap());

        let dispatch_id = "20260725T200000Z-linked-refusal";
        let run_dir = tachi_home.path().join("runs").join(dispatch_id);
        let outside = tempfile::tempdir().expect("outside target");
        std::fs::create_dir_all(run_dir.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(outside.path(), &run_dir).unwrap();

        let status_path = mission_dir.join("status.json");
        let mut status: Value =
            serde_json::from_str(&std::fs::read_to_string(&status_path).unwrap()).unwrap();
        status["dispatch_id"] = json!(dispatch_id);
        status["run_dir"] = json!(run_dir);
        std::fs::write(&status_path, serde_json::to_string_pretty(&status).unwrap()).unwrap();
        assert!(
            !mission_dir.join("result.md").exists(),
            "spawn must not create a mission result before collection"
        );

        let mut collect = params("collect");
        collect.arena_id = Some(arena_id.clone());
        collect.mission_id = Some(mission_id.clone());
        let collected: Value =
            serde_json::from_str(&handle_tachi_arena(&server, collect).await.unwrap()).unwrap();
        let mission = &collected["missions"][0];
        assert_eq!(
            mission["state"],
            json!("artifact_read_error"),
            "{mission:#}"
        );
        assert_eq!(mission["result_source"], json!("result_read_error"));
        assert_ne!(mission["state"], json!("pending_result"));
        assert_eq!(mission["result"], json!(""));
        let error = mission["artifact_read_error"]
            .as_str()
            .expect("caller-visible refusal");
        let expected_refusal = format!(
            "refusing linked dispatch read: run directory {} resolves outside runs root {}",
            outside.path().canonicalize().unwrap().display(),
            tachi_home
                .path()
                .join("runs")
                .canonicalize()
                .unwrap()
                .display(),
        );
        assert_eq!(error, expected_refusal);
        // [1319-D1] collect never copies the linked result into the mission;
        // the canonical run_dir/result.md is the single source of truth.
        assert!(
            !mission_dir.join("result.md").exists(),
            "collect must not write a result.md for an escaped linked run"
        );

        std::fs::remove_file(&run_dir).unwrap();
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::json!({"state": "completed"}).to_string(),
        )
        .unwrap();
        std::fs::write(
            run_dir.join("result.md"),
            vec![b'x'; crate::arena_ops::state::ARENA_LINKED_RESULT_MAX_BYTES],
        )
        .unwrap();
        let mut exact_collect = params("collect");
        exact_collect.arena_id = Some(arena_id.clone());
        exact_collect.mission_id = Some(mission_id.clone());
        let exact: Value = serde_json::from_str(
            &handle_tachi_arena(&server, exact_collect)
                .await
                .expect("exact-limit linked result must be collected"),
        )
        .unwrap();
        assert_eq!(
            exact["missions"][0]["result"].as_str().map(str::len),
            Some(crate::arena_ops::state::ARENA_LINKED_RESULT_MAX_BYTES)
        );

        // [1319-D1] linked_result_copies ratchet: the canonical dispatch result
        // is surfaced without writing it into mission result.md.
        assert!(
            !mission_dir.join("result.md").exists(),
            "linked result must not be copied into mission result.md"
        );

        std::fs::write(
            run_dir.join("result.md"),
            vec![b'x'; crate::arena_ops::state::ARENA_LINKED_RESULT_MAX_BYTES + 1],
        )
        .unwrap();
        let mut over_collect = params("collect");
        over_collect.arena_id = Some(arena_id);
        over_collect.mission_id = Some(mission_id);
        let over: Value = serde_json::from_str(
            &handle_tachi_arena(&server, over_collect)
                .await
                .expect("over-limit refusal is returned as mission state"),
        )
        .unwrap();
        assert_eq!(over["missions"][0]["state"], json!("artifact_read_error"));
        assert!(over["missions"][0]["artifact_read_error"]
            .as_str()
            .is_some_and(|error| error.contains("named limit")));
    });

    drop(original_home);
}

#[tokio::test]
async fn arena_spawn_launch_failure_returns_recoverable_mission_status() {
    let _root = temp_arena_root();
    let server = server();
    let arena_id = "arena_20260606T000000Z_test_launch_fail".to_string();
    let mut spawn = params("spawn");
    spawn.arena_id = Some(arena_id);
    spawn.prompt = Some("launch with bad profile".into());
    spawn.harness = Some("opencode".into());
    spawn.profile = Some("missing_dispatch_profile".into());
    spawn.launch = true;
    spawn.dispatch_reason = Some(tachi_params::TachiDispatchReason::ExplicitUserRequest);
    let spawned: Value =
        serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();

    assert_eq!(spawned["state"], json!("launch_failed"));
    assert_eq!(spawned["launch"]["status"], json!("failed"));
    assert_eq!(spawned["launch"]["recoverable"], json!(true));
    assert_eq!(spawned["status"]["state"], json!("launch_failed"));
    assert!(spawned["status"]["launch_error"]
        .as_str()
        .unwrap()
        .contains("missing_dispatch_profile"));
    assert!(PathBuf::from(spawned["prompt_path"].as_str().unwrap()).exists());
    assert!(PathBuf::from(spawned["status_path"].as_str().unwrap()).exists());
}

#[tokio::test]
async fn arena_worker_launch_requires_native_first_exception_before_mission_artifacts() {
    let _root = temp_arena_root();
    let server = server();
    let arena_id = "arena_20260606T000000Z_test_native_first".to_string();
    let arena_dir = crate::arena_ops::state::arena_dir(&arena_id).unwrap();

    let mut spawn = params("spawn");
    spawn.arena_id = Some(arena_id);
    spawn.mission_id = Some("must-not-exist".into());
    spawn.prompt = Some("ordinary local parallel work".into());
    spawn.harness = Some("opencode".into());
    spawn.launch = true;
    let error = handle_tachi_arena(&server, spawn)
        .await
        .expect_err("launch-capable lanes require a typed exception");

    assert!(error.contains("native subagent"), "{error}");
    assert!(
        error.contains("zero mission or dispatch artifacts"),
        "{error}"
    );
    assert!(!arena_dir.join("missions/must-not-exist").exists());
}

#[tokio::test]
async fn arena_removed_actions_are_rejected() {
    let _root = temp_arena_root();
    let server = server();
    for removed in ["open", "abort", "close", "reap"] {
        let mut p = params(removed);
        p.arena_id = Some("arena_20260606T000000Z_test_reject".to_string());
        let error = handle_tachi_arena(&server, p)
            .await
            .expect_err("removed action must be rejected");
        assert!(
            error.contains("Invalid action"),
            "removed action {removed} should be rejected: {error}"
        );
    }
}
