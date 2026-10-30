use super::*;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::any;
use axum::{Json, Router};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct RequestSeen {
    method: Method,
    peer: SocketAddr,
    headers: HeaderMap,
    body: Bytes,
}

#[derive(Clone, Default)]
struct MockState {
    requests: Arc<Mutex<Vec<RequestSeen>>>,
    head_seen: Arc<tokio::sync::Notify>,
    release_head: Option<Arc<tokio::sync::Notify>>,
}

struct Mock {
    endpoint: String,
    state: MockState,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn mock_provider(stall_head: bool) -> Mock {
    async fn handle(
        State(state): State<MockState>,
        ConnectInfo(peer): ConnectInfo<SocketAddr>,
        method: Method,
        headers: HeaderMap,
        body: Bytes,
    ) -> (StatusCode, Json<Value>) {
        state.requests.lock().unwrap().push(RequestSeen {
            method: method.clone(),
            peer,
            headers,
            body,
        });
        if method == Method::HEAD {
            state.head_seen.notify_one();
            if let Some(release) = &state.release_head {
                release.notified().await;
            }
            // An unauthenticated response warms the connection, but must not
            // mark the real credential as rejected.
            return (StatusCode::UNAUTHORIZED, Json(json!({"error": "no key"})));
        }
        (
            StatusCode::OK,
            Json(json!({"data": [{"index": 0, "embedding": vec![0.0_f32; 1024]}]})),
        )
    }

    let state = MockState {
        release_head: stall_head.then(|| Arc::new(tokio::sync::Notify::new())),
        ..MockState::default()
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/embeddings", any(handle))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    Mock {
        endpoint,
        state,
        task,
    }
}

fn env() -> Vec<EnvRestore> {
    vec![
        EnvRestore::unset("VOYAGE_API_KEY"),
        EnvRestore::unset("TACHI_EMBEDDING_MODEL"),
        EnvRestore::unset("TACHI_EMBEDDING_DIM"),
        EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1"),
        EnvRestore::set("TACHI_RECALL_PROVIDER_ATTEMPTS", "1"),
        EnvRestore::set("TACHI_QUERY_EMBEDDING_CACHE", "1"),
    ]
}

fn client_with_key() -> LlmClient {
    let client = LlmClient::new().unwrap();
    client.set_provider_secret_pool(
        "VOYAGE_API_KEY",
        vec![
            ProviderSecret {
                key_id: "prewarm-first".into(),
                value: "test-prewarm-first-key".into(),
            },
            ProviderSecret {
                key_id: "prewarm-second".into(),
                value: "test-prewarm-second-key".into(),
            },
        ],
    );
    client
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn voyage_prewarm_is_once_credential_free_and_reuses_the_embedding_connection() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _env = env();
    let mock = mock_provider(false).await;
    let _base = EnvRestore::set("VOYAGE_BASE_URL", &mock.endpoint);
    let client = client_with_key();
    client.note_recall_provider_outcome(true);
    let health_before = serde_json::to_value(client.provider_health_status()).unwrap();

    let clone = client.clone();
    tokio::join!(
        client.prewarm_voyage_connection(),
        clone.prewarm_voyage_connection()
    );
    client.prewarm_voyage_connection().await;
    assert_eq!(client.recall_timeout_streak_for_tests(), 1);
    assert_eq!(
        serde_json::to_value(client.provider_health_status()).unwrap(),
        health_before,
        "a transport warmup is not an authentication or health probe"
    );
    assert_eq!(
        client.required_secret(&["VOYAGE_API_KEY"]).unwrap(),
        "test-prewarm-first-key",
        "warmup must not advance key rotation"
    );

    let first = client
        .embed_voyage("actual first query", "query")
        .await
        .unwrap();
    let repeat = client
        .embed_voyage("actual first query", "query")
        .await
        .unwrap();
    assert_eq!(first, repeat);
    let requests = mock.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "one HEAD plus one real embedding POST");
    assert_eq!(requests[0].method, Method::HEAD);
    assert!(!requests[0].headers.contains_key(AUTHORIZATION));
    assert!(requests[0].body.is_empty());
    assert_eq!(requests[1].method, Method::POST);
    assert!(requests[1].headers.contains_key(AUTHORIZATION));
    assert_eq!(
        requests[0].peer, requests[1].peer,
        "same actual TCP connection"
    );
    let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(body["input"], json!(["actual first query"]));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn voyage_prewarm_skips_missing_alias_invalid_model_and_credential_urls() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _env = env();
    let mock = mock_provider(false).await;
    let _base = EnvRestore::set("VOYAGE_BASE_URL", &mock.endpoint);
    LlmClient::new().unwrap().prewarm_voyage_connection().await;
    {
        let _alias = EnvRestore::set("VOYAGE_API_KEY", "vault://voyage");
        LlmClient::new().unwrap().prewarm_voyage_connection().await;
    }
    {
        let _model = EnvRestore::set("TACHI_EMBEDDING_MODEL", "undeclared-width");
        client_with_key().prewarm_voyage_connection().await;
    }
    for base in [
        mock.endpoint.replacen("http://", "http://user:secret@", 1),
        mock.endpoint
            .replacen("http://", "http:////user:secret@", 1),
        format!("{}/?api_key=must-not-be-sent", mock.endpoint),
    ] {
        let _unsafe_base = EnvRestore::set("VOYAGE_BASE_URL", base);
        client_with_key().prewarm_voyage_connection().await;
    }
    assert!(mock.state.requests.lock().unwrap().is_empty());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn voyage_prewarm_timeout_is_bounded_without_retry_or_health_mutation() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _env = env();
    let mock = mock_provider(true).await;
    let _base = EnvRestore::set("VOYAGE_BASE_URL", &mock.endpoint);
    let client = client_with_key();
    let health_before = serde_json::to_value(client.provider_health_status()).unwrap();
    // Five times the request budget allows scheduler noise while detecting
    // accidental reuse of the ordinary client-wide 60-second timeout.
    tokio::time::timeout(Duration::from_secs(15), client.prewarm_voyage_connection())
        .await
        .expect("warmup must respect its short request budget");
    client.prewarm_voyage_connection().await;
    assert_eq!(mock.state.requests.lock().unwrap().len(), 1);
    assert_eq!(client.recall_timeout_streak_for_tests(), 0);
    assert_eq!(
        serde_json::to_value(client.provider_health_status()).unwrap(),
        health_before
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn voyage_prewarm_in_flight_does_not_hold_up_a_real_query() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _env = env();
    let mock = mock_provider(true).await;
    let _base = EnvRestore::set("VOYAGE_BASE_URL", &mock.endpoint);
    let client = client_with_key();
    let warm_client = client.clone();
    let warmup = tokio::spawn(async move { warm_client.prewarm_voyage_connection().await });
    tokio::time::timeout(Duration::from_secs(15), mock.state.head_seen.notified())
        .await
        .expect("warmup request reached provider");
    let vector = client
        .embed_voyage("query during stalled HEAD", "query")
        .await
        .unwrap();
    assert_eq!(vector.len(), 1024);
    // The provider still has not replied to HEAD. Do not require the POST to
    // beat the warmup's real-time deadline on a loaded runner.
    mock.state.release_head.as_ref().unwrap().notify_one();
    warmup.await.unwrap();
    let requests = mock.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_ne!(
        requests[0].peer, requests[1].peer,
        "query may use another connection"
    );
}
