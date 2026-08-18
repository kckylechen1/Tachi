//! Production-seam discriminators for direct-provider model overrides.
//!
//! A caller-supplied model must reach the provider byte-for-byte through both
//! public chat entry points. Bounding at log and storage sinks must never alter
//! the request payload used for routing.

use super::*;

use crate::llm::provider_health::{ChatLaneConfig, ProviderRuntimeConfig};
use crate::{RerankConfig, RerankProviderKind};
use std::sync::{Arc, Mutex};

const KEY_ENV: &str = "TACHI_TEST_ONLY_DIRECT_PROVIDER_OVERRIDE_API_KEY";
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
    let client = LlmClient::new_with_config(config, None).expect("client initializes");
    client.set_provider_secret_pool(
        KEY_ENV,
        vec![crate::ProviderSecret {
            key_id: format!("{KEY_ENV}_1"),
            value: "test-key".to_string(),
        }],
    );
    (client, seen, server)
}

#[tokio::test]
async fn model_override_is_sent_unchanged_on_chat_lane() {
    let (client, seen, server) = recording_provider().await;

    client
        .call_extract_llm("system", "user", Some("chat.premium"), 0.0, 16)
        .await
        .expect("the call succeeds");

    // The provider receives exactly the string the caller passed.
    let sent = seen.lock().unwrap_or_else(|p| p.into_inner()).clone();
    assert_eq!(sent, vec!["chat.premium".to_string()]);

    server.abort();
}

#[tokio::test]
async fn model_override_is_sent_unchanged_on_receipt_only_entry() {
    let (client, seen, server) = recording_provider().await;

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

    // The receipt-only entry has the same pass-through contract.
    let sent = seen.lock().unwrap_or_else(|p| p.into_inner()).clone();
    assert_eq!(sent, vec!["chat.premium".to_string()]);

    server.abort();
}
