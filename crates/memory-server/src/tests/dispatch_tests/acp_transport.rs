use super::super::{make_server, make_server_with_temp_home};
use super::{
    dispatch_params, task_params, wait_for_dispatch_result, wait_for_dispatch_status,
    write_acpx_control_fixture, write_fake_acpx_control_module, EnvVarGuard,
};
use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};
#[tokio::test]
async fn tachi_task_facade_defaults_to_json_and_keeps_markdown_escape_hatch() {
    let server = make_server();

    let mut json_params = task_params("profiles");
    json_params.format = None;
    let json_body = server
        .tachi_task(Parameters(json_params))
        .await
        .expect("default profiles should succeed");
    let parsed: Value = serde_json::from_str(&json_body).expect("default profiles JSON");
    assert!(parsed["dispatch_profiles"].as_array().is_some());

    let mut markdown_params = task_params("profiles");
    markdown_params.format = Some("markdown".to_string());
    let markdown = server
        .tachi_task(Parameters(markdown_params))
        .await
        .expect("markdown profiles should succeed");
    assert!(markdown.starts_with("## Tachi task profiles"), "{markdown}");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_wait_returns_terminal_dispatch_status() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let server = make_server();
    let dispatch_id = "dispatch-wait-complete";
    let run_dir = temp_home.path().join("runs").join(dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("run dir");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string_pretty(&json!({
            "dispatch_id": dispatch_id,
            "agent": "codex",
            "task": "wait for completed dispatch",
            "state": "TASK_STATE_COMPLETED",
            "exit_code": 0,
            "updated_at": Utc::now().to_rfc3339(),
        }))
        .expect("status json"),
    )
    .expect("write status");
    std::fs::write(run_dir.join("result.md"), "done").expect("result");

    let mut params = task_params("wait");
    params.dispatch_id = Some(dispatch_id.to_string());
    params.timeout_secs = Some(0);
    let response = server
        .tachi_task(Parameters(params))
        .await
        .expect("wait should succeed");
    let parsed: Value = serde_json::from_str(&response).expect("wait JSON");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["terminal"], json!(true));
    assert_eq!(parsed["state"], json!("TASK_STATE_COMPLETED"));
    assert_eq!(parsed["task"]["result_written"], json!(true));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_status_runs_acpx_status_control() {
    let (server, temp_home) = make_server_with_temp_home();
    let temp_python = tempfile::tempdir().expect("temp python module");
    write_fake_acpx_control_module(temp_python.path());
    let _pythonpath = EnvVarGuard::set_path("PYTHONPATH", temp_python.path());
    let dispatch_id = "99991231T235957Z-acpx-status-control";
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(dispatch_id);
    write_acpx_control_fixture(&run_dir, dispatch_id);

    let mut params = task_params("status");
    params.dispatch_id = Some(dispatch_id.to_string());
    params.timeout_secs = Some(5);
    let response = server
        .tachi_task(Parameters(params))
        .await
        .expect("status should succeed");
    let parsed: Value = serde_json::from_str(&response).expect("status JSON");
    assert_eq!(parsed["status"], json!("ok"));
    assert_eq!(
        parsed["acpx_status"]["stdout_json"]["action"],
        json!("status")
    );
    assert_eq!(
        parsed["acpx_status"]["stdout_json"]["state"],
        json!("running")
    );
    assert!(run_dir.join("acpx_status.json").exists());
    let trajectory = std::fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("trajectory");
    assert!(trajectory.contains("acpx_control_invoked"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_cancel_invokes_acpx_cancel_and_records_request() {
    let (server, temp_home) = make_server_with_temp_home();
    let temp_python = tempfile::tempdir().expect("temp python module");
    write_fake_acpx_control_module(temp_python.path());
    let _pythonpath = EnvVarGuard::set_path("PYTHONPATH", temp_python.path());
    let dispatch_id = "99991231T235956Z-acpx-cancel-control";
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(dispatch_id);
    write_acpx_control_fixture(&run_dir, dispatch_id);

    let mut params = task_params("cancel");
    params.dispatch_id = Some(dispatch_id.to_string());
    params.timeout_secs = Some(5);
    let response = server
        .tachi_task(Parameters(params))
        .await
        .expect("cancel should succeed");
    let parsed: Value = serde_json::from_str(&response).expect("cancel JSON");
    assert_eq!(parsed["status"], json!("cancel_requested"));
    assert_eq!(
        parsed["acpx_cancel"]["stdout_json"]["action"],
        json!("cancel")
    );
    assert!(run_dir.join("acpx_cancel.json").exists());

    let status: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .expect("updated status JSON");
    assert_eq!(status["cancel_requested"], json!(true));
    assert_eq!(
        status["acpx_cancel"]["stdout_json"]["state"],
        json!("cancelled")
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_acpx_transport_persists_events_and_final_result() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_python = tempfile::tempdir().expect("temp python module");
    let fake_acpx = temp_python.path().join("fake_acpx.py");
    std::fs::write(
        &fake_acpx,
        r#"import json
print(json.dumps({"type": "message", "message": "working"}))
print(json.dumps({"event": "tool_call_start", "tool_name": "noop"}))
print(json.dumps({"event": "end_turn", "final_response": "acpx done"}))
"#,
    )
    .expect("fake acpx module");

    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _pythonpath = EnvVarGuard::set_path("PYTHONPATH", temp_python.path());
    let _acpx_command = EnvVarGuard::set_value("TACHI_ACPX_COMMAND", "python3");
    let _acpx_args = EnvVarGuard::set_value("TACHI_ACPX_ARGS", "-m fake_acpx");
    let _acpx_agent = EnvVarGuard::set_value("TACHI_ACPX_AGENT", "codex");
    let _acpx_mode = EnvVarGuard::set_value("TACHI_ACPX_RUN_MODE", "exec");
    let _acpx_session_mode = EnvVarGuard::set_value("TACHI_ACPX_SESSION_MODE", "");
    let _acpx_session = EnvVarGuard::set_value("TACHI_ACPX_SESSION", "");

    let server = make_server();
    let mut params = dispatch_params(Some("codex"), "Run through fake acpx");
    params.harness_transport = Some("acpx".to_string());
    params.cwd = Some(temp_home.path().to_string_lossy().to_string());
    params.timeout_secs = 5;

    let response = server
        .tachi_dispatch(Parameters(params))
        .await
        .expect("acpx dispatch should start");
    let parsed: Value = serde_json::from_str(&response).expect("dispatch JSON");
    assert_eq!(parsed["execution_backend"], json!("acpx"));
    assert_eq!(parsed["acpx"]["permissions"], json!("approve-reads"));
    assert_eq!(parsed["acpx"]["mode"], json!("exec"));
    assert_eq!(parsed["acpx"]["session"], json!(null));
    assert!(
        parsed["acpx"]["prompt_file"]
            .as_str()
            .is_some_and(|path| path.ends_with("/prompt.md")),
        "acpx should execute the canonical Tachi prompt.md: {parsed:#}"
    );
    assert_eq!(
        parsed["acpx"]["controls"]["status"]["supported"],
        json!(false)
    );
    let dispatch_id = parsed["dispatch_id"].as_str().expect("dispatch id");
    let run_dir = temp_home.path().join("runs").join(dispatch_id);
    assert!(run_dir.join("prompt.md").exists());
    assert!(!run_dir.join("acpx_prompt.md").exists());

    let result = wait_for_dispatch_result(&run_dir).await;
    assert_eq!(result, "acpx done");

    let acpx_events =
        std::fs::read_to_string(run_dir.join("acpx_events.jsonl")).expect("acpx events");
    assert!(acpx_events.contains("tool_call_start"));
    let trajectory = std::fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("trajectory");
    assert!(trajectory.contains("execution_backend_prepared"));
    assert!(trajectory.contains("acpx_tool_event"));
    assert!(trajectory.contains("acpx_events_persisted"));
    let status = wait_for_dispatch_status(&run_dir).await;
    assert_eq!(status["execution_backend"], json!("acpx"));
    assert_eq!(status["acpx"]["mode"], json!("exec"));
    assert_eq!(
        status["acpx_events"]["final_response_extracted"],
        json!(true)
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_acpx_session_mode_derives_raven_for_review_profile() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_python = tempfile::tempdir().expect("temp python module");
    std::fs::write(
        temp_python.path().join("fake_acpx_session.py"),
        r#"import json
print(json.dumps({"type": "message", "message": "reviewing"}))
print(json.dumps({"event": "end_turn", "final_response": "raven done"}))
"#,
    )
    .expect("fake acpx session module");

    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _pythonpath = EnvVarGuard::set_path("PYTHONPATH", temp_python.path());
    let _acpx_command = EnvVarGuard::set_value("TACHI_ACPX_COMMAND", "python3");
    let _acpx_args = EnvVarGuard::set_value("TACHI_ACPX_ARGS", "-m fake_acpx_session");
    let _acpx_agent = EnvVarGuard::set_value("TACHI_ACPX_AGENT", "codex");
    let _acpx_mode = EnvVarGuard::set_value("TACHI_ACPX_RUN_MODE", "session");
    let _acpx_session = EnvVarGuard::set_value("TACHI_ACPX_SESSION", "");

    let server = make_server();
    let mut params = dispatch_params(Some("codex"), "Review through fake acpx session");
    params.harness_transport = Some("acpx".to_string());
    params.cwd = Some(temp_home.path().to_string_lossy().to_string());
    params.profile = Some("codex_55_review".to_string());
    params.timeout_secs = 5;

    let response = server
        .tachi_dispatch(Parameters(params))
        .await
        .expect("acpx session dispatch should start");
    let parsed: Value = serde_json::from_str(&response).expect("dispatch JSON");
    assert_eq!(parsed["execution_backend"], json!("acpx"));
    assert_eq!(parsed["acpx"]["mode"], json!("session"));
    assert_eq!(parsed["acpx"]["session"], json!("raven"));
    assert_eq!(
        parsed["acpx"]["controls"]["status"]["supported"],
        json!(true)
    );

    let dispatch_id = parsed["dispatch_id"].as_str().expect("dispatch id");
    let run_dir = temp_home.path().join("runs").join(dispatch_id);
    let result = wait_for_dispatch_result(&run_dir).await;
    assert_eq!(result, "raven done");
    let status = wait_for_dispatch_status(&run_dir).await;
    assert_eq!(status["acpx"]["mode"], json!("session"));
    assert_eq!(status["acpx"]["session"], json!("raven"));
    assert_eq!(
        status["acpx"]["controls"]["cancel"]["supported"],
        json!(true)
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_native_acp_transport_persists_and_reuses_session() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let project = tempfile::tempdir().expect("temp project");
    let temp_python = tempfile::tempdir().expect("temp python module");
    let mock_log = temp_python.path().join("methods.log");
    let fake_acp = temp_python.path().join("fake_native_acp.py");
    std::fs::write(
        &fake_acp,
        r#"import json
import os
import sys

SESSION_ID = "native-session-1"
AGENT_SESSION_ID = "agent-native-1"
LOG = os.environ["ACP_MOCK_LOG"]

def send(message):
    print(json.dumps(message, separators=(",", ":")), flush=True)

def log(method):
    with open(LOG, "a", encoding="utf-8") as handle:
        handle.write(method + "\n")

for raw in sys.stdin:
    if not raw.strip():
        continue
    message = json.loads(raw)
    method = message.get("method", "")
    request_id = message.get("id")
    log(method)
    if method == "initialize":
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": {"loadSession": True},
            },
        })
    elif method == "session/new":
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "sessionId": SESSION_ID,
                "_meta": {"agentSessionId": AGENT_SESSION_ID},
            },
        })
    elif method == "session/resume":
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "sessionId": SESSION_ID,
                "_meta": {"agentSessionId": AGENT_SESSION_ID},
            },
        })
    elif method == "session/load":
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "sessionId": SESSION_ID,
                "_meta": {"agentSessionId": AGENT_SESSION_ID},
            },
        })
    elif method == "session/prompt":
        send({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": SESSION_ID,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "native "},
                },
            },
        })
        send({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": SESSION_ID,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "ok"},
                },
            },
        })
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"stopReason": "end_turn"},
        })
    else:
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": -32601, "message": "unsupported " + method},
        })
"#,
    )
    .expect("fake native ACP adapter");

    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _mock_log = EnvVarGuard::set_path("ACP_MOCK_LOG", &mock_log);
    let _run_mode = EnvVarGuard::set_value("TACHI_ACP_NATIVE_RUN_MODE", "session");
    let _legacy_run_mode = EnvVarGuard::set_value("TACHI_ACP_RUN_MODE", "session");
    let _session = EnvVarGuard::set_value("TACHI_ACP_NATIVE_SESSION", "unit-native");
    let _legacy_session = EnvVarGuard::set_value("TACHI_ACP_SESSION", "unit-native");

    let server = make_server();
    let mut first = dispatch_params(Some("custom"), "Run through native ACP first");
    first.harness_transport = Some("acp-native".to_string());
    first.command = vec![
        "python3".to_string(),
        fake_acp.to_string_lossy().to_string(),
    ];
    first.cwd = Some(project.path().to_string_lossy().to_string());
    first.timeout_secs = 5;

    let first_response = server
        .tachi_dispatch(Parameters(first.clone()))
        .await
        .expect("native ACP dispatch should start");
    let first_parsed: Value = serde_json::from_str(&first_response).expect("dispatch JSON");
    assert_eq!(first_parsed["execution_backend"], json!("acp_native"));
    assert_eq!(first_parsed["acp_native"]["mode"], json!("session"));
    assert_eq!(first_parsed["acp_native"]["session"], json!("unit-native"));

    let first_dispatch_id = first_parsed["dispatch_id"].as_str().expect("dispatch id");
    let first_run_dir = temp_home.path().join("runs").join(first_dispatch_id);
    assert_eq!(wait_for_dispatch_result(&first_run_dir).await, "native ok");
    let stream = std::fs::read_to_string(first_run_dir.join("acp.stream.ndjson"))
        .expect("native ACP stream");
    assert!(stream.contains("\"method\":\"session/prompt\""), "{stream}");
    assert!(stream.contains("\"method\":\"session/update\""), "{stream}");

    let session_record_path = first_parsed["acp_native"]["session_record"]
        .as_str()
        .expect("session record path");
    let session_distill_path = first_parsed["acp_native"]["session_distill"]
        .as_str()
        .expect("session distill path");
    let session_record: Value =
        serde_json::from_str(&std::fs::read_to_string(session_record_path).expect("record"))
            .expect("record JSON");
    assert_eq!(session_record["schema"], json!("tachi.acp_session.v1"));
    assert_eq!(session_record["acp_session_id"], json!("native-session-1"));
    assert!(
        std::path::Path::new(session_distill_path).exists(),
        "session distill markdown should be written"
    );

    let mut second = first;
    second.task = "Run through native ACP second".to_string();
    let second_response = server
        .tachi_dispatch(Parameters(second))
        .await
        .expect("second native ACP dispatch should start");
    let second_parsed: Value = serde_json::from_str(&second_response).expect("dispatch JSON");
    assert_eq!(
        second_parsed["acp_native"]["session_record"],
        json!(session_record_path)
    );
    let second_dispatch_id = second_parsed["dispatch_id"].as_str().expect("dispatch id");
    let second_run_dir = temp_home.path().join("runs").join(second_dispatch_id);
    assert_eq!(wait_for_dispatch_result(&second_run_dir).await, "native ok");
    let second_status = wait_for_dispatch_status(&second_run_dir).await;
    assert_eq!(second_status["execution_backend"], json!("acp_native"));
    assert_eq!(second_status["acp_native"]["session"], json!("unit-native"));

    let methods = std::fs::read_to_string(&mock_log).expect("mock method log");
    assert_eq!(
        methods
            .lines()
            .filter(|method| *method == "session/new")
            .count(),
        1,
        "only the first native ACP turn should create a session: {methods}"
    );
    assert_eq!(
        methods
            .lines()
            .filter(|method| *method == "session/resume")
            .count(),
        1,
        "the second native ACP turn should resume the stored session: {methods}"
    );
}

#[test]
fn dispatch_run_cleanup_only_for_completed_success() {
    assert!(crate::dispatch_ops::should_cleanup_run(
        Some(0),
        Some("TASK_STATE_COMPLETED")
    ));
    assert!(!crate::dispatch_ops::should_cleanup_run(
        Some(0),
        Some("TASK_STATE_FAILED")
    ));
    assert!(!crate::dispatch_ops::should_cleanup_run(
        Some(0),
        Some("TASK_STATE_INPUT_REQUIRED")
    ));
    assert!(!crate::dispatch_ops::should_cleanup_run(
        Some(1),
        Some("TASK_STATE_COMPLETED")
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn dispatch_run_dir_is_created_with_0o700() {
    use std::os::unix::fs::PermissionsExt;

    let (server, _temp_home) = make_server_with_temp_home();
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");

    let mut params = dispatch_params(Some("custom"), "smoke dispatch dir mode");
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print('ok')".to_string(),
    ];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("custom dispatch should start");
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");

    let tachi_home = std::env::var("TACHI_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").expect("HOME set by TempHomeGuard"))
                .join(".tachi")
        });
    let run_dir = tachi_home.join("runs").join(dispatch_id);
    assert!(run_dir.exists(), "run dir should exist: {run_dir:?}");
    let mode = std::fs::metadata(&run_dir)
        .expect("run dir metadata")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o700,
        "dispatch run dir should be restricted to owner"
    );
}
