//! Exercise self-reporting through the actual HTTP tool/profile/project gates.
use super::*;
use serde_json::{json, Value};

fn tool_payload(envelope: Value) -> Value {
    assert!(envelope.get("error").is_none(), "{envelope:#}");
    assert_ne!(envelope["result"]["isError"], true, "{envelope:#}");
    serde_json::from_str(envelope["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[test]
fn http_presence_brief_reports_workspace_and_task_without_static_client_binding() {
    let temp = tempfile::tempdir().unwrap();
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        let cwd = temp.path().join("workspace-alpha");
        std::fs::create_dir(&cwd).unwrap();
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).unwrap();
            server
                .resolve_or_register_workspace_root(cwd.to_str().unwrap())
                .unwrap();
            let (daemon, cancel, handle) = spawn_test_http_daemon(server, &global).await;
            let (a, ah, init) = http_mcp_initialize(&daemon.url, http_headers(&[]), None).await;
            assert!(init.get("error").is_none(), "{init}");
            http_mcp_initialized(&a, &daemon.url, ah.clone()).await;
            let (b, bh, _) = http_mcp_initialize(&daemon.url, http_headers(&[]), None).await;
            http_mcp_initialized(&b, &daemon.url, bh.clone()).await;
            let call = |action: &str, session: &str, summary: &str| {
                json!({"action":action,
                "agent_id":"test-agent", "session_id":session, "cwd":cwd,
                "task":summary, "format":"json"})
                .as_object()
                .unwrap()
                .clone()
            };
            let first = tool_payload(
                http_mcp_call_tool(
                    &a,
                    &daemon.url,
                    ah.clone(),
                    10,
                    "tachi_task",
                    call("brief", "session-a", "Review parser"),
                )
                .await,
            );
            assert_eq!(
                first["staff"]["self"]["reported_work"]["summary"],
                "Review parser"
            );
            assert!(first["staff"]["self"]["client"].is_string());
            assert_eq!(first["staff"]["self"]["project_bound"], false);
            assert_eq!(first["staff"]["self"]["identity_configured"], false);
            let second = tool_payload(
                http_mcp_call_tool(
                    &b,
                    &daemon.url,
                    bh,
                    11,
                    "tachi_task",
                    call("board", "session-b", "Fix UI"),
                )
                .await,
            );
            assert_eq!(second["staff"]["count"], 2);
            assert!(second["staff"].to_string().contains("Review parser"));
            let observed = tool_payload(
                http_mcp_call_tool(
                    &a,
                    &daemon.url,
                    ah.clone(),
                    12,
                    "tachi_staff",
                    json!({"action":"roster"}).as_object().unwrap().clone(),
                )
                .await,
            );
            assert!(observed.to_string().contains("Review parser"));
            assert!(observed.to_string().contains("Fix UI"));
            let denied = http_mcp_call_tool(
                &a,
                &daemon.url,
                ah,
                13,
                "tachi_memory",
                json!({"action":"save", "project":"workspace-alpha", "text":"must not write"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .await;
            assert!(
                denied.get("error").is_some() || denied["result"]["isError"] == true,
                "{denied:#}"
            );
            // Stateless requests use the caller's conversation ID, not their
            // shared identity/client/project rate-limit bucket.
            for (id, session, summary) in [(20, "modern-a", "Modern one"),
                (21, "modern-b", "Modern two"), (22, "modern-a", "Modern updated")] {
                let response = a.post(&daemon.url)
                    .headers(http_headers(&[("mcp-protocol-version", "2026-07-28"),
                        ("mcp-method", "tools/call"), ("mcp-name", "tachi_task")]))
                    .json(&json!({"jsonrpc":"2.0", "id":id, "method":"tools/call", "params":{
                        "name":"tachi_task", "arguments":call("board", session, summary),
                        "_meta":{
                            "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                            "io.modelcontextprotocol/clientCapabilities":{},
                            "io.modelcontextprotocol/clientInfo":{"name":"modern-client","version":"1"},
                            "tachiAgentIdentity":"shared-seat", "tachiProfile":"standard"
                        }}})).send().await.unwrap().text().await.unwrap();
                let result = tool_payload(parse_http_mcp_payload(&response, id));
                let staff = &result["staff"];
                assert_eq!(staff["self"]["reported_work"]["summary"], summary);
                assert_eq!(staff["self"]["client"], "modern-client");
                if id == 22 {
                    assert_eq!(staff["count"], 4);
                    assert!(staff.to_string().contains("Modern two"));
                    assert!(!staff.to_string().contains("Modern one"));
                }
            }
            cancel.cancel();
            handle.await.unwrap();
        });
    });
}
