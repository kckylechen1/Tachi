use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

const PEER: &str = r#"
import json, os, sys
mode = os.environ["FAKE_MODE"]
log = open(os.environ["FAKE_LOG"], "a")
def emit(message): print(json.dumps(message), flush=True)
def record(line): log.write(line.strip() + "\n"); log.flush()
for line in sys.stdin:
    record(line)
    message = json.loads(line)
    method, rid = message.get("method"), message.get("id")
    if method == "initialize": emit({"id": rid, "result": {"userAgent": "fake"}})
    elif method == "model/list":
        models = [{"id": "gpt-5.5"}] if mode == "no_model" else [{"id": "gpt-6.1-sol"}, {"id": "gpt-5.5"}]
        emit({"id": rid, "result": {"data": models}})
    elif method == "thread/start":
        params = message["params"]
        model = "gpt-5.5" if mode == "wrong_model" else params["model"]
        emit({"id": rid, "result": {"model": model, "reasoningEffort": params["config"].get("model_reasoning_effort"), "thread": {"id": "thr-1"}}})
    elif method == "turn/start":
        own = {"threadId": "thr-1", "turnId": "turn-1"}
        done = {"method": "turn/completed", "params": {"threadId": "thr-1", "turn": {"id": "turn-1", "status": "failed" if mode == "failed" else "completed"}}}
        answer = {"method": "item/completed", "params": {**own, "item": {"type": "agentMessage", "text": "PONG", "phase": "final_answer"}}}
        if mode == "early":
            emit(answer); emit(done)
        emit({"id": rid, "result": {"turn": {"id": "turn-1", "status": "inProgress"}}})
        if mode == "early":
            continue
        if mode == "foreign":
            emit({"method": "item/completed", "params": {"threadId": "thr-other", "turnId": "turn-1", "item": {"type": "agentMessage", "text": "FOREIGN", "phase": "final_answer"}}})
            emit({"method": "turn/completed", "params": {"threadId": "thr-other", "turn": {"id": "turn-1", "status": "completed"}}})
            emit({"method": "item/completed", "params": {"threadId": "thr-1", "turnId": "turn-0", "item": {"type": "agentMessage", "text": "STALE", "phase": "final_answer"}}})
        if mode == "approval":
            emit({"id": "srv-1", "method": "item/commandExecution/requestApproval", "params": {}})
            record(sys.stdin.readline())
        if mode == "reroute":
            emit({"method": "model/rerouted", "params": {"threadId": "thr-1", "turnId": "turn-1", "fromModel": "gpt-6.1-sol", "toModel": "gpt-5.5", "reason": "capacity"}})
        emit({"method": "item/agentMessage/delta", "params": {**own, "delta": "PO"}})
        emit({"method": "item/completed", "params": {**own, "item": {"type": "agentMessage", "text": "checking the workspace", "phase": "commentary"}}})
        if mode != "commentary":
            emit(answer)
        emit(done)
"#;

async fn run(mode: &str) -> (Result<String, String>, Vec<Value>) {
    let root = tempfile::tempdir().unwrap();
    let peer = root.path().join("peer.py");
    let log = root.path().join("client.jsonl");
    std::fs::write(&peer, PEER).unwrap();
    let spec = super::super::NativeAcpRunSpec {
        managed: false,
        sandbox_cwd: None,
        #[cfg(any(unix, windows))]
        approval: None,
        mcp_servers: vec![
            json!({"type":"http","name":"tachi","url":"http://127.0.0.1:6919/mcp",
            "headers":[{"name":"X-Tachi-Agent-Identity","value":"staff:codex-test"}]}),
        ],
        command: "python3".into(),
        args: vec!["-B".into(), peer.to_string_lossy().into_owned()],
        cwd: root.path().to_path_buf(),
        cwd_authority: None,
        prompt: "Reply PONG".into(),
        mode: super::super::NativeAcpRunMode::Session,
        permission_label: "server-read-only".into(),
        session: None,
        session_record_path: None,
        session_distill_path: None,
        metadata: json!({"profile_contract": {
            "backend": "codex", "dialect": "codex_app_server", "model": "gpt-6.1-sol", "effort": "high",
        }}),
        env: HashMap::from([
            ("FAKE_MODE".into(), mode.into()),
            ("FAKE_LOG".into(), log.to_string_lossy().into_owned()),
        ]),
        env_remove: HashSet::new(),
    };
    let result = super::super::run_native_acp_dispatch(
        spec,
        root.path(),
        &root.path().join("trajectory.jsonl"),
        "codex-test",
        "codex",
        std::time::Duration::from_secs(10),
    )
    .await
    .map(|result| result.output);
    let sent = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (result, sent)
}

fn methods(sent: &[Value]) -> Vec<&str> {
    sent.iter().filter_map(|m| m["method"].as_str()).collect()
}

#[cfg(unix)]
#[tokio::test]
async fn one_turn_pins_identity_and_injects_run_private_config() {
    let (result, sent) = run("ok").await;
    assert_eq!(result.unwrap(), "PONG");
    assert_eq!(
        methods(&sent),
        [
            "initialize",
            "initialized",
            "model/list",
            "thread/start",
            "turn/start"
        ]
    );
    assert!(sent.iter().all(|message| message.get("jsonrpc").is_none()));
    let start = &sent[3]["params"];
    assert_eq!(start["model"], "gpt-6.1-sol");
    assert_eq!(start["sandbox"], "danger-full-access");
    assert_eq!(start["approvalPolicy"], "never");
    assert_eq!(start["config"]["model_reasoning_effort"], "high");
    assert_eq!(
        start["config"]["mcp_servers"]["tachi"],
        json!({"url":"http://127.0.0.1:6919/mcp","http_headers":{"X-Tachi-Agent-Identity":"staff:codex-test"}})
    );
    assert_eq!(sent[4]["params"]["threadId"], "thr-1");
}

#[cfg(unix)]
#[tokio::test]
async fn output_is_bound_to_the_active_thread_turn_and_final_answer() {
    for mode in ["foreign", "early"] {
        let (result, _) = run(mode).await;
        assert_eq!(result.expect(mode), "PONG", "{mode}");
    }
    let (result, _) = run("commentary").await;
    let error = result.expect_err("commentary is not a result");
    assert!(error.contains("no result text"), "{error}");
}

#[cfg(unix)]
#[tokio::test]
async fn server_approval_requests_are_declined() {
    let (result, sent) = run("approval").await;
    assert_eq!(result.unwrap(), "PONG");
    let reply = sent
        .iter()
        .find(|m| m["id"] == "srv-1")
        .expect("approval reply");
    assert_eq!(reply["result"]["decision"], "decline");
}

#[cfg(unix)]
#[tokio::test]
async fn identity_and_terminal_failures_never_fall_back() {
    for (mode, expected, forbidden) in [
        (
            "no_model",
            "does not offer requested model=gpt-6.1-sol",
            "thread/start",
        ),
        (
            "wrong_model",
            "did not acknowledge model=gpt-6.1-sol",
            "turn/start",
        ),
        ("reroute", "model identity mismatch", ""),
        ("failed", "native turn stopped with failed", ""),
    ] {
        let (result, sent) = run(mode).await;
        let error = result.expect_err(mode);
        assert!(error.contains(expected), "{mode}: {error}");
        assert!(
            !methods(&sent).contains(&forbidden),
            "{mode}: {:?}",
            methods(&sent)
        );
        if mode != "failed" {
            assert_eq!(
                super::super::supervised::classify_failure(&error),
                "identity_mismatch",
                "{mode}"
            );
        }
    }
}
