//! HTTP executor discrimination tests.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, Response, StatusCode};
use axum::routing::post;
use axum::Router;
use tokio::io::AsyncWriteExt;
use tokio::task::JoinHandle;
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::llm::LlmClient;

fn executor() -> BrokerHttpExecutor {
    crate::install_tls_provider();
    BrokerHttpExecutor::new(LlmClient::build_http_client().expect("test HTTP client"))
}

fn request_for(endpoint: &str, stream: StreamSelection) -> CanonicalInvocationRequest {
    let mut parts = minimal_parts();
    parts.target = InvocationTarget::Resolved {
        target: ResolvedWireTarget::new(ResolvedWireTargetParts {
            deployment_id: "dep-executor-test".to_string(),
            endpoint: EndpointUrl::new(endpoint).expect("loopback endpoint"),
            provider_model_id: "test-model".to_string(),
        })
        .expect("resolved target"),
    };
    parts.stream = stream;
    CanonicalInvocationRequest::new(parts).expect("executor fixture request")
}

fn lease(material: &str) -> LeasedAuthMaterial {
    let auth = api_key_lease();
    LeasedAuthMaterial::new(AuthMaterialKind::ApiKey, auth.lease_ref(), material)
}

async fn serve(app: Router) -> (String, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind executor fixture");
    let address = listener.local_addr().expect("executor fixture address");
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve executor fixture");
    });
    (format!("http://{address}/v1/chat/completions"), task)
}

#[tokio::test]
async fn pre_cancel_never_polls_the_send_future_or_hits_the_server() {
    let hits = Arc::new(AtomicUsize::new(0));
    let route_hits = Arc::clone(&hits);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            route_hits.fetch_add(1, Ordering::SeqCst);
            async { StatusCode::NO_CONTENT }
        }),
    );
    let (endpoint, task) = serve(app).await;
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let outcome = executor()
        .execute(
            &OpenAiCompatWire::new(),
            &request_for(&endpoint, StreamSelection::Disabled),
            api_key_lease(),
            Some(&lease("PRE-CANCEL-SECRET")),
            &cancellation,
            |_| panic!("non-streaming request emitted an event"),
        )
        .await;

    assert_eq!(
        outcome.terminal_disposition(),
        &InvocationDispositionV1::CancelledBeforeSend
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn one_429_is_one_send_and_preserves_retry_after() {
    let hits = Arc::new(AtomicUsize::new(0));
    let route_hits = Arc::clone(&hits);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            route_hits.fetch_add(1, Ordering::SeqCst);
            async {
                Response::builder()
                    .status(StatusCode::TOO_MANY_REQUESTS)
                    .header("retry-after", "7")
                    .body(Body::from(r#"{"error":{"message":"slow down"}}"#))
                    .expect("429 response")
            }
        }),
    );
    let (endpoint, task) = serve(app).await;
    let cancellation = CancellationToken::new();

    let outcome = executor()
        .execute(
            &OpenAiCompatWire::new(),
            &request_for(&endpoint, StreamSelection::Disabled),
            api_key_lease(),
            Some(&lease("RATE-LIMIT-SECRET")),
            &cancellation,
            |_| {},
        )
        .await;

    assert!(matches!(
        outcome.terminal_disposition(),
        InvocationDispositionV1::ProviderRejected {
            status: 429,
            retry_after: Some(RetryAfter::Seconds(7)),
            ..
        }
    ));
    assert_eq!(hits.load(Ordering::SeqCst), 1, "executor retried");
    task.abort();
}

#[tokio::test]
async fn accepted_request_cancel_is_never_reported_as_confirmed() {
    let hits = Arc::new(AtomicUsize::new(0));
    let accepted = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let route_hits = Arc::clone(&hits);
    let route_accepted = Arc::clone(&accepted);
    let route_release = Arc::clone(&release);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |_request: Request| {
            let accepted = Arc::clone(&route_accepted);
            let release = Arc::clone(&route_release);
            route_hits.fetch_add(1, Ordering::SeqCst);
            async move {
                accepted.notify_one();
                release.notified().await;
                StatusCode::NO_CONTENT
            }
        }),
    );
    let (endpoint, task) = serve(app).await;
    let cancellation = CancellationToken::new();
    let run_cancel = cancellation.clone();
    let run = tokio::spawn(async move {
        executor()
            .execute(
                &OpenAiCompatWire::new(),
                &request_for(&endpoint, StreamSelection::Disabled),
                api_key_lease(),
                Some(&lease("ACCEPTED-CANCEL-SECRET")),
                &run_cancel,
                |_| {},
            )
            .await
    });
    accepted.notified().await;
    cancellation.cancel();
    let outcome = run.await.expect("executor task");

    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        outcome.terminal_disposition(),
        &InvocationDispositionV1::CancelledOutcomeUnknown
    );
    release.notify_waiters();
    task.abort();
}

#[tokio::test]
async fn partial_stream_cancel_uses_decoder_visibility_state() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let (mut writer, reader) = tokio::io::duplex(4096);
            tokio::spawn(async move {
                writer
                    .write_all(
                        b"data: {\"id\":\"resp-1\",\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
                    )
                    .await
                    .expect("write first SSE event");
                std::future::pending::<()>().await;
            });
            let stream = ReaderStream::new(reader);
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .body(Body::from_stream(stream))
                .expect("stream response")
        }),
    );
    let (endpoint, task) = serve(app).await;
    let cancellation = CancellationToken::new();
    let sink_cancel = cancellation.clone();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink_events = Arc::clone(&events);

    let outcome = executor()
        .execute(
            &OpenAiCompatWire::new(),
            &request_for(&endpoint, StreamSelection::Enabled),
            api_key_lease(),
            Some(&lease("STREAM-CANCEL-SECRET")),
            &cancellation,
            move |event| {
                if matches!(event, CanonicalStreamEvent::TextDelta { .. }) {
                    sink_cancel.cancel();
                }
                sink_events.lock().expect("events lock").push(event);
            },
        )
        .await;

    assert_eq!(
        outcome.terminal_disposition(),
        &InvocationDispositionV1::CancelledAfterPartial
    );
    let events = events.lock().expect("events lock");
    assert!(events
        .iter()
        .any(|event| matches!(event, CanonicalStreamEvent::TextDelta { text } if text == "hello")));
    assert!(matches!(
        events.last(),
        Some(CanonicalStreamEvent::Failed {
            disposition: InvocationDispositionV1::CancelledAfterPartial
        })
    ));
    task.abort();
}

#[tokio::test]
async fn successful_body_is_capped_and_outcome_never_serializes_secret() {
    let seen_auth = Arc::new(Mutex::new(None::<String>));
    let route_auth = Arc::clone(&seen_auth);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |headers: HeaderMap| {
            *route_auth.lock().expect("auth lock") = headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            async { vec![b'x'; MAX_RESPONSE_BODY_BYTES + 1] }
        }),
    );
    let (endpoint, task) = serve(app).await;
    let cancellation = CancellationToken::new();
    let secret = "BODY-CEILING-SECRET-MARKER";

    let outcome = executor()
        .execute(
            &OpenAiCompatWire::new(),
            &request_for(&endpoint, StreamSelection::Disabled),
            api_key_lease(),
            Some(&lease(secret)),
            &cancellation,
            |_| {},
        )
        .await;

    assert!(matches!(
        outcome.terminal_disposition(),
        InvocationDispositionV1::ProtocolError {
            violation: ProtocolViolation::MalformedBody {
                detail: "response body exceeded the executor byte ceiling"
            }
        }
    ));
    assert_eq!(
        seen_auth.lock().expect("auth lock").as_deref(),
        Some("Bearer BODY-CEILING-SECRET-MARKER")
    );
    let serialized = serde_json::to_string(&outcome).expect("serialize secret-free outcome");
    assert!(
        !serialized.contains(secret),
        "secret reached outcome: {serialized}"
    );
    task.abort();
}

#[test]
fn leased_material_has_no_debug_or_serde_surface() {
    fn assert_serialize<T: serde::Serialize>() {}
    assert_serialize::<BrokerExecutionOutcome>();

    let source = include_str!("../executor.rs");
    let declaration = source
        .find("pub struct LeasedAuthMaterial")
        .expect("leased material declaration");
    let attribute_window = &source[declaration.saturating_sub(160)..declaration];
    assert!(
        !attribute_window.contains("#[derive("),
        "leased material gained a derived trait surface: {attribute_window}"
    );
}
