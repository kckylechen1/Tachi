//! Host metadata routes calls without mutating the shared connection.
mod automatic_binding;
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
        let meta = context
            .map(|context| json!({"com.zcode/request-context":context}))
            .unwrap_or_else(|| json!({}));
        self.call_with_meta(id, tool, args, meta, None).await
    }

    async fn call_with_meta(
        &self,
        id: i64,
        tool: &str,
        args: Value,
        extra: Value,
        inputs: Option<Value>,
    ) -> Value {
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
        meta.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let body = self
            .client
            .post(&self.url)
            .headers(headers)
            .json(&json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                "params":{"name":tool,"arguments":args,"_meta":meta,"inputResponses":inputs}}))
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

#[test]
fn http_admitted_unbound_session_dynamically_switches_workspaces_across_tools() {
    let temp = tempfile::tempdir().unwrap();
    with_tachi_home(temp.path(), || {
        let _embedding = EnvRestore::set("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", "1");
        let global = temp.path().join("global/memory.db");
        let umbrella = temp.path().join("Projects");
        let repo_a = umbrella.join("Sigil");
        let repo_b = umbrella.join("Quant");
        std::fs::create_dir_all(repo_a.join(".git")).unwrap();
        std::fs::create_dir_all(repo_a.join("src")).unwrap();
        std::fs::write(repo_a.join("src/lib.rs"), "// sigil").unwrap();
        std::fs::create_dir_all(repo_b.join(".git")).unwrap();
        std::fs::create_dir_all(repo_b.join("src")).unwrap();
        std::fs::write(repo_b.join("src/lib.rs"), "// quant").unwrap();

        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).unwrap();
            let observer = server.clone();
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;

            for modern in [false, true] {
                let admitted =
                    Caller::new(&daemon.url, modern, None, Some("antigravity-local")).await;
                let anonymous = Caller::new(&daemon.url, modern, None, None).await;

                // 1. Anonymous unbound session must still fail closed on explicit or inferred writes.
                assert_denied(
                    anonymous
                        .call(
                            10,
                            "tachi_memory",
                            json!({
                                "action": "save",
                                "project": repo_a,
                                "text": "anonymous must stay blocked"
                            }),
                            None,
                        )
                        .await,
                );
                assert_denied(
                    anonymous
                        .call(
                            11,
                            "tachi_memory",
                            json!({
                                "action": "save",
                                "files": [repo_b.join("src/lib.rs")],
                                "text": "anonymous file-inferred write must stay blocked"
                            }),
                            None,
                        )
                        .await,
                );

                // 2. Umbrella non-git unregistered parent folder must fail closed without creating `.tachi`.
                assert_denied(
                    admitted
                        .call(
                            12,
                            "tachi_memory",
                            json!({
                                "action": "save",
                                "project": umbrella,
                                "text": "umbrella folder is not a git repo"
                            }),
                            None,
                        )
                        .await,
                );
                assert!(!umbrella.join(".tachi").exists());

                // 3. Same admitted unbound session dynamically switches between Repo A (via `project` path)
                //    and Repo B (inferred from `files` without `project`)!
                let saved_a = admitted
                    .call(
                        13,
                        "tachi_memory",
                        json!({
                            "action": "save",
                            "id": format!("dyn-a-{modern}"),
                            "project": repo_a,
                            "text": format!("DYNWORKSPACE alpha-{modern} lossless"),
                            "summary": format!("DYNWORKSPACE alpha-{modern}"),
                            "path": "/notes/dyn-workspace",
                            "category": "fact",
                            "force": true
                        }),
                        None,
                    )
                    .await;
                assert!(saved_a.get("error").is_none(), "{saved_a:#}");
                assert_ne!(saved_a["result"]["isError"], true, "{saved_a:#}");

                let saved_b = admitted
                    .call(
                        14,
                        "tachi_memory",
                        json!({
                            "action": "save",
                            "id": format!("dyn-b-{modern}"),
                            "files": [repo_b.join("src/lib.rs")],
                            "text": format!("DYNWORKSPACE beta-{modern} lossless"),
                            "summary": format!("DYNWORKSPACE beta-{modern}"),
                            "path": "/notes/dyn-workspace",
                            "category": "fact",
                            "force": true
                        }),
                        None,
                    )
                    .await;
                assert!(saved_b.get("error").is_none(), "{saved_b:#}");
                assert_ne!(saved_b["result"]["isError"], true, "{saved_b:#}");

                let project_a = observer
                    .resolve_existing_workspace_project(repo_a.to_str().unwrap())
                    .unwrap();
                let project_b = observer
                    .resolve_existing_workspace_project(repo_b.to_str().unwrap())
                    .unwrap();
                assert_ne!(project_a, project_b);

                // Conflicting file paths across two workspaces must fail closed on save.
                assert_denied(
                    admitted
                        .call(
                            15,
                            "tachi_memory",
                            json!({
                                "action": "save",
                                "files": [repo_a.join("src/lib.rs"), repo_b.join("src/lib.rs")],
                                "text": "ambiguous multi-repo write"
                            }),
                            None,
                        )
                        .await,
                );

                // 4. Search switches cleanly via explicit repo path, canonical name, or `file_context`.
                let search_a = payload(
                    &admitted
                        .call(
                            16,
                            "tachi_memory",
                            json!({
                                "action": "search",
                                "project": repo_a,
                                "query": "DYNWORKSPACE",
                                "scope": "memory",
                                "format": "json"
                            }),
                            None,
                        )
                        .await,
                );
                let rows_a = &search_a["sections"][0]["rows"];
                assert!(
                    rows_a.to_string().contains(&format!("alpha-{modern}")),
                    "{search_a:#}"
                );
                assert!(
                    !rows_a.to_string().contains(&format!("beta-{modern}")),
                    "{search_a:#}"
                );

                let search_b = payload(
                    &admitted
                        .call(
                            17,
                            "tachi_memory",
                            json!({
                                "action": "search",
                                "file_context": [repo_b.join("src/lib.rs")],
                                "query": "DYNWORKSPACE",
                                "scope": "memory",
                                "format": "json"
                            }),
                            None,
                        )
                        .await,
                );
                let rows_b = &search_b["sections"][0]["rows"];
                assert!(
                    rows_b.to_string().contains(&format!("beta-{modern}")),
                    "{search_b:#}"
                );
                assert!(
                    !rows_b.to_string().contains(&format!("alpha-{modern}")),
                    "{search_b:#}"
                );

                // 5. Briefing with workspace path reports full session readiness for that call.
                let briefing = payload(
                    &admitted
                        .call(
                            18,
                            "tachi_memory",
                            json!({
                                "action": "briefing",
                                "project": repo_a,
                                "compact": true,
                                "scope": "memory",
                                "enable_rerank": false,
                                "format": "json"
                            }),
                            None,
                        )
                        .await,
                );
                assert_eq!(briefing["readiness"]["ready"], true, "{briefing:#}");
                assert_eq!(
                    briefing["readiness"]["available"],
                    json!({
                        "project_writes": true,
                        "claims": true,
                        "a2a": true,
                        "staff_start": true
                    }),
                    "{briefing:#}"
                );

                // 6. WorkClaim via `worktree_path` (omitting `project`) and `tachi_staff(roster)` via repo path succeed!
                let claim_args = json!({
                    "action": "claim",
                    "issue_ref": format!("owner/repo#dyn-{modern}"),
                    "branch": format!("dyn-{modern}"),
                    "worktree_path": repo_a,
                    "claim_scope": [format!("dyn-{modern}/**")],
                    "claim_role": "executor",
                    "claim_mode": "writable",
                    "expected_head": "test-head",
                    "lease_expires_at": "2030-01-01T00:00:00Z"
                });
                assert_denied(anonymous.call(19, "tachi_task", claim_args.clone(), None).await);
                let claim = payload(&admitted.call(20, "tachi_task", claim_args, None).await);
                assert_eq!(claim["status"], "completed", "{claim:#}");

                let roster = payload(
                    &admitted
                        .call(
                            21,
                            "tachi_staff",
                            json!({"action": "roster", "project": repo_a}),
                            None,
                        )
                        .await,
                );
                assert_eq!(roster["available"], true, "{roster:#}");
                assert_eq!(roster["project"], project_a, "{roster:#}");

                // 7. No sticky session mutation: bare save with neither `project` nor `files` still fails closed.
                assert_denied(
                    admitted
                        .call(
                            22,
                            "tachi_memory",
                            json!({
                                "action": "save",
                                "text": "must not inherit previous call's workspace"
                            }),
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
