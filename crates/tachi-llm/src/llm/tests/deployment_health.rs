//! Production-seam discriminators for deployment health (tachi#1681 D4, PR-C
//! item 4).
//!
//! The one the design names: **429 is dual-recorded and 401/403 is not**. Both
//! halves are asserted against real rows in a real store, because the failure
//! mode this leaf exists to prevent — an auth failure cooling down a
//! deployment, or a deployment cooldown quietly rewriting a credential row —
//! is invisible at the type level once a seam decides to call the writer.

use super::*;

use memcore::db::model_catalog::get_model_deployment_health;
use memcore::vault::health::{EvidenceKind, TypedOutcome};

use crate::llm::catalog_import::{env_deployment_id, DeploymentAttribution};
use crate::llm::provider_health::{ChatLaneConfig, ProviderRuntimeConfig, SelectedProviderSecret};
use crate::{RerankConfig, RerankProviderKind};

const EXTRACT_ENDPOINT: &str = "https://api.siliconflow.cn/v1/chat/completions";
const EXTRACT_MODEL: &str = "Qwen/Qwen3.5-27B";
const KEY_ENV: &str = "TACHI_TEST_ONLY_DEPLOYMENT_HEALTH_API_KEY";

fn config() -> ProviderRuntimeConfig {
    config_at(EXTRACT_ENDPOINT)
}

fn config_at(extract_endpoint: &str) -> ProviderRuntimeConfig {
    let lane = |base_url: &str, model: &str| ChatLaneConfig {
        base_url: base_url.to_string(),
        model: model.to_string(),
        api_key_envs: vec![KEY_ENV],
    };
    ProviderRuntimeConfig {
        extract: lane(extract_endpoint, EXTRACT_MODEL),
        summary: lane(EXTRACT_ENDPOINT, "Qwen/Qwen3.5-7B"),
        reasoning: lane(EXTRACT_ENDPOINT, "deepseek-reasoner"),
        distill: lane(EXTRACT_ENDPOINT, "deepseek-chat"),
        rerank: RerankConfig {
            provider: RerankProviderKind::Voyage,
            local_endpoint: None,
        },
    }
}

/// A store with the four `env:` chat-lane rows the import produces, and the
/// client that will record against them.
fn client_with_catalog(db_path: &std::path::Path) -> LlmClient {
    client_with_catalog_at(db_path, &config())
}

fn client_with_catalog_at(db_path: &std::path::Path, config: &ProviderRuntimeConfig) -> LlmClient {
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf-8 path"))
        .expect("initialize the store");
    crate::llm::catalog_import::import_env_chat_lanes(
        store.connection(),
        config,
        "2026-08-13T00:00:00.000Z",
    )
    .expect("import the env lanes");
    drop(store);
    LlmClient::new_with_config(config.clone(), Some(db_path)).expect("client initializes")
}

fn selected() -> SelectedProviderSecret {
    SelectedProviderSecret {
        logical_name: KEY_ENV.to_string(),
        key_id: format!("{KEY_ENV}_1"),
        value: String::new(),
    }
}

fn extract_attribution() -> DeploymentAttribution<'static> {
    DeploymentAttribution::EnvLane {
        lane: "extract",
        endpoint: EXTRACT_ENDPOINT,
        model: EXTRACT_MODEL,
    }
}

// ─── the attribution rule, at the seam ───────────────────────────────────────

#[test]
fn a_lane_throttle_is_recorded_on_the_credential_and_the_deployment() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let client = client_with_catalog(&db_path);

    client.apply_key_outcome(
        &selected(),
        TypedOutcome::RateLimited {
            retry_after_secs: Some(30),
        },
        EvidenceKind::SelfReported,
        None,
        extract_attribution(),
    );

    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("reopen");

    // Authority one: the credential that was throttled, unchanged behaviour.
    let credential = store
        .vault_get_key_health(KEY_ENV, &format!("{KEY_ENV}_1"))
        .expect("read credential health")
        .expect("the credential row still lands");
    assert_eq!(credential.status, HEALTH_RATE_LIMITED);
    assert!(credential.cooldown_until.is_some());

    // Authority two: the deployment that throttled it.
    let deployment = get_model_deployment_health(store.connection(), &env_deployment_id("extract"))
        .expect("read deployment health")
        .expect("the deployment row lands too — this is the dual record");
    assert_eq!(deployment.state, "cooldown");
    assert!(
        deployment.cooldown_until.is_some(),
        "a 429 must cool the deployment down, not only the key that carried it"
    );
    assert_eq!(deployment.evidence_kind, Some(EvidenceKind::SelfReported));
    assert_eq!(client.deployment_health_record_counts().recorded, 1);

    // …and the two cooldowns are separate facts: no sibling lane was touched.
    for lane in ["summary", "reasoning", "distill"] {
        assert!(
            get_model_deployment_health(store.connection(), &env_deployment_id(lane))
                .expect("read")
                .is_none(),
            "a throttle on one deployment must not write health for another"
        );
    }
}

#[test]
fn an_auth_failure_never_reaches_the_deployment_authority() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let client = client_with_catalog(&db_path);

    // Attributed on purpose: the seam is *told* which deployment served the
    // request, and still must not record an auth outcome against it. A weaker
    // test (passing `Unattributed`) would pass for the wrong reason.
    client.apply_key_outcome(
        &selected(),
        TypedOutcome::AuthFailed,
        EvidenceKind::SelfReported,
        Some("401 unauthorized"),
        extract_attribution(),
    );

    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("reopen");
    assert_eq!(
        store
            .vault_get_key_health(KEY_ENV, &format!("{KEY_ENV}_1"))
            .expect("read")
            .expect("the credential row is where an auth failure belongs")
            .status,
        HEALTH_AUTH_FAILED
    );
    assert!(
        get_model_deployment_health(store.connection(), &env_deployment_id("extract"))
            .expect("read")
            .is_none(),
        "an auth failure says nothing about the deployment; recording one here would cool down \
         every sibling deployment that shares the rejected key"
    );
    assert_eq!(client.deployment_health_record_counts().recorded, 0);
}

#[test]
fn an_outcome_from_a_tier_the_catalog_does_not_describe_is_counted_not_written() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let client = client_with_catalog(&db_path);

    // A #1197 cross-provider fallback tier: same lane, different provider.
    client.apply_key_outcome(
        &selected(),
        TypedOutcome::RateLimited {
            retry_after_secs: Some(30),
        },
        EvidenceKind::SelfReported,
        None,
        DeploymentAttribution::EnvLane {
            lane: "extract",
            endpoint: "https://api.deepseek.com/chat/completions",
            model: "deepseek-chat",
        },
    );
    // A lane with no catalog row at all.
    client.apply_key_outcome(
        &selected(),
        TypedOutcome::RateLimited {
            retry_after_secs: Some(30),
        },
        EvidenceKind::SelfReported,
        None,
        DeploymentAttribution::EnvLane {
            lane: "embedding",
            endpoint: "https://api.voyageai.com/v1/embeddings",
            model: "voyage-4",
        },
    );

    let counts = client.deployment_health_record_counts();
    assert_eq!(counts.recorded, 0);
    assert_eq!(
        counts.skipped_different_request, 1,
        "a fallback tier's throttle must be counted, not attributed to the primary deployment"
    );
    assert_eq!(counts.skipped_unknown_deployment, 1);

    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("reopen");
    assert!(
        get_model_deployment_health(store.connection(), &env_deployment_id("extract"))
            .expect("read")
            .is_none()
    );
    // The credential half still happened: a skipped health record must never
    // change what the existing path does.
    assert_eq!(
        store
            .vault_get_key_health(KEY_ENV, &format!("{KEY_ENV}_1"))
            .expect("read")
            .expect("row")
            .status,
        HEALTH_RATE_LIMITED
    );
}

#[test]
fn a_channel_with_no_catalog_row_records_nothing_and_counts_nothing() {
    // The MCP/CLI `record-key-result` channel and the rerank lane: no
    // deployment was named, so this is not a skip to investigate — it is a
    // path that never had one.
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let client = client_with_catalog(&db_path);

    client.record_provider_key_result_blocking(
        KEY_ENV,
        &format!("{KEY_ENV}_1"),
        Some(429),
        None,
        Some(30),
        None,
    );

    assert_eq!(
        client.deployment_health_record_counts(),
        crate::DeploymentHealthRecordCounts::default()
    );
}

// ─── the real HTTP path ──────────────────────────────────────────────────────
//
// Everything above drives `apply_key_outcome` directly, which is where the
// attribution rule lives — but the codex review of PR-C found that the chat
// lane's *own* branches for 402, 5xx, transport and protocol failures never
// reached the seam at all, so those outcomes were not "deployment-only
// records", they were absent. These tests go through `call_extract_llm` and a
// real socket, so a branch that forgets to record cannot pass them.

/// Serve `app` on a loopback port and return a client whose `extract` lane
/// points at it, with the catalog imported from that same config (so
/// `env:extract` describes exactly the endpoint the request will use).
async fn chat_lane_against(
    app: axum::Router,
    db_path: &std::path::Path,
) -> (LlmClient, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock provider");
    let port = listener.local_addr().expect("mock provider addr").port();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock provider");
    });

    let config = config_at(&format!("http://127.0.0.1:{port}/chat/completions"));
    let client = client_with_catalog_at(db_path, &config);
    client.set_provider_secret_pool(
        KEY_ENV,
        vec![crate::ProviderSecret {
            key_id: format!("{KEY_ENV}_1"),
            value: "test-key".to_string(),
        }],
    );
    (client, server)
}

fn deployment_row(db_path: &std::path::Path) -> Option<memcore::catalog::ModelDeploymentHealth> {
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf-8 path")).expect("reopen");
    get_model_deployment_health(store.connection(), &env_deployment_id("extract")).expect("read")
}

#[tokio::test]
async fn a_chat_lane_402_records_the_deployment_and_leaves_the_credential_alone() {
    use axum::{http::StatusCode, response::IntoResponse, routing::post, Router};

    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let app = Router::new().route(
        "/chat/completions",
        post(|| async {
            (
                StatusCode::PAYMENT_REQUIRED,
                r#"{"error":{"message":"quota exhausted"}}"#,
            )
                .into_response()
        }),
    );
    let (client, server) = chat_lane_against(app, &db_path).await;

    client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect_err("a 402 still fails the call");
    client
        .await_provider_health_persistence()
        .await
        .expect("health writes settle");

    // The deployment half: 402 is a quota signal, which #1681 D4 classes with
    // 429 — it cools the deployment down. Before this fix the status fell
    // through the lane's `!status.is_success()` arm as an untyped lane outage
    // and nothing was recorded at all.
    let deployment = deployment_row(&db_path).expect("the 402 must reach the deployment authority");
    assert_eq!(deployment.state, "cooldown");
    assert!(
        deployment.cooldown_until.is_some(),
        "a quota-exhausted deployment must be cooled down, not just logged"
    );
    assert_eq!(client.deployment_health_record_counts().recorded, 1);

    // The credential half: untouched. A 402 says the quota behind the
    // deployment is spent, not that this key is bad — and the fence #1681 D4
    // draws is that the deployment write can never reach into the credential
    // authority's tables.
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("reopen");
    assert!(
        store
            .vault_get_key_health(KEY_ENV, &format!("{KEY_ENV}_1"))
            .expect("read credential health")
            .is_none(),
        "recording deployment health must not mint a credential row"
    );

    server.abort();
}

#[tokio::test]
async fn a_chat_lane_5xx_records_the_deployment_only_and_honours_an_obsolete_retry_after() {
    use axum::{
        http::{header::RETRY_AFTER, HeaderMap, HeaderValue, StatusCode},
        response::IntoResponse,
        routing::post,
        Router,
    };

    // An RFC 850 `Retry-After` naming a moment two minutes out. Deliberately
    // that format: `lane_calls.rs` parses the header as `u64` delta-seconds for
    // its own retry sleep, so an obsolete date form leaves lane timing on the
    // ordinary jittered backoff (the test stays fast) while the deployment
    // authority — which parses the raw header through `RetryAfter::parse` —
    // still gets the instant. That is CP3 and CP6(d) closed in production at
    // once: all three HTTP-date formats accepted, and fed by a real response.
    let until = chrono::Utc::now() + chrono::Duration::seconds(120);
    let header = until.format("%A, %d-%b-%y %H:%M:%S GMT").to_string();

    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let app = Router::new().route(
        "/chat/completions",
        post(move || {
            let header = header.clone();
            async move {
                let mut headers = HeaderMap::new();
                headers.insert(
                    RETRY_AFTER,
                    HeaderValue::from_str(&header).expect("header is ascii"),
                );
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    headers,
                    r#"{"error":"upstream is down"}"#,
                )
                    .into_response()
            }
        }),
    );
    let (client, server) = chat_lane_against(app, &db_path).await;

    client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect_err("a 503 on every attempt still fails the call");
    client
        .await_provider_health_persistence()
        .await
        .expect("health writes settle");

    let deployment = deployment_row(&db_path).expect("the 503 must reach the deployment authority");
    assert_eq!(
        deployment.state, "error",
        "a 5xx is the deployment's own failure — an error state, not a throttle"
    );
    assert!(
        deployment.cooldown_until.is_some(),
        "the provider named a Retry-After in RFC 850 form; refusing to read it would throw away \
         an instruction the provider actually gave"
    );
    assert_eq!(
        deployment.last_error.as_deref(),
        Some("provider returned HTTP 503"),
        "generated from the closed vocabulary, never from the provider's body"
    );
    let counts = client.deployment_health_record_counts();
    assert!(
        counts.recorded >= 1,
        "every attempt is its own observation, got {counts:?}"
    );
    assert_eq!(counts.skipped_unknown_deployment, 0);
    assert_eq!(counts.skipped_different_request, 0);
    assert_eq!(counts.failed, 0);

    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("reopen");
    assert!(
        store
            .vault_get_key_health(KEY_ENV, &format!("{KEY_ENV}_1"))
            .expect("read")
            .is_none(),
        "a 5xx is deployment-only (#1681 D4): the credential authority has nothing to say about it"
    );

    server.abort();
}

#[tokio::test]
async fn a_chat_lane_protocol_failure_records_an_unusable_response() {
    use axum::{http::StatusCode, response::IntoResponse, routing::post, Router};

    // 200 OK, and a body that is not the protocol. The lane fails it as a lane
    // outage; the deployment authority records that the deployment answered
    // and the answer was of no use.
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let app = Router::new().route(
        "/chat/completions",
        post(|| async { (StatusCode::OK, "this is not JSON").into_response() }),
    );
    let (client, server) = chat_lane_against(app, &db_path).await;

    client
        .call_extract_llm("system", "user", None, 0.0, 16)
        .await
        .expect_err("an unparseable body fails the call");
    client
        .await_provider_health_persistence()
        .await
        .expect("health writes settle");

    let deployment = deployment_row(&db_path).expect("a protocol failure is deployment evidence");
    assert_eq!(deployment.state, "error");
    assert_eq!(
        deployment.cooldown_until, None,
        "a malformed body is not an instruction to back off"
    );
    assert_eq!(
        deployment.last_error.as_deref(),
        Some("unusable response"),
        "no status: the answer's shape was wrong, not its status line"
    );

    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("reopen");
    assert!(store
        .vault_get_key_health(KEY_ENV, &format!("{KEY_ENV}_1"))
        .expect("read")
        .is_none());

    server.abort();
}

#[tokio::test]
async fn a_chat_lane_transport_failure_records_the_deployment_as_unreachable() {
    use axum::{routing::post, Router};

    // The socket is bound, the client connects, and the server is killed
    // before it can answer: no status line ever arrives, which is the one
    // outcome that has no credential reading at all.
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let app = Router::new().route(
        "/chat/completions",
        post(|| async {
            // Never answers. The connection is dropped when the task is
            // aborted below, which is what the client sees as a transport
            // failure.
            std::future::pending::<()>().await;
        }),
    );
    let (client, server) = chat_lane_against(app, &db_path).await;

    let call = client.call_extract_llm("system", "user", None, 0.0, 16);
    let abort = async {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        server.abort();
    };
    let (result, ()) = tokio::join!(call, abort);
    result.expect_err("a dropped connection fails the call");
    client
        .await_provider_health_persistence()
        .await
        .expect("health writes settle");

    let deployment =
        deployment_row(&db_path).expect("a transport failure is deployment-only evidence");
    assert_eq!(deployment.state, "error");
    assert_eq!(
        deployment.last_error.as_deref(),
        Some("no response from the provider")
    );
    assert_eq!(deployment.cooldown_until, None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn an_embedding_body_that_parses_but_is_not_a_batch_is_never_a_success() {
    use axum::{http::StatusCode, response::IntoResponse, routing::post, Router};

    // Reentrant GlobalTestLock (R2): `VOYAGE_BASE_URL` and the embedding
    // config vars are process-wide, and `.lock()` already swallows poison.
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let _attempts = EnvRestore::set("TACHI_RECALL_PROVIDER_ATTEMPTS", "1");
    // Pin the default model and width: the row's declared dimension is what
    // the response is validated against.
    let _model = EnvRestore::unset("TACHI_EMBEDDING_MODEL");
    let _dimension = EnvRestore::unset("TACHI_EMBEDDING_DIM");

    // 200 OK, valid JSON — and not a batch. One input, zero embeddings back.
    // The status line and the syntax are both fine, so every check the rework
    // wired up (transport, status, JSON syntax) passes; only the *semantic*
    // read can catch this one, which is why it was the branch still recording
    // a false success (codex re-review of PR-C, CP6).
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock provider");
    let port = listener.local_addr().expect("mock provider addr").port();
    let app = Router::new().route(
        "/v1/embeddings",
        post(|| async { (StatusCode::OK, r#"{"data":[]}"#).into_response() }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock provider");
    });
    let _base = EnvRestore::set("VOYAGE_BASE_URL", format!("http://127.0.0.1:{port}"));

    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let endpoint = crate::llm::embedding::voyage_embeddings_endpoint();
    let embedding = crate::llm::embedding_config::EmbeddingConfig::from_env()
        .expect("the default embedding config resolves");
    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf-8 path"))
        .expect("initialize the store");
    crate::llm::catalog_import::import_env_embedding_lane(
        store.connection(),
        &embedding,
        &endpoint,
        "2026-08-13T00:00:00.000Z",
    )
    .expect("import the embedding lane");
    drop(store);

    let client = LlmClient::new_with_config(config(), Some(&db_path)).expect("client initializes");
    client.set_provider_secret_pool(
        "VOYAGE_API_KEY",
        vec![ProviderSecret {
            key_id: "VOYAGE_API_KEY_1".to_string(),
            value: "test-key".to_string(),
        }],
    );

    let err = client
        .embed_voyage_batch(&["probe".to_string()], "query")
        .await
        .expect_err("an empty batch is not a usable answer");
    assert!(
        err.contains("Voyage batch returned 0 embeddings for 1 inputs"),
        "the caller must still see why, got: {err}"
    );
    client
        .await_provider_health_persistence()
        .await
        .expect("health writes settle");

    let store = memcore::MemoryStore::open(db_path.to_str().expect("utf-8 path")).expect("reopen");
    let deployment = get_model_deployment_health(
        store.connection(),
        &env_deployment_id(crate::llm::catalog_import::ENV_EMBEDDING_LANE),
    )
    .expect("read")
    .expect("a response the lane could not use is deployment evidence");
    assert_eq!(
        deployment.state, "error",
        "the deployment answered and the answer was unusable; recording `Served` here would \
         clear a real cooldown on the strength of a response the caller was handed an error for"
    );
    assert_eq!(
        deployment.last_error.as_deref(),
        Some("unusable response"),
        "no status: the answer's shape was wrong, not its status line"
    );
    assert_eq!(
        deployment.cooldown_until, None,
        "a malformed body is not an instruction to back off"
    );
    assert_eq!(
        deployment.last_success_at, None,
        "nothing was served, so nothing may claim a success instant"
    );

    server.abort();
}

#[test]
fn a_success_after_a_throttle_clears_the_deployment_cooldown() {
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let temp = tempfile::tempdir().expect("temp db");
    let db_path = temp.path().join("vault.db");
    let client = client_with_catalog(&db_path);

    client.apply_key_outcome(
        &selected(),
        TypedOutcome::RateLimited {
            retry_after_secs: Some(30),
        },
        EvidenceKind::SelfReported,
        None,
        extract_attribution(),
    );
    client.apply_key_outcome(
        &selected(),
        TypedOutcome::Success,
        EvidenceKind::SelfReported,
        None,
        extract_attribution(),
    );

    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("reopen");
    let deployment = get_model_deployment_health(store.connection(), &env_deployment_id("extract"))
        .expect("read")
        .expect("row");
    assert_eq!(deployment.state, "ok");
    assert_eq!(deployment.cooldown_until, None);
    assert_eq!(client.deployment_health_record_counts().recorded, 2);
}
