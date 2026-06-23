use super::*;

struct EnvGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set_path(key: &'static str, value: &Path) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: arena tests that use this helper hold tachi_arena_root_env_lock.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, original }
    }

    fn set_os(key: &'static str, value: std::ffi::OsString) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: arena tests that use this helper hold tachi_arena_root_env_lock.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: arena tests that use this helper hold tachi_arena_root_env_lock.
        unsafe {
            if let Some(value) = self.original.as_ref() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

async fn wait_for_nonempty_file(path: &Path) -> String {
    for _ in 0..40 {
        if let Ok(raw) = std::fs::read_to_string(path) {
            if !raw.trim().is_empty() {
                return raw;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    std::fs::read_to_string(path).unwrap_or_default()
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
    let _home = EnvGuard::set_path("TACHI_HOME", temp_home.path());
    let path_value = std::env::join_paths(
        std::iter::once(fake_bin.path().to_path_buf()).chain(
            std::env::var_os("PATH")
                .and_then(|raw| std::env::split_paths(&raw).next().map(|_| raw))
                .into_iter()
                .flat_map(|raw| std::env::split_paths(&raw).collect::<Vec<_>>()),
        ),
    )
    .expect("join PATH");
    let _path = EnvGuard::set_os("PATH", path_value);

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
        spawned["status"]["run_dir"]
            .as_str()
            .expect("linked run dir"),
    );
    let spawned_status = spawned["status"]
        .as_object()
        .expect("spawned status object");
    assert!(
        !spawned_status.contains_key("dispatch_response"),
        "arena status must not persist full dispatch response: {spawned_status:#?}"
    );
    assert_eq!(spawned["status"]["dispatch_link"]["redacted"], json!(true));
    assert_eq!(
        spawned["status"]["dispatch_link"]["source"],
        json!("dispatch_response_summary")
    );
    let dispatch_result = wait_for_nonempty_file(&run_dir.join("result.md")).await;
    assert!(dispatch_result.contains("fake opencode completed"));

    let mut board = params("board");
    board.arena_id = Some(arena_id.clone());
    let board: Value =
        serde_json::from_str(&handle_tachi_arena(&server, board).await.unwrap()).unwrap();
    let mission = &board["result"]["missions"][0];
    assert_eq!(mission["dispatch_id"], json!(dispatch_id));
    assert_eq!(mission["linked_dispatch"]["redacted"], json!(true));
    assert_eq!(
        mission["linked_dispatch"]["source"],
        json!("dispatch_run_summary")
    );
    assert!(
        mission["linked_dispatch"].get("status").is_none(),
        "linked dispatch must be a redacted summary: {mission:#}"
    );
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
    assert!(collected["missions"][0]["result"]
        .as_str()
        .unwrap()
        .contains("fake opencode completed"));
    assert_eq!(
        collected["missions"][0]["completion_draft"]["arguments"]["action"],
        json!("complete")
    );
}
