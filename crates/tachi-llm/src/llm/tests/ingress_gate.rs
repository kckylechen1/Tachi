//! Production-seam discriminators for the resolver-at-ingress gate (#1681
//! PR-D debt (a); codex #1681 review BUG-5).
//!
//! The gate's own unit tests live beside it. What these add is the half that
//! cannot be asserted there: that the gate is actually *wired* at the seam
//! that accepts an arbitrary model string, and — the property that makes it a
//! report and not a behaviour change — that the model the provider receives is
//! byte-identical with the gate in place.

use super::*;

use crate::llm::provider_health::{ChatLaneConfig, ProviderRuntimeConfig};
use crate::{RerankConfig, RerankProviderKind};
use std::sync::{Arc, Mutex};

const KEY_ENV: &str = "TACHI_TEST_ONLY_INGRESS_GATE_API_KEY";
const EXTRACT_MODEL: &str = "Qwen/Qwen3.5-27B";

fn config_at(endpoint: &str) -> ProviderRuntimeConfig {
    let lane = |model: &str| ChatLaneConfig {
        base_url: endpoint.to_string(),
        model: model.to_string(),
        api_key_envs: vec![KEY_ENV],
    };
    ProviderRuntimeConfig {
        extract: lane(EXTRACT_MODEL),
        summary: lane("Qwen/Qwen3.5-7B"),
        reasoning: lane("deepseek-reasoner"),
        distill: lane("deepseek-chat"),
        rerank: RerankConfig {
            provider: RerankProviderKind::Voyage,
            local_endpoint: None,
        },
    }
}

/// A mock provider that answers every call and records the `model` field of
/// each request body it received.
async fn recording_provider() -> (
    LlmClient,
    Arc<Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
    tempfile::TempDir,
) {
    use axum::{routing::post, Json, Router};

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    let app = Router::new().route(
        "/chat/completions",
        post(move |Json(body): Json<serde_json::Value>| {
            let recorder = Arc::clone(&recorder);
            async move {
                recorder
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(
                        body.get("model")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("(absent)")
                            .to_string(),
                    );
                Json(serde_json::json!({
                    "choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}]
                }))
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock provider");
    let port = listener.local_addr().expect("mock provider addr").port();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock provider");
    });

    let config = config_at(&format!("http://127.0.0.1:{port}/chat/completions"));
    // The temp dir is handed back rather than dropped here: it has to outlive
    // the client's DB handle, and a `mem::forget` to achieve that would leak a
    // directory on every run.
    let temp = tempfile::tempdir().expect("temp db");
    let client = LlmClient::new_with_config(config, Some(&temp.path().join("vault.db")))
        .expect("client initializes");
    client.set_provider_secret_pool(
        KEY_ENV,
        vec![crate::ProviderSecret {
            key_id: format!("{KEY_ENV}_1"),
            value: "test-key".to_string(),
        }],
    );
    (client, seen, server, temp)
}

#[tokio::test]
async fn an_alias_shaped_override_is_counted_and_still_sent_unchanged() {
    let (client, seen, server, _temp) = recording_provider().await;

    client
        .call_extract_llm("system", "user", Some("chat.premium"), 0.0, 16)
        .await
        .expect("the call succeeds");

    // The report: one unresolved reference, on the lane that took it.
    let reported = client.unresolved_model_references();
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].lane, "extract");
    assert_eq!(reported[0].reference, "chat.premium");
    assert_eq!(reported[0].count, 1);
    assert_eq!(client.unresolved_model_reference_overflow(), 0);

    // And the behaviour: unchanged. The provider received exactly the string
    // the caller passed. A gate that "helpfully" substituted the lane model
    // here would break live routing and would pass every test that only
    // checked the counter.
    let sent = seen.lock().unwrap_or_else(|p| p.into_inner()).clone();
    assert_eq!(sent, vec!["chat.premium".to_string()]);

    server.abort();
}

/// #1681 PR-D review (CP5, round 2): `call_reasoning_llm_provider_only_with_
/// receipt` is a second public door onto `call_provider_tier` and was
/// calling it directly, skipping the gate entirely — an override routed
/// through this entry was invisible to the unresolved-reference count. Same
/// fixture, same assertions as the `call_extract_llm` case above, just
/// through the door that used to be unobserved.
#[tokio::test]
async fn the_receipt_only_provider_entry_is_now_observed_too() {
    let (client, seen, server, _temp) = recording_provider().await;

    client
        .call_reasoning_llm_provider_only_with_receipt(
            "system",
            "user",
            Some("chat.premium"),
            0.0,
            16,
        )
        .await
        .expect("the call succeeds");

    let reported = client.unresolved_model_references();
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].lane, "reasoning");
    assert_eq!(reported[0].reference, "chat.premium");
    assert_eq!(reported[0].count, 1);
    assert_eq!(client.unresolved_model_reference_overflow(), 0);

    // Behaviour is still unchanged: the provider received the raw override.
    let sent = seen.lock().unwrap_or_else(|p| p.into_inner()).clone();
    assert_eq!(sent, vec!["chat.premium".to_string()]);

    server.abort();
}

#[tokio::test]
async fn an_ordinary_lane_call_reports_nothing() {
    let (client, seen, server, _temp) = recording_provider().await;

    client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect("the call succeeds");
    // …and neither does a caller that re-states the lane's own model.
    client
        .call_extract_llm("system", "user", Some(EXTRACT_MODEL), 0.0, 16)
        .await
        .expect("the call succeeds");

    assert!(
        client.unresolved_model_references().is_empty(),
        "the lane's configured model is not a reference anything had to resolve"
    );
    let sent = seen.lock().unwrap_or_else(|p| p.into_inner()).clone();
    assert_eq!(
        sent,
        vec![EXTRACT_MODEL.to_string(), EXTRACT_MODEL.to_string()]
    );

    server.abort();
}
