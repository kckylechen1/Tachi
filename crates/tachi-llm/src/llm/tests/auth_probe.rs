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

/// Start a loopback mock provider and hand back its URL, the requests it saw,
/// and the serving task's `JoinHandle`.
///
/// The handle is returned (#1621) so each caller states how long its mock is
/// meant to live instead of relying on "whenever this test's runtime drops".
/// That matters most for the proxy-trap server in
/// `ambient_proxy_cannot_receive_probe_bearer_or_own_transport`, whose whole
/// job is to still be listening at assertion time so an empty observation log
/// means "nothing was sent here", never "nobody was home".
///
/// `bind` → `local_addr` → `spawn` ordering is load-bearing and already
/// correct: the port is reserved before any client is handed the URL.
async fn start_mock(
    status: StatusCode,
    body: &'static str,
    redirect_to: Option<&'static str>,
) -> (String, RequestObservations, tokio::task::JoinHandle<()>) {
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
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().fallback(any(mock_handler)).with_state(state),
        )
        .await
        .expect("serve mock");
    });
    (format!("http://{address}"), observations, server)
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
    let (server, observations, _mock) = start_mock(
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
    let (server, observations, _mock) = start_mock(
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
    let (server, observations, _mock) = start_mock(StatusCode::UNAUTHORIZED, hostile, None).await;
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
    let (server, observations, _mock) = start_mock(
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
    let (server, observations, _mock) = start_mock(StatusCode::OK, r#"{"data":[]}"#, None).await;
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
        let (server, observations, _mock) = start_mock(status, "must remain private", None).await;
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
    let (server, observations, _mock) = start_mock(StatusCode::OK, hostile, None).await;
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

/// CONCURRENCY NOTE (#1621): this test mutates process-global proxy env vars
/// via `EnvRestore`, and `global_test_lock` does not hold back the ~20
/// `chat_lanes` `#[tokio::test]`s running beside it in the same libtest
/// process. It is safe only because *both* reqwest clients in this crate are
/// unconditionally `.no_proxy()`: the probe's own at `llm/auth_probe.rs`, and
/// the shared pooled client at `llm/provider_health/config.rs`. When the
/// pooled one was proxy-honouring, this test's env vars pointed those 20 tests
/// at the trap server below and they failed with 502s and connection errors.
/// If either `.no_proxy()` is ever removed, that flake comes straight back —
/// and the assertion here would stop meaning what it says.
///
/// The assertions keep their teeth regardless: `.no_proxy()` on the probe
/// client is product code, not a test fixture, so what is verified here is the
/// shipped behaviour.
#[tokio::test(flavor = "current_thread")]
async fn ambient_proxy_cannot_receive_probe_bearer_or_own_transport() {
    let _env_lock = crate::test_support::global_test_lock().lock();
    let (intended_server, intended_observations, _intended_mock) = start_mock(
        StatusCode::OK,
        r#"{"data":[{"id":"deepseek-reasoner"}]}"#,
        None,
    )
    .await;
    let (proxy_trap, proxy_observations, proxy_mock) = start_mock(
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

    // An empty trap log must mean "nothing was sent here", never "the trap died
    // before the probe ran" (#1621).
    assert!(
        !proxy_mock.is_finished(),
        "proxy trap must still be serving, otherwise its empty log proves nothing"
    );
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

// ── #1680 D6: per-member probing, registry-driven targets, Probed evidence ──

use crate::llm::{
    auth_probe_descriptor_for_host, auth_probe_descriptor_for_provider_kind,
    ProviderProbeDescriptor, AUTH_PROBE_DESCRIPTORS, DEEPSEEK_AUTH_PROBE, ZAI_AUTH_PROBE,
};
use memcore::vault::health::EvidenceKind;

const MEMBER_POOL: &str = "__1680_MEMBER_PROBE_POOL";

/// A client with a real vault DB, holding a two-member pool so a probe of one
/// member has a sibling that must not move.
fn member_client(db_path: &std::path::Path) -> LlmClient {
    // Create the vault schema up front: the client only *reads* an existing
    // DB at construction, and the account-table assertions below need those
    // tables to exist before the first probe, not after the first write.
    drop(
        memcore::MemoryStore::open(db_path.to_str().expect("db path"))
            .expect("initialize vault db"),
    );
    let unused = lane("https://unused.invalid/v1/chat/completions", "unused");
    let client = LlmClient::new_with_config(
        ProviderRuntimeConfig {
            extract: unused.clone(),
            summary: unused.clone(),
            reasoning: unused.clone(),
            distill: unused,
            rerank: RerankConfig {
                provider: RerankProviderKind::Voyage,
                local_endpoint: None,
            },
        },
        Some(db_path),
    )
    .expect("construct client");
    assert!(client.set_provider_secret_pool(
        MEMBER_POOL,
        vec![
            ProviderSecret {
                key_id: format!("{MEMBER_POOL}_1"),
                value: "synthetic-probe-key".to_string(),
            },
            ProviderSecret {
                key_id: format!("{MEMBER_POOL}_2"),
                value: "synthetic-probe-key".to_string(),
            },
        ],
    ));
    client
}

fn account_table_counts(db_path: &std::path::Path) -> Vec<(String, i64)> {
    let conn = rusqlite::Connection::open(db_path).expect("open vault db");
    [
        "provider_accounts",
        "provider_account_aliases",
        "provider_account_events",
        "account_custody",
    ]
    .into_iter()
    .map(|table| {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap_or_else(|err| panic!("count {table}: {err}"));
        (table.to_string(), count)
    })
    .collect()
}

/// The probe table is the only host list, and it is matched exactly. A
/// lookalike host is not "a DeepSeek probe with a bad configuration" — it is
/// simply not in the table.
#[test]
fn probe_descriptors_are_matched_by_exact_host_only() {
    assert_eq!(
        auth_probe_descriptor_for_host("api.deepseek.com"),
        Some(&DEEPSEEK_AUTH_PROBE)
    );
    assert_eq!(
        auth_probe_descriptor_for_host("api.deepseek.com.attacker.invalid"),
        None
    );
    assert_eq!(auth_probe_descriptor_for_host("deepseek.com"), None);
    assert_eq!(auth_probe_descriptor_for_host(""), None);

    // A family with several recognized hosts resolves to the one that has a
    // documented endpoint; Z.AI has none on either host, so it stays
    // endpoint-free rather than borrowing another family's.
    assert_eq!(
        auth_probe_descriptor_for_provider_kind("deepseek")
            .and_then(|descriptor| descriptor.endpoint),
        Some("https://api.deepseek.com/models")
    );
    assert_eq!(
        auth_probe_descriptor_for_provider_kind("zai").map(|descriptor| descriptor.endpoint),
        Some(None)
    );
    assert_eq!(auth_probe_descriptor_for_provider_kind("openai"), None);

    // Every declared endpoint belongs to its own declared host, so no
    // descriptor can send one family's credential to another family's URL.
    for descriptor in AUTH_PROBE_DESCRIPTORS {
        let ProviderProbeDescriptor { host, endpoint, .. } = descriptor;
        if let Some(endpoint) = endpoint {
            let url = reqwest::Url::parse(endpoint).expect("descriptor endpoint parses");
            assert_eq!(url.scheme(), "https", "{endpoint}");
            assert_eq!(url.host_str(), Some(*host), "{endpoint}");
        }
    }
}

/// #1680 disc-4, on the probe channel: a 401 for one pool member updates that
/// member and nothing else.
#[tokio::test]
async fn a_member_probe_records_only_the_member_it_probed() {
    let (server, _observations, _mock) =
        start_mock(StatusCode::UNAUTHORIZED, "hostile body", None).await;
    let temp = tempfile::tempdir().expect("temp vault db");
    let db_path = temp.path().join("vault.db");
    let client = member_client(&db_path);
    let sibling = format!("{MEMBER_POOL}_1");
    let probed = format!("{MEMBER_POOL}_2");

    let sibling_before =
        client.record_provider_key_result(MEMBER_POOL, &sibling, Some(200), None, None, None);
    let sibling_before = serde_json::to_string(&sibling_before).expect("serialize sibling");

    let (result, health) = client
        .probe_member_auth_and_record_with_endpoint_for_tests(
            &DEEPSEEK_AUTH_PROBE,
            MEMBER_POOL,
            &probed,
            &format!("{server}/models"),
        )
        .await;

    assert_eq!(result.auth_class, ProviderAuthProbeClass::AuthFailed);
    assert_eq!(result.provider_host, "api.deepseek.com");
    // A member probe is about a credential, not a lane model.
    assert_eq!(result.effective_model, "");
    assert_eq!(result.selected_model_present, None);

    let health = health.expect("an answered probe records the member's health");
    assert_eq!(health.logical_name, MEMBER_POOL);
    assert_eq!(health.key_id, probed);
    assert!(health.auth_failed);
    assert_eq!(
        EvidenceKind::from_metadata(&health.metadata),
        Some(EvidenceKind::Probed),
        "a probe records its own observation as Probed"
    );

    let members = client
        .provider_health_memory_snapshot()
        .remove(MEMBER_POOL)
        .expect("pool health");
    assert_eq!(members.len(), 2);
    assert_eq!(
        serde_json::to_string(members.get(&sibling).expect("sibling row")).expect("serialize"),
        sibling_before,
        "a 401 for {probed} must not touch {sibling}"
    );
}

/// #1680 disc-5: a probe that could not learn anything must not be able to
/// clear a real auth failure — nor, in the other direction, manufacture one.
#[tokio::test]
async fn an_inconclusive_member_probe_never_rewrites_the_health_binding() {
    let temp = tempfile::tempdir().expect("temp vault db");
    let db_path = temp.path().join("vault.db");
    let client = member_client(&db_path);
    let probed = format!("{MEMBER_POOL}_1");

    let failed =
        client.record_provider_key_result(MEMBER_POOL, &probed, Some(401), None, None, None);
    assert!(failed.auth_failed);

    for (status, body, redirect, expected) in [
        (
            StatusCode::FOUND,
            "hostile redirect",
            Some("/followed"),
            ProviderAuthProbeClass::RedirectRefused,
        ),
        (
            StatusCode::OK,
            "not JSON",
            None,
            ProviderAuthProbeClass::MalformedResponse,
        ),
        (
            StatusCode::NOT_FOUND,
            "nothing here",
            None,
            ProviderAuthProbeClass::UnexpectedStatus,
        ),
    ] {
        let (server, _observations, _mock) = start_mock(status, body, redirect).await;
        let (result, health) = client
            .probe_member_auth_and_record_with_endpoint_for_tests(
                &DEEPSEEK_AUTH_PROBE,
                MEMBER_POOL,
                &probed,
                &format!("{server}/models"),
            )
            .await;

        assert_eq!(result.auth_class, expected);
        let health = health.expect("an attempted probe still stamps the attempt");
        assert_eq!(health.status, failed.status, "{expected:?}");
        assert!(
            health.auth_failed,
            "{expected:?} must not clear the failure"
        );
        assert_eq!(health.error_count, failed.error_count, "{expected:?}");
        assert_eq!(health.last_error, failed.last_error, "{expected:?}");
        assert_eq!(health.last_success, failed.last_success, "{expected:?}");
        assert_eq!(health.cooldown_until, failed.cooldown_until, "{expected:?}");
        assert_eq!(
            EvidenceKind::from_metadata(&health.metadata),
            Some(EvidenceKind::Probed),
            "{expected:?}"
        );
    }
}

/// A probe that never made a request records nothing at all — not even an
/// attempt stamp. Z.AI is the live case: a recognized family with no
/// documented non-generating endpoint.
#[tokio::test]
async fn a_probe_that_never_left_the_process_records_nothing() {
    let temp = tempfile::tempdir().expect("temp vault db");
    let db_path = temp.path().join("vault.db");
    let client = member_client(&db_path);
    let probed = format!("{MEMBER_POOL}_1");

    let result = client
        .probe_member_auth_no_content(&ZAI_AUTH_PROBE, MEMBER_POOL, &probed)
        .await;
    assert_eq!(
        result.auth_class,
        ProviderAuthProbeClass::UnsupportedNoDocumentedProbe
    );
    assert_eq!(result.provider_host, "api.z.ai");

    let (result, health) = client
        .probe_member_auth_and_record(&ZAI_AUTH_PROBE, MEMBER_POOL, &probed)
        .await;
    assert_eq!(
        result.auth_class,
        ProviderAuthProbeClass::UnsupportedNoDocumentedProbe
    );
    assert!(health.is_none(), "no request means no evidence");
    assert!(
        !client
            .provider_health_memory_snapshot()
            .contains_key(MEMBER_POOL),
        "an unmade probe must not create a health row"
    );

    // A member with no key material is the same answer.
    let (result, health) = client
        .probe_member_auth_and_record(&DEEPSEEK_AUTH_PROBE, MEMBER_POOL, "no-such-member")
        .await;
    assert_eq!(
        result.auth_class,
        ProviderAuthProbeClass::CredentialUnavailable
    );
    assert!(health.is_none());
}

/// #1680 disc-5, the other half: the probe channel writes health and only
/// health. Account identity moves through `vault reconcile apply` alone, so a
/// probe — however it turns out — leaves every `provider_accounts` table
/// exactly as it found it.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn the_probe_channel_never_writes_a_provider_account_row() {
    // `TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST` is process-global, and
    // this test asserts on what reached the DB.
    let _lock = crate::test_support::global_test_lock().lock();
    let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
    let (server, _observations, _mock) =
        start_mock(StatusCode::OK, r#"{"data":[{"id":"deepseek-chat"}]}"#, None).await;
    let temp = tempfile::tempdir().expect("temp vault db");
    let db_path = temp.path().join("vault.db");
    let client = member_client(&db_path);
    let probed = format!("{MEMBER_POOL}_2");
    let before = account_table_counts(&db_path);

    let (result, health) = client
        .probe_member_auth_and_record_with_endpoint_for_tests(
            &DEEPSEEK_AUTH_PROBE,
            MEMBER_POOL,
            &probed,
            &format!("{server}/models"),
        )
        .await;
    assert_eq!(result.auth_class, ProviderAuthProbeClass::AuthOk);
    let health = health.expect("an answered probe records health");
    assert_eq!(health.status, "ok");
    client
        .await_provider_health_persistence()
        .await
        .expect("probe health persist should finish");

    let store = memcore::MemoryStore::open(db_path.to_str().expect("db path")).expect("reopen");
    let persisted = store
        .vault_get_key_health(MEMBER_POOL, &probed)
        .expect("read persisted health")
        .expect("the probe's verdict must be durable");
    assert_eq!(
        EvidenceKind::from_metadata(&persisted.metadata),
        Some(EvidenceKind::Probed),
        "Probed and SelfReported must stay distinguishable in storage"
    );
    drop(store);

    assert_eq!(
        account_table_counts(&db_path),
        before,
        "the probe channel must not write any provider-account table"
    );
}
