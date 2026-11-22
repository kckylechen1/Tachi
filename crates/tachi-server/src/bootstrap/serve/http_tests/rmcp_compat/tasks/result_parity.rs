use super::*;

#[test]
fn modern_tasks_result_matches_legacy_for_global_and_historical_runs() {
    let home = tempfile::tempdir().unwrap();
    with_tachi_home(home.path(), || {
        let (_runs, _env, server, run) = task_fixture();
        std::fs::remove_file(run.join("mcp-tasks.json")).unwrap();
        server.set_session_identity(None, None, Some(tachi_hub::ToolProfile::standard()));
        crate::claims_ops::admit_agent_connection(&server, Some(OWNER.into()), true).unwrap();
        std::fs::write(run.join("result.md"), "Historical/global report 中文\n").unwrap();
        write_run_status(&run, "TASK_STATE_COMPLETED", 2);
        test_runtime().block_on(async {
            let (daemon, stop, task) = spawn_test_http_daemon(std::ops::Deref::deref(&server).clone(), &server.global_db_path_buf()).await;
            let identity = json!({"tachiAgentIdentity":OWNER,"tachiProfile":"standard","tachiClient":"reiclaw"});
            let (client, headers, initialized) = http_mcp_initialize(&daemon.url, http_headers(&[]), Some(identity.clone())).await;
            assert!(initialized.get("error").is_none(), "{initialized:#}");
            http_mcp_initialized(&client, &daemon.url, headers.clone()).await;
            let mut meta = modern_meta(identity);
            meta[CAPABILITIES] = json!({"extensions":{EXTENSION:{}}});
            for bound in [false, true] {
                if bound { crate::mcp_tasks::prepare_run_binding(&server, &run, DISPATCH, None).unwrap(); }
                let legacy = http_mcp_call_tool(&client, &daemon.url, headers.clone(), 2, "tachi_staff", json!({"action":"result","dispatch_id":DISPATCH,"format":"json"}).as_object().unwrap().clone()).await;
                let (status, modern) = task_rpc(&client, &daemon.url, 3, "tools/call", result_params(meta.clone()), &[]).await;
                assert_rpc_ok(status, &modern);
                assert_eq!(modern["result"]["resultType"], "complete");
                let modern_body: serde_json::Value = serde_json::from_str(modern["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
                let legacy_body: serde_json::Value = serde_json::from_str(&http_tool_text(&legacy)).unwrap();
                assert_eq!(modern_body, legacy_body);
                assert_eq!(modern_body["result"]["body"], "Historical/global report 中文\n");
                assert_eq!(run.join("mcp-tasks.json").exists(), bound);
                if bound {
                    let mut intruder = meta.clone(); intruder["tachiAgentIdentity"] = json!("agent.intruder");
                    let (_, denied) = task_rpc(&client, &daemon.url, 4, "tools/call", result_params(intruder), &[]).await;
                    assert_eq!(denied["error"]["code"], -32602, "{denied:#}");
                    assert_eq!(denied["error"]["message"], "task unavailable");
                }
            }
            stop.cancel(); task.await.unwrap();
        });
    });
}
