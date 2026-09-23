// provider_selector.rs — EXTRACT_PROVIDER / SUMMARY_PROVIDER coverage.
//
// The selector is the opt-in canonical provider selection for the two
// front-line lanes (see `load_lane_selected_provider`). These tests freeze:
//
// - explicit `deepseek` works even while stale SiliconFlow-branded
//   EXTRACT_*/SUMMARY_* aliases are still present;
// - the wire Authorization header carries the canonical key only;
// - a selected lane refuses a foreign endpoint (known or unknown host) at
//   construction, with or without any key configured — env now, Vault-only
//   later via the request-path bind;
// - a SiliconFlow-only key pool cannot fall back into, or be sent to, a
//   DeepSeek-selected lane;
// - unset/empty selectors keep the legacy alias chain byte-for-byte;
// - invalid selector values fail closed;
// - model overrides never move the endpoint or the credential.

use super::super::{
    ChatLaneConfig, LaneConfigOverlay, LaneFieldOverlay, ProviderRuntimeConfig,
    DEEPSEEK_AUTH_PROBE, SILICONFLOW_AUTH_PROBE,
};
use super::*;
use crate::{materialize_provider_secrets_from_durable_source, DurableVaultLoad};

/// Unset every env var front-line resolution (and the foundry lanes resolved
/// alongside it in `from_env`) may consult, so selector tests are
/// deterministic regardless of the host shell.
fn frontline_selector_env_guards() -> Vec<EnvRestore> {
    [
        "EXTRACT_PROVIDER",
        "SUMMARY_PROVIDER",
        "EXTRACT_API_KEY",
        "EXTRACT_BASE_URL",
        "EXTRACT_MODEL",
        "SUMMARY_API_KEY",
        "SUMMARY_BASE_URL",
        "SUMMARY_MODEL",
        "EXTRACTOR_BASE_URL",
        "EXTRACTOR_MODEL",
        "DEEPSEEK_API_KEY",
        "DEEPSEEK_BASE_URL",
        "DEEPSEEK_MODEL",
        "SILICONFLOW_API_KEY",
        "SILICONFLOW_BASE_URL",
        "SILICONFLOW_MODEL",
        "DISTILL_API_KEY",
        "DISTILL_BASE_URL",
        "DISTILL_MODEL",
        "REASONING_API_KEY",
        "REASONING_BASE_URL",
        "REASONING_MODEL",
        "ZAI_API_KEY",
        "BIGMODEL_API_KEY",
        "EXTRACT_FALLBACK_API_KEY",
        "EXTRACT_FALLBACK_BASE_URL",
        "EXTRACT_FALLBACK_MODEL",
        "SUMMARY_FALLBACK_API_KEY",
        "SUMMARY_FALLBACK_BASE_URL",
        "SUMMARY_FALLBACK_MODEL",
        "TACHI_BACKEND_EXTRACT_TIER",
        "TACHI_BACKEND_SUMMARY_TIER",
        RERANK_PROVIDER_ENV,
        RERANK_LOCAL_ENDPOINT_ENV,
    ]
    .into_iter()
    .map(EnvRestore::unset)
    .collect()
}

#[test]
fn explicit_deepseek_selector_uses_canonical_key_despite_stale_aliases() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();
    let _selector = EnvRestore::set("EXTRACT_PROVIDER", "deepseek");
    let _stale_extract_key = EnvRestore::set("EXTRACT_API_KEY", "stale-extract-alias-key");
    let _stale_sf_key = EnvRestore::set("SILICONFLOW_API_KEY", "stale-siliconflow-key");
    let _extract_base = EnvRestore::set(
        "EXTRACT_BASE_URL",
        "https://api.deepseek.com/chat/completions",
    );
    let _extract_model = EnvRestore::set("EXTRACT_MODEL", "deepseek-v4-flash");
    let _canonical = EnvRestore::set("DEEPSEEK_API_KEY", "deepseek-canonical-key");

    let client = LlmClient::new().expect("explicit DeepSeek extract lane should construct");
    let extract = client.lane(ChatLane::Extract);
    assert_eq!(
        extract.base_url,
        "https://api.deepseek.com/chat/completions"
    );
    assert_eq!(extract.model, "deepseek-v4-flash");
    assert_eq!(extract.api_key_envs, vec!["DEEPSEEK_API_KEY"]);
    assert_eq!(
        client.provider_key_id_for_tests(&extract.api_key_envs),
        Some("DEEPSEEK_API_KEY".to_string())
    );
    assert_eq!(
        client
            .provider_secret_for_tests(&extract.api_key_envs)
            .as_deref(),
        Some("deepseek-canonical-key")
    );
}

#[test]
fn explicit_deepseek_selector_defaults_endpoint_and_model_from_descriptor() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();
    let _extract_selector = EnvRestore::set("EXTRACT_PROVIDER", "deepseek");
    let _summary_selector = EnvRestore::set("SUMMARY_PROVIDER", "deepseek");
    let _canonical = EnvRestore::set("DEEPSEEK_API_KEY", "deepseek-canonical-key");

    let config = ProviderRuntimeConfig::from_env().expect("descriptor defaults should resolve");
    assert_eq!(config.extract.base_url, DEEPSEEK_AUTH_PROBE.chat.base_url);
    assert_eq!(config.extract.model, "deepseek-v4-flash");
    assert_eq!(config.extract.api_key_envs, vec!["DEEPSEEK_API_KEY"]);
    assert_eq!(config.summary.base_url, DEEPSEEK_AUTH_PROBE.chat.base_url);
    assert_eq!(config.summary.model, "deepseek-v4-flash");
    assert_eq!(config.summary.api_key_envs, vec!["DEEPSEEK_API_KEY"]);
}

/// Public request-boundary proof for the selected-provider chain. The
/// production selector guard pins a selected lane's endpoint to
/// `https://{provider-host}` on the default port, so a loopback plain-HTTP
/// capture cannot be reached through the env resolver; the wire shape is
/// therefore exercised through the env-free injected-config seam — the same
/// pattern the existing cross-provider boundary tests use. The lane carries
/// the canonical-only key chain the selector resolves, stale alias keys sit
/// in env, and the wire must show the canonical Bearer credential only.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn deepseek_selected_extract_request_carries_canonical_key_only() {
    use axum::{
        body::{to_bytes, Body},
        extract::Request,
        http::header::AUTHORIZATION,
        response::Response,
        routing::any,
        Router,
    };
    use std::sync::{Arc, Mutex};

    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();
    let _stale_extract_key = EnvRestore::set("EXTRACT_API_KEY", "stale-extract-alias-key");
    let _stale_sf_key = EnvRestore::set("SILICONFLOW_API_KEY", "stale-siliconflow-key");
    // Env selection through the canonical chain (no Vault pool): this is the
    // same fallback pass a production request would run.
    let _canonical = EnvRestore::set("DEEPSEEK_API_KEY", "deepseek-canonical-key");

    let seen = Arc::new(Mutex::new(Vec::<(String, String, serde_json::Value)>::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind capture provider");
    let addr = listener.local_addr().expect("capture provider address");
    let app = Router::new().fallback(any({
        let seen = Arc::clone(&seen);
        move |request: Request| {
            let seen = Arc::clone(&seen);
            async move {
                let path = request.uri().path().to_string();
                let authorization = request
                    .headers()
                    .get(AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let body = to_bytes(request.into_body(), 64 * 1024)
                    .await
                    .expect("bounded request body");
                let body = serde_json::from_slice(&body).expect("JSON request body");
                seen.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push((path, authorization, body));
                Response::new(Body::from(
                    r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
                ))
            }
        }
    }));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("capture provider");
    });

    let unused = ChatLaneConfig {
        base_url: "https://unused.test/v1/chat/completions".to_string(),
        model: "unused".to_string(),
        api_key_envs: vec!["UNUSED_API_KEY"],
    };
    let client = LlmClient::new_with_config(
        ProviderRuntimeConfig {
            // The exact chain `EXTRACT_PROVIDER=deepseek` resolves to;
            // plain-http host pin is the test-seam stand-in for the
            // descriptor endpoint the production guard would enforce.
            extract: ChatLaneConfig {
                base_url: format!(
                    "http://{}/deepseek/chat/completions",
                    DEEPSEEK_AUTH_PROBE.host
                ),
                model: "deepseek-v4-flash".to_string(),
                api_key_envs: vec!["DEEPSEEK_API_KEY"],
            },
            summary: unused.clone(),
            reasoning: unused.clone(),
            distill: unused,
            rerank: RerankConfig {
                provider: RerankProviderKind::Voyage,
                local_endpoint: None,
            },
        },
        None,
    )
    .expect("injected canonical-chain extract lane should build");
    client.replace_http_client_for_tests(
        LlmClient::http_client_with_host_resolved_for_tests(DEEPSEEK_AUTH_PROBE.host, addr)
            .expect("resolved DeepSeek test client"),
    );

    client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect("DeepSeek capture provider should answer");

    let requests = seen
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    assert_eq!(
        requests.len(),
        1,
        "expected one provider request: {requests:?}"
    );
    let (path, authorization, body) = &requests[0];
    assert_eq!(path, "/deepseek/chat/completions");
    assert_eq!(authorization, "Bearer deepseek-canonical-key");
    assert_eq!(body["model"], "deepseek-v4-flash");
    let wire = format!("{path}{authorization}{body}");
    assert!(
        !wire.contains("stale-extract-alias-key") && !wire.contains("stale-siliconflow-key"),
        "stale alias credentials must never reach the wire: {wire}"
    );
    server.abort();
}

/// The foreign-endpoint refusal is key-independent: it fires at construction
/// with no key configured at all, so it also covers a Vault-only key that
/// materializes later against the stored endpoint.
#[test]
fn selected_provider_rejects_foreign_known_host_without_any_key() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();
    let _selector = EnvRestore::set("EXTRACT_PROVIDER", "deepseek");
    let _sf_base = EnvRestore::set(
        "EXTRACT_BASE_URL",
        "https://api.siliconflow.cn/v1/chat/completions",
    );

    let error = ProviderRuntimeConfig::from_env()
        .expect_err("SiliconFlow endpoint must be refused for a DeepSeek-selected lane");
    assert!(
        error.contains("EXTRACT_PROVIDER")
            && error.contains("api.deepseek.com")
            && error.contains("api.siliconflow.cn")
            && error.contains("refusing credential-bearing request"),
        "unexpected rejection error: {error}"
    );
}

#[test]
fn selected_provider_rejects_unknown_host() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();
    let _selector = EnvRestore::set("SUMMARY_PROVIDER", "deepseek");
    let _unknown_base = EnvRestore::set(
        "SUMMARY_BASE_URL",
        "https://llm-gateway.internal.example/v1/chat/completions",
    );

    let error = ProviderRuntimeConfig::from_env()
        .expect_err("unknown endpoint host must be refused for a selected lane");
    assert!(
        error.contains("SUMMARY_PROVIDER")
            && error.contains("api.deepseek.com")
            && error.contains("llm-gateway.internal.example")
            && error.contains("refusing credential-bearing request"),
        "unexpected rejection error: {error}"
    );
}

#[test]
fn selected_provider_rejects_mismatched_canonical_provider_base_url() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();
    let _selector = EnvRestore::set("EXTRACT_PROVIDER", "deepseek");
    let _mismatched = EnvRestore::set(
        "DEEPSEEK_BASE_URL",
        "https://api.siliconflow.cn/v1/chat/completions",
    );

    let error = ProviderRuntimeConfig::from_env()
        .expect_err("canonical provider base URL on a foreign host must be refused, not skipped");
    assert!(
        error.contains("DEEPSEEK_BASE_URL")
            && error.contains("api.deepseek.com")
            && error.contains("refusing credential-bearing request"),
        "unexpected rejection error: {error}"
    );
}

/// Vault-only credential: the canonical key pool selects the same provider,
/// and a later Vault-wins overlay that moves the endpoint off the selected
/// provider fails closed before any credential-bearing request.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn vault_only_deepseek_key_keeps_canonical_selection_and_rejects_mismatched_overlay() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();
    let _selector = EnvRestore::set("EXTRACT_PROVIDER", "deepseek");

    let client = LlmClient::new().expect("selector without any key should still construct");
    let extract = client.lane(ChatLane::Extract);
    assert_eq!(extract.base_url, DEEPSEEK_AUTH_PROBE.chat.base_url);
    assert_eq!(extract.api_key_envs, vec!["DEEPSEEK_API_KEY"]);

    assert!(client.set_provider_secret("DEEPSEEK_API_KEY", "vault-deepseek-key"));
    assert_eq!(
        client.provider_key_id_for_tests(&extract.api_key_envs),
        Some("DEEPSEEK_API_KEY".to_string())
    );

    client.apply_lane_config_overlay(LaneConfigOverlay {
        extract: LaneFieldOverlay {
            base_url: Some("https://api.siliconflow.cn/v1/chat/completions".to_string()),
            model: Some("siliconflow-model".to_string()),
        },
        ..LaneConfigOverlay::default()
    });
    let error = client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect_err("cross-provider overlay must fail closed");
    assert!(
        error.contains("refusing credential-bearing request"),
        "unexpected fail-closed error: {error}"
    );
}

/// Selection provenance must outlive env construction: a Vault overlay cannot
/// downgrade the selected provider's pinned HTTPS/default-port transport, even
/// when the overlay retains the provider's exact host. The capture resolver
/// makes any accidental request observable without contacting a real provider.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn selected_provider_rejects_http_same_host_overlay_before_request() {
    use axum::{routing::any, Json, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();
    let _selector = EnvRestore::set("EXTRACT_PROVIDER", "deepseek");
    let _canonical = EnvRestore::set("DEEPSEEK_API_KEY", "deepseek-canonical-key");
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new().fallback(any({
        let requests = Arc::clone(&requests);
        move || {
            let requests = Arc::clone(&requests);
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                Json(serde_json::json!({"choices": []}))
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind capture provider");
    let addr = listener.local_addr().expect("capture provider address");
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve capture provider");
    });

    let client = LlmClient::new().expect("selected lane should construct");
    client.replace_http_client_for_tests(
        LlmClient::http_client_with_host_resolved_for_tests(DEEPSEEK_AUTH_PROBE.host, addr)
            .expect("resolved DeepSeek test client"),
    );
    client.apply_lane_config_overlay(LaneConfigOverlay {
        extract: LaneFieldOverlay {
            base_url: Some("http://api.deepseek.com/chat/completions".to_string()),
            model: None,
        },
        ..LaneConfigOverlay::default()
    });

    let error = client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect_err("selected-provider HTTP overlay must fail before request");
    assert!(
        error.contains("https://api.deepseek.com")
            && error.contains("refusing credential-bearing request"),
        "unexpected fail-closed error: {error}"
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "bad selected-provider endpoint must never receive a request"
    );
    server.abort();
}

/// A SiliconFlow-only key pool cannot serve — or be sent to — a
/// DeepSeek-selected lane: the call fails with a clear missing-key error and
/// zero outbound requests, and the error never echoes the other provider's
/// secret value.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn siliconflow_key_alone_cannot_serve_a_deepseek_selected_lane() {
    use axum::{routing::any, Json, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();
    let _selector = EnvRestore::set("EXTRACT_PROVIDER", "deepseek");
    let _sf_key = EnvRestore::set("SILICONFLOW_API_KEY", "siliconflow-test-key");

    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new().fallback(any({
        let requests = Arc::clone(&requests);
        move || {
            let requests = Arc::clone(&requests);
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                Json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "must-not-happen"},
                        "finish_reason": "stop"
                    }]
                }))
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind unreachable capture provider");
    let addr = listener
        .local_addr()
        .expect("unreachable capture provider addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve unreachable capture provider");
    });

    let client = LlmClient::new().expect("selector without canonical key should still construct");
    let extract = client.lane(ChatLane::Extract);
    assert_eq!(extract.api_key_envs, vec!["DEEPSEEK_API_KEY"]);
    assert!(client
        .provider_key_id_for_tests(&extract.api_key_envs)
        .is_none());
    client.replace_http_client_for_tests(
        LlmClient::http_client_with_host_resolved_for_tests(DEEPSEEK_AUTH_PROBE.host, addr)
            .expect("resolved DeepSeek test client"),
    );

    let error = client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect_err("missing canonical key must fail without a provider request");
    assert!(
        error.contains("Missing API key"),
        "unexpected unavailable error: {error}"
    );
    assert!(
        !error.contains("siliconflow-test-key"),
        "unavailable error must not echo another provider's secret value: {error}"
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "no request may leave the process without the canonical key"
    );
    server.abort();
}

#[test]
fn invalid_provider_selector_fails_closed() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();
    {
        let _selector = EnvRestore::set("EXTRACT_PROVIDER", "zai");
        let error = ProviderRuntimeConfig::from_env()
            .expect_err("non-front-line provider kind must be rejected");
        assert!(
            error.contains("EXTRACT_PROVIDER")
                && error.contains("deepseek|siliconflow")
                && error.contains("'zai'"),
            "unexpected rejection error: {error}"
        );
    }
    let _selector = EnvRestore::set("SUMMARY_PROVIDER", "openai");
    let error = ProviderRuntimeConfig::from_env().expect_err("unknown provider must be rejected");
    assert!(
        error.contains("SUMMARY_PROVIDER") && error.contains("deepseek|siliconflow"),
        "unexpected rejection error: {error}"
    );
}

#[test]
fn unset_or_empty_selector_preserves_legacy_alias_chain() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();
    let _extract_key = EnvRestore::set("EXTRACT_API_KEY", "legacy-extract-key");

    let client = LlmClient::new().expect("legacy chain should construct");
    let extract = client.lane(ChatLane::Extract);
    assert_eq!(
        extract.api_key_envs,
        vec!["EXTRACT_API_KEY", "SILICONFLOW_API_KEY"]
    );
    assert_eq!(
        extract.base_url,
        "https://api.siliconflow.cn/v1/chat/completions"
    );
    assert_eq!(extract.model, "Qwen/Qwen3.5-27B");
    assert_eq!(
        client.provider_key_id_for_tests(&extract.api_key_envs),
        Some("EXTRACT_API_KEY".to_string())
    );

    let _empty_selector = EnvRestore::set("EXTRACT_PROVIDER", "   ");
    let config = ProviderRuntimeConfig::from_env().expect("empty selector behaves as unset");
    assert_eq!(
        config.extract.api_key_envs,
        vec!["EXTRACT_API_KEY", "SILICONFLOW_API_KEY"]
    );
}

#[test]
fn model_override_keeps_selected_provider_boundary() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();
    let _selector = EnvRestore::set("EXTRACT_PROVIDER", "deepseek");
    let _canonical = EnvRestore::set("DEEPSEEK_API_KEY", "deepseek-canonical-key");
    {
        let _extract_model = EnvRestore::set("EXTRACT_MODEL", "custom-deepseek-model");
        let config = ProviderRuntimeConfig::from_env().expect("lane model override should resolve");
        assert_eq!(config.extract.model, "custom-deepseek-model");
        assert_eq!(config.extract.base_url, DEEPSEEK_AUTH_PROBE.chat.base_url);
        assert_eq!(config.extract.api_key_envs, vec!["DEEPSEEK_API_KEY"]);
    }
    let _provider_model = EnvRestore::set("DEEPSEEK_MODEL", "provider-canonical-model");
    let config = ProviderRuntimeConfig::from_env().expect("provider model should resolve");
    assert_eq!(config.extract.model, "provider-canonical-model");
    assert_eq!(config.extract.base_url, DEEPSEEK_AUTH_PROBE.chat.base_url);
    assert_eq!(config.extract.api_key_envs, vec!["DEEPSEEK_API_KEY"]);
}

#[test]
fn siliconflow_selector_uses_canonical_sf_key_and_endpoint() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();
    let _selector = EnvRestore::set("EXTRACT_PROVIDER", "siliconflow");
    let _stale_deepseek = EnvRestore::set("DEEPSEEK_API_KEY", "stale-deepseek-key");
    let _sf_key = EnvRestore::set("SILICONFLOW_API_KEY", "siliconflow-canonical-key");

    let client = LlmClient::new().expect("explicit SiliconFlow extract lane should construct");
    let extract = client.lane(ChatLane::Extract);
    assert_eq!(
        extract.base_url,
        "https://api.siliconflow.cn/v1/chat/completions"
    );
    assert_eq!(extract.model, "Qwen/Qwen3.5-27B");
    assert_eq!(extract.api_key_envs, vec!["SILICONFLOW_API_KEY"]);
    assert_eq!(
        client.provider_key_id_for_tests(&extract.api_key_envs),
        Some("SILICONFLOW_API_KEY".to_string())
    );
    assert_eq!(
        SILICONFLOW_AUTH_PROBE.host, "api.siliconflow.cn",
        "descriptor table drifted; update the selector expectations"
    );

    // The host pin is symmetric: a SiliconFlow-selected lane refuses a
    // DeepSeek endpoint just as a DeepSeek-selected lane refuses SiliconFlow.
    let _deepseek_base = EnvRestore::set(
        "EXTRACT_BASE_URL",
        "https://api.deepseek.com/chat/completions",
    );
    let error = ProviderRuntimeConfig::from_env()
        .expect_err("DeepSeek endpoint must be refused for a SiliconFlow-selected lane");
    assert!(
        error.contains("EXTRACT_PROVIDER")
            && error.contains("api.siliconflow.cn")
            && error.contains("api.deepseek.com")
            && error.contains("refusing credential-bearing request"),
        "unexpected rejection error: {error}"
    );
}

/// A selected provider's endpoint is https-only on the default port — the
/// auth-probe posture applied to the opt-in selector path. Plaintext http
/// and non-443 ports are refused at construction for both selectable
/// providers, whether or not any key is configured (refusal precedes any
/// credential-bearing request); implicit-443 and explicit-`:443` https on
/// the exact host are accepted.
#[test]
fn selected_provider_endpoint_requires_https_and_default_port() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();

    let cases = [
        ("deepseek", "DEEPSEEK_API_KEY", "api.deepseek.com"),
        ("siliconflow", "SILICONFLOW_API_KEY", "api.siliconflow.cn"),
    ];
    for (provider, canonical_key, host) in cases {
        // Existing canonical key: the refusal must fire before the key could
        // ever be attached to a request.
        let _selector = EnvRestore::set("EXTRACT_PROVIDER", provider);
        let _canonical = EnvRestore::set(canonical_key, "canonical-synthetic-key");
        for rejected in [
            format!("http://{host}/chat/completions"),
            format!("https://{host}:8443/chat/completions"),
        ] {
            let _base = EnvRestore::set("EXTRACT_BASE_URL", &rejected);
            let error = ProviderRuntimeConfig::from_env()
                .err()
                .unwrap_or_else(|| panic!("{provider}-selected lane must refuse {rejected}"));
            assert!(
                error.contains("EXTRACT_PROVIDER")
                    && error.contains(host)
                    && error.contains("refusing credential-bearing request"),
                "unexpected rejection error for {rejected}: {error}"
            );
            assert!(
                !error.contains("canonical-synthetic-key"),
                "endpoint refusal must not echo credential material: {error}"
            );
        }
        for accepted in [
            format!("https://{host}/chat/completions"),
            format!("https://{host}:443/chat/completions"),
        ] {
            let _base = EnvRestore::set("EXTRACT_BASE_URL", &accepted);
            let config = ProviderRuntimeConfig::from_env()
                .unwrap_or_else(|_| panic!("{provider}-selected lane should accept {accepted}"));
            assert_eq!(config.extract.base_url, accepted);
            assert_eq!(config.extract.api_key_envs, vec![canonical_key]);
        }
        drop(_canonical);

        // Missing-key state: same refusals, key-independent.
        for rejected in [
            format!("http://{host}/chat/completions"),
            format!("https://{host}:8443/chat/completions"),
        ] {
            let _base = EnvRestore::set("EXTRACT_BASE_URL", &rejected);
            let error = ProviderRuntimeConfig::from_env().err().unwrap_or_else(|| {
                panic!("keyless {provider}-selected lane must still refuse {rejected}")
            });
            assert!(
                error.contains("refusing credential-bearing request"),
                "unexpected rejection error for {rejected}: {error}"
            );
        }
        drop(_selector);
    }
}

/// Credential material smuggled inside the endpoint URL must be refused by
/// the existing downstream constructor guard (userinfo / credential-shaped
/// query key), and no refusal may echo the smuggled material.
#[test]
fn selected_provider_endpoint_refusal_carries_no_url_credential_material() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();
    let _selector = EnvRestore::set("EXTRACT_PROVIDER", "deepseek");
    let _canonical = EnvRestore::set("DEEPSEEK_API_KEY", "deepseek-canonical-key");

    {
        let _userinfo = EnvRestore::set(
            "EXTRACT_BASE_URL",
            "https://ak-user:supersecret-pw@api.deepseek.com/chat/completions",
        );
        let error = LlmClient::new()
            .err()
            .expect("userinfo-bearing endpoint must be refused at construction");
        assert!(
            error.contains("endpoint refused") && error.contains("userinfo"),
            "unexpected refusal error: {error}"
        );
        assert!(
            !error.contains("supersecret-pw") && !error.contains("ak-user"),
            "userinfo refusal must not echo credential material: {error}"
        );
    }
    let _query = EnvRestore::set(
        "EXTRACT_BASE_URL",
        "https://api.deepseek.com/chat/completions?api_key=sk-url-smuggled-token",
    );
    let error = LlmClient::new()
        .err()
        .expect("credential-shaped query key must be refused at construction");
    assert!(
        error.contains("endpoint refused") && error.contains("api_key"),
        "unexpected refusal error: {error}"
    );
    assert!(
        !error.contains("sk-url-smuggled-token"),
        "query refusal must not echo credential material: {error}"
    );
}

/// Finding: an explicitly DeepSeek-selected front-line lane must not
/// synthesize #1197's automatic DeepSeek fallback — that would duplicate the
/// primary provider and its canonical key pool. The suppression holds for
/// the production env constructor and survives later Vault materialization
/// of the canonical pool (no delayed synthesis can resurrect the duplicate).
#[test]
fn explicit_deepseek_selection_suppresses_automatic_same_provider_fallback() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();
    let _extract_selector = EnvRestore::set("EXTRACT_PROVIDER", "deepseek");
    let _summary_selector = EnvRestore::set("SUMMARY_PROVIDER", "deepseek");
    let _canonical = EnvRestore::set("DEEPSEEK_API_KEY", "deepseek-canonical-key");

    let client = LlmClient::new().expect("selected DeepSeek lanes should construct");
    assert_eq!(
        client.provider_key_id_for_tests(&client.lane(ChatLane::Extract).api_key_envs),
        Some("DEEPSEEK_API_KEY".to_string()),
        "primary extract tier stays canonical"
    );
    assert!(
        client.fallback_lane(ChatLane::Extract).is_none(),
        "no automatic same-provider fallback may duplicate the DeepSeek primary"
    );
    assert!(
        client.fallback_lane(ChatLane::Summary).is_none(),
        "no automatic same-provider fallback may duplicate the DeepSeek primary"
    );

    // Delayed canonical-pool materialization (Vault-only key, canonical env
    // absent): the primary selects the pool, and the fallback stays absent.
    let _remove_env_key = EnvRestore::unset("DEEPSEEK_API_KEY");
    let client = LlmClient::new().expect("selector without env key should still construct");
    let report =
        materialize_provider_secrets_from_durable_source(&client, ["DEEPSEEK_API_KEY"], || {
            Ok(DurableVaultLoad::from_pools(
                [(
                    "DEEPSEEK_API_KEY".to_string(),
                    vec![ProviderSecret {
                        key_id: "DEEPSEEK_API_KEY".to_string(),
                        value: "vault-deepseek-key".to_string(),
                    }],
                )]
                .into_iter()
                .collect(),
                crate::VaultSourceAvailability::Readable,
            ))
        })
        .expect("durable-source materialization of the canonical pool");
    assert_eq!(report.loaded, 1);
    assert_eq!(
        client.provider_key_id_for_tests(&client.lane(ChatLane::Extract).api_key_envs),
        Some("DEEPSEEK_API_KEY".to_string()),
        "materialized canonical pool serves the primary tier"
    );
    assert!(
        client.fallback_lane(ChatLane::Extract).is_none()
            && client.fallback_lane(ChatLane::Summary).is_none(),
        "Vault materialization must not resurrect the suppressed duplicate fallback"
    );
}

/// Suppression removes only the automatic convenience entry. An explicit,
/// independent `*_FALLBACK_API_KEY` tier (different provider, own URL/model)
/// survives a DeepSeek selection as configured operator intent.
#[test]
fn explicit_independent_fallback_survives_deepseek_selection() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();
    let _extract_selector = EnvRestore::set("EXTRACT_PROVIDER", "deepseek");
    let _canonical = EnvRestore::set("DEEPSEEK_API_KEY", "deepseek-canonical-key");
    let _explicit_key = EnvRestore::set("EXTRACT_FALLBACK_API_KEY", "zai-fallback-key");
    let _explicit_base = EnvRestore::set(
        "EXTRACT_FALLBACK_BASE_URL",
        "https://api.z.ai/api/paas/v4/chat/completions",
    );
    let _explicit_model = EnvRestore::set("EXTRACT_FALLBACK_MODEL", "glm-4.5");

    let client = LlmClient::new().expect("explicit independent fallback should construct");
    let fallback = client
        .fallback_lane(ChatLane::Extract)
        .expect("explicit EXTRACT_FALLBACK_API_KEY intent must survive");
    assert_eq!(fallback.api_key_envs, vec!["EXTRACT_FALLBACK_API_KEY"]);
    assert_eq!(
        fallback.base_url,
        "https://api.z.ai/api/paas/v4/chat/completions"
    );
    assert_eq!(fallback.model, "glm-4.5");
}

/// The suppression is same-provider only: a SiliconFlow-selected lane keeps
/// #1197's automatic *cross-provider* DeepSeek fallback, and the legacy
/// unset-selector behavior is unchanged.
#[test]
fn cross_provider_and_legacy_automatic_fallbacks_are_unchanged() {
    let _guard = crate::test_support::global_test_lock().lock();
    let _env = frontline_selector_env_guards();
    let _canonical = EnvRestore::set("DEEPSEEK_API_KEY", "deepseek-fallback-key");

    {
        let _selector = EnvRestore::set("EXTRACT_PROVIDER", "siliconflow");
        let client = LlmClient::new().expect("SiliconFlow-selected lane should construct");
        let fallback = client
            .fallback_lane(ChatLane::Extract)
            .expect("cross-provider automatic fallback is #1197 intent, not a duplicate");
        assert_eq!(
            fallback.base_url,
            "https://api.deepseek.com/chat/completions"
        );
        assert_eq!(
            fallback.api_key_envs,
            vec!["EXTRACT_FALLBACK_API_KEY", "DEEPSEEK_API_KEY"]
        );
    }
    // Legacy: no selector at all.
    let client = LlmClient::new().expect("legacy chain should construct");
    let fallback = client
        .fallback_lane(ChatLane::Extract)
        .expect("legacy automatic DeepSeek fallback must keep resolving");
    assert_eq!(
        fallback.api_key_envs,
        vec!["EXTRACT_FALLBACK_API_KEY", "DEEPSEEK_API_KEY"]
    );
}
