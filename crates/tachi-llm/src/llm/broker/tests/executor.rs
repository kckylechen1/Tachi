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
fn executor() -> BrokerHttpExecutor {
    crate::install_tls_provider();
    BrokerHttpExecutor::for_test().expect("test HTTP executor")
}

fn request_parts_for(endpoint: &str, stream: StreamSelection) -> CanonicalInvocationRequestParts {
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
    parts
}

fn request_for(endpoint: &str, stream: StreamSelection) -> CanonicalInvocationRequest {
    let parts = request_parts_for(endpoint, stream);
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
async fn executor_refuses_redirects_before_anthropic_key_can_cross_origins() {
    let destination_hits = Arc::new(AtomicUsize::new(0));
    let destination_key = Arc::new(Mutex::new(None::<String>));
    let route_hits = Arc::clone(&destination_hits);
    let route_key = Arc::clone(&destination_key);
    let destination = Router::new().route(
        "/stolen",
        post(move |headers: HeaderMap| {
            route_hits.fetch_add(1, Ordering::SeqCst);
            *route_key.lock().expect("destination key lock") = headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            async { StatusCode::NO_CONTENT }
        }),
    );
    let (destination_endpoint, destination_task) = serve(destination).await;
    let redirect_target = destination_endpoint.replace("/v1/chat/completions", "/stolen");
    let source = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let location = redirect_target.clone();
            async move {
                Response::builder()
                    .status(StatusCode::TEMPORARY_REDIRECT)
                    .header("location", location)
                    .body(Body::empty())
                    .expect("redirect response")
            }
        }),
    );
    let (source_endpoint, source_task) = serve(source).await;
    let mut parts = request_parts_for(&source_endpoint, StreamSelection::Disabled);
    parts.sampling.max_output_tokens = Some(16);
    let request = CanonicalInvocationRequest::new(parts).expect("Anthropic redirect request");

    let outcome = executor()
        .execute(
            &AnthropicWire::new(),
            &request,
            api_key_lease(),
            Some(&lease("REDIRECT-CANARY-MATERIAL")),
            &CancellationToken::new(),
            |_| {},
        )
        .await;

    assert!(matches!(
        outcome.terminal_disposition(),
        InvocationDispositionV1::ProviderRejected { status: 307, .. }
    ));
    assert_eq!(destination_hits.load(Ordering::SeqCst), 0);
    assert_eq!(*destination_key.lock().expect("destination key lock"), None);
    source_task.abort();
    destination_task.abort();
}

#[tokio::test]
async fn a_connect_deadline_shorter_than_the_shared_pool_ceiling_refuses_before_send() {
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
    let mut parts = request_parts_for(&endpoint, StreamSelection::Disabled);
    parts.deadline.connect_ms = Some(1);
    parts.deadline.first_byte_ms = Some(1_000);
    let request = CanonicalInvocationRequest::new(parts).expect("short connect deadline request");

    let outcome = executor()
        .execute(
            &OpenAiCompatWire::new(),
            &request,
            api_key_lease(),
            Some(&lease("CONNECT-DEADLINE-CANARY")),
            &CancellationToken::new(),
            |_| {},
        )
        .await;

    assert_eq!(
        outcome.terminal_disposition(),
        &InvocationDispositionV1::RefusedBeforeSend {
            refusal: BeforeSendRefusal::UnrepresentableRequest {
                detail: "connect deadline is shorter than the shared HTTP pool can enforce",
            },
        }
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn a_response_head_deadline_never_claims_the_provider_accepted_the_request() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            StatusCode::NO_CONTENT
        }),
    );
    let (endpoint, task) = serve(app).await;
    let mut parts = request_parts_for(&endpoint, StreamSelection::Disabled);
    parts.deadline.first_byte_ms = Some(10);
    let request = CanonicalInvocationRequest::new(parts).expect("response-head deadline request");

    let outcome = executor()
        .execute(
            &OpenAiCompatWire::new(),
            &request,
            api_key_lease(),
            Some(&lease("RESPONSE-HEAD-CANARY")),
            &CancellationToken::new(),
            |_| {},
        )
        .await;

    assert_eq!(
        outcome.terminal_disposition(),
        &InvocationDispositionV1::OutcomeUnknown {
            phase: SendPhase::Sending,
        }
    );
    task.abort();
}

#[tokio::test]
async fn first_byte_deadline_stops_an_accepted_response_body_as_outcome_unknown() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let (writer, reader) = tokio::io::duplex(64);
            tokio::spawn(async move {
                let _writer = writer;
                std::future::pending::<()>().await;
            });
            Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(ReaderStream::new(reader)))
                .expect("stalling response")
        }),
    );
    let (endpoint, task) = serve(app).await;
    let mut parts = request_parts_for(&endpoint, StreamSelection::Disabled);
    parts.deadline.first_byte_ms = Some(10);
    let request = CanonicalInvocationRequest::new(parts).expect("deadline request");

    let outcome = executor()
        .execute(
            &OpenAiCompatWire::new(),
            &request,
            api_key_lease(),
            Some(&lease("DEADLINE-CANARY-MATERIAL")),
            &CancellationToken::new(),
            |_| {},
        )
        .await;

    assert_eq!(
        outcome.terminal_disposition(),
        &InvocationDispositionV1::OutcomeUnknown {
            phase: SendPhase::Receiving
        }
    );
    task.abort();
}

#[tokio::test]
async fn a_completed_stream_emits_unknown_usage_before_completion_when_provider_omits_it() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .body(Body::from(
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n\
                     data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                     data: [DONE]\n\n",
                ))
                .expect("completed stream")
        }),
    );
    let (endpoint, task) = serve(app).await;
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);

    let outcome = executor()
        .execute(
            &OpenAiCompatWire::new(),
            &request_for(&endpoint, StreamSelection::Enabled),
            api_key_lease(),
            Some(&lease("USAGE-CANARY-MATERIAL")),
            &CancellationToken::new(),
            move |event| sink.lock().expect("events lock").push(event),
        )
        .await;

    assert!(matches!(
        outcome.terminal_disposition(),
        InvocationDispositionV1::Completed { .. }
    ));
    let events = events.lock().expect("events lock");
    assert!(matches!(
        events.get(events.len() - 2),
        Some(CanonicalStreamEvent::Usage { usage }) if usage == &UsageObservationV1::unknown()
    ));
    assert!(matches!(
        events.last(),
        Some(CanonicalStreamEvent::Completed { .. })
    ));
    task.abort();
}

#[tokio::test]
async fn trailing_data_in_the_terminal_chunk_is_reported_without_rewriting_completion() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .body(Body::from(
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n\
                     data: [DONE]\n\n\
                     data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"late\"},\"finish_reason\":null}]}\n\n",
                ))
                .expect("stream with trailing data")
        }),
    );
    let (endpoint, task) = serve(app).await;

    let outcome = executor()
        .execute(
            &OpenAiCompatWire::new(),
            &request_for(&endpoint, StreamSelection::Enabled),
            api_key_lease(),
            Some(&lease("TRAILING-DATA-CANARY")),
            &CancellationToken::new(),
            |_| {},
        )
        .await;

    assert!(matches!(
        outcome.terminal_disposition(),
        InvocationDispositionV1::Completed { .. }
    ));
    assert_eq!(
        outcome.stream_decode_error(),
        Some(StreamDecodeErrorKind::IllegalSequence)
    );
    task.abort();
}

#[tokio::test]
async fn trailing_data_in_a_later_transport_chunk_is_reported_too() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let (mut writer, reader) = tokio::io::duplex(4_096);
            tokio::spawn(async move {
                writer
                    .write_all(
                        b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n\
                          data: [DONE]\n\n",
                    )
                    .await
                    .expect("write terminal stream chunk");
                writer.flush().await.expect("flush terminal stream chunk");
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                writer
                    .write_all(
                        b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"late\"},\"finish_reason\":null}]}\n\n",
                    )
                    .await
                    .expect("write trailing stream chunk");
            });
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .body(Body::from_stream(ReaderStream::new(reader)))
                .expect("split trailing stream")
        }),
    );
    let (endpoint, task) = serve(app).await;

    let outcome = executor()
        .execute(
            &OpenAiCompatWire::new(),
            &request_for(&endpoint, StreamSelection::Enabled),
            api_key_lease(),
            Some(&lease("SPLIT-TRAILING-DATA-CANARY")),
            &CancellationToken::new(),
            |_| {},
        )
        .await;

    assert!(matches!(
        outcome.terminal_disposition(),
        InvocationDispositionV1::Completed { .. }
    ));
    assert_eq!(
        outcome.stream_decode_error(),
        Some(StreamDecodeErrorKind::IllegalSequence)
    );
    task.abort();
}

#[tokio::test]
async fn a_later_provider_close_marker_does_not_corrupt_an_existing_terminal_failure() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let (mut writer, reader) = tokio::io::duplex(4_096);
            tokio::spawn(async move {
                writer
                    .write_all(b"data: {\"error\":{\"message\":\"overloaded\"}}\n\n")
                    .await
                    .expect("write provider error chunk");
                writer.flush().await.expect("flush provider error chunk");
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                writer
                    .write_all(b"data: [DONE]")
                    .await
                    .expect("write provider close marker");
            });
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .body(Body::from_stream(ReaderStream::new(reader)))
                .expect("split provider error stream")
        }),
    );
    let (endpoint, task) = serve(app).await;

    let outcome = executor()
        .execute(
            &OpenAiCompatWire::new(),
            &request_for(&endpoint, StreamSelection::Enabled),
            api_key_lease(),
            Some(&lease("PROVIDER-ERROR-CLOSE-CANARY")),
            &CancellationToken::new(),
            |_| {},
        )
        .await;

    assert!(matches!(
        outcome.terminal_disposition(),
        InvocationDispositionV1::ProtocolError { .. }
    ));
    assert_eq!(outcome.stream_decode_error(), None);
    task.abort();
}

#[tokio::test]
async fn pending_trailing_data_at_clean_eof_is_not_dropped_after_completion() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let (mut writer, reader) = tokio::io::duplex(4_096);
            tokio::spawn(async move {
                writer
                    .write_all(
                        b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n\
                          data: [DONE]\n\n",
                    )
                    .await
                    .expect("write completed stream chunk");
                writer.flush().await.expect("flush completed stream chunk");
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                writer
                    .write_all(
                        b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"late\"},\"finish_reason\":null}]}",
                    )
                    .await
                    .expect("write pending trailing frame");
            });
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .body(Body::from_stream(ReaderStream::new(reader)))
                .expect("pending trailing stream")
        }),
    );
    let (endpoint, task) = serve(app).await;

    let outcome = executor()
        .execute(
            &OpenAiCompatWire::new(),
            &request_for(&endpoint, StreamSelection::Enabled),
            api_key_lease(),
            Some(&lease("PENDING-TRAILING-DATA-CANARY")),
            &CancellationToken::new(),
            |_| {},
        )
        .await;

    assert!(matches!(
        outcome.terminal_disposition(),
        InvocationDispositionV1::Completed { .. }
    ));
    assert_eq!(
        outcome.stream_decode_error(),
        Some(StreamDecodeErrorKind::IllegalSequence)
    );
    task.abort();
}

#[tokio::test]
async fn a_blank_lease_reference_refuses_before_network() {
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
    let auth = AuthMaterialRef::leased(AuthMaterialKind::ApiKey, "");
    let leased = LeasedAuthMaterial::new(AuthMaterialKind::ApiKey, "", "BLANK-LEASE-MATERIAL");

    let outcome = executor()
        .execute(
            &OpenAiCompatWire::new(),
            &request_for(&endpoint, StreamSelection::Disabled),
            auth,
            Some(&leased),
            &CancellationToken::new(),
            |_| {},
        )
        .await;

    assert!(matches!(
        outcome.terminal_disposition(),
        InvocationDispositionV1::RefusedBeforeSend { .. }
    ));
    assert_eq!(hits.load(Ordering::SeqCst), 0);
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
