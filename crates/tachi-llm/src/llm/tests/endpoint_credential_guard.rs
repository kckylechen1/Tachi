//! Construction and dispatch discriminators for credential-bearing chat URLs.

use super::*;

use crate::llm::provider_health::{ChatLaneConfig, LaneFallbackConfig, ProviderRuntimeConfig};
use crate::{RerankConfig, RerankProviderKind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const KEY_ENV: &str = "TACHI_TEST_ONLY_ENDPOINT_GUARD_API_KEY";
const SECRET_VALUE: &str = "must-never-appear-in-an-error";

fn lane(endpoint: impl Into<String>) -> ChatLaneConfig {
    ChatLaneConfig {
        base_url: endpoint.into(),
        model: "test-model".to_string(),
        api_key_envs: vec![KEY_ENV],
    }
}

fn clean_config(endpoint: &str) -> ProviderRuntimeConfig {
    ProviderRuntimeConfig {
        extract: lane(endpoint),
        summary: lane(endpoint),
        reasoning: lane(endpoint),
        distill: lane(endpoint),
        rerank: RerankConfig {
            provider: RerankProviderKind::Voyage,
            local_endpoint: None,
        },
    }
}

fn set_primary_endpoint(config: &mut ProviderRuntimeConfig, lane: &str, endpoint: String) {
    match lane {
        "extract" => config.extract.base_url = endpoint,
        "summary" => config.summary.base_url = endpoint,
        "reasoning" => config.reasoning.base_url = endpoint,
        "distill" => config.distill.base_url = endpoint,
        _ => panic!("unknown test lane: {lane}"),
    }
}

fn construction_error(config: ProviderRuntimeConfig) -> String {
    match LlmClient::new_with_config(config, None) {
        Ok(_) => panic!("credential-bearing endpoint must fail during construction"),
        Err(error) => error,
    }
}

#[test]
fn injected_url_userinfo_is_refused_without_echoing_credential_material() {
    let endpoint = format!("https://user:{SECRET_VALUE}@api.example.invalid/v1");
    let error = construction_error(clean_config(&endpoint));

    assert!(error.contains("extract"));
    assert!(error.contains("userinfo credentials"));
    assert!(!error.contains("user:"));
    assert!(!error.contains(SECRET_VALUE));
    assert!(!error.contains(&endpoint));
}

#[test]
fn every_primary_lane_is_guarded_at_the_common_constructor() {
    for lane_name in ["extract", "summary", "reasoning", "distill"] {
        let mut config = clean_config("https://api.example.invalid/v1");
        set_primary_endpoint(
            &mut config,
            lane_name,
            format!("https://api.example.invalid/v1?token={SECRET_VALUE}"),
        );

        let error = construction_error(config);
        assert!(error.contains(lane_name), "{lane_name}: {error}");
        assert!(error.contains("'token'"), "{lane_name}: {error}");
        assert!(!error.contains(SECRET_VALUE), "{lane_name}: {error}");
    }
}

#[test]
fn every_canonical_query_key_is_refused_case_insensitively_and_secret_negative() {
    for key in memcore::catalog::endpoint::CREDENTIAL_SHAPED_QUERY_KEYS {
        let upper = key.to_uppercase();
        let endpoint = format!("https://api.example.invalid/v1?{upper}={SECRET_VALUE}");
        let error = construction_error(clean_config(&endpoint));

        assert!(error.contains(&format!("'{upper}'")), "{key}: {error}");
        assert!(!error.contains(SECRET_VALUE), "{key}: {error}");
        assert!(!error.contains(&endpoint), "{key}: {error}");
    }
}

#[test]
fn directly_injected_fallback_endpoint_uses_the_same_guard() {
    let config = clean_config("https://api.example.invalid/v1");
    let fallbacks = LaneFallbackConfig {
        reasoning: Some(lane(format!(
            "https://api.example.invalid/v1?authorization={SECRET_VALUE}"
        ))),
        ..LaneFallbackConfig::default()
    };

    let error = match LlmClient::new_with_config_and_fallbacks(config, fallbacks, None) {
        Ok(_) => panic!("credential-bearing fallback must fail during construction"),
        Err(error) => error,
    };
    assert!(error.contains("reasoning fallback"));
    assert!(error.contains("'authorization'"));
    assert!(!error.contains(SECRET_VALUE));
}

#[tokio::test]
async fn poisoned_lane_refuses_before_any_sibling_endpoint_is_contacted() {
    use axum::{routing::post, Json, Router};

    let requests = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&requests);
    let app = Router::new().route(
        "/chat/completions",
        post(move || {
            let observed = Arc::clone(&observed);
            async move {
                observed.fetch_add(1, Ordering::SeqCst);
                Json(serde_json::json!({
                    "choices": [{"message": {"content": "unexpected"}, "finish_reason": "stop"}]
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind contact counter");
    let port = listener
        .local_addr()
        .expect("contact counter address")
        .port();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("contact counter");
    });

    let clean_endpoint = format!("http://127.0.0.1:{port}/chat/completions");
    let mut config = clean_config(&clean_endpoint);
    config.reasoning.base_url =
        format!("http://user:{SECRET_VALUE}@127.0.0.1:{port}/chat/completions");

    let error = construction_error(config);
    assert!(error.contains("reasoning"));
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "constructor refusal must happen before any poisoned or sibling lane can send"
    );

    server.abort();
}

#[tokio::test]
async fn ordinary_query_and_clean_loopback_endpoint_still_construct_and_dispatch() {
    use axum::{routing::post, Json, Router};

    let app = Router::new().route(
        "/chat/completions",
        post(|| async {
            Json(serde_json::json!({
                "choices": [{"message": {"content": "clean"}, "finish_reason": "stop"}]
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock provider");
    let port = listener.local_addr().expect("mock provider address").port();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock provider");
    });

    let endpoint =
        format!("http://127.0.0.1:{port}/chat/completions?api-version=2026-08-18&monkey=1");
    let client = LlmClient::new_with_config(clean_config(&endpoint), None)
        .expect("ordinary query keys remain allowed");
    client.set_provider_secret_pool(
        KEY_ENV,
        vec![crate::ProviderSecret {
            key_id: KEY_ENV.to_string(),
            value: "test-key".to_string(),
        }],
    );

    let response = client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect("clean loopback dispatch succeeds");
    assert_eq!(response, "clean");

    server.abort();
}
