use super::super::make_server;
use crate::tool_params::{CompactContextParams, Message};
use rmcp::handler::server::wrapper::Parameters;

fn base_params(persist: bool) -> CompactContextParams {
    CompactContextParams {
        agent_id: "main".to_string(),
        conversation_id: "conv-1099".to_string(),
        window_id: "window-1099".to_string(),
        trigger: "manual".to_string(),
        messages: vec![Message {
            role: "user".to_string(),
            content: "Some content to compact.".to_string(),
        }],
        current_summary: None,
        path_prefix: None,
        project: None,
        target_tokens: 128,
        max_output_tokens: 256,
        persist,
    }
}

/// #1099 discrimination: on origin/main, `persist=true` returned a nominal
/// `"status": "skipped"|"completed"` success with two always-empty
/// (`captured_memory_ids`/`queued_job_ids`) fields and, at best, an
/// easy-to-miss `warning` — nominal success with no persistence, forbidden
/// by the #1099 contract. This must now be a loud refusal (`Err`) that never
/// reaches the empty-messages/model-call paths, pointing callers at the real
/// persistence tool. Red on origin/main (old code returns `Ok(..)` with
/// `status: "skipped"`), green after this change (returns `Err(..)`).
#[tokio::test]
async fn compact_context_persist_true_is_refused_not_nominal_success() {
    let server = make_server();

    let err = server
        .compact_context(Parameters(base_params(true)))
        .await
        .expect_err("persist=true must be refused, not a nominal success");

    assert!(
        err.contains("compact_session_memory"),
        "refusal must point at the real persistence path: {err}"
    );
    assert!(
        err.contains("persist"),
        "refusal must explain which parameter triggered it: {err}"
    );
}

/// The refusal fires before the empty-messages short-circuit too — persist
/// intent is rejected regardless of whether there is anything to compact.
#[tokio::test]
async fn compact_context_persist_true_is_refused_even_with_empty_messages() {
    let server = make_server();
    let mut params = base_params(true);
    params.messages = vec![Message {
        role: "user".to_string(),
        content: "   ".to_string(),
    }];

    let err = server
        .compact_context(Parameters(params))
        .await
        .expect_err("persist=true must be refused even when messages are empty");
    assert!(err.contains("compact_session_memory"), "{err}");
}

/// `persist=false` (the default) is unaffected — the handler still reaches
/// the ordinary empty-messages short-circuit and returns success with no
/// dead `captured_memory_ids`/`queued_job_ids` fields (#1099: those fields
/// never described a real operation and are gone entirely now).
#[tokio::test]
async fn compact_context_persist_false_reaches_normal_short_circuit() {
    let server = make_server();
    let mut params = base_params(false);
    params.messages = vec![Message {
        role: "user".to_string(),
        content: "   ".to_string(),
    }];

    let result = server
        .compact_context(Parameters(params))
        .await
        .expect("persist=false should not be refused");
    let json: serde_json::Value = serde_json::from_str(&result).expect("json");
    assert_eq!(json["status"], serde_json::json!("skipped"));
    assert!(
        json.get("captured_memory_ids").is_none(),
        "dead placeholder field must not be in the response: {json}"
    );
    assert!(
        json.get("queued_job_ids").is_none(),
        "dead placeholder field must not be in the response: {json}"
    );
}
