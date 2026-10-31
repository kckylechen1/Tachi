use super::*;

fn task_proxy(
    server: &crate::MemoryServer,
    daemon: crate::cli_client::DaemonInfo,
    app_home: &Path,
) -> StdioProxyServer {
    StdioProxyServer {
        adapter_started_at: chrono::Utc::now(),
        tool_profile: Some(tachi_hub::ToolProfile::coordinate()),
        daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
        app_home: app_home.to_path_buf(),
        global_db_path: server.global_db_path_buf(),
        project_db_path: server.project_db_path_buf(),
        client_project: Some(PROJECT.into()),
        resolved_agent_identity: Default::default(),
        rate_limit_session: ProxyRateLimitSession::mint(),
    }
}

fn rpc(id: i64, method: &str, mut params: serde_json::Value) -> serde_json::Value {
    params["_meta"] = task_meta(OWNER);
    json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params})
}

#[test]
fn tasks_stdio_transport_never_replays_an_ambiguous_update_or_downgrades_discovery() {
    use crate::cli_client::{DaemonTaskRequest, DaemonTaskResponse, TaskForwardIdentity};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let home = tempfile::tempdir().unwrap();
    with_tachi_home(home.path(), || {
        test_runtime().block_on(async {
        for mode in ["tasks", "no-tasks", "legacy-only"] {
            let methods = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let writes = std::sync::Arc::new(AtomicUsize::new(0));
            let seen = methods.clone();
            let applied = writes.clone();
            let router = axum::Router::new().route("/mcp", axum::routing::post(move |axum::Json(request): axum::Json<serde_json::Value>| {
                let seen = seen.clone(); let applied = applied.clone();
                async move {
                    use axum::response::IntoResponse;
                    let method = request["method"].as_str().unwrap().to_string();
                    seen.lock().unwrap().push(method.clone());
                    if method == "server/discover" {
                        let response = if mode == "legacy-only" {
                            json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"method not found"}})
                        } else {
                            let capabilities = if mode == "tasks" {
                                rmcp::model::ServerCapabilities::builder().enable_tools().enable_tasks().build()
                            } else { rmcp::model::ServerCapabilities::builder().enable_tools().build() };
                            let result = rmcp::model::DiscoverResult::from_server_info(
                                vec![rmcp::model::ProtocolVersion::V_2026_07_28],
                                rmcp::model::ServerConfig::new(capabilities),
                            );
                            json!({"jsonrpc":"2.0","id":request["id"],"result":result})
                        };
                        axum::Json(response).into_response()
                    } else {
                        assert_eq!(method, "tasks/update", "must never retry legacy initialize");
                        applied.fetch_add(1, Ordering::SeqCst);
                        // The mutation took effect, but no MCP response survived.
                        (axum::http::StatusCode::SERVICE_UNAVAILABLE, "lost after dispatch").into_response()
                    }
                }
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let serving = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let daemon = crate::cli_client::DaemonInfo {
                url: format!("http://{addr}/mcp"), global_db: None, project_db: None,
                version: Some(env!("CARGO_PKG_VERSION").into()), pid: None, internal_proxy_token: None,
            };
            let identity = || TaskForwardIdentity {
                elicitation_form: false, project: Some(PROJECT), profile: Some(tachi_hub::ToolProfile::coordinate()),
                client: Some("phase-tests"), agent: crate::cli_client::ProxyIdentityForward::Header(OWNER.into()),
                rate_limit_session: "stdio-tasks-phase-tests",
            };
            if mode == "no-tasks" {
                assert!(matches!(crate::cli_client::forward_daemon_task(&daemon, DaemonTaskRequest::Discover, identity()).await,
                    Ok(DaemonTaskResponse::Capability(false))));
                methods.lock().unwrap().clear();
            }
            let request = DaemonTaskRequest::Update(rmcp::model::UpdateTaskParams::new("task-phase-fixture", Default::default()));
            let error = match crate::cli_client::forward_daemon_task(&daemon, request, identity()).await {
                Err(error) => error, Ok(_) => panic!("fixture must refuse the update"),
            };
            assert_eq!(error.before_dispatch, mode != "tasks");
            assert_eq!(writes.load(Ordering::SeqCst), usize::from(mode == "tasks"));
            assert_eq!(&*methods.lock().unwrap(), if mode == "tasks" {
                vec!["server/discover", "tasks/update"]
            } else { vec!["server/discover"] }.as_slice());
            serving.abort(); let _ = serving.await;
        }
    })
    });
}

#[cfg(unix)]
#[test]
fn tasks_stdio_native_acp_elicitation_resumes_the_real_permission_waiter() {
    let home = tempfile::tempdir().expect("home");
    with_tachi_home(home.path(), || {
        let (_runs, _env, server, run) = task_fixture();
        test_runtime().block_on(async {
            let (daemon, stop, daemon_task) = spawn_test_http_daemon(
                std::ops::Deref::deref(&server).clone(), &server.global_db_path_buf(),
            ).await;
            let proxy = task_proxy(&server, daemon, home.path());
            let created = stdio_responses(proxy.clone(), &[
                rpc(1, "tools/call", result_params(task_meta(OWNER))),
            ]).await;
            let handle = created[0]["result"]["taskId"].as_str().unwrap().to_owned();
            let process = tokio::spawn(crate::dispatch_ops::run_native_permission_fixture(
                std::ops::Deref::deref(&server).clone(), run.clone(), DISPATCH.into(),
                std::time::Duration::from_secs(20), false,
            ));
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                while server.native_acp_approvals.accepted_anchor(DISPATCH).is_none() {
                    assert!(!process.is_finished(), "ACP exited before requesting permission");
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            }).await.expect("permission wait");
            let no_form = stdio_responses(proxy.clone(), &[
                rpc(2, "tasks/get", json!({"taskId":handle})),
            ]).await;
            assert!(no_form[0].get("error").is_some(), "elicitation needs the real outer capability");
            let mut get = rpc(3, "tasks/get", json!({"taskId":handle}));
            get["params"]["_meta"][CAPABILITIES]["elicitation"] = json!({"form":{}});
            let pending = stdio_responses(proxy.clone(), &[get]).await;
            assert_eq!(pending[0]["result"]["status"], "input_required", "{pending:#?}");
            let requests = pending[0]["result"]["inputRequests"].as_object().unwrap();
            let (key, input) = requests.iter().next().unwrap();
            assert_eq!(input["method"], "elicitation/create");
            assert!(!run.join("replies.jsonl").exists());
            let mut update = rpc(4, "tasks/update", json!({
                "taskId":handle, "inputResponses":{key:{"resultType":"complete","action":"decline"}}
            }));
            update["params"]["_meta"][CAPABILITIES]["elicitation"] = json!({"form":{}});
            let ack = stdio_responses(proxy, &[update]).await;
            assert_eq!(ack[0]["result"], json!({"resultType":"complete"}), "{ack:#?}");
            assert_eq!(process.await.unwrap().unwrap(), "continued the same process");
            let replies = std::fs::read_to_string(run.join("replies.jsonl")).unwrap();
            assert_eq!(replies.lines().count(), 1);
            let reply: serde_json::Value = serde_json::from_str(replies.lines().next().unwrap()).unwrap();
            assert_eq!(reply["result"]["outcome"]["optionId"], "deny-read-once");
            stop.cancel();
            daemon_task.await.unwrap();
        });
    });
}

#[test]
fn tasks_stdio_forwarding_preserves_handles_across_connections_and_completion() {
    let home = tempfile::tempdir().expect("home");
    with_tachi_home(home.path(), || {
        let (_runs, _env, server, run) = task_fixture();
        test_runtime().block_on(async {
            let (daemon, stop, task) = spawn_test_http_daemon(
                std::ops::Deref::deref(&server).clone(),
                &server.global_db_path_buf(),
            )
            .await;
            let proxy = task_proxy(&server, daemon, home.path());
            let responses = stdio_responses(
                proxy.clone(),
                &[
                    rpc(1, "server/discover", json!({})),
                    rpc(2, "tools/call", result_params(task_meta(OWNER))),
                ],
            )
            .await;
            for response in &responses {
                assert!(response.get("error").is_none(), "{response:#}");
            }
            assert_eq!(
                responses[0]["result"]["capabilities"]["extensions"][EXTENSION],
                json!({})
            );
            assert_eq!(responses[1]["result"]["resultType"], "task");
            let handle = responses[1]["result"]["taskId"]
                .as_str()
                .expect("Task handle")
                .to_owned();
            let before = receipt_bytes(&run);
            let mut requests = vec![rpc(
                3,
                "tasks/update",
                json!({
                    "taskId":handle, "inputResponses":{"unknown":{"resultType":"complete"}}
                }),
            )];
            for id in 4..=15 {
                requests.push(rpc(id, "tasks/get", json!({"taskId":handle})));
            }
            let responses = stdio_responses(proxy.clone(), &requests).await;
            assert_eq!(
                responses[0]["result"],
                json!({"resultType":"complete"}),
                "{responses:#?}"
            );
            for response in &responses[1..] {
                assert!(response.get("error").is_none(), "{response:#}");
                assert_eq!(response["result"]["taskId"], handle);
                assert_eq!(response["result"]["status"], "working");
            }
            assert_eq!(
                receipt_bytes(&run),
                before,
                "polling and unknown input never alter work"
            );

            let responses = stdio_responses(
                proxy.clone(),
                &[rpc(16, "tasks/cancel", json!({"taskId":handle}))],
            )
            .await;
            assert!(responses[0]["error"]["message"]
                .as_str()
                .expect("honest cancel refusal")
                .contains("no current controller"));
            assert_eq!(receipt_bytes(&run), before);

            std::fs::write(run.join("result.md"), "stdio durable result 中文\n").expect("report");
            write_run_status(&run, "TASK_STATE_COMPLETED", 2);
            let responses = stdio_responses(
                proxy.clone(),
                &[
                    rpc(17, "tasks/get", json!({"taskId":handle})),
                    rpc(18, "tasks/cancel", json!({"taskId":handle})),
                ],
            )
            .await;
            assert_eq!(
                responses[0]["result"]["status"], "completed",
                "{responses:#?}"
            );
            assert!(responses[0]["result"]["result"]["content"][0]["text"]
                .as_str()
                .expect("terminal content")
                .contains("stdio durable result"));
            assert_eq!(responses[1]["result"], json!({"resultType":"complete"}));
            let terminal = responses[0]["result"].clone();
            std::fs::write(run.join("result.md"), "later report change").expect("edit report");
            let reconnected =
                stdio_responses(proxy, &[rpc(19, "tasks/get", json!({"taskId":handle}))]).await;
            assert_eq!(
                reconnected[0]["result"], terminal,
                "terminal response is immutable after disconnect"
            );
            stop.cancel();
            task.await.expect("daemon exit");
        });
    });
}

#[test]
fn tasks_stdio_keeps_request_identity_and_capability_boundaries() {
    let home = tempfile::tempdir().expect("home");
    with_tachi_home(home.path(), || {
        let (_runs, _env, server, run) = task_fixture();
        let _no_spawn = EnvRestore::set("TACHI_DISABLE_AUTO_DAEMON", "1");
        let _no_agent = EnvRestore::remove(crate::session_identity::ENV_AGENT_IDENTITY);
        test_runtime().block_on(async {
            let (daemon, stop, task) = spawn_test_http_daemon(
                std::ops::Deref::deref(&server).clone(),
                &server.global_db_path_buf(),
            )
            .await;
            let proxy = task_proxy(&server, daemon, home.path());
            let created = stdio_responses(
                proxy.clone(),
                &[rpc(1, "tools/call", result_params(task_meta(OWNER)))],
            )
            .await;
            let handle = created[0]["result"]["taskId"]
                .as_str()
                .expect("Task handle");
            let before = receipt_bytes(&run);
            let mut foreign = rpc(2, "tasks/get", json!({"taskId":handle}));
            foreign["params"]["_meta"] = task_meta("agent.task.foreign");
            let mut absent = rpc(3, "tasks/get", json!({"taskId":handle}));
            absent["params"]["_meta"]
                .as_object_mut()
                .unwrap()
                .remove("tachiAgentIdentity");
            let mut project = rpc(4, "tasks/get", json!({"taskId":handle}));
            project["params"]["_meta"]["tachiProject"] = json!("other-project");
            let mut profile = rpc(5, "tasks/get", json!({"taskId":handle}));
            profile["params"]["_meta"]["tachiProfile"] = json!("admin");
            let mut no_caps = rpc(6, "tasks/get", json!({"taskId":handle}));
            no_caps["params"]["_meta"][CAPABILITIES] = json!({});
            let responses = stdio_responses(
                proxy.clone(),
                &[
                    foreign,
                    absent,
                    project,
                    profile,
                    no_caps,
                    rpc(7, "tasks/get", json!({"taskId":"not-a-task"})),
                ],
            )
            .await;
            for response in &responses {
                assert!(response.get("error").is_some(), "{response:#}");
            }
            assert_eq!(
                responses[0]["error"], responses[5]["error"],
                "foreign and missing Tasks are non-disclosing"
            );
            assert_eq!(receipt_bytes(&run), before);

            let mut ordinary = rpc(8, "tools/call", result_params(task_meta(OWNER)));
            ordinary["params"]["_meta"][CAPABILITIES] = json!({});
            let responses = stdio_responses(proxy.clone(), &[ordinary]).await;
            assert_eq!(
                responses[0]["result"]["resultType"], "complete",
                "{responses:#?}"
            );
            assert!(responses[0]["result"].get("taskId").is_none());

            let legacy = stdio_responses(
                proxy,
                &[
                    json!({"jsonrpc":"2.0", "id":9, "method":"initialize", "params":{
                        "protocolVersion":"2025-11-25", "capabilities":{},
                        "clientInfo":{"name":"legacy-task-proxy", "version":"1"},
                        "_meta":task_identity(OWNER)
                    }}),
                    json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
                ],
            )
            .await;
            assert!(legacy[0]["result"]["capabilities"]["extensions"]
                .get(EXTENSION)
                .is_none());
            stop.cancel();
            task.await.expect("daemon exit");
        });
    });
}
