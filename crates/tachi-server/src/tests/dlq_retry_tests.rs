use super::{call_tool_on_server, make_server};
use crate::shared_defs::DeadLetter;
use crate::tool_params::DlqRetryParams;
use chrono::Utc;
use memory_server_runtime::AgentProfile;
use serde_json::json;

fn dlq_capture_refusal_events(server: &crate::MemoryServer) -> Vec<memcore::TachiEventRecord> {
    server
        .with_global_store(|store| {
            store
                .list_tachi_events(&memcore::TachiEventQuery {
                    event_type: Some("dlq_capture_refused".to_string()),
                    limit: 100,
                    ..memcore::TachiEventQuery::default()
                })
                .map_err(|error| error.to_string())
        })
        .expect("query DLQ refusal lifecycle events")
}

#[tokio::test]
async fn f1098_call_tool_emits_one_loud_signal_for_each_refused_dlq_capture() {
    let server = make_server();
    server.set_tool_profile(Some(tachi_hub::ToolProfile::admin()));

    crate::utils::lock_or_recover(&server.tool_discovery.skill_tools, "skill_tools").insert(
        "newly_registered_mutation".to_string(),
        "skill:test".to_string(),
    );

    let cases = [(
        "newly_registered_mutation",
        None,
        rmcp::model::ErrorCode::INTERNAL_ERROR,
        "Skill 'skill:test' not found in Hub",
        None,
    )];

    for (tool_name, arguments, expected_code, expected_message, expected_action) in cases {
        let err = call_tool_on_server(server.clone(), tool_name, arguments)
            .await
            .expect_err("injected production-boundary call must fail");
        assert_eq!(err.code, expected_code, "original error code changed");
        assert!(
            err.message.contains(expected_message),
            "original error message changed for {tool_name}: {}",
            err.message
        );
        assert_eq!(
            err.data, None,
            "durable refusal signaling must not rewrite the original error data"
        );
        assert!(
            server.dead_letters_lock().is_empty(),
            "refused {tool_name} failure must not be enqueued"
        );

        let matching: Vec<_> = dlq_capture_refusal_events(&server)
            .into_iter()
            .filter(|event| event.payload["tool_name"] == tool_name)
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "{tool_name} must emit exactly one dlq_capture_refused signal"
        );
        assert_eq!(
            matching[0].payload["action"].as_str(),
            expected_action,
            "refusal signal must preserve the routed action"
        );
        assert_eq!(
            matching[0].payload["reason"],
            "default_deny_no_explicit_safe_replay_authority"
        );
    }

    // Retired-proxy zombie names never reach dispatch: they return the
    // unknown-tool result, enqueue nothing, and emit no refusal signal.
    let zombie_args = serde_json::Map::from_iter([("action".to_string(), json!("save"))]);
    for tool_name in ["remote__unclassified", "remote__tachi_memory"] {
        let result = call_tool_on_server(server.clone(), tool_name, Some(zombie_args.clone()))
            .await
            .expect("zombie route should yield a tool result");
        assert_eq!(
            result.is_error,
            Some(true),
            "zombie route {tool_name} must be an error result"
        );
        assert!(
            server.dead_letters_lock().is_empty(),
            "zombie {tool_name} failure must not be enqueued"
        );
        let matching: Vec<_> = dlq_capture_refusal_events(&server)
            .into_iter()
            .filter(|event| event.payload["tool_name"] == tool_name)
            .collect();
        assert!(
            matching.is_empty(),
            "zombie {tool_name} never reaches dispatch, so no refusal signal may fire"
        );
    }

    let safe_args = serde_json::Map::from_iter([("action".to_string(), json!("metrics"))]);
    assert!(
        crate::shared_defs::should_enqueue_dlq("tachi_event", Some(&safe_args), false),
        "known safe read admission must remain unchanged"
    );
}

#[tokio::test]
async fn retry_dispatch_rejects_unclassified_remote_route() {
    let server = make_server();

    let err = server
        .retry_dispatch(
            "gateway-only__echo",
            Some(serde_json::Map::from_iter([(
                "text".to_string(),
                json!("hello"),
            )])),
        )
        .await
        .expect_err("unclassified remote routes must be denied by the replay gate");

    assert!(
        err.to_string()
            .contains("explicit typed action-effect classification"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn retry_dispatch_rejects_native_write_tools() {
    let server = make_server();

    let err = server
        .retry_dispatch(
            "save_memory",
            Some(serde_json::Map::from_iter([(
                "text".to_string(),
                json!("do not replay writes from dlq"),
            )])),
        )
        .await
        .expect_err("native write tools must be retried by explicit MCP calls only");

    assert!(
        err.to_string().contains("cannot be retried via DLQ"),
        "unexpected error: {err}"
    );
}

/// #1098 / #1170: the DLQ retry boundary must not treat a route it cannot
/// classify as safe merely because it is not in an old mutation deny-list.
/// With the proxy retired, `remote__*` wire names are simply unclassified
/// routes — the replay gate still fails them closed.
#[tokio::test]
async fn f1098_retry_requires_an_explicit_safe_effect_classification() {
    let server = make_server();
    crate::utils::lock_or_recover(&server.tool_discovery.skill_tools, "skill_tools").insert(
        "newly_registered_mutation".to_string(),
        "skill:test".to_string(),
    );

    let mut write_args = serde_json::Map::new();
    write_args.insert("action".to_string(), json!("save"));
    for (tool_name, arguments) in [
        ("unknown_route", None),
        ("remote__unclassified", None),
        ("remote__tachi_memory", Some(write_args)),
        ("newly_registered_mutation", None),
    ] {
        let err = server
            .retry_dispatch(tool_name, arguments)
            .await
            .expect_err(
                "unclassified, aliased, mutating, and newly registered routes must fail closed",
            );
        assert!(
            err.to_string()
                .contains("explicit typed action-effect classification"),
            "{tool_name} bypassed the explicit replay classification gate: {err}"
        );
    }

    let aliased_read_args = serde_json::Map::from_iter([("action".to_string(), json!("search"))]);
    let err = server
        .retry_dispatch("remote__tachi_memory", Some(aliased_read_args))
        .await
        .expect_err("external remote routes must not borrow local facade replay authority");
    assert!(
        err.to_string()
            .contains("explicit typed action-effect classification"),
        "malicious remote route bypassed the replay classification gate: {err}"
    );
}

#[tokio::test]
async fn f1098_telemetry_writing_searches_are_not_dlq_replayable() {
    let server = make_server();

    let cases = [
        (
            "search_memory",
            serde_json::Map::from_iter([("query".to_string(), json!("audit trail"))]),
        ),
        (
            "tachi_memory",
            serde_json::Map::from_iter([
                ("action".to_string(), json!("search")),
                ("query".to_string(), json!("audit trail")),
            ]),
        ),
    ];

    for (tool_name, arguments) in cases {
        let err = server
            .retry_dispatch(tool_name, Some(arguments))
            .await
            .expect_err("record_access=true searches must not replay through the DLQ");
        assert!(
            err.to_string()
                .contains("explicit typed action-effect classification"),
            "telemetry-writing {tool_name} bypassed the replay classification gate: {err}"
        );
    }
}

#[tokio::test]
async fn f1098_known_safe_local_read_retains_typed_replay_authority() {
    let server = make_server();
    let arguments = serde_json::Map::from_iter([("action".to_string(), json!("metrics"))]);

    let err = server
        .retry_dispatch("tachi_event", Some(arguments))
        .await
        .expect_err("native routes still require an explicit MCP retry");
    assert!(
        err.to_string().contains("native or non-idempotent"),
        "known safe read did not pass the typed replay gate: {err}"
    );
    assert!(
        !err.to_string()
            .contains("no explicit typed action-effect classification"),
        "known safe read lost its typed replay authority: {err}"
    );
}

#[tokio::test]
async fn dlq_retry_respects_rate_limiter_before_dispatch() {
    let server = make_server();
    {
        let mut guard = server.agent_runtime_write();
        guard.agent_profile = Some(AgentProfile {
            agent_id: "dlq-rate-limit-test".to_string(),
            display_name: "DLQ Rate Limit Test".to_string(),
            capabilities: vec![],
            tool_filter: None,
            rate_limit_rpm: None,
            rate_limit_burst: Some(1),
            registered_at: Utc::now().to_rfc3339(),
        });
    }

    let dead_letter_id = "dlq-rate-limit-1".to_string();
    let args = serde_json::Map::from_iter([(
        "text".to_string(),
        json!("do not replay native writes repeatedly"),
    )]);
    server.dead_letters_lock().push_back(DeadLetter {
        id: dead_letter_id.clone(),
        tool_name: "save_memory".to_string(),
        arguments: Some(args),
        error: "original failure".to_string(),
        error_category: "internal".to_string(),
        timestamp: Utc::now().to_rfc3339(),
        retry_count: 0,
        max_retries: 3,
        status: "pending".to_string(),
    });

    let first = crate::dlq_ops::handle_dlq_retry(
        &server,
        DlqRetryParams {
            dead_letter_id: dead_letter_id.clone(),
        },
    )
    .await
    .expect_err("native retry should still be rejected after passing rate limit");
    assert!(
        first.contains("cannot be retried via DLQ"),
        "unexpected first retry error: {first}"
    );

    let second = crate::dlq_ops::handle_dlq_retry(
        &server,
        DlqRetryParams {
            dead_letter_id: dead_letter_id.clone(),
        },
    )
    .await
    .expect_err("second identical retry should be rate limited before dispatch");

    assert!(
        second.contains("Loop detected"),
        "expected burst rate-limit error, got: {second}"
    );
    let dlq = server.dead_letters_lock();
    let dl = dlq
        .iter()
        .find(|dl| dl.id == dead_letter_id)
        .expect("dead letter should remain");
    assert_eq!(dl.retry_count, 2);
    assert_eq!(dl.status, "pending");
    assert!(dl.error.contains("Loop detected"));
}
