use super::*;
use crate::server_state::MemoryServer;
use std::path::PathBuf;

const DISPATCH_ID: &str = "20260803T101010Z-claude-deadbeef";

fn status_args() -> serde_json::Map<String, serde_json::Value> {
    json!({"action":"status", "dispatch_id":DISPATCH_ID, "format":"json"})
        .as_object()
        .expect("status args")
        .clone()
}

fn seed_receipt(server: &MemoryServer) -> (PathBuf, Vec<u8>) {
    let run_dir = server.tachi_home_dir().join("runs").join(DISPATCH_ID);
    std::fs::create_dir_all(&run_dir).expect("own fixture run directory");
    let path = run_dir.join("status.json");
    let bytes = serde_json::to_vec(&json!({
        "dispatch_id":DISPATCH_ID,
        "state":"TASK_STATE_WORKING",
        "status_revision":7,
        "run_dir":run_dir,
        "polling_fixture":"canonical receipt remains authoritative"
    }))
    .expect("receipt bytes");
    std::fs::write(&path, &bytes).expect("seed own canonical receipt");
    (path, bytes)
}

fn assert_status_payload(result: &rmcp::model::CallToolResult) {
    assert_ne!(
        result.is_error,
        Some(true),
        "real status success: {result:?}"
    );
    let wire = serde_json::to_value(result).expect("wire result");
    let payload: serde_json::Value =
        serde_json::from_str(wire["content"][0]["text"].as_str().expect("canonical text"))
            .expect("canonical JSON");
    assert_eq!(payload["dispatch_id"], DISPATCH_ID);
    assert_eq!(payload["state"], "TASK_STATE_WORKING");
    assert_eq!(payload["status_revision"], 7);
    assert_eq!(
        payload["polling_fixture"],
        "canonical receipt remains authoritative"
    );
}

// The fixture owns process-global home/run-root guards until all async reads
// finish, as required by the existing Staff filesystem owner.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn staff_status_polling_allows_twelve_identical_canonical_reads() {
    let (server, _home) = make_server_with_temp_home();
    server.set_tool_profile(Some(tachi_hub::ToolProfile::admin()));
    {
        let mut limiter = server.rate_limiter_lock();
        limiter.rpm = 32;
        limiter.burst = 8;
    }
    let (receipt_path, before) = seed_receipt(&server);
    let session = server.rate_limit_session_id();
    let mut block_counts = Vec::new();
    for call in 1..=12 {
        let result = call_tool_on_server(server.clone(), "tachi_staff", Some(status_args()))
            .await
            .unwrap_or_else(|error| panic!("canonical poll {call} must pass: {error:?}"));
        assert_status_payload(&result);
        block_counts.push(result.content.len());
    }
    assert!(
        block_counts.iter().all(|&count| count == 1),
        "polling must not emit loop warnings: {block_counts:?}"
    );
    assert_eq!(
        std::fs::read(receipt_path).expect("receipt after polling"),
        before
    );
    let limiter = server.rate_limiter_lock();
    assert_eq!(
        limiter.windows[&session].len(),
        12,
        "every poll counts toward RPM"
    );
    assert!(limiter.bursts.is_empty(), "status creates no loop window");
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn staff_status_polling_consumes_shared_rpm_and_resolves_live_profile_limits() {
    let (server, _home) = make_server_with_temp_home();
    server.set_tool_profile(Some(tachi_hub::ToolProfile::admin()));
    {
        let mut limiter = server.rate_limiter_lock();
        limiter.rpm = 10;
        limiter.burst = 8;
    }
    let (receipt_path, before) = seed_receipt(&server);
    let other = server.clone();
    assert_eq!(
        server.rate_limit_session_id(),
        other.rate_limit_session_id()
    );
    let mut block_counts = Vec::new();
    for call in 1..=9 {
        let result = call_tool_on_server(other.clone(), "tachi_staff", Some(status_args()))
            .await
            .unwrap_or_else(|error| panic!("canonical poll {call} must pass: {error:?}"));
        assert_status_payload(&result);
        block_counts.push(result.content.len());
    }
    assert!(
        block_counts.iter().all(|&count| count == 1),
        "polling must not emit loop warnings: {block_counts:?}"
    );
    let ordinary = call_tool_on_server(server.clone(), "runtime_info", None)
        .await
        .expect("ordinary call shares the last RPM slot");
    assert_ne!(ordinary.is_error, Some(true));
    let error = call_tool_on_server(other.clone(), "tachi_staff", Some(status_args()))
        .await
        .expect_err("configured RPM rejects the next poll");
    assert!(error.message.contains("Rate limited"), "{error:?}");
    {
        let mut runtime = server.agent_runtime_write();
        runtime.agent_profile = Some(AgentProfile {
            agent_id: "polling-rpm-fixture".to_string(),
            display_name: "Polling RPM Fixture".to_string(),
            capabilities: vec![],
            tool_filter: None,
            rate_limit_rpm: Some(12),
            rate_limit_burst: Some(1),
            registered_at: Utc::now().to_rfc3339(),
        });
    }
    let result = call_tool_on_server(other, "tachi_staff", Some(status_args()))
        .await
        .expect("live RPM override grants one more poll despite the tight loop limit");
    assert_status_payload(&result);
    let ordinary = call_tool_on_server(
        server.clone(),
        "runtime_info",
        Some(json!({"marker":1}).as_object().unwrap().clone()),
    )
    .await
    .expect("ordinary call uses the final shared override slot");
    assert_ne!(ordinary.is_error, Some(true));
    let error = call_tool_on_server(
        server.clone(),
        "runtime_info",
        Some(json!({"marker":2}).as_object().unwrap().clone()),
    )
    .await
    .expect_err("poll traffic exhausts the shared budget for ordinary calls too");
    assert!(error.message.contains("Rate limited"), "{error:?}");
    assert_eq!(
        std::fs::read(receipt_path).expect("receipt after polling"),
        before
    );
    let session = server.rate_limit_session_id();
    let limiter = server.rate_limiter_lock();
    assert_eq!(limiter.windows[&session].len(), 12);
}

#[tokio::test]
async fn staff_status_polling_does_not_exempt_writes_nonpoll_reads_or_unknown_routes() {
    let server = make_server();
    server.set_tool_profile(Some(tachi_hub::ToolProfile::admin()));
    {
        let mut limiter = server.rate_limiter_lock();
        limiter.rpm = 0;
        limiter.burst = 8;
    }
    // Start/cancel/preflight omit required fields, refusing before any launch,
    // cancellation or probe. Unrouted names never call a remote endpoint.
    for (tool, args) in [
        ("tachi_staff", json!({"action":"start"})),
        ("tachi_staff", json!({"action":"cancel"})),
        ("tachi_staff", json!({"action":"preflight"})),
        ("tachi_staff", json!({"action":"unknown"})),
        ("tachi_staff", json!({"action":["status"]})),
        ("tachi_staff", json!({})),
        ("tachi_staff", json!({"action":"status"})),
        (
            "tachi_staff",
            json!({"action":"status", "dispatch_id":null}),
        ),
        ("tachi_staff", json!({"action":"status", "dispatch_id":7})),
        (
            "tachi_staff",
            json!({"action":"status", "dispatch_id":DISPATCH_ID, "unknown_field":true}),
        ),
        (
            "tachi_staff",
            json!({"action":"status", "dispatch_id":DISPATCH_ID, "format":7}),
        ),
        ("tachi_staff", json!({"action":"status", "dispatch_id":""})),
        (
            "tachi_staff",
            json!({"action":"status", "dispatch_id":"../foreign"}),
        ),
        (
            "tachi_staff",
            json!({"action":"status", "dispatch_id":"/foreign/run"}),
        ),
        (
            "tachi_staff",
            json!({"action":"status", "dispatch_id":DISPATCH_ID, "identical_call_policy":"AllowPolling"}),
        ),
        ("tachi_task", json!({"action":"status"})),
        ("runtime_info", json!({})),
        ("foreign__tachi_staff", json!({"action":"status"})),
        ("unknown_polling_tool", json!({"action":"status"})),
    ] {
        let arguments = args.as_object().expect("case args").clone();
        for call in 1..=8 {
            if let Err(error) =
                call_tool_on_server(server.clone(), tool, Some(arguments.clone())).await
            {
                assert!(
                    !error.message.contains("Loop detected"),
                    "{tool} {args} call {call}: {error:?}"
                );
            }
        }
        let error = call_tool_on_server(server.clone(), tool, Some(arguments))
            .await
            .expect_err("the ninth identical non-poll call remains blocked");
        assert!(
            error.message.contains("Loop detected"),
            "{tool} {args}: {error:?}"
        );
    }
}
