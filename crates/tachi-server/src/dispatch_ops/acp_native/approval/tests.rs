use super::*;
use crate::test_support::EnvRestore;
use rmcp::model::{CallToolRequestParams, CallToolResponse, TaskPayload};
use std::path::PathBuf;

const PROJECT: &str = "native-permission-project";
const DISPATCH: &str = "20261030T130000Z-native-permission";

fn fixture() -> (
    tempfile::TempDir,
    EnvRestore,
    crate::tests::TestServer,
    PathBuf,
) {
    let (server, _) = crate::tests::make_server_with_project_fixture(PROJECT);
    let runs = tempfile::tempdir().unwrap();
    let env = EnvRestore::set_path("TACHI_HOME", runs.path());
    server.set_session_identity(
        Some("native-input-tests".into()),
        Some(PROJECT.into()),
        Some(tachi_hub::ToolProfile::coordinate()),
    );
    crate::claims_ops::admit_agent_connection(&server, Some("agent.native.input".into()), true)
        .unwrap();
    let run = runs.path().join("runs").join(DISPATCH);
    std::fs::create_dir_all(&run).unwrap();
    crate::mcp_tasks::prepare_run_binding(&server, &run, DISPATCH, Some(PROJECT)).unwrap();
    let anchor = AnchoredRunStatus::open(&run).unwrap();
    let lock = anchor.lock();
    let _lock = lock.lock().unwrap();
    let fence = anchor.acquire_fence().unwrap();
    write_status(&anchor, &json!({"dispatch_id":DISPATCH,"state":"TASK_STATE_WORKING","status_revision":1,"project":PROJECT}), &fence).unwrap();
    (runs, env, server, run)
}

fn context(server: &MemoryServer) -> Arc<NativeAcpApprovalContext> {
    let _mode = EnvRestore::set("TACHI_ACP_APPROVAL_MODE", "interactive");
    NativeAcpApprovalContext::from_environment(
        server,
        &tachi_params::ExecutionGrant::new("fixed-read-grant"),
    )
    .unwrap()
    .unwrap()
}

fn task(server: &MemoryServer) -> String {
    let request = CallToolRequestParams::new("tachi_staff").with_arguments(
        json!({"action":"result","dispatch_id":DISPATCH,"format":"json"})
            .as_object()
            .unwrap()
            .clone(),
    );
    let CallToolResponse::Task(result) = crate::mcp_tasks::begin_result(server, &request).unwrap()
    else {
        panic!("Task handle");
    };
    result.task.task_id
}

fn read_request() -> Value {
    json!({"toolCall":{"toolCallId":"read-1","kind":"read","title":"Read the fixture"},"options":[
        {"optionId":"allow-read-once","kind":"allow_once","name":"Read once"},
        {"optionId":"deny-read-once","kind":"reject_once","name":"Deny"}
    ]})
}

async fn pending_key(server: &MemoryServer, task: &str) -> String {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let result = crate::mcp_tasks::get(server, task).unwrap();
            if let TaskPayload::InputRequired { input_requests } = result.task.payload {
                assert_eq!(input_requests.len(), 1);
                let (key, request) = input_requests.into_iter().next().unwrap();
                let wire = serde_json::to_value(request).unwrap();
                assert_eq!(wire["method"], "elicitation/create");
                assert_eq!(
                    wire["params"]["requestedSchema"]["properties"]["approve"]["type"],
                    "boolean"
                );
                return key;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("live permission waiter")
}

fn answer(key: &str, approve: bool) -> InputResponses {
    InputResponses::from([(
        key.to_string(),
        json!({"resultType":"complete","action":"accept","content":{"approve":approve}}),
    )])
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn native_permission_reply_refuses_a_regressed_canonical_receipt() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_home, _env, server, run) = fixture();
    let task_id = task(&server);
    let origin = context(&server);
    runtime().block_on(async {
        let path = run.clone();
        let waiting =
            tokio::spawn(async move { origin.decide(&path, DISPATCH, &read_request()).await });
        let key = pending_key(&server, &task_id).await;
        let before = std::fs::read(run.join("status.json")).unwrap();
        let status: Value = serde_json::from_slice(&before).unwrap();
        let anchor = server
            .native_acp_approvals
            .accepted_anchor(DISPATCH)
            .unwrap();
        {
            let lock = anchor.lock();
            let _guard = lock.lock().unwrap();
            let fence = anchor.acquire_fence().unwrap();
            let mut binding: Value =
                serde_json::from_str(&anchor.read_mcp_binding().unwrap().unwrap()).unwrap();
            // The result observer has already seen a newer canonical receipt.
            binding["result"]["observed_revision"] =
                json!(status["status_revision"].as_u64().unwrap() + 1);
            anchor
                .write_mcp_binding(&serde_json::to_vec(&binding).unwrap(), &fence)
                .unwrap();
        }
        assert!(crate::mcp_tasks::update(&server, &task_id, &answer(&key, true)).is_err());
        assert_eq!(std::fs::read(run.join("status.json")).unwrap(), before);
        assert!(
            !waiting.is_finished(),
            "a regressed receipt cannot release the child"
        );
        waiting.abort();
        assert!(waiting.await.unwrap_err().is_cancelled());
    });
}

#[test]
fn native_permission_input_is_one_shot_revision_bound_and_not_recovered_from_disk() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_home, _env, server, run) = fixture();
    let task_id = task(&server);
    let origin_context = context(&server);
    runtime().block_on(async {
        let path = run.clone();
        let waiting = tokio::spawn(async move {
            origin_context
                .decide(&path, DISPATCH, &read_request())
                .await
        });
        let key = pending_key(&server, &task_id).await;
        let before = std::fs::read(run.join("status.json")).unwrap();
        crate::mcp_tasks::update(&server, &task_id, &answer("unknown", true)).unwrap();
        assert_eq!(std::fs::read(run.join("status.json")).unwrap(), before);
        assert!(!waiting.is_finished());
        let observer = server.clone_for_mcp_session();
        observer.set_session_identity(
            Some("observer".into()),
            Some(PROJECT.into()),
            Some(tachi_hub::ToolProfile::delegate()),
        );
        crate::claims_ops::admit_agent_connection(
            &observer,
            Some("agent.native.input".into()),
            true,
        )
        .unwrap();
        assert!(crate::mcp_tasks::update(&observer, &task_id, &answer(&key, true)).is_err());
        assert_eq!(
            std::fs::read(run.join("status.json")).unwrap(),
            before,
            "result access does not grant permission to resume work"
        );
        let mut malformed = answer(&key, true);
        malformed.get_mut(&key).unwrap()["content"]["permission_profile"] = json!("write-all");
        assert!(crate::mcp_tasks::update(&server, &task_id, &malformed).is_err());
        assert_eq!(std::fs::read(run.join("status.json")).unwrap(), before);

        let mut restarted = std::ops::Deref::deref(&server).clone();
        restarted.controller_epoch = "another-daemon".into();
        restarted.native_acp_approvals = Arc::new(NativeAcpApprovals::default());
        let historical = crate::mcp_tasks::get(&restarted, &task_id).unwrap();
        assert_eq!(historical.task.status(), rmcp::model::TaskStatus::Working);
        crate::mcp_tasks::update(&restarted, &task_id, &answer(&key, true)).unwrap();
        assert!(
            !waiting.is_finished(),
            "persisted input cannot recreate a control channel"
        );
        assert_eq!(std::fs::read(run.join("status.json")).unwrap(), before);

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut callers = Vec::new();
        for _ in 0..2 {
            let caller = std::ops::Deref::deref(&server).clone();
            let task_id = task_id.clone();
            let replies = answer(&key, true);
            let barrier = barrier.clone();
            callers.push(tokio::task::spawn_blocking(move || {
                barrier.wait();
                crate::mcp_tasks::update(&caller, &task_id, &replies)
            }));
        }
        for caller in callers {
            caller.await.unwrap().unwrap();
        }
        assert!(waiting.await.unwrap().unwrap());
        let after = std::fs::read(run.join("status.json")).unwrap();
        let record: Value = serde_json::from_slice(&after).unwrap();
        assert_eq!(record["status_revision"], 3);
        assert_eq!(record[FIELD]["decision"]["event"], "acp_permission_decided");
        assert_eq!(record[FIELD]["decision"]["approved"], true);
        assert_eq!(record[HISTORY].as_array().unwrap().len(), 1);
        crate::mcp_tasks::update(&server, &task_id, &answer(&key, false)).unwrap();
        assert_eq!(std::fs::read(run.join("status.json")).unwrap(), after);
        assert!(server
            .native_acp_approvals
            .accepted_anchor(DISPATCH)
            .is_none());
        let next_context = context(&server);
        let path = run.clone();
        let next =
            tokio::spawn(
                async move { next_context.decide(&path, DISPATCH, &read_request()).await },
            );
        let next_key = pending_key(&server, &task_id).await;
        assert_ne!(next_key, key);
        let pending = std::fs::read(run.join("status.json")).unwrap();
        crate::mcp_tasks::update(&server, &task_id, &answer(&key, true)).unwrap();
        assert_eq!(std::fs::read(run.join("status.json")).unwrap(), pending);
        assert!(!next.is_finished());
        crate::mcp_tasks::update(&server, &task_id, &answer(&next_key, false)).unwrap();
        assert!(!next.await.unwrap().unwrap());
        let decided: Value =
            serde_json::from_slice(&std::fs::read(run.join("status.json")).unwrap()).unwrap();
        assert_eq!(decided[HISTORY].as_array().unwrap().len(), 2);
        assert_eq!(decided[HISTORY][0], record[HISTORY][0]);
        crate::dispatch_ops::write_status_json(
            &run,
            DISPATCH,
            false,
            None,
            None,
            "not_required",
            Some(0),
            None,
            None,
            None,
            Some(json!({"state":"TASK_STATE_COMPLETED", FIELD:null, HISTORY:[]})),
        );
        let terminal: Value =
            serde_json::from_slice(&std::fs::read(run.join("status.json")).unwrap()).unwrap();
        assert_eq!(terminal[FIELD], decided[FIELD]);
        assert_eq!(
            terminal[HISTORY], decided[HISTORY],
            "terminal writer must retain the decisions unchanged"
        );
    });
}

#[test]
fn native_permission_input_refuses_foreign_owner_revocation_and_superseded_state() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_home, _env, server, run) = fixture();
    let task_id = task(&server);
    let origin = context(&server);
    runtime().block_on(async {
        let path = run.clone();
        let origin_connection = origin.admission.connection.clone();
        let waiting =
            tokio::spawn(async move { origin.decide(&path, DISPATCH, &read_request()).await });
        let key = pending_key(&server, &task_id).await;
        let before = std::fs::read(run.join("status.json")).unwrap();
        let foreign = server.clone_for_mcp_session();
        foreign.set_session_identity(
            Some("foreign".into()),
            Some(PROJECT.into()),
            Some(tachi_hub::ToolProfile::coordinate()),
        );
        crate::claims_ops::admit_agent_connection(&foreign, Some("other-owner".into()), true)
            .unwrap();
        assert!(crate::mcp_tasks::update(&foreign, &task_id, &answer(&key, true)).is_err());
        assert_eq!(std::fs::read(run.join("status.json")).unwrap(), before);

        // A new client connection cannot revive the original admission after
        // its durable row is no longer current.
        let reconnect = server.clone_for_mcp_session();
        reconnect.set_session_identity(
            Some("reconnect".into()),
            Some(PROJECT.into()),
            Some(tachi_hub::ToolProfile::coordinate()),
        );
        crate::claims_ops::admit_agent_connection(
            &reconnect,
            Some("agent.native.input".into()),
            true,
        )
        .unwrap();
        server
            .with_global_store(|store| {
                store
                    .connection()
                    .execute(
                        "UPDATE identity_admissions SET state='unavailable' WHERE connection_id=?1",
                        [&origin_connection],
                    )
                    .map_err(|e| e.to_string())?;
                Ok(())
            })
            .unwrap();
        assert!(crate::mcp_tasks::update(&reconnect, &task_id, &answer(&key, true)).is_err());
        assert!(!waiting.is_finished());
        assert_eq!(std::fs::read(run.join("status.json")).unwrap(), before);

        let anchor = server
            .native_acp_approvals
            .accepted_anchor(DISPATCH)
            .unwrap();
        {
            let lock = anchor.lock();
            let _lock = lock.lock().unwrap();
            let fence = anchor.acquire_fence().unwrap();
            let mut status = read_status(&anchor).unwrap();
            advance(&mut status).unwrap();
            status["state"] = json!("TASK_STATE_CANCELED");
            write_status(&anchor, &status, &fence).unwrap();
        }
        let terminal = std::fs::read(run.join("status.json")).unwrap();
        crate::mcp_tasks::update(&reconnect, &task_id, &answer(&key, true)).unwrap();
        assert_eq!(std::fs::read(run.join("status.json")).unwrap(), terminal);
        assert!(!tokio::time::timeout(Duration::from_secs(3), waiting)
            .await
            .expect("terminal winner withdraws the owner")
            .unwrap()
            .unwrap());
        assert_eq!(
            std::fs::read(run.join("status.json")).unwrap(),
            terminal,
            "withdrawal preserves a terminal winner"
        );
    });
}

#[cfg(unix)]
pub(crate) async fn run_native_permission_fixture(
    server: MemoryServer,
    run: PathBuf,
    dispatch: String,
    timeout: Duration,
    defer: bool,
) -> Result<String, String> {
    let adapter = run.join("permission-adapter.sh");
    let request = read_request();
    std::fs::write(&adapter, format!(r#"#!/bin/sh
IFS= read -r _
printf '%s\n' '{{"jsonrpc":"2.0","id":"tachi-acp-1","result":{{"protocolVersion":1}}}}'
IFS= read -r _
printf '%s\n' '{{"jsonrpc":"2.0","id":"tachi-acp-2","result":{{"sessionId":"live-1"}}}}'
IFS= read -r _
printf '%s\n' '{{"jsonrpc":"2.0","id":"permission-1","method":"session/request_permission","params":{request}}}'
IFS= read -r answer
printf '%s\n' "$answer" >>"$REPLY_LOG"
printf '%s\n' '{{"jsonrpc":"2.0","id":"tachi-acp-3","result":{{"content":[{{"type":"text","text":"continued the same process"}}]}}}}'
"#)).unwrap();
    let replies = run.join("replies.jsonl");
    let spec = super::super::NativeAcpRunSpec {
        approval: Some(context(&server)),
        command: "/bin/sh".into(),
        args: vec![adapter.to_string_lossy().into_owned()],
        cwd: run.clone(),
        cwd_authority: None,
        prompt: "read test".into(),
        mode: super::super::NativeAcpRunMode::OneShot,
        permission_label: "approve-reads".into(),
        session: None,
        session_record_path: None,
        session_distill_path: None,
        metadata: json!({}),
        env: HashMap::from([("REPLY_LOG".into(), replies.to_string_lossy().into_owned())]),
        env_remove: Default::default(),
    };
    super::super::run_native_acp_dispatch_with_liveness(
        spec,
        &run,
        &run.join("trajectory.jsonl"),
        &dispatch,
        "custom",
        timeout,
        defer,
    )
    .await
    .result
    .map(|result| result.output)
}

#[cfg(unix)]
#[test]
fn native_permission_real_acp_process_waits_for_input_and_receives_exactly_one_reply() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_home, _env, server, run) = fixture();
    let task_id = task(&server);
    let replies = run.join("replies.jsonl");
    runtime().block_on(async {
        let path = run.clone();
        let process = tokio::spawn(run_native_permission_fixture(
            std::ops::Deref::deref(&server).clone(),
            path,
            DISPATCH.into(),
            Duration::from_secs(15),
            false,
        ));
        let key = pending_key(&server, &task_id).await;
        assert!(
            !replies.exists(),
            "ACP must not auto-approve while awaiting input"
        );
        let reconnect = server.clone_for_mcp_session();
        reconnect.set_session_identity(
            Some("reconnected-client".into()),
            Some(PROJECT.into()),
            Some(tachi_hub::ToolProfile::coordinate()),
        );
        crate::claims_ops::admit_agent_connection(
            &reconnect,
            Some("agent.native.input".into()),
            true,
        )
        .unwrap();
        crate::mcp_tasks::update(&reconnect, &task_id, &answer(&key, true)).unwrap();
        let result = process.await.unwrap().unwrap();
        assert_eq!(result, "continued the same process");
        crate::mcp_tasks::update(&reconnect, &task_id, &answer(&key, true)).unwrap();
        let replies = std::fs::read_to_string(replies).unwrap();
        assert_eq!(replies.lines().count(), 1);
        let reply: Value = serde_json::from_str(replies.lines().next().unwrap()).unwrap();
        assert_eq!(reply["id"], "permission-1");
        assert_eq!(reply["result"]["outcome"]["optionId"], "allow-read-once");
        let trajectory = std::fs::read_to_string(run.join("trajectory.jsonl")).unwrap();
        assert_eq!(
            trajectory
                .lines()
                .filter(|line| line.contains("acp_native_permission_receipt"))
                .count(),
            1
        );
    });
}

#[test]
fn native_interactive_permission_cannot_expand_policy_or_select_allow_always() {
    use super::super::permission::{
        interactive_permission_decision, interactive_permission_eligible,
    };
    for kind in ["edit", "execute", "unknown"] {
        let mut request = read_request();
        request["toolCall"]["kind"] = json!(kind);
        assert!(!interactive_permission_eligible("approve-reads", &request));
        assert!(!interactive_permission_decision("approve-reads", &request, true).allowed);
    }
    let mut always = read_request();
    always["options"][0]["kind"] = json!("allow_always");
    assert!(!interactive_permission_decision("approve-reads", &always, true).allowed);
    let mut conflicting = read_request();
    conflicting["kind"] = json!("edit");
    assert!(!interactive_permission_decision("approve-reads", &conflicting, true).allowed);
    let mut duplicate = read_request();
    duplicate["options"][1]["optionId"] = json!("allow-read-once");
    assert!(!interactive_permission_decision("approve-reads", &duplicate, true).allowed);
    assert!(!interactive_permission_decision("other-policy", &read_request(), true).allowed);
    assert_eq!(
        interactive_permission_decision("approve-reads", &read_request(), false).response
            ["outcome"]["optionId"],
        "deny-read-once"
    );
}

#[test]
fn native_permission_owner_reissues_after_route_metadata_without_accepting_the_stale_key() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_home, _env, server, run) = fixture();
    let task_id = task(&server);
    let origin = context(&server);
    runtime().block_on(async {
        let path = run.clone();
        let waiting =
            tokio::spawn(async move { origin.decide(&path, DISPATCH, &read_request()).await });
        let key = pending_key(&server, &task_id).await;
        crate::dispatch_ops::stamp_route_decision_id(&run, "late-route-receipt").unwrap();
        let changed = std::fs::read(run.join("status.json")).unwrap();
        crate::mcp_tasks::update(&server, &task_id, &answer(&key, true)).unwrap();
        assert_eq!(std::fs::read(run.join("status.json")).unwrap(), changed);
        let renewed = pending_key(&server, &task_id).await;
        assert_ne!(renewed, key);
        assert!(!waiting.is_finished());
        let pending = std::fs::read(run.join("status.json")).unwrap();
        crate::mcp_tasks::update(&server, &task_id, &answer(&key, true)).unwrap();
        assert_eq!(std::fs::read(run.join("status.json")).unwrap(), pending);
        crate::mcp_tasks::update(&server, &task_id, &answer(&renewed, true)).unwrap();
        assert!(waiting.await.unwrap().unwrap());
        let record: Value =
            serde_json::from_slice(&std::fs::read(run.join("status.json")).unwrap()).unwrap();
        assert_eq!(record["route_decision_id"], "late-route-receipt");
        assert_eq!(record[HISTORY].as_array().unwrap().len(), 1);
        assert_eq!(record[HISTORY][0]["request_id"], renewed);
    });
}

#[cfg(unix)]
#[test]
fn native_permission_runner_timeout_withdraws_input_without_releasing_the_child() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_home, _env, server, run) = fixture();
    let task_id = task(&server);
    runtime().block_on(async {
        let process = tokio::spawn(run_native_permission_fixture(
            std::ops::Deref::deref(&server).clone(),
            run.clone(),
            DISPATCH.into(),
            Duration::from_secs(2),
            false,
        ));
        let key = pending_key(&server, &task_id).await;
        let result = process.await.unwrap();
        assert!(result.unwrap_err().contains("timed out"));
        assert!(server
            .native_acp_approvals
            .accepted_anchor(DISPATCH)
            .is_none());
        let status = std::fs::read(run.join("status.json")).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&status).unwrap()[FIELD]["decision"]["event"],
            "acp_permission_withdrawn"
        );
        crate::mcp_tasks::update(&server, &task_id, &answer(&key, true)).unwrap();
        assert_eq!(std::fs::read(run.join("status.json")).unwrap(), status);
        if let Ok(replies) = std::fs::read_to_string(run.join("replies.jsonl")) {
            assert!(
                replies.trim().is_empty(),
                "EOF cannot be mistaken for approval"
            );
        }
    });
}

#[cfg(unix)]
#[test]
fn native_permission_required_postflight_denies_without_publishing_pending_worker_input() {
    let _serial = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_home, _env, server, run) = fixture();
    runtime().block_on(async {
        run_native_permission_fixture(
            std::ops::Deref::deref(&server).clone(),
            run.clone(),
            DISPATCH.into(),
            Duration::from_secs(5),
            true,
        )
        .await
        .expect("bounded ACP run");
    });
    assert!(server
        .native_acp_approvals
        .accepted_anchor(DISPATCH)
        .is_none());
    let status: Value =
        serde_json::from_slice(&std::fs::read(run.join("status.json")).unwrap()).unwrap();
    assert!(status.get(FIELD).is_none());
    let replies = std::fs::read_to_string(run.join("replies.jsonl")).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(replies.trim()).unwrap()["result"]["outcome"]["optionId"],
        "deny-read-once"
    );
    assert!(!run.join(super::super::ACP_STREAM_FILE).exists());
    let trajectory = std::fs::read_to_string(run.join("trajectory.jsonl")).unwrap_or_default();
    assert!(!trajectory.contains("Read the fixture"));
    assert!(!trajectory.contains("acp_native_permission_receipt"));
}
