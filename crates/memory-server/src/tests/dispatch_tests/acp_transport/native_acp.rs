use super::*;

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
