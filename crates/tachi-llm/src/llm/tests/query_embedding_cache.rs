//! #2109 — Voyage query-embedding LRU on the search hot path.

use super::*;

use crate::llm::provider_health::{ChatLaneConfig, ProviderRuntimeConfig};
use crate::{RerankConfig, RerankProviderKind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn unused_lane() -> ChatLaneConfig {
    ChatLaneConfig {
        base_url: "https://unused.test/v1/chat/completions".to_string(),
        model: "unused".to_string(),
        api_key_envs: vec!["UNUSED_API_KEY"],
    }
}

fn test_config() -> ProviderRuntimeConfig {
    ProviderRuntimeConfig {
        extract: unused_lane(),
        summary: unused_lane(),
        reasoning: unused_lane(),
        distill: unused_lane(),
        rerank: RerankConfig {
            provider: RerankProviderKind::Voyage,
            local_endpoint: None,
        },
    }
}

fn install_voyage_key(client: &LlmClient) {
    client.set_provider_secret_pool(
        "VOYAGE_API_KEY",
        vec![ProviderSecret {
            key_id: "VOYAGE_API_KEY_1".to_string(),
            value: "test-voyage-key".to_string(),
        }],
    );
}

/// Mock Voyage `/v1/embeddings` that returns 1024-d zeros and counts hits.
async fn spawn_embed_counter() -> (u16, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    use axum::{routing::post, Json, Router};

    let hits = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&hits);
    let app = Router::new().route(
        "/v1/embeddings",
        post({
            let observed = Arc::clone(&observed);
            move |body: Json<serde_json::Value>| {
                let observed = Arc::clone(&observed);
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    let inputs = body
                        .0
                        .get("input")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(1);
                    let data: Vec<serde_json::Value> = (0..inputs)
                        .map(|index| {
                            json!({
                                "index": index,
                                "embedding": vec![0.0_f32; 1024],
                            })
                        })
                        .collect();
                    Json(json!({ "data": data }))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind query-embed mock");
    let port = listener.local_addr().expect("addr").port();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock serve");
    });
    (port, hits, server)
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn second_identical_query_embed_skips_network() {
    let (port, hits, server) = spawn_embed_counter().await;

    let _guard = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
    let _attempts = EnvRestore::set("TACHI_RECALL_PROVIDER_ATTEMPTS", "1");
    let _model = EnvRestore::unset("TACHI_EMBEDDING_MODEL");
    let _dimension = EnvRestore::unset("TACHI_EMBEDDING_DIM");
    let _cache_on = EnvRestore::unset("TACHI_QUERY_EMBEDDING_CACHE");
    let _cache_cap = EnvRestore::unset("TACHI_QUERY_EMBEDDING_CACHE_CAPACITY");
    let _base = EnvRestore::set("VOYAGE_BASE_URL", format!("http://127.0.0.1:{port}"));

    let client = LlmClient::new_with_config(test_config(), None).expect("client");
    install_voyage_key(&client);

    let q = "cache-hit probe alpha";
    let first = client
        .embed_voyage(q, "query")
        .await
        .expect("first query embed");
    assert_eq!(first.len(), 1024);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "first call must hit network"
    );

    let second = client
        .embed_voyage(q, "query")
        .await
        .expect("second query embed");
    assert_eq!(second.len(), 1024);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "identical query must be served from LRU with zero new HTTP"
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn document_input_type_is_not_cached() {
    let (port, hits, server) = spawn_embed_counter().await;

    let _guard = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
    let _attempts = EnvRestore::set("TACHI_RECALL_PROVIDER_ATTEMPTS", "1");
    let _model = EnvRestore::unset("TACHI_EMBEDDING_MODEL");
    let _dimension = EnvRestore::unset("TACHI_EMBEDDING_DIM");
    let _cache_on = EnvRestore::unset("TACHI_QUERY_EMBEDDING_CACHE");
    let _base = EnvRestore::set("VOYAGE_BASE_URL", format!("http://127.0.0.1:{port}"));

    let client = LlmClient::new_with_config(test_config(), None).expect("client");
    install_voyage_key(&client);

    let text = "document cache bypass probe";
    client
        .embed_voyage(text, "document")
        .await
        .expect("first document embed");
    client
        .embed_voyage(text, "document")
        .await
        .expect("second document embed");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "document embeds must never use the query LRU"
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn env_disable_turns_cache_off() {
    let (port, hits, server) = spawn_embed_counter().await;

    let _guard = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
    let _attempts = EnvRestore::set("TACHI_RECALL_PROVIDER_ATTEMPTS", "1");
    let _model = EnvRestore::unset("TACHI_EMBEDDING_MODEL");
    let _dimension = EnvRestore::unset("TACHI_EMBEDDING_DIM");
    let _cache_off = EnvRestore::set("TACHI_QUERY_EMBEDDING_CACHE", "0");
    let _base = EnvRestore::set("VOYAGE_BASE_URL", format!("http://127.0.0.1:{port}"));

    // from_env reads the disable knob at construction.
    let client = LlmClient::new_with_config(test_config(), None).expect("client");
    install_voyage_key(&client);

    let q = "disabled cache probe";
    client.embed_voyage(q, "query").await.expect("first");
    client.embed_voyage(q, "query").await.expect("second");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "TACHI_QUERY_EMBEDDING_CACHE=0 must disable caching"
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn concurrent_identical_queries_singleflight_one_http() {
    use axum::{routing::post, Json, Router};

    // Slow mock so the barrier cohort overlaps on the miss/singleflight path
    // instead of racing past a zero-latency response.
    let hits = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&hits);
    let app = Router::new().route(
        "/v1/embeddings",
        post({
            let observed = Arc::clone(&observed);
            move |body: Json<serde_json::Value>| {
                let observed = Arc::clone(&observed);
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    let inputs = body
                        .0
                        .get("input")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(1);
                    let data: Vec<serde_json::Value> = (0..inputs)
                        .map(|index| {
                            json!({
                                "index": index,
                                "embedding": vec![0.0_f32; 1024],
                            })
                        })
                        .collect();
                    Json(json!({ "data": data }))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind singleflight mock");
    let port = listener.local_addr().expect("addr").port();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock serve");
    });

    let _guard = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
    let _attempts = EnvRestore::set("TACHI_RECALL_PROVIDER_ATTEMPTS", "1");
    let _model = EnvRestore::unset("TACHI_EMBEDDING_MODEL");
    let _dimension = EnvRestore::unset("TACHI_EMBEDDING_DIM");
    let _cache_on = EnvRestore::unset("TACHI_QUERY_EMBEDDING_CACHE");
    let _base = EnvRestore::set("VOYAGE_BASE_URL", format!("http://127.0.0.1:{port}"));

    let client = LlmClient::new_with_config(test_config(), None).expect("client");
    install_voyage_key(&client);

    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let q = "singleflight stampede probe";
    let mut handles = Vec::new();
    for _ in 0..8 {
        let client = client.clone();
        let barrier = Arc::clone(&barrier);
        let q = q.to_string();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            client.embed_voyage(&q, "query").await
        }));
    }

    for handle in handles {
        let emb = handle.await.expect("join").expect("concurrent query embed");
        assert_eq!(emb.len(), 1024);
    }

    let n = hits.load(Ordering::SeqCst);
    assert_eq!(
        n, 1,
        "singleflight must coalesce concurrent identical query embeds to one HTTP, got {n}"
    );

    server.abort();
}

struct ControlledEmbeddings {
    port: u16,
    hits: Arc<AtomicUsize>,
    started: tokio::sync::mpsc::UnboundedReceiver<String>,
    a: Arc<tokio::sync::Semaphore>,
    b: Arc<tokio::sync::Semaphore>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for ControlledEmbeddings {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn controlled_embeddings() -> ControlledEmbeddings {
    use axum::{routing::post, Json, Router};
    let hits = Arc::new(AtomicUsize::new(0));
    let a = Arc::new(tokio::sync::Semaphore::new(0));
    let b = Arc::new(tokio::sync::Semaphore::new(0));
    let (sent, started) = tokio::sync::mpsc::unbounded_channel();
    let app = Router::new().route(
        "/v1/embeddings",
        post({
            let hits = hits.clone();
            let a = a.clone();
            let b = b.clone();
            move |body: Json<serde_json::Value>| {
                let (hits, a, b, sent) = (hits.clone(), a.clone(), b.clone(), sent.clone());
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    let input = body["input"].as_array().expect("batch input");
                    let first = input[0].as_str().expect("text");
                    sent.send(first.to_string()).expect("test observes request");
                    let gate = if first == "a" { a } else { b };
                    gate.acquire().await.expect("response gate").forget();
                    let data: Vec<_> = input.iter().enumerate().map(|(index, text)| {
                        json!({"index": index, "embedding": vec![if text == "a" { 1.0_f32 } else { 2.0_f32 }; 1024]})
                    }).collect();
                    Json(json!({"data": data}))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("address").port();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    ControlledEmbeddings {
        port,
        hits,
        started,
        a,
        b,
        server,
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn late_mixed_batch_waiters_keep_results_after_lru_eviction() {
    use std::time::Duration;
    let _guard = crate::test_support::global_test_lock().lock();
    let mut mock = controlled_embeddings().await;
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
    let _attempts = EnvRestore::set("TACHI_RECALL_PROVIDER_ATTEMPTS", "1");
    let _model = EnvRestore::unset("TACHI_EMBEDDING_MODEL");
    let _dimension = EnvRestore::unset("TACHI_EMBEDDING_DIM");
    let _cache_on = EnvRestore::unset("TACHI_QUERY_EMBEDDING_CACHE");
    let _capacity = EnvRestore::set("TACHI_QUERY_EMBEDDING_CACHE_CAPACITY", "1");
    let _base = EnvRestore::set("VOYAGE_BASE_URL", format!("http://127.0.0.1:{}", mock.port));
    let client = LlmClient::new_with_config(test_config(), None).expect("client");
    install_voyage_key(&client);

    let first_client = client.clone();
    let first = tokio::spawn(async move { first_client.embed_voyage("a", "query").await });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), mock.started.recv())
            .await
            .expect("a starts")
            .as_deref(),
        Some("a")
    );
    let mixed_client = client.clone();
    let mixed = tokio::spawn(async move {
        mixed_client
            .embed_voyage_batch(&["a".into(), "b".into(), "a".into()], "query")
            .await
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), mock.started.recv())
            .await
            .expect("b starts")
            .as_deref(),
        Some("b")
    );
    // A completes while the mixed batch is still awaiting its own B request.
    mock.a.add_permits(1);
    first.await.expect("first task").expect("first result");
    mock.b.add_permits(1);
    let results = tokio::time::timeout(Duration::from_secs(3), mixed)
        .await
        .expect("late waiters must finish")
        .expect("mixed task")
        .expect("mixed result");
    assert_eq!(
        results,
        vec![vec![1.0; 1024], vec![2.0; 1024], vec![1.0; 1024]]
    );
    assert_eq!(
        mock.hits.load(Ordering::SeqCst),
        2,
        "LRU eviction must not refetch a completed flight"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn cancelled_leader_releases_its_reservation() {
    use std::time::Duration;
    let _guard = crate::test_support::global_test_lock().lock();
    let mut mock = controlled_embeddings().await;
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
    let _attempts = EnvRestore::set("TACHI_RECALL_PROVIDER_ATTEMPTS", "1");
    let _model = EnvRestore::unset("TACHI_EMBEDDING_MODEL");
    let _dimension = EnvRestore::unset("TACHI_EMBEDDING_DIM");
    let _cache_on = EnvRestore::unset("TACHI_QUERY_EMBEDDING_CACHE");
    let _base = EnvRestore::set("VOYAGE_BASE_URL", format!("http://127.0.0.1:{}", mock.port));
    let client = LlmClient::new_with_config(test_config(), None).expect("client");
    install_voyage_key(&client);
    let leader_client = client.clone();
    let leader = tokio::spawn(async move { leader_client.embed_voyage("a", "query").await });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), mock.started.recv())
            .await
            .expect("leader starts")
            .as_deref(),
        Some("a")
    );
    leader.abort();
    assert!(leader.await.expect_err("leader cancelled").is_cancelled());
    mock.a.add_permits(2);
    let result = tokio::time::timeout(Duration::from_secs(3), client.embed_voyage("a", "query"))
        .await
        .expect("successor must not inherit abandoned flight")
        .expect("successor result");
    assert_eq!(result, vec![1.0; 1024]);
    assert_eq!(
        mock.hits.load(Ordering::SeqCst),
        2,
        "cancelled result is not cached; successor owns a fresh request"
    );
}
