use super::*;
// Shared OpenCode OpenAPI fixture lives in `crate::test_support`; the local
// `spawn_auth_gated_opencode_doc_server` below is a distinct (auth-gated)
// variant that uses it.
use crate::test_support::OPENCODE_DOC_FIXTURE;

struct EnvGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set_path(key: &'static str, value: &std::path::Path) -> Self {
        let original = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, original }
    }

    fn remove(key: &'static str) -> Self {
        let original = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.original.as_ref() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn test_dispatch_params(agent: Option<&str>, task: &str) -> TachiDispatchParams {
    TachiDispatchParams {
        agent: agent.map(str::to_string),
        profile: None,
        task: task.to_string(),
        cwd: None,
        skills: Vec::new(),
        context_query: None,
        model: None,
        timeout_secs: 5,
        permission_profile: None,
        allowed_tools: Vec::new(),
        max_turns: None,
        sandbox: None,
        inject_tachi_mcp: None,
        inject_hub_mcps: None,
        command: Vec::new(),
        harness_transport: None,
        harness_server_url: None,
        project: None,
        stage: None,
        credential_profiles: Vec::new(),
        issue_ref: None,
        pr_ref: None,
        flow_id: None,
        tool_profile: None,
        auto_capability_bundle: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
    }
}

fn spawn_auth_gated_opencode_doc_server() -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe server");
    let port = listener.local_addr().expect("local addr").port();
    let handle = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept probe");
            let mut buf = [0_u8; 2048];
            let n = std::io::Read::read(&mut stream, &mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let is_doc = request.starts_with("GET /doc ");
            let has_auth = request.contains("Authorization: Basic ");
            let (status, body) = if is_doc && !has_auth {
                ("401 Unauthorized", "")
            } else if is_doc {
                ("200 OK", OPENCODE_DOC_FIXTURE)
            } else {
                ("200 OK", "<title>OpenCode</title>")
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write response");
        }
    });
    (format!("http://127.0.0.1:{port}"), handle)
}

fn single_run_dir(run_root: &std::path::Path) -> std::path::PathBuf {
    let runs = std::fs::read_dir(run_root)
        .expect("read run root")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    assert_eq!(runs.len(), 1, "expected one run dir, got {runs:?}");
    runs.into_iter().next().expect("one run dir")
}

async fn wait_for_result(run_dir: &std::path::Path) -> String {
    let result_path = run_dir.join("result.md");
    for _ in 0..120 {
        if let Ok(result) = tokio::fs::read_to_string(&result_path).await {
            return result;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("dispatch result was not written: {}", result_path.display());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn generate_mcp_config_sets_owner_only_permissions() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("HOME", temp_home.path());
    std::env::remove_var("TACHI_HOME");

    let server = crate::tests::make_server();
    let path = generate_mcp_config(&server, "test-perms", true, false, None, &[])
        .await
        .expect("generate mcp config")
        .expect("config path");

    assert!(path.exists());
    let temp_leftovers: Vec<_> = std::fs::read_dir(path.parent().expect("config parent"))
        .expect("read config parent")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("dispatch-test-perms-mcp.json.tmp."))
        .collect();
    assert!(
        temp_leftovers.is_empty(),
        "MCP config atomic write should not leave temp files: {temp_leftovers:?}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "MCP config mode should be 0o600, got {:#o}",
            mode
        );
    }

    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn opencode_serve_dispatch_fails_fast_when_probe_auth_fails() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvGuard::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let run_root = dispatch_runs_root();
    let _password = EnvGuard::remove("OPENCODE_SERVER_PASSWORD");
    let _username = EnvGuard::remove("OPENCODE_SERVER_USERNAME");
    let server = crate::tests::make_server();
    let (server_url, probe_server) = spawn_auth_gated_opencode_doc_server();

    let mut params = test_dispatch_params(Some("custom"), "should fail before subprocess");
    params.harness_transport = Some("opencode_serve".to_string());
    params.harness_server_url = Some(server_url);
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print('should-not-run')".to_string(),
    ];

    let err = handle_tachi_dispatch(&server, params)
        .await
        .expect_err("opencode_serve dispatch should fail before spawn");
    probe_server.join().expect("probe server thread");

    assert!(
        err.contains("opencode_serve attach not ready"),
        "unexpected error: {err}"
    );
    assert!(
        err.contains("OPENCODE_SERVER_PASSWORD"),
        "error should tell the user how to fix auth: {err}"
    );
    assert!(
        !err.contains("Session not found"),
        "preflight should not surface the subprocess fallback error: {err}"
    );

    let run_dir = single_run_dir(&run_root);
    let result = std::fs::read_to_string(run_dir.join("result.md")).expect("read result");
    assert!(result.contains("HTTP 401"), "result={result}");
    assert!(
        result.contains("OPENCODE_SERVER_PASSWORD"),
        "result={result}"
    );
    assert!(!result.contains("Session not found"), "result={result}");
    let status: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .expect("status JSON");
    assert_eq!(status["state"], json!("TASK_STATE_FAILED"));
    assert_eq!(
        status["harness_server_status"]["doc_error"],
        json!("OpenCode /doc returned HTTP 401")
    );
    assert_eq!(
        status["harness_server_status"]["server_password_configured"],
        json!(false)
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn opencode_serve_preflight_uses_dispatch_credential_env() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvGuard::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let _password = EnvGuard::remove("OPENCODE_SERVER_PASSWORD");
    let _username = EnvGuard::remove("OPENCODE_SERVER_USERNAME");
    let server = crate::tests::make_server();
    let project = tempfile::tempdir().expect("temp project");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::write(
        credentials_dir.join("opencode.json"),
        r#"{
          "credential_profiles": {
            "opencode_server_auth": {
              "entries": { "password": "OPENCODE_SERVER_PASSWORD_TEST" },
              "allowed_consumers": { "agents": ["custom"] },
              "materializers": [
                { "type": "env", "source": "password", "target": "OPENCODE_SERVER_PASSWORD" }
              ]
            }
          }
        }"#,
    )
    .expect("write credential profile");
    server
        .vault_init(rmcp::handler::server::wrapper::Parameters(
            crate::vault_ops::VaultInitParams {
                password: "dispatch opencode serve auth test".to_string(),
            },
        ))
        .await
        .expect("vault_init");
    server
        .vault_set(rmcp::handler::server::wrapper::Parameters(
            crate::vault_ops::VaultSetParams {
                name: "OPENCODE_SERVER_PASSWORD_TEST".to_string(),
                value: "test123".to_string(),
                agent_id: None,
                secret_type: "api_key".to_string(),
                description: "dispatch opencode serve password".to_string(),
                allowed_agents: Some(vec!["custom".to_string()]),
                enable_rotation: false,
                rotation_strategy: None,
            },
        ))
        .await
        .expect("vault_set");
    let (server_url, probe_server) = spawn_auth_gated_opencode_doc_server();

    let mut params = test_dispatch_params(Some("custom"), "should pass preflight");
    params.cwd = Some(project.path().to_string_lossy().to_string());
    params.harness_transport = Some("opencode_serve".to_string());
    params.harness_server_url = Some(server_url);
    params.credential_profiles = vec!["opencode_server_auth".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print('credential-ok')".to_string(),
    ];

    let raw = handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch should start with per-dispatch opencode serve auth");
    probe_server.join().expect("probe server thread");
    assert!(
        !raw.contains("test123"),
        "dispatch response must not leak credential values: {raw}"
    );
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_result(&run_dir).await;
    assert!(result.contains("credential-ok"), "result={result}");
}

#[test]
fn mcp_cleanup_removes_temp_config_on_drop() {
    let temp_home = tempfile::tempdir().expect("temp home");
    let path = temp_home.path().join("dispatch-test-mcp.json");
    std::fs::write(&path, b"{}").expect("write temp config");
    assert!(path.exists());
    {
        let _cleanup = McpCleanup(Some(path.clone()));
    }
    assert!(!path.exists(), "MCP config should be removed on drop");
}

#[test]
fn dispatch_runs_root_uses_canonical_tachi_home_aliases() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let sigil_home = tempfile::tempdir().expect("sigil home");
    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    let original_sigil_home = std::env::var_os("SIGIL_HOME");
    let original_app_home = std::env::var_os("TACHI_APP_HOME");

    std::env::set_var("HOME", temp_home.path());
    std::env::remove_var("TACHI_HOME");
    std::env::set_var("SIGIL_HOME", sigil_home.path());
    std::env::remove_var("TACHI_APP_HOME");
    assert_eq!(dispatch_runs_root(), sigil_home.path().join("runs"));

    std::env::remove_var("SIGIL_HOME");
    std::env::set_var("TACHI_APP_HOME", "~/custom-tachi");
    assert_eq!(
        dispatch_runs_root(),
        temp_home.path().join("custom-tachi").join("runs")
    );

    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
    if let Some(value) = original_sigil_home {
        std::env::set_var("SIGIL_HOME", value);
    } else {
        std::env::remove_var("SIGIL_HOME");
    }
    if let Some(value) = original_app_home {
        std::env::set_var("TACHI_APP_HOME", value);
    } else {
        std::env::remove_var("TACHI_APP_HOME");
    }
}

#[test]
fn recover_orphaned_dispatch_runs_marks_working_runs_failed() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("HOME", temp_home.path());
    std::env::remove_var("TACHI_HOME");

    let run_dir = dispatch_runs_root().join("20260614T000000Z-claude-deadbeef");
    std::fs::create_dir_all(&run_dir).expect("run dir");
    std::fs::write(
        run_dir.join("status.json"),
        json!({
            "dispatch_id": "20260614T000000Z-claude-deadbeef",
            "state": "TASK_STATE_WORKING",
            "agent": "claude",
        })
        .to_string(),
    )
    .expect("status");

    let recovered = recover_orphaned_dispatch_runs();
    assert_eq!(
        recovered,
        vec!["20260614T000000Z-claude-deadbeef".to_string()]
    );

    let status: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .unwrap();
    assert_eq!(status["state"], "TASK_STATE_FAILED");
    assert_eq!(status["recovery_reason"], "daemon_restart_orphan_recovery");

    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[test]
fn global_dispatch_slot_blocks_duplicate_active_task_without_flow_id() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("HOME", temp_home.path());
    std::env::remove_var("TACHI_HOME");

    let first = reserve_global_dispatch_slot("same task without flow", "dispatch-one")
        .expect("first global reserve");
    let duplicate = reserve_global_dispatch_slot("same task without flow", "dispatch-two")
        .expect_err("duplicate active task should be blocked without flow_id");
    assert!(
        duplicate.contains("duplicate dispatch blocked"),
        "unexpected error: {duplicate}"
    );

    release_flow_dispatch_slot(Some(first));
    assert!(
        reserve_global_dispatch_slot("same task without flow", "dispatch-three").is_ok(),
        "slot should be reusable after release"
    );

    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[test]
fn flow_dispatch_slot_blocks_duplicate_active_task() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("HOME", temp_home.path());
    std::env::remove_var("TACHI_HOME");

    let flow_id = format!(
        "flow_20260610T000000Z_duplicate_slot_{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = crate::shell_ops::run_dir_for_flow_id(&flow_id).expect("flow run dir");
    std::fs::create_dir_all(&run_dir).expect("create flow dir");

    let first = reserve_flow_dispatch_slot(Some(&flow_id), "same task", "dispatch-one")
        .expect("first reserve")
        .expect("slot path");
    let duplicate = reserve_flow_dispatch_slot(Some(&flow_id), "same task", "dispatch-two")
        .expect_err("duplicate active task should be blocked");
    assert!(
        duplicate.contains("duplicate dispatch blocked"),
        "unexpected error: {duplicate}"
    );

    release_flow_dispatch_slot(Some(first));
    assert!(
        reserve_flow_dispatch_slot(Some(&flow_id), "same task", "dispatch-three")
            .expect("reserve after release")
            .is_some(),
        "slot should be reusable after release"
    );

    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[test]
fn flow_dispatch_slot_reclaims_stale_lock_when_run_status_is_missing() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runs_root = tempfile::tempdir().expect("temp runs root");
    let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", runs_root.path());

    let flow_id = format!(
        "flow_20260610T000001Z_stale_slot_{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let task = "same task";
    let old_dispatch_id = "dispatch-stale-lock";
    let new_dispatch_id = "dispatch-new-lock";
    let run_dir = crate::shell_ops::run_dir_for_flow_id(&flow_id).expect("flow run dir");
    let lock_dir = run_dir.join(".dispatch-dedupe");
    std::fs::create_dir_all(&lock_dir).expect("create lock dir");
    let task_hash = crate::utils::stable_hash(task);
    let stale_created_at =
        (Utc::now() - chrono::Duration::seconds(DISPATCH_DEDUPE_STALE_LOCK_SECS + 1)).to_rfc3339();
    crate::utils::write_owner_only_file_atomic(
        &lock_dir.join(format!("{task_hash}.json")),
        serde_json::to_vec_pretty(&json!({
            "scope": "flow",
            "task_hash": task_hash,
            "dispatch_id": old_dispatch_id,
            "task": task,
            "flow_id": flow_id,
            "created_at": stale_created_at,
        }))
        .expect("serialize stale lock")
        .as_slice(),
    )
    .expect("write stale lock");

    let reserved = reserve_flow_dispatch_slot(Some(&flow_id), task, new_dispatch_id)
        .expect("stale lock should be reclaimed")
        .expect("slot path");
    let lock: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&reserved).expect("read lock"))
            .expect("parse lock");
    assert_eq!(lock["dispatch_id"], json!(new_dispatch_id));

    release_flow_dispatch_slot(Some(reserved));
    if let Some(value) = original_run_root {
        std::env::set_var("TACHI_RUN_ROOT", value);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}
