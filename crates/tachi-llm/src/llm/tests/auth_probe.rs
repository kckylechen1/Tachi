use super::*;
use crate::llm::{
    ChatLaneConfig, ProviderAuthProbeClass, ProviderAuthProbeFamily, ProviderRuntimeConfig,
};
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header::AUTHORIZATION, Method, StatusCode},
    response::Response,
    routing::any,
    Router,
};
use std::sync::{Arc, Mutex};

const TEST_KEY_NAME: &str = "__1059_AUTH_PROBE_TEST_KEY";
type RequestObservations = Arc<Mutex<Vec<(Method, String, usize, bool)>>>;

#[derive(Clone)]
struct MockState {
    observations: RequestObservations,
    status: StatusCode,
    body: &'static str,
    redirect_to: Option<&'static str>,
}

async fn mock_handler(State(state): State<MockState>, request: Request) -> Response<Body> {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let auth_ok = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        == Some("Bearer synthetic-probe-key");
    let body = to_bytes(request.into_body(), 1024)
        .await
        .expect("bounded request body");
    state
        .observations
        .lock()
        .expect("observations lock")
        .push((method, path, body.len(), auth_ok));

    let mut response = Response::builder().status(state.status);
    if let Some(location) = state.redirect_to {
        response = response.header("location", location);
    }
    response
        .body(Body::from(state.body))
        .expect("mock response")
}

async fn start_mock(
    status: StatusCode,
    body: &'static str,
    redirect_to: Option<&'static str>,
) -> (String, RequestObservations) {
    let observations = Arc::new(Mutex::new(Vec::new()));
    let state = MockState {
        observations: Arc::clone(&observations),
        status,
        body,
        redirect_to,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let address = listener.local_addr().expect("mock address");
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().fallback(any(mock_handler)).with_state(state),
        )
        .await
        .expect("serve mock");
    });
    (format!("http://{address}"), observations)
}

fn lane(base_url: &str, model: &str) -> ChatLaneConfig {
    ChatLaneConfig {
        base_url: base_url.to_string(),
        model: model.to_string(),
        api_key_envs: vec![TEST_KEY_NAME],
    }
}

fn client(reasoning: ChatLaneConfig) -> LlmClient {
    let unused = lane("https://unused.invalid/v1/chat/completions", "unused");
    let client = LlmClient::new_with_config(
        ProviderRuntimeConfig {
            extract: unused.clone(),
            summary: unused.clone(),
            reasoning,
            distill: unused,
            rerank: RerankConfig {
                provider: RerankProviderKind::Voyage,
                local_endpoint: None,
            },
        },
        None,
    )
    .expect("construct client");
    assert!(client.set_provider_secret(TEST_KEY_NAME, "synthetic-probe-key"));
    client
}

#[tokio::test]
async fn deepseek_probe_is_one_exact_bodyless_get_and_finds_model() {
    let (server, observations) = start_mock(
        StatusCode::OK,
        r#"{"data":[{"id":"deepseek-chat"},{"id":"deepseek-reasoner"}]}"#,
        None,
    )
    .await;
    let client = client(lane(
        "https://api.deepseek.com/chat/completions",
        "deepseek-reasoner",
    ));
    let cursor_before = client
        .provider_state
        .read()
        .expect("provider state")
        .indices
        .get(TEST_KEY_NAME)
        .copied();

    let result = client
        .probe_reasoning_auth_with_endpoint_for_tests(&format!("{server}/models"))
        .await;

    assert_eq!(result.provider_family, ProviderAuthProbeFamily::DeepSeek);
    assert_eq!(result.provider_host, "api.deepseek.com");
    assert_eq!(result.effective_model, "deepseek-reasoner");
    assert_eq!(result.auth_class, ProviderAuthProbeClass::AuthOk);
    assert_eq!(result.selected_model_present, Some(true));
    assert_eq!(result.model_count, Some(2));
    assert_eq!(
        *observations.lock().expect("observations"),
        vec![(Method::GET, "/models".to_string(), 0, true)]
    );
    assert_eq!(
        client
            .provider_state
            .read()
            .expect("provider state")
            .indices
            .get(TEST_KEY_NAME)
            .copied(),
        cursor_before,
        "probe must not rotate the provider credential cursor"
    );
}

#[tokio::test]
async fn siliconflow_probe_reports_absent_model_without_dumping_ids() {
    let (server, observations) = start_mock(
        StatusCode::OK,
        r#"{"data":[{"id":"public/model-a"}]}"#,
        None,
    )
    .await;
    let client = client(lane(
        "https://api.siliconflow.cn/v1/chat/completions",
        "public/model-b",
    ));

    let result = client
        .probe_reasoning_auth_with_endpoint_for_tests(&format!("{server}/v1/models"))
        .await;
    let safe_json = serde_json::to_string(&result).expect("safe result JSON");

    assert_eq!(result.provider_family, ProviderAuthProbeFamily::SiliconFlow);
    assert_eq!(result.provider_host, "api.siliconflow.cn");
    assert_eq!(result.auth_class, ProviderAuthProbeClass::AuthOk);
    assert_eq!(result.selected_model_present, Some(false));
    assert_eq!(result.model_count, Some(1));
    assert!(!safe_json.contains("public/model-a"), "{safe_json}");
    assert_eq!(observations.lock().expect("observations").len(), 1);
}

#[tokio::test]
async fn hostile_auth_body_is_redacted_and_never_retried() {
    let hostile = "echo synthetic-probe-key and private source text";
    let (server, observations) = start_mock(StatusCode::UNAUTHORIZED, hostile, None).await;
    let client = client(lane(
        "https://api.deepseek.com/chat/completions",
        "deepseek-reasoner",
    ));

    let result = client
        .probe_reasoning_auth_with_endpoint_for_tests(&format!("{server}/models"))
        .await;
    let safe_json = serde_json::to_string(&result).expect("safe result JSON");

    assert_eq!(result.auth_class, ProviderAuthProbeClass::AuthFailed);
    assert_eq!(observations.lock().expect("observations").len(), 1);
    assert!(!safe_json.contains("synthetic-probe-key"), "{safe_json}");
    assert!(!safe_json.contains("private source text"), "{safe_json}");
}

#[tokio::test]
async fn redirect_is_refused_without_following_or_retrying() {
    let (server, observations) = start_mock(
        StatusCode::FOUND,
        "hostile redirect body",
        Some("/followed"),
    )
    .await;
    let client = client(lane(
        "https://api.siliconflow.cn/v1/chat/completions",
        "public/model",
    ));

    let result = client
        .probe_reasoning_auth_with_endpoint_for_tests(&format!("{server}/v1/models"))
        .await;

    assert_eq!(result.auth_class, ProviderAuthProbeClass::RedirectRefused);
    assert_eq!(observations.lock().expect("observations").len(), 1);
}

#[tokio::test]
async fn zai_is_unsupported_without_any_network_request() {
    let (server, observations) = start_mock(StatusCode::OK, r#"{"data":[]}"#, None).await;
    let client = client(lane(
        "https://open.bigmodel.cn/api/paas/v4/chat/completions",
        "glm-4.5",
    ));

    let result = client
        .probe_reasoning_auth_with_endpoint_for_tests(&format!("{server}/must-not-be-called"))
        .await;

    assert_eq!(result.provider_family, ProviderAuthProbeFamily::ZaiBigModel);
    assert_eq!(
        result.auth_class,
        ProviderAuthProbeClass::UnsupportedNoDocumentedProbe
    );
    assert!(observations.lock().expect("observations").is_empty());
}

#[tokio::test]
async fn documented_statuses_map_without_retry() {
    for (status, expected) in [
        (StatusCode::FORBIDDEN, ProviderAuthProbeClass::AuthFailed),
        (
            StatusCode::PAYMENT_REQUIRED,
            ProviderAuthProbeClass::ProviderExhausted,
        ),
        (
            StatusCode::TOO_MANY_REQUESTS,
            ProviderAuthProbeClass::RateLimited,
        ),
        (StatusCode::BAD_GATEWAY, ProviderAuthProbeClass::Transient),
    ] {
        let (server, observations) = start_mock(status, "must remain private", None).await;
        let client = client(lane(
            "https://api.deepseek.com/chat/completions",
            "deepseek-reasoner",
        ));
        let result = client
            .probe_reasoning_auth_with_endpoint_for_tests(&format!("{server}/models"))
            .await;
        assert_eq!(result.auth_class, expected);
        assert_eq!(observations.lock().expect("observations").len(), 1);
    }
}

#[tokio::test]
async fn malformed_success_body_fails_closed_without_body_disclosure() {
    let hostile = "not JSON; echo private source text";
    let (server, observations) = start_mock(StatusCode::OK, hostile, None).await;
    let client = client(lane(
        "https://api.deepseek.com/chat/completions",
        "deepseek-reasoner",
    ));

    let result = client
        .probe_reasoning_auth_with_endpoint_for_tests(&format!("{server}/models"))
        .await;
    let safe_json = serde_json::to_string(&result).expect("safe result JSON");

    assert_eq!(result.auth_class, ProviderAuthProbeClass::MalformedResponse);
    assert!(!safe_json.contains("private source text"), "{safe_json}");
    assert_eq!(observations.lock().expect("observations").len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn ambient_proxy_cannot_receive_probe_bearer_or_own_transport() {
    let _env_lock = crate::test_support::global_test_lock().lock();
    let (intended_server, intended_observations) = start_mock(
        StatusCode::OK,
        r#"{"data":[{"id":"deepseek-reasoner"}]}"#,
        None,
    )
    .await;
    let (proxy_trap, proxy_observations) = start_mock(
        StatusCode::BAD_GATEWAY,
        "proxy must not receive request",
        None,
    )
    .await;
    let intended_url = reqwest::Url::parse(&intended_server).expect("intended mock URL");
    let intended_address = std::net::SocketAddr::from((
        std::net::Ipv4Addr::LOCALHOST,
        intended_url.port().expect("intended mock port"),
    ));
    let endpoint = format!("http://auth-probe.test:{}/models", intended_address.port());
    let client = client(lane(
        "https://api.deepseek.com/chat/completions",
        "deepseek-reasoner",
    ));
    let _proxy_env = [
        EnvRestore::set("HTTP_PROXY", &proxy_trap),
        EnvRestore::set("HTTPS_PROXY", &proxy_trap),
        EnvRestore::set("ALL_PROXY", &proxy_trap),
        EnvRestore::set("http_proxy", &proxy_trap),
        EnvRestore::set("https_proxy", &proxy_trap),
        EnvRestore::set("all_proxy", &proxy_trap),
        EnvRestore::unset("NO_PROXY"),
        EnvRestore::unset("no_proxy"),
    ];

    let result = client
        .probe_reasoning_auth_with_direct_endpoint_for_tests(
            &endpoint,
            "auth-probe.test",
            intended_address,
        )
        .await;

    assert!(
        proxy_observations.lock().expect("proxy trap").is_empty(),
        "credential-bearing probe must bypass every ambient proxy"
    );
    assert_eq!(result.auth_class, ProviderAuthProbeClass::AuthOk);
    assert_eq!(
        *intended_observations.lock().expect("intended"),
        vec![(Method::GET, "/models".to_string(), 0, true)]
    );
}
