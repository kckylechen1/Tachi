use super::state::{validate_arena_id, validate_mission_id};
use super::{handle_tachi_arena, tachi_arena_root_env_lock};
use crate::tool_params::SaveMemoryParams;
use crate::{MemoryServer, TachiArenaParams};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

struct ArenaRootGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
    original: Option<std::ffi::OsString>,
    path: PathBuf,
}

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

impl Drop for ArenaRootGuard {
    fn drop(&mut self) {
        // SAFETY: arena tests serialize access to TACHI_ARENA_ROOT with
        // tachi_arena_root_env_lock(), so no concurrent env mutation occurs.
        unsafe {
            if let Some(value) = self.original.as_ref() {
                std::env::set_var("TACHI_ARENA_ROOT", value);
            } else {
                std::env::remove_var("TACHI_ARENA_ROOT");
            }
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn temp_arena_root() -> ArenaRootGuard {
    let guard = tachi_arena_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let original = std::env::var_os("TACHI_ARENA_ROOT");
    let path = std::env::temp_dir().join(format!("tachi-arena-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    // SAFETY: protected by tachi_arena_root_env_lock(); see Drop impl.
    unsafe {
        std::env::set_var("TACHI_ARENA_ROOT", &path);
    }
    ArenaRootGuard {
        _guard: guard,
        original,
        path,
    }
}

fn server() -> MemoryServer {
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-arena-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    MemoryServer::new(db_path, None).expect("test server")
}

fn params(action: &str) -> TachiArenaParams {
    TachiArenaParams {
        action: action.to_string(),
        format: None,
        arena_id: None,
        mission_id: None,
        title: None,
        objective: None,
        prompt: None,
        harness: None,
        role: None,
        cwd: None,
        skills: Vec::new(),
        scope: Vec::new(),
        permissions: Vec::new(),
        timeout_secs: None,
        launch: false,
        profile: None,
        model: None,
        project: None,
        flow_id: None,
        issue_ref: None,
        pr_ref: None,
        permission_profile: None,
        sandbox: None,
        credential_profiles: Vec::new(),
        tool_profile: None,
        auto_capability_bundle: None,
        reason: None,
        dry_run: None,
        force: false,
        require_collected: None,
    }
}

#[tokio::test]
async fn tachi_arena_facade_defaults_to_json_and_keeps_markdown_escape_hatch() {
    let server = server();

    let json_params: rmcp::handler::server::wrapper::Parameters<TachiArenaParams> =
        rmcp::handler::server::wrapper::Parameters(params("board"));
    let json_body: String = server
        .tachi_arena(json_params)
        .await
        .expect("default board should succeed");
    let parsed: Value = serde_json::from_str(&json_body).expect("default board JSON");
    assert_eq!(parsed["action"], json!("board"));

    let mut markdown_params = params("board");
    markdown_params.format = Some("markdown".to_string());
    let markdown_params: rmcp::handler::server::wrapper::Parameters<TachiArenaParams> =
        rmcp::handler::server::wrapper::Parameters(markdown_params);
    let markdown: String = server
        .tachi_arena(markdown_params)
        .await
        .expect("markdown board should succeed");
    assert!(markdown.starts_with("## Tachi arena board"), "{markdown}");
    assert!(markdown.contains("```json"), "{markdown}");
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

async fn save_arena_feedback_rule(server: &MemoryServer) -> String {
    let raw = crate::memory_search_ops::handle_save_memory(
            server,
            SaveMemoryParams {
                text: "Arena code-audit workers must report grep evidence for unused or dead-code claims.".to_string(),
                summary: "Subagent audit prompts require explicit search evidence".to_string(),
                path: "/feedback/subagent/code-audit/grep-evidence".to_string(),
                importance: 0.8,
                category: "prompt_rule".to_string(),
                topic: "Subagent audit prompts require explicit search evidence".to_string(),
                keywords: vec![
                    "feedback_rule".to_string(),
                    "prompt_rule".to_string(),
                    "dead_code".to_string(),
                    "grep".to_string(),
                ],
                persons: Vec::new(),
                entities: Vec::new(),
                location: String::new(),
                scope: "project".to_string(),
                vector: None,
                id: None,
                force: true,
                auto_link: true,
                project: None,
                retention_policy: Some("durable".to_string()),
                domain: None,
                timestamp: None,
                valid_from: None,
                valid_until: None,
                metadata: Some(json!({
                    "kind": "feedback_rule",
                    "category": "prompt_rule",
                    "applies_to": {
                        "task_type": ["explore"],
                        "profiles": ["codex_55_review"],
                        "stage": ["explore"]
                    },
                    "trigger_keywords": ["unused", "dead code", "grep"],
                    "prompt_patch": "Search both identifier and call forms before making dead-code claims.",
                    "evidence_contract": ["grep_commands", "paths_searched", "uncertainty_notes"]
                })),
            },
        )
        .await
        .expect("feedback rule save should succeed");
    serde_json::from_str::<Value>(&raw)
        .expect("save JSON")
        .get("id")
        .and_then(Value::as_str)
        .expect("saved rule id")
        .to_string()
}

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
async fn arena_spawn_injects_applicable_feedback_rules_into_mission_prompt() {
    let _root = temp_arena_root();
    let server = server();
    let rule_id = save_arena_feedback_rule(&server).await;

    let mut open = params("open");
    open.title = Some("Feedback Arena".into());
    open.objective = Some("coordinate code-audit workers".into());
    let opened: Value =
        serde_json::from_str(&handle_tachi_arena(&server, open).await.unwrap()).unwrap();
    let arena_id = opened["arena_id"].as_str().unwrap().to_string();

    let mut spawn = params("spawn");
    spawn.arena_id = Some(arena_id);
    spawn.prompt = Some("Explore unused functions and dead code with grep evidence.".into());
    spawn.harness = Some("codex".into());
    spawn.role = Some("explore".into());
    spawn.profile = Some("codex_55_review".into());
    let spawned: Value =
        serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
    let mission_dir = PathBuf::from(spawned["mission_dir"].as_str().unwrap());
    let prompt = std::fs::read_to_string(mission_dir.join("prompt.md")).unwrap();
    assert!(prompt.contains("## Applicable feedback rules"), "{prompt}");
    assert!(
        prompt.contains("Search both identifier and call forms"),
        "{prompt}"
    );

    let status: Value =
        serde_json::from_str(&std::fs::read_to_string(mission_dir.join("status.json")).unwrap())
            .unwrap();
    assert_eq!(status["feedback_rules"]["rules"][0]["id"], json!(rule_id));
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
    handle_tachi_arena(&server, spawn).await.unwrap();

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
    assert_eq!(reaped["stale_missions"].as_array().unwrap().len(), 1);

    let mut close = params("close");
    close.arena_id = Some(arena_id);
    let closed: Value =
        serde_json::from_str(&handle_tachi_arena(&server, close).await.unwrap()).unwrap();
    assert_eq!(closed["state"], "closed");
}

#[test]
fn arena_ids_reject_traversal() {
    for invalid in ["../../x", "arena_../x", "arena_bad/name", "notarena_x"] {
        assert!(validate_arena_id(invalid).is_err(), "{invalid}");
    }
    assert!(validate_arena_id("arena_20260606T000000Z_demo_deadbeef").is_ok());
    assert!(validate_mission_id("mission_explore_deadbeef").is_ok());
    let err = validate_mission_id("bad/name").unwrap_err();
    assert!(err.contains("Expected prefix 'mission_'"));
}

#[test]
#[cfg(unix)]
fn append_event_writes_synced_and_owner_only_file() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    crate::utils::append_run_event(tmp.path(), json!({"event": "test"})).unwrap();

    let path = tmp.path().join("events.jsonl");
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(contents.contains("\"event\":\"test\""));

    let meta = std::fs::metadata(&path).unwrap();
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "events.jsonl should be owner-readable only");
}
