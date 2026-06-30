use super::*;
use crate::arena_ops::state::mission_dir;

#[tokio::test]
async fn arena_open_spawn_collect_close_writes_tracked_documents() {
    let _root = temp_arena_root();
    let server = server();
    let mut open = params("open");
    open.title = Some("Arena Test".into());
    open.objective = Some("coordinate tracked workers".into());
    let raw = handle_tachi_arena(&server, open).await.unwrap();
    let opened: Value = serde_json::from_str(&raw).unwrap();
    let arena_id = opened["arena_id"].as_str().unwrap().to_string();
    let arena_dir = PathBuf::from(opened["arena_dir"].as_str().unwrap());
    assert!(arena_dir.join("arena.md").exists());
    assert!(arena_dir.join("manifest.json").exists());

    let mut spawn = params("spawn");
    spawn.arena_id = Some(arena_id.clone());
    spawn.prompt = Some("inspect the code".into());
    spawn.harness = Some("codex".into());
    spawn.role = Some("explore".into());
    spawn.skills = vec!["skill:waza-check".into()];
    let raw = handle_tachi_arena(&server, spawn).await.unwrap();
    let spawned: Value = serde_json::from_str(&raw).unwrap();
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

    let mut close = params("close");
    close.arena_id = Some(arena_id);
    let raw = handle_tachi_arena(&server, close).await.unwrap();
    let closed: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(closed["state"], "closed");
    assert!(arena_dir.join("summary.md").exists());
    let summary = std::fs::read_to_string(arena_dir.join("summary.md")).unwrap();
    assert!(summary.contains(&mission_id));
    assert!(summary.contains("Summary: done"));
}

#[tokio::test]
async fn arena_board_refreshes_external_plan_and_result_writes() {
    let _root = temp_arena_root();
    let server = server();
    let mut open = params("open");
    open.objective = Some("refresh board".into());
    let opened: Value =
        serde_json::from_str(&handle_tachi_arena(&server, open).await.unwrap()).unwrap();
    let arena_id = opened["arena_id"].as_str().unwrap().to_string();

    let mut spawn = params("spawn");
    spawn.arena_id = Some(arena_id.clone());
    spawn.prompt = Some("write files externally".into());
    let spawned: Value =
        serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
    let mission_dir = PathBuf::from(spawned["mission_dir"].as_str().unwrap());
    std::fs::write(mission_dir.join("plan.md"), "Plan: external write\n").unwrap();
    std::fs::write(mission_dir.join("result.md"), "Summary: external result\n").unwrap();

    let mut board = params("board");
    board.arena_id = Some(arena_id);
    let board: Value =
        serde_json::from_str(&handle_tachi_arena(&server, board).await.unwrap()).unwrap();
    let mission = &board["result"]["missions"][0];
    assert_eq!(mission["plan_written"], true);
    assert_eq!(mission["result_written"], true);

    let board: Value =
        serde_json::from_str(&handle_tachi_arena(&server, params("board")).await.unwrap()).unwrap();
    let arena = &board["arenas"][0];
    assert_eq!(arena["mission_count"], json!(1));
    assert_eq!(arena["active_missions"], json!(1));
    assert_eq!(arena["pending_collect"], json!(1));
    assert!(arena["board_path"]
        .as_str()
        .unwrap()
        .ends_with("board.json"));
}

#[tokio::test]
async fn arena_collect_marks_corrupt_result_as_read_error_instead_of_pending() {
    let _root = temp_arena_root();
    let server = server();
    let mut open = params("open");
    open.objective = Some("collect corrupt result".into());
    let opened: Value =
        serde_json::from_str(&handle_tachi_arena(&server, open).await.unwrap()).unwrap();
    let arena_id = opened["arena_id"].as_str().unwrap().to_string();

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
    collect.arena_id = Some(arena_id);
    collect.mission_id = Some(mission_id);
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
}

#[tokio::test]
async fn arena_close_blocks_active_missions_until_reaped_or_aborted() {
    let _root = temp_arena_root();
    let server = server();
    let mut open = params("open");
    open.objective = Some("block close".into());
    let opened: Value =
        serde_json::from_str(&handle_tachi_arena(&server, open).await.unwrap()).unwrap();
    let arena_id = opened["arena_id"].as_str().unwrap().to_string();
    let mut spawn = params("spawn");
    spawn.arena_id = Some(arena_id.clone());
    spawn.prompt = Some("stay active".into());
    let spawned: Value =
        serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
    let mission_id = spawned["mission_id"].as_str().unwrap().to_string();

    let mut close = params("close");
    close.arena_id = Some(arena_id.clone());
    let blocked: Value =
        serde_json::from_str(&handle_tachi_arena(&server, close).await.unwrap()).unwrap();
    assert_eq!(blocked["state"], "blocked");

    let mut reap = params("reap");
    reap.arena_id = Some(arena_id.clone());
    reap.dry_run = Some(false);
    let reaped: Value =
        serde_json::from_str(&handle_tachi_arena(&server, reap).await.unwrap()).unwrap();
    assert_eq!(reaped["stale_missions"].as_array().unwrap().len(), 0);

    let mission_dir = mission_dir(&arena_id, &mission_id).unwrap();
    let status_path = mission_dir.join("status.json");
    let mut status: Value =
        serde_json::from_str(&std::fs::read_to_string(&status_path).unwrap()).unwrap();
    status["created_at"] = json!((chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339());
    crate::utils::write_json_file_owner_only(&status_path, &status).unwrap();

    let mut reap = params("reap");
    reap.arena_id = Some(arena_id.clone());
    reap.dry_run = Some(false);
    let reaped: Value =
        serde_json::from_str(&handle_tachi_arena(&server, reap).await.unwrap()).unwrap();
    assert_eq!(reaped["stale_missions"].as_array().unwrap().len(), 1);

    let mut close = params("close");
    close.arena_id = Some(arena_id);
    let closed: Value =
        serde_json::from_str(&handle_tachi_arena(&server, close).await.unwrap()).unwrap();
    assert_eq!(closed["state"], "closed");
}

#[tokio::test]
async fn arena_spawn_launch_failure_returns_recoverable_mission_status() {
    let _root = temp_arena_root();
    let server = server();
    let mut open = params("open");
    open.objective = Some("recover failed launch".into());
    let opened: Value =
        serde_json::from_str(&handle_tachi_arena(&server, open).await.unwrap()).unwrap();
    let arena_id = opened["arena_id"].as_str().unwrap().to_string();

    let mut spawn = params("spawn");
    spawn.arena_id = Some(arena_id);
    spawn.prompt = Some("launch with bad profile".into());
    spawn.harness = Some("opencode".into());
    spawn.profile = Some("missing_dispatch_profile".into());
    spawn.launch = true;
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
