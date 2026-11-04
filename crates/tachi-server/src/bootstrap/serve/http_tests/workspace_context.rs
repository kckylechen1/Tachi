//! ZCode host metadata routes calls without mutating the shared connection.
use super::*;
use serde_json::{json, Value};

struct Caller {
    client: reqwest::Client,
    headers: reqwest::header::HeaderMap,
    url: String,
    modern: bool,
}

impl Caller {
    async fn new(url: &str, modern: bool, project: Option<&str>, identity: Option<&str>) -> Self {
        let mut headers =
            http_headers(&[("x-tachi-client", "zcode"), ("x-tachi-profile", "standard")]);
        if let Some(project) = project {
            headers.insert("x-tachi-project", project.parse().unwrap());
        }
        if let Some(identity) = identity {
            headers.insert("x-tachi-agent-identity", identity.parse().unwrap());
        }
        let client = if modern {
            reqwest::Client::new()
        } else {
            let (client, session_headers, init) = http_mcp_initialize(url, headers, None).await;
            assert!(init.get("error").is_none(), "{init}");
            headers = session_headers;
            http_mcp_initialized(&client, url, headers.clone()).await;
            client
        };
        Self {
            client,
            headers,
            url: url.into(),
            modern,
        }
    }

    async fn call(&self, id: i64, tool: &str, args: Value, context: Option<Value>) -> Value {
        let mut headers = self.headers.clone();
        let mut meta = json!({});
        if self.modern {
            headers.insert("mcp-protocol-version", "2026-07-28".parse().unwrap());
            headers.insert("mcp-method", "tools/call".parse().unwrap());
            headers.insert("mcp-name", tool.parse().unwrap());
            meta = json!({
                "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                "io.modelcontextprotocol/clientCapabilities":{},
                "io.modelcontextprotocol/clientInfo":{"name":"zcode","version":"3.17.2"}
            });
        }
        if let Some(context) = context {
            meta["com.zcode/request-context"] = context;
        }
        let body = self
            .client
            .post(&self.url)
            .headers(headers)
            .json(&json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                "params":{"name":tool,"arguments":args,"_meta":meta}}))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        parse_http_mcp_payload(&body, id)
    }
}

fn payload(envelope: &Value) -> Value {
    assert!(envelope.get("error").is_none(), "{envelope:#}");
    assert_ne!(envelope["result"]["isError"], true, "{envelope:#}");
    serde_json::from_str(&http_tool_text(envelope)).unwrap()
}

fn assert_denied(envelope: Value) {
    assert!(
        envelope.get("error").is_some() || envelope["result"]["isError"] == true,
        "{envelope:#}"
    );
}

#[test]
fn http_zcode_workspace_context_isolates_default_memory_and_preserves_unbound_session() {
    let temp = tempfile::tempdir().unwrap();
    with_tachi_home(temp.path(), || {
        let _embedding = EnvRestore::set("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", "1");
        let global = temp.path().join("global/memory.db");
        let a = temp.path().join("Sigil");
        let b = temp.path().join("Quant");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).unwrap();
            let observer = server.clone();
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            for modern in [false, true] {
                let first = Caller::new(&daemon.url, modern, None, None).await;
                let second = Caller::new(&daemon.url, modern, None, None).await;
                let ctx_a = json!({"workspace_path":a});
                let ctx_b = json!({"workspace_path":b});
                let id = format!("zcode-memory-{modern}");
                let save = |label: &str| {
                    json!({"action":"save", "id":id,
                    "text":format!("ZCODEWORKSPACE {label} lossless text"),
                    "summary":format!("ZCODEWORKSPACE {label}"), "path":"/notes/zcode-workspace",
                    "category":"fact", "scope":"project", "force":true})
                };
                let (saved_a, saved_b) = tokio::join!(
                    first.call(10, "tachi_memory", save("alpha"), Some(ctx_a.clone())),
                    second.call(10, "tachi_memory", save("beta"), Some(ctx_b.clone()))
                );
                for result in [saved_a, saved_b] {
                    assert!(result.get("error").is_none(), "{result}");
                    assert_ne!(result["result"]["isError"], true, "{result}");
                }
                let project_a = observer
                    .resolve_existing_workspace_project(a.to_str().unwrap())
                    .unwrap();
                let project_b = observer
                    .resolve_existing_workspace_project(b.to_str().unwrap())
                    .unwrap();
                assert_ne!(project_a, project_b);
                // Alternate on the SAME transport as well as distinct clients.
                for (n, ctx, expected, absent, project) in [
                    (11, &ctx_a, "alpha", "beta", &project_a),
                    (12, &ctx_b, "beta", "alpha", &project_b),
                    (13, &ctx_a, "alpha", "beta", &project_a),
                ] {
                    let result = first
                        .call(
                            n,
                            "tachi_memory",
                            json!({"action":"search",
                        "query":"ZCODEWORKSPACE", "scope":"memory", "format":"json"}),
                            Some(ctx.clone()),
                        )
                        .await;
                    let data = payload(&result);
                    let rows = &data["sections"][0]["rows"];
                    assert!(rows.to_string().contains(expected), "{data:#}");
                    assert!(!rows.to_string().contains(absent), "{data:#}");
                    assert_eq!(rows[0]["store"]["project"], *project, "{data:#}");
                }
                let board = payload(
                    &first
                        .call(
                            14,
                            "tachi_task",
                            json!({"action":"board", "format":"json"}),
                            Some(ctx_a),
                        )
                        .await,
                );
                assert_eq!(board["staff"]["self"]["project_bound"], true, "{board:#}");
                assert_eq!(board["staff"]["project"], project_a, "{board:#}");
                let unbound = payload(
                    &first
                        .call(
                            15,
                            "tachi_task",
                            json!({"action":"board", "format":"json"}),
                            None,
                        )
                        .await,
                );
                assert_eq!(
                    unbound["staff"]["self"]["project_bound"], false,
                    "{unbound:#}"
                );
                assert_denied(
                    first
                        .call(
                            16,
                            "tachi_memory",
                            json!({"action":"save", "project":project_a,
                    "text":"unbound must not inherit earlier context"}),
                            None,
                        )
                        .await,
                );
                assert!(observer.session_project().is_none());
            }
            cancel.cancel();
            task.await.unwrap();
        });
    });
}

#[test]
fn http_zcode_workspace_context_rejects_invalid_paths_and_binding_conflicts() {
    let temp = tempfile::tempdir().unwrap();
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        let a = temp.path().join("Sigil");
        let b = temp.path().join("Quant");
        let unknown = temp.path().join("unregistered");
        for root in [&a, &b, &unknown] {
            std::fs::create_dir(root).unwrap();
        }
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).unwrap();
            let project = server
                .resolve_or_register_workspace_root(a.to_str().unwrap())
                .unwrap();
            server
                .resolve_or_register_workspace_root(b.to_str().unwrap())
                .unwrap();
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            for modern in [false, true] {
                let bound = Caller::new(&daemon.url, modern, Some(&project), None).await;
                let args = json!({"action":"board", "format":"json"});
                let ok = payload(
                    &bound
                        .call(
                            20,
                            "tachi_task",
                            args.clone(),
                            Some(json!({"workspace_path":a})),
                        )
                        .await,
                );
                assert_eq!(ok["staff"]["project"], project, "{ok:#}");
                for (i, root) in [&b, &unknown].into_iter().enumerate() {
                    assert_denied(
                        bound
                            .call(
                                21 + i as i64,
                                "tachi_memory",
                                json!({"action":"save",
                        "text":"must not reach storage", "scope":"project"}),
                                Some(json!({"workspace_path":root})),
                            )
                            .await,
                    );
                }
                assert!(!unknown.join(".tachi").exists());
                let unbound = Caller::new(&daemon.url, modern, None, None).await;
                for (i, context) in [
                    Value::Null,
                    json!([]),
                    json!({"workspace_path":null}),
                    json!({"workspace_path":0}),
                    json!({"workspace_path":""}),
                    json!({"workspace_path":"  "}),
                    json!({"workspace_path":"relative"}),
                    json!({"workspace_path":temp.path().join("missing")}),
                ]
                .into_iter()
                .enumerate()
                {
                    assert_denied(
                        unbound
                            .call(30 + i as i64, "tachi_task", args.clone(), Some(context))
                            .await,
                    );
                }
                // Metadata with no workspace, and model-supplied cwd, remain advisory.
                let advisory = payload(
                    &unbound
                        .call(
                            40,
                            "tachi_task",
                            json!({"action":"brief",
                    "agent_id":"zcode", "cwd":a, "format":"json"}),
                            Some(json!({"session_id":"conversation"})),
                        )
                        .await,
                );
                assert_eq!(
                    advisory["staff"]["self"]["project_bound"], false,
                    "{advisory:#}"
                );
            }
            cancel.cancel();
            task.await.unwrap();
        });
    });
}

#[test]
fn http_zcode_workspace_context_preserves_presence_and_stable_alias() {
    let temp = tempfile::tempdir().unwrap();
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        let a = temp.path().join("Sigil");
        let b = temp.path().join("Quant");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).unwrap();
            server
                .resolve_or_register_workspace_root(a.to_str().unwrap())
                .unwrap();
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            // Keep the existing, explicitly bound stable alias as the label.
            for modern in [false, true] {
                let alias = Caller::new(&daemon.url, modern, Some("Sigil"), None).await;
                let board = payload(
                    &alias
                        .call(
                            10,
                            "tachi_task",
                            json!({"action":"board", "format":"json"}),
                            Some(json!({"workspace_path":a})),
                        )
                        .await,
                );
                assert_eq!(board["staff"]["project"], "Sigil", "{board:#}");
            }
            for session in [None, Some("conversation")] {
                let caller = Caller::new(&daemon.url, false, None, None).await;
                let mut args = json!({"action":"brief", "agent_id":"zcode",
                    "cwd":a, "task":"Keep this report", "format":"json"});
                if let Some(session) = session {
                    args["session_id"] = json!(session);
                }
                let first = payload(
                    &caller
                        .call(20, "tachi_task", args, Some(json!({"workspace_path":a})))
                        .await,
                );
                let key = first["staff"]["self"]["presence_session_key"].clone();
                assert_eq!(
                    first["staff"]["self"]["reported_work"]["summary"],
                    "Keep this report"
                );
                payload(
                    &caller
                        .call(
                            21,
                            "tachi_staff",
                            json!({"action":"roster"}),
                            Some(json!({"workspace_path":a})),
                        )
                        .await,
                );
                let second = payload(
                    &caller
                        .call(
                            22,
                            "tachi_task",
                            json!({"action":"board", "format":"json"}),
                            Some(json!({"workspace_path":a})),
                        )
                        .await,
                );
                assert_eq!(second["staff"]["self"]["presence_session_key"], key);
                assert_eq!(second["staff"]["self"]["session_id"], json!(session));
                assert_eq!(
                    second["staff"]["self"]["reported_work"],
                    first["staff"]["self"]["reported_work"]
                );
                let self_rows: Vec<_> = second["staff"]["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|row| row["is_self"] == true)
                    .collect();
                assert_eq!(self_rows.len(), 1, "{second:#}");
                assert_eq!(self_rows[0]["reported_work"]["summary"], "Keep this report");
                let other = payload(
                    &caller
                        .call(
                            23,
                            "tachi_task",
                            json!({"action":"board", "format":"json"}),
                            Some(json!({"workspace_path":b})),
                        )
                        .await,
                );
                assert_eq!(other["staff"]["self"]["reported"], false, "{other:#}");
                assert!(
                    !other["staff"].to_string().contains("Keep this report"),
                    "{other:#}"
                );
            }
            cancel.cancel();
            task.await.unwrap();
        });
    });
}

#[test]
fn http_zcode_workspace_context_keeps_claim_admission_separate() {
    let temp = tempfile::tempdir().unwrap();
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        let root = temp.path().join("Sigil");
        std::fs::create_dir(&root).unwrap();
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).unwrap();
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            for modern in [false, true] {
                let ctx = json!({"workspace_path":root});
                let worktree = temp.path().join(format!("worktree-{modern}"));
                std::fs::create_dir(&worktree).unwrap();
                let args = json!({"action":"claim", "issue_ref":format!("owner/repo#zcode-{modern}"),
                    "branch":format!("zcode-{modern}"), "worktree_path":worktree,
                    "claim_scope":[format!("scope-{modern}/**")], "claim_role":"executor",
                    "claim_mode":"writable", "expected_head":"test-head",
                    "lease_expires_at":"2030-01-01T00:00:00Z"});
                let anonymous = Caller::new(&daemon.url, modern, None, None).await;
                assert_denied(anonymous.call(10, "tachi_task", args.clone(), Some(ctx.clone())).await);
                let admitted = Caller::new(&daemon.url, modern, None, Some("zcode-test-identity")).await;
                let claim = payload(&admitted.call(11, "tachi_task", args, Some(ctx)).await);
                assert_eq!(claim["status"], "completed", "{claim:#}");
                assert!(claim["claim_id"].as_str().is_some_and(|id| id.starts_with("claim-")));
            }
            cancel.cancel();
            task.await.unwrap();
        });
    });
}
