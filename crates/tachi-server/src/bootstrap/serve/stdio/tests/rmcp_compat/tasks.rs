//! Tasks conformance through the resident daemon's real HTTP admission path.
use super::*;
use crate::managed_run_control::AnchoredRunStatus;
use std::path::PathBuf;

const PROJECT: &str = "mcp-task-http-project";
const OWNER: &str = "agent.task.http.owner";
const DISPATCH: &str = "20261030T120000Z-task-http";
const EXTENSION: &str = "io.modelcontextprotocol/tasks";
const CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const VERSION: &str = "io.modelcontextprotocol/protocolVersion";

fn task_identity(agent: &str) -> serde_json::Value {
    json!({
        "tachiProject":PROJECT, "tachiAgentIdentity":agent,
        "tachiProfile":"coordinate", "tachiClient":"task-http-tests"
    })
}

fn task_meta(agent: &str) -> serde_json::Value {
    let mut meta = modern_meta(task_identity(agent));
    meta[CAPABILITIES] = json!({"extensions":{EXTENSION:{}}});
    meta
}

fn result_params(meta: serde_json::Value) -> serde_json::Value {
    json!({
        "_meta":meta, "name":"tachi_staff",
        "arguments":{"action":"result", "dispatch_id":DISPATCH, "format":"json"}
    })
}

fn write_run_status(run: &Path, state: &str, revision: u64) {
    let anchor = AnchoredRunStatus::open(run).expect("open fixture run");
    let lock = anchor.lock();
    let _guard = lock.lock().expect("lock fixture run");
    let fence = anchor.acquire_fence().expect("fence fixture run");
    anchor
        .write_atomic(
            &serde_json::to_vec(&json!({
                "dispatch_id":DISPATCH, "state":state, "status_revision":revision,
                "updated_at":chrono::Utc::now().to_rfc3339(), "project":PROJECT
            }))
            .expect("fixture receipt JSON"),
            &fence,
        )
        .expect("write canonical fixture receipt");
}

fn task_fixture() -> (
    tempfile::TempDir,
    EnvRestore,
    crate::tests::TestServer,
    PathBuf,
) {
    let (server, _) = crate::tests::make_server_with_project_fixture(PROJECT);
    let runs = tempfile::tempdir().expect("run root");
    let env = EnvRestore::set_path("TACHI_HOME", runs.path());
    server.set_session_identity(
        Some("task-http-tests".into()),
        Some(PROJECT.into()),
        Some(tachi_hub::ToolProfile::coordinate()),
    );
    crate::claims_ops::admit_agent_connection(&server, Some(OWNER.into()), true)
        .expect("admit fixture run owner");
    let run = runs.path().join("runs").join(DISPATCH);
    std::fs::create_dir_all(&run).expect("fixture run directory");
    crate::mcp_tasks::prepare_run_binding(&server, &run, DISPATCH, Some(PROJECT))
        .expect("persist run owner through production start binding");
    write_run_status(&run, "TASK_STATE_WORKING", 1);
    (runs, env, server, run)
}

fn receipt_bytes(run: &Path) -> (Vec<u8>, Vec<u8>) {
    (
        std::fs::read(run.join("status.json")).expect("canonical status"),
        std::fs::read(run.join("mcp-tasks.json")).expect("result binding"),
    )
}

fn task_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("task HTTP client")
}

async fn task_rpc(
    client: &reqwest::Client,
    url: &str,
    id: i64,
    method: &str,
    params: serde_json::Value,
    overrides: &[(&str, &str)],
) -> (reqwest::StatusCode, serde_json::Value) {
    let mut pairs = vec![
        ("mcp-protocol-version", "2026-07-28"),
        ("mcp-method", method),
    ];
    let name = if method == "tools/call" {
        params["name"].as_str()
    } else if method.starts_with("tasks/") {
        params["taskId"].as_str()
    } else {
        None
    };
    if let Some(name) = name {
        pairs.push(("mcp-name", name));
    }
    for (header, field) in [
        ("x-tachi-project", "tachiProject"),
        ("x-tachi-agent-identity", "tachiAgentIdentity"),
        ("x-tachi-profile", "tachiProfile"),
        ("x-tachi-client", "tachiClient"),
    ] {
        if let Some(value) = params["_meta"][field].as_str() {
            pairs.push((header, value));
        }
    }
    pairs.extend_from_slice(overrides);
    let response = client
        .post(url)
        .headers(http_headers(&pairs))
        .json(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
        .send()
        .await
        .unwrap_or_else(|error| panic!("{method} request: {error}"));
    let status = response.status();
    assert!(
        !response.headers().contains_key("mcp-session-id"),
        "modern Tasks requests must remain stateless"
    );
    let body = response.text().await.expect("task HTTP response body");
    (status, parse_http_mcp_payload(&body, id))
}

fn assert_rpc_ok(status: reqwest::StatusCode, response: &serde_json::Value) {
    assert_eq!(status, reqwest::StatusCode::OK, "{response:#}");
    assert!(response.get("error").is_none(), "{response:#}");
}

#[test]
fn tasks_http_discovery_result_get_and_update_follow_modern_wire_contract() {
    let home = tempfile::tempdir().expect("home");
    with_tachi_home(home.path(), || {
        let (_runs, _env, server, run) = task_fixture();
        test_runtime().block_on(async {
            let (daemon, cancel, task) = spawn_test_http_daemon(
                std::ops::Deref::deref(&server).clone(),
                &server.global_db_path_buf(),
            )
            .await;
            let client = task_client();
            let (status, discovery) = task_rpc(
                &client, &daemon.url, 1, "server/discover",
                json!({"_meta":task_meta(OWNER)}), &[],
            ).await;
            assert_rpc_ok(status, &discovery);
            assert_eq!(discovery["result"]["resultType"], "complete");
            assert_eq!(discovery["result"]["capabilities"]["extensions"][EXTENSION], json!({}));

            let (_, _, legacy) = http_mcp_initialize(
                &daemon.url, http_headers(&[]), Some(task_identity(OWNER)),
            ).await;
            assert!(legacy.get("error").is_none(), "{legacy:#}");
            assert!(legacy["result"]["capabilities"]["extensions"].get(EXTENSION).is_none());

            let before_status = receipt_bytes(&run).0;
            let (status, created) = task_rpc(
                &client, &daemon.url, 2, "tools/call", result_params(task_meta(OWNER)), &[],
            ).await;
            assert_rpc_ok(status, &created);
            let handle = created["result"]["taskId"].as_str().expect("flat taskId").to_owned();
            assert_eq!(created["result"]["resultType"], "task");
            assert_eq!(created["result"]["status"], "working");
            assert!(created["result"].get("task").is_none(), "no legacy nested task envelope");
            assert!(created["result"].get("content").is_none());
            assert!(created["result"]["ttlMs"].as_u64().is_some_and(|ttl| ttl > 0));
            assert!(created["result"]["pollIntervalMs"].as_u64().is_some_and(|interval| interval > 0));
            for field in ["createdAt", "lastUpdatedAt"] {
                chrono::DateTime::parse_from_rfc3339(created["result"][field].as_str().expect(field))
                    .expect("task timestamp");
            }
            assert_eq!(receipt_bytes(&run).0, before_status, "result waiting must not start or alter work");

            let (status, fetched) = task_rpc(
                &client, &daemon.url, 3, "tasks/get",
                json!({"_meta":task_meta(OWNER), "taskId":handle}), &[],
            ).await;
            assert_rpc_ok(status, &fetched);
            assert_eq!(fetched["result"]["resultType"], "complete");
            assert_eq!(fetched["result"]["taskId"], handle);
            assert_eq!(fetched["result"]["status"], "working");
            assert!(fetched["result"].get("task").is_none());
            assert!(fetched["result"].get("inputRequests").is_none());

            let before_update = receipt_bytes(&run);
            let (status, updated) = task_rpc(
                &client, &daemon.url, 4, "tasks/update",
                json!({"_meta":task_meta(OWNER), "taskId":handle,
                    "inputResponses":{"unknown-request":{"resultType":"complete","value":"ignored"}}}),
                &[],
            ).await;
            assert_rpc_ok(status, &updated);
            assert_eq!(updated["result"], json!({"resultType":"complete"}));
            assert_eq!(receipt_bytes(&run), before_update, "unknown input has no delivery owner and must not write");

            std::fs::write(run.join("result.md"), "Canonical HTTP result 中文\n").expect("report");
            write_run_status(&run, "TASK_STATE_COMPLETED", 2);
            let (status, completed) = task_rpc(
                &client, &daemon.url, 5, "tasks/get",
                json!({"_meta":task_meta(OWNER), "taskId":handle}), &[],
            ).await;
            assert_rpc_ok(status, &completed);
            assert_eq!(completed["result"]["resultType"], "complete");
            assert_eq!(completed["result"]["taskId"], handle);
            assert_eq!(completed["result"]["status"], "completed");
            assert!(completed["result"].get("error").is_none());
            let result: serde_json::Value = serde_json::from_str(
                completed["result"]["result"]["content"][0]["text"].as_str().expect("canonical result text")
            ).expect("canonical result JSON");
            assert_eq!(result["dispatch_id"], DISPATCH);
            assert_eq!(result["state"], "TASK_STATE_COMPLETED");
            assert_eq!(result["result"]["body"], "Canonical HTTP result 中文\n");
            let terminal_receipts = receipt_bytes(&run);
            let (status, cancelled) = task_rpc(
                &client, &daemon.url, 6, "tasks/cancel",
                json!({"_meta":task_meta(OWNER), "taskId":handle}), &[],
            ).await;
            assert_rpc_ok(status, &cancelled);
            assert_eq!(cancelled["result"], json!({"resultType":"complete"}));
            assert_eq!(receipt_bytes(&run), terminal_receipts, "terminal cancel is an idempotent acknowledgement");
            cancel.cancel();
            task.await.expect("daemon stopped");
        });
    });
}

#[test]
fn tasks_http_without_negotiation_preserves_modern_and_legacy_normal_results() {
    let home = tempfile::tempdir().expect("home");
    with_tachi_home(home.path(), || {
        let (_runs, _env, server, run) = task_fixture();
        test_runtime().block_on(async {
            let (daemon, cancel, task) = spawn_test_http_daemon(
                std::ops::Deref::deref(&server).clone(),
                &server.global_db_path_buf(),
            )
            .await;
            let before = receipt_bytes(&run);
            let (status, modern) = task_rpc(
                &task_client(),
                &daemon.url,
                1,
                "tools/call",
                result_params(modern_meta(task_identity(OWNER))),
                &[],
            )
            .await;
            assert_rpc_ok(status, &modern);
            assert_eq!(modern["result"]["resultType"], "complete");
            assert!(modern["result"]["content"].is_array(), "{modern:#}");
            assert!(modern["result"].get("taskId").is_none());
            assert_ne!(modern["result"]["isError"], true);
            let result: serde_json::Value =
                serde_json::from_str(&http_tool_text(&modern)).expect("ordinary result JSON");
            assert_eq!(result["dispatch_id"], DISPATCH);
            assert_eq!(result["state"], "TASK_STATE_WORKING");

            let (client, headers, initialized) =
                http_mcp_initialize(&daemon.url, http_headers(&[]), Some(task_identity(OWNER)))
                    .await;
            assert!(initialized.get("error").is_none(), "{initialized:#}");
            http_mcp_initialized(&client, &daemon.url, headers.clone()).await;
            let legacy = http_mcp_call_tool(
                &client,
                &daemon.url,
                headers,
                2,
                "tachi_staff",
                result_params(json!({}))["arguments"]
                    .as_object()
                    .expect("arguments")
                    .clone(),
            )
            .await;
            assert!(legacy.get("error").is_none(), "{legacy:#}");
            assert!(
                legacy["result"].get("resultType").is_none(),
                "legacy wire shape must remain legacy"
            );
            assert!(legacy["result"].get("taskId").is_none());
            assert_eq!(legacy["result"]["content"], modern["result"]["content"]);
            assert_eq!(
                receipt_bytes(&run),
                before,
                "ordinary result reads must not allocate a task"
            );
            cancel.cancel();
            task.await.expect("daemon stopped");
        });
    });
}

#[test]
fn tasks_http_rejects_missing_negotiation_route_conflicts_and_foreign_principals() {
    let home = tempfile::tempdir().expect("home");
    with_tachi_home(home.path(), || {
        let (_runs, _env, server, run) = task_fixture();
        test_runtime().block_on(async {
            let (daemon, cancel, task) = spawn_test_http_daemon(
                std::ops::Deref::deref(&server).clone(),
                &server.global_db_path_buf(),
            )
            .await;
            let client = task_client();
            let (status, created) = task_rpc(
                &client,
                &daemon.url,
                1,
                "tools/call",
                result_params(task_meta(OWNER)),
                &[],
            )
            .await;
            assert_rpc_ok(status, &created);
            let handle = created["result"]["taskId"]
                .as_str()
                .expect("taskId")
                .to_owned();
            let before = receipt_bytes(&run);
            for (id, missing) in [(2, VERSION), (3, CAPABILITIES)] {
                let mut meta = task_meta(OWNER);
                meta.as_object_mut().expect("metadata").remove(missing);
                let (status, denied) = task_rpc(
                    &client,
                    &daemon.url,
                    id,
                    "tasks/get",
                    json!({"_meta":meta, "taskId":handle}),
                    &[],
                )
                .await;
                assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{denied:#}");
                assert_eq!(denied["error"]["code"], -32602, "{denied:#}");
            }
            for (id, method, mut params) in [
                (4, "tasks/get", json!({"taskId":handle})),
                (
                    5,
                    "tasks/update",
                    json!({"taskId":handle,"inputResponses":{}}),
                ),
                (6, "tasks/cancel", json!({"taskId":handle})),
            ] {
                params["_meta"] = modern_meta(task_identity(OWNER));
                let (_, denied) = task_rpc(&client, &daemon.url, id, method, params, &[]).await;
                assert_eq!(denied["error"]["code"], -32021, "{method}: {denied:#}");
                assert!(denied.get("result").is_none());
            }
            for (id, header, wrong) in [
                (7, "mcp-name", "another-task"),
                (8, "mcp-method", "tasks/update"),
                (9, "mcp-protocol-version", "2025-11-25"),
                (10, "x-tachi-agent-identity", "agent.header.other"),
                (11, "x-tachi-project", "another-project"),
            ] {
                let (status, denied) = task_rpc(
                    &client,
                    &daemon.url,
                    id,
                    "tasks/get",
                    json!({"_meta":task_meta(OWNER), "taskId":handle}),
                    &[(header, wrong)],
                )
                .await;
                assert_eq!(
                    status,
                    reqwest::StatusCode::BAD_REQUEST,
                    "{header}: {denied:#}"
                );
                assert_eq!(denied["error"]["code"], -32020, "{header}: {denied:#}");
            }

            let foreign = "agent.task.http.other";
            let (_, unknown) = task_rpc(
                &client,
                &daemon.url,
                12,
                "tasks/get",
                json!({"_meta":task_meta(foreign), "taskId":"unknown-task"}),
                &[],
            )
            .await;
            assert_eq!(unknown["error"]["code"], -32602, "{unknown:#}");
            for (id, method, params) in [
                (13, "tools/call", result_params(task_meta(foreign))),
                (
                    14,
                    "tasks/get",
                    json!({"_meta":task_meta(foreign),"taskId":handle}),
                ),
                (
                    15,
                    "tasks/update",
                    json!({"_meta":task_meta(foreign),"taskId":handle,"inputResponses":{}}),
                ),
                (
                    16,
                    "tasks/cancel",
                    json!({"_meta":task_meta(foreign),"taskId":handle}),
                ),
            ] {
                let (_, denied) = task_rpc(&client, &daemon.url, id, method, params, &[]).await;
                assert_eq!(
                    denied["error"], unknown["error"],
                    "{method} must not disclose another principal's task"
                );
                assert!(denied.get("result").is_none());
            }
            assert_eq!(
                receipt_bytes(&run),
                before,
                "rejected requests must not change canonical or task receipts"
            );
            cancel.cancel();
            task.await.expect("daemon stopped");
        });
    });
}

#[test]
fn tasks_http_polling_preserves_rpm_and_strict_mutation_limits() {
    let home = tempfile::tempdir().expect("home");
    with_tachi_home(home.path(), || {
        let (_runs, _env, server, run) = task_fixture();
        test_runtime().block_on(async {
            let (daemon, stop, task) = spawn_test_http_daemon(
                std::ops::Deref::deref(&server).clone(),
                &server.global_db_path_buf(),
            )
            .await;
            let client = task_client();
            let (status, created) = task_rpc(
                &client,
                &daemon.url,
                1,
                "tools/call",
                result_params(task_meta(OWNER)),
                &[],
            )
            .await;
            assert_rpc_ok(status, &created);
            let handle = created["result"]["taskId"].as_str().unwrap().to_owned();
            {
                let mut limiter = server.rate_limiter_lock();
                limiter.rpm = 32;
                limiter.burst = 8;
                limiter.windows.clear();
                limiter.bursts.clear();
            }
            // Identity, client and project remain identical across every HTTP
            // request, including calls after the default loop threshold.
            for id in 1..=20 {
                if id == 12 {
                    write_run_status(&run, "TASK_STATE_COMPLETED", 2);
                }
                let (status, response) = task_rpc(
                    &client,
                    &daemon.url,
                    id,
                    "tasks/get",
                    json!({"_meta":task_meta(OWNER), "taskId":handle}),
                    &[],
                )
                .await;
                assert_rpc_ok(status, &response);
                assert_eq!(
                    response["result"]["status"],
                    if id < 12 { "working" } else { "completed" }
                );
            }
            {
                let mut limiter = server.rate_limiter_lock();
                assert_eq!(
                    limiter.windows.len(),
                    1,
                    "all requests share one admitted identity bucket"
                );
                assert_eq!(limiter.windows.values().next().unwrap().len(), 20);
                assert!(
                    limiter.bursts.is_empty(),
                    "only valid get calls receive polling treatment"
                );
                limiter.windows.clear();
                limiter.rpm = 3;
                limiter.burst = 1;
            }
            for id in 1..=4 {
                let (status, response) = task_rpc(
                    &client,
                    &daemon.url,
                    id,
                    "tasks/get",
                    json!({"_meta":task_meta(OWNER), "taskId":handle}),
                    &[],
                )
                .await;
                if id <= 3 {
                    assert_rpc_ok(status, &response);
                } else {
                    assert!(
                        response["error"]["message"]
                            .as_str()
                            .unwrap()
                            .contains("Rate limited"),
                        "{response:#}"
                    );
                }
            }
            {
                let mut limiter = server.rate_limiter_lock();
                limiter.windows.clear();
                limiter.bursts.clear();
                limiter.rpm = 100;
                limiter.burst = 8;
            }
            let before = receipt_bytes(&run);
            for (method, id) in [
                ("tasks/update", handle.as_str()),
                ("tasks/cancel", handle.as_str()),
                ("tasks/get", "not-a-task"),
            ] {
                for attempt in 1..=9 {
                    let mut params = json!({"_meta":task_meta(OWNER), "taskId":id});
                    if method == "tasks/update" {
                        params["inputResponses"] = json!({});
                    }
                    let (_, response) =
                        task_rpc(&client, &daemon.url, attempt, method, params, &[]).await;
                    if attempt == 9 {
                        assert!(
                            response["error"]["message"]
                                .as_str()
                                .unwrap()
                                .contains("Loop detected"),
                            "{method}: {response:#}"
                        );
                    } else if method == "tasks/get" {
                        assert_eq!(response["error"]["message"], "task unavailable");
                    } else {
                        assert_eq!(response["result"], json!({"resultType":"complete"}));
                    }
                }
            }
            assert_eq!(
                receipt_bytes(&run),
                before,
                "rate-limit checks cannot alter work or final results"
            );
            stop.cancel();
            task.await.expect("daemon stopped");
        });
    });
}
