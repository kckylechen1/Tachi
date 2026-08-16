//! The Broker's single HTTP execution point.
//!
//! Provider adapters remain sans-IO. This module holds one clone of the shared
//! pooled `reqwest::Client`, injects leased auth material in memory, sends
//! exactly once, and drives streaming decoders incrementally.

use std::future::{poll_fn, Future};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::{
    AuthMaterialKind, AuthMaterialRef, AuthPlacement, BeforeSendRefusal,
    CanonicalInvocationRequest, CanonicalStreamEvent, HttpMethod, InvocationDispositionV1,
    ProtocolViolation, ProviderWire, ResponseHeaders, SendPhase, StreamDecodeErrorKind, StreamEof,
    StreamSelection, TransportErrorKind, UsageObservationV1, WireHttpRequest, WireOutcome,
    WireStreamDecoder,
};

/// Maximum retained body for a successful non-streaming response.
pub const MAX_RESPONSE_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Maximum retained prefix of a provider rejection body.
pub const MAX_ERROR_BODY_EXCERPT_BYTES: usize = 64 * 1024;

const BODY_CEILING_DETAIL: &str = "response body exceeded the executor byte ceiling";
const REQUEST_BUILD_DETAIL: &str = "executor could not construct the admitted HTTP request";
const LEASE_MISMATCH_DETAIL: &str = "leased auth material did not match its opaque reference";
const CONNECT_DEADLINE_DETAIL: &str =
    "connect deadline is shorter than the shared HTTP pool can enforce";

/// Secret material resolved from one opaque lease.
///
/// The type is public so a future lease boundary can hand it to the executor,
/// but only code inside this crate can construct one. It intentionally has no
/// `Debug`, `Serialize`, or `Deserialize` implementation.
pub struct LeasedAuthMaterial {
    kind: AuthMaterialKind,
    lease_ref: String,
    secret: String,
}

impl LeasedAuthMaterial {
    #[allow(
        dead_code,
        reason = "the durable lease boundary lands after this slice"
    )]
    pub(crate) fn new(
        kind: AuthMaterialKind,
        lease_ref: impl Into<String>,
        secret: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            lease_ref: lease_ref.into(),
            secret: secret.into(),
        }
    }
}

/// Secret-free result of one executor attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrokerExecutionOutcome {
    disposition: InvocationDispositionV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wire_outcome: Option<WireOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stream_decode_error: Option<StreamDecodeErrorKind>,
}

impl BrokerExecutionOutcome {
    fn disposition(disposition: InvocationDispositionV1) -> Self {
        Self {
            disposition,
            wire_outcome: None,
            stream_decode_error: None,
        }
    }

    fn wire(wire_outcome: WireOutcome) -> Self {
        Self {
            disposition: wire_outcome.disposition(),
            wire_outcome: Some(wire_outcome),
            stream_decode_error: None,
        }
    }

    fn stream(
        disposition: InvocationDispositionV1,
        stream_decode_error: Option<StreamDecodeErrorKind>,
    ) -> Self {
        Self {
            disposition,
            wire_outcome: None,
            stream_decode_error,
        }
    }

    /// The terminal safety disposition.
    pub fn terminal_disposition(&self) -> &InvocationDispositionV1 {
        &self.disposition
    }

    /// The parsed non-streaming outcome, when this was a body response.
    pub fn wire_outcome(&self) -> Option<&WireOutcome> {
        self.wire_outcome.as_ref()
    }

    /// A decoder error observed alongside the first-writer-wins disposition.
    pub fn stream_decode_error(&self) -> Option<StreamDecodeErrorKind> {
        self.stream_decode_error
    }
}

/// Executes provider wire requests through the existing shared pooled client.
#[derive(Clone)]
pub struct BrokerHttpExecutor {
    http: reqwest::Client,
}

impl BrokerHttpExecutor {
    #[cfg(test)]
    pub(crate) fn for_test() -> Result<Self, String> {
        crate::llm::LlmClient::build_http_client().map(|http| Self { http })
    }

    /// Build and execute one provider request. This method never retries.
    pub async fn execute<W, F>(
        &self,
        wire: &W,
        request: &CanonicalInvocationRequest,
        auth: AuthMaterialRef<'_>,
        leased: Option<&LeasedAuthMaterial>,
        cancellation: &CancellationToken,
        mut emit: F,
    ) -> BrokerExecutionOutcome
    where
        W: ProviderWire + ?Sized,
        F: FnMut(CanonicalStreamEvent),
    {
        if cancellation.is_cancelled() {
            return BrokerExecutionOutcome::disposition(
                InvocationDispositionV1::CancelledBeforeSend,
            );
        }
        let deadline = request.deadline();
        if deadline.connect_ms.is_some_and(|millis| {
            millis < crate::llm::LlmClient::SHARED_CONNECT_TIMEOUT_SECS.saturating_mul(1_000)
        }) {
            return BrokerExecutionOutcome::disposition(
                InvocationDispositionV1::RefusedBeforeSend {
                    refusal: BeforeSendRefusal::UnrepresentableRequest {
                        detail: CONNECT_DEADLINE_DETAIL,
                    },
                },
            );
        }
        let deadlines = ExecutionDeadlines::new(deadline);

        let mut decoder = if request.stream() == StreamSelection::Enabled {
            match wire.new_stream_decoder() {
                Ok(decoder) => Some(decoder),
                Err(unavailable) => {
                    return BrokerExecutionOutcome::disposition(
                        InvocationDispositionV1::RefusedBeforeSend {
                            refusal: unavailable.into_refusal(),
                        },
                    );
                }
            }
        } else {
            None
        };

        let wire_request = match wire.build_request(request, auth) {
            Ok(request) => request,
            Err(refusal) => {
                return BrokerExecutionOutcome::disposition(
                    InvocationDispositionV1::RefusedBeforeSend { refusal },
                );
            }
        };
        let http_request = match self.build_http_request(&wire_request, auth, leased) {
            Ok(request) => request,
            Err(refusal) => {
                return BrokerExecutionOutcome::disposition(
                    InvocationDispositionV1::RefusedBeforeSend { refusal },
                );
            }
        };

        let send_was_polled = AtomicBool::new(false);
        let send = self.http.execute(http_request);
        let mut send = pin!(send);
        let tracked_send = poll_fn(|cx| {
            send_was_polled.store(true, Ordering::Release);
            send.as_mut().poll(cx)
        });
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                let disposition = if send_was_polled.load(Ordering::Acquire) {
                    InvocationDispositionV1::CancelledOutcomeUnknown
                } else {
                    InvocationDispositionV1::CancelledBeforeSend
                };
                return BrokerExecutionOutcome::disposition(disposition);
            }
            _ = wait_until(deadlines.response_head) => {
                let phase = if send_was_polled.load(Ordering::Acquire) {
                    SendPhase::Sending
                } else {
                    SendPhase::Connecting
                };
                return BrokerExecutionOutcome::disposition(
                    InvocationDispositionV1::OutcomeUnknown { phase },
                );
            }
            response = tracked_send => response,
        };
        let mut response = match response {
            Ok(response) => response,
            Err(error) => {
                let phase = if error.is_connect() {
                    SendPhase::Connecting
                } else {
                    SendPhase::AwaitingResponse
                };
                return BrokerExecutionOutcome::disposition(
                    InvocationDispositionV1::OutcomeUnknown { phase },
                );
            }
        };

        let status = response.status().as_u16();
        let headers = response_headers(response.headers());
        if !(200..300).contains(&status) {
            return self
                .read_rejection(
                    wire,
                    status,
                    &headers,
                    &mut response,
                    cancellation,
                    deadlines,
                )
                .await;
        }

        match decoder.as_mut() {
            Some(decoder) => {
                self.drive_stream(
                    decoder.as_mut(),
                    &mut response,
                    cancellation,
                    deadlines,
                    &mut emit,
                )
                .await
            }
            None => {
                self.read_non_streaming(
                    wire,
                    status,
                    &headers,
                    &mut response,
                    cancellation,
                    deadlines,
                )
                .await
            }
        }
    }

    fn build_http_request(
        &self,
        request: &WireHttpRequest,
        auth: AuthMaterialRef<'_>,
        leased: Option<&LeasedAuthMaterial>,
    ) -> Result<reqwest::Request, BeforeSendRefusal> {
        let method = match request.method() {
            HttpMethod::Post => reqwest::Method::POST,
            HttpMethod::Get => reqwest::Method::GET,
        };
        let mut builder = self.http.request(method, request.url());
        for header in request.headers() {
            builder = builder.header(header.name(), header.value());
        }
        match request.auth_placement() {
            AuthPlacement::None => {
                if auth.kind() != AuthMaterialKind::None || leased.is_some() {
                    return Err(BeforeSendRefusal::AuthMaterialUnsupported {
                        offered: auth.kind().as_str().to_string(),
                    });
                }
            }
            AuthPlacement::Header { name, prefix } => {
                let Some(leased) = leased else {
                    return Err(BeforeSendRefusal::AuthMaterialUnsupported {
                        offered: auth.kind().as_str().to_string(),
                    });
                };
                if leased.kind != auth.kind() {
                    return Err(BeforeSendRefusal::AuthMaterialUnsupported {
                        offered: leased.kind.as_str().to_string(),
                    });
                }
                if auth.lease_ref().trim().is_empty() || leased.lease_ref != auth.lease_ref() {
                    return Err(BeforeSendRefusal::UnrepresentableRequest {
                        detail: LEASE_MISMATCH_DETAIL,
                    });
                }
                builder = builder.header(*name, format!("{prefix}{}", leased.secret));
            }
        }
        builder.body(request.body().to_vec()).build().map_err(|_| {
            BeforeSendRefusal::UnrepresentableRequest {
                detail: REQUEST_BUILD_DETAIL,
            }
        })
    }

    async fn read_rejection<W: ProviderWire + ?Sized>(
        &self,
        wire: &W,
        status: u16,
        headers: &ResponseHeaders,
        response: &mut reqwest::Response,
        cancellation: &CancellationToken,
        deadlines: ExecutionDeadlines,
    ) -> BrokerExecutionOutcome {
        let mut body = Vec::new();
        let mut first_byte_seen = false;
        loop {
            let chunk = tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    return BrokerExecutionOutcome::disposition(
                        InvocationDispositionV1::CancelledOutcomeUnknown,
                    );
                }
                _ = wait_until(deadlines.body(first_byte_seen)) => {
                    return BrokerExecutionOutcome::disposition(
                        InvocationDispositionV1::OutcomeUnknown {
                            phase: SendPhase::Receiving,
                        },
                    );
                }
                chunk = response.chunk() => chunk,
            };
            match chunk {
                Ok(Some(chunk)) => {
                    first_byte_seen |= !chunk.is_empty();
                    let remaining = MAX_ERROR_BODY_EXCERPT_BYTES.saturating_sub(body.len());
                    body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                    if body.len() == MAX_ERROR_BODY_EXCERPT_BYTES {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    return BrokerExecutionOutcome::disposition(
                        InvocationDispositionV1::OutcomeUnknown {
                            phase: SendPhase::Receiving,
                        },
                    );
                }
            }
        }
        let excerpt = String::from_utf8_lossy(&body);
        let outcome = WireOutcome::Rejected {
            status,
            classification: wire.classify_error(status, headers, &excerpt),
        };
        BrokerExecutionOutcome::wire(outcome)
    }

    async fn read_non_streaming<W: ProviderWire + ?Sized>(
        &self,
        wire: &W,
        status: u16,
        headers: &ResponseHeaders,
        response: &mut reqwest::Response,
        cancellation: &CancellationToken,
        deadlines: ExecutionDeadlines,
    ) -> BrokerExecutionOutcome {
        let mut body = Vec::new();
        let mut first_byte_seen = false;
        loop {
            let chunk = tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    return BrokerExecutionOutcome::disposition(
                        InvocationDispositionV1::CancelledOutcomeUnknown,
                    );
                }
                _ = wait_until(deadlines.body(first_byte_seen)) => {
                    return BrokerExecutionOutcome::disposition(
                        InvocationDispositionV1::OutcomeUnknown {
                            phase: SendPhase::Receiving,
                        },
                    );
                }
                chunk = response.chunk() => chunk,
            };
            match chunk {
                Ok(Some(chunk)) => {
                    first_byte_seen |= !chunk.is_empty();
                    if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BODY_BYTES {
                        return BrokerExecutionOutcome::disposition(
                            InvocationDispositionV1::ProtocolError {
                                violation: ProtocolViolation::MalformedBody {
                                    detail: BODY_CEILING_DETAIL,
                                },
                            },
                        );
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(_) => {
                    return BrokerExecutionOutcome::disposition(
                        InvocationDispositionV1::OutcomeUnknown {
                            phase: SendPhase::Receiving,
                        },
                    );
                }
            }
        }
        BrokerExecutionOutcome::wire(wire.parse_response(status, headers, &body))
    }

    async fn drive_stream<F>(
        &self,
        decoder: &mut dyn WireStreamDecoder,
        response: &mut reqwest::Response,
        cancellation: &CancellationToken,
        deadlines: ExecutionDeadlines,
        emit: &mut F,
    ) -> BrokerExecutionOutcome
    where
        F: FnMut(CanonicalStreamEvent),
    {
        let mut first_byte_seen = false;
        let mut saw_usage = false;
        let mut terminal_seen = false;
        loop {
            let chunk = tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    if terminal_seen {
                        return terminal_outcome(decoder, None);
                    }
                    let events = decoder.on_cancel();
                    emit_events(events, &mut saw_usage, emit);
                    return terminal_outcome(decoder, None);
                }
                _ = wait_until(deadlines.body(first_byte_seen)) => {
                    if terminal_seen {
                        return terminal_outcome(decoder, None);
                    }
                    let events = decoder.on_transport_error(TransportErrorKind::ReadTimeout);
                    emit_events(events, &mut saw_usage, emit);
                    return terminal_outcome(decoder, None);
                }
                chunk = response.chunk() => chunk,
            };
            match chunk {
                Ok(Some(chunk)) if terminal_seen => {
                    if chunk.is_empty() {
                        continue;
                    }
                    match decoder.push_bytes(&chunk) {
                        Ok(events) if events.is_empty() => match decoder.push_bytes(&[]) {
                            Ok(events) if events.is_empty() => continue,
                            Ok(_) => {
                                return terminal_outcome(
                                    decoder,
                                    Some(StreamDecodeErrorKind::IllegalSequence),
                                )
                            }
                            Err(error) => return terminal_outcome(decoder, Some(error.kind)),
                        },
                        Ok(_) => {
                            return terminal_outcome(
                                decoder,
                                Some(StreamDecodeErrorKind::IllegalSequence),
                            )
                        }
                        Err(error) => return terminal_outcome(decoder, Some(error.kind)),
                    }
                }
                Ok(Some(chunk)) => match decoder.push_bytes(&chunk) {
                    Ok(events) => {
                        first_byte_seen |= !chunk.is_empty();
                        let emitted_terminal = emit_events(events, &mut saw_usage, emit);
                        if decoder.terminal_disposition().is_some() {
                            if !emitted_terminal {
                                emit_terminal_failure(decoder, emit);
                            }
                            let trailing_error =
                                decoder.push_bytes(&[]).err().map(|error| error.kind);
                            if trailing_error.is_some() {
                                return terminal_outcome(decoder, trailing_error);
                            }
                            terminal_seen = true;
                        }
                    }
                    Err(error) => {
                        emit_terminal_failure(decoder, emit);
                        return terminal_outcome(decoder, Some(error.kind));
                    }
                },
                Ok(None) => {
                    if terminal_seen {
                        return terminal_outcome(decoder, None);
                    }
                    let (events, decode_error) = match decoder.finish(StreamEof::Clean) {
                        Ok(events) => (events, None),
                        Err(error) => (Vec::new(), Some(error.kind)),
                    };
                    let emitted_terminal = emit_events(events, &mut saw_usage, emit);
                    if !emitted_terminal {
                        emit_terminal_failure(decoder, emit);
                    }
                    return terminal_outcome(decoder, decode_error);
                }
                Err(error) => {
                    if terminal_seen {
                        return terminal_outcome(decoder, None);
                    }
                    let events = decoder.on_transport_error(transport_error_kind(&error));
                    emit_events(events, &mut saw_usage, emit);
                    return terminal_outcome(decoder, None);
                }
            }
        }
    }
}

fn response_headers(headers: &reqwest::header::HeaderMap) -> ResponseHeaders {
    ResponseHeaders::from_pairs(
        headers
            .iter()
            .filter_map(|(name, value)| value.to_str().ok().map(|value| (name.as_str(), value))),
    )
}

fn transport_error_kind(error: &reqwest::Error) -> TransportErrorKind {
    if error.is_timeout() {
        TransportErrorKind::ReadTimeout
    } else if error.is_connect() {
        TransportErrorKind::ConnectionReset
    } else {
        TransportErrorKind::Other
    }
}

fn emit_events<F>(events: Vec<CanonicalStreamEvent>, saw_usage: &mut bool, emit: &mut F) -> bool
where
    F: FnMut(CanonicalStreamEvent),
{
    let mut emitted_terminal = false;
    for event in events {
        if matches!(event, CanonicalStreamEvent::Usage { .. }) {
            *saw_usage = true;
        }
        if matches!(event, CanonicalStreamEvent::Completed { .. }) && !*saw_usage {
            emit(CanonicalStreamEvent::Usage {
                usage: UsageObservationV1::unknown(),
            });
            *saw_usage = true;
        }
        emitted_terminal |= event.is_terminal();
        emit(event);
    }
    emitted_terminal
}

#[derive(Debug, Clone, Copy)]
struct ExecutionDeadlines {
    total: Option<tokio::time::Instant>,
    first_byte: Option<tokio::time::Instant>,
    response_head: Option<tokio::time::Instant>,
}

impl ExecutionDeadlines {
    fn new(context: super::DeadlineContext) -> Self {
        let started = tokio::time::Instant::now();
        let total = deadline_at(started, context.total_ms);
        let first_byte = deadline_at(started, context.first_byte_ms);
        Self {
            total,
            first_byte,
            response_head: earliest(total, first_byte),
        }
    }

    fn body(self, first_byte_seen: bool) -> Option<tokio::time::Instant> {
        if first_byte_seen {
            self.total
        } else {
            earliest(self.total, self.first_byte)
        }
    }
}

impl crate::llm::LlmClient {
    /// Bind the Broker executor to this client's existing credential-safe
    /// pool. The raw client is deliberately not injectable: redirect and
    /// proxy policy are send-safety invariants, and one logical Broker attempt
    /// must remain one physical HTTP send.
    pub fn broker_http_executor(&self) -> BrokerHttpExecutor {
        BrokerHttpExecutor {
            http: self.http_client(),
        }
    }
}

fn deadline_at(started: tokio::time::Instant, millis: Option<u64>) -> Option<tokio::time::Instant> {
    millis.and_then(|millis| started.checked_add(Duration::from_millis(millis)))
}

fn earliest(
    left: Option<tokio::time::Instant>,
    right: Option<tokio::time::Instant>,
) -> Option<tokio::time::Instant> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
        (None, None) => None,
    }
}

async fn wait_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

fn emit_terminal_failure<F>(decoder: &dyn WireStreamDecoder, emit: &mut F)
where
    F: FnMut(CanonicalStreamEvent),
{
    if let Some(disposition) = decoder.terminal_disposition() {
        match disposition {
            InvocationDispositionV1::Completed { completion } => {
                emit(CanonicalStreamEvent::Completed { completion });
            }
            disposition => emit(CanonicalStreamEvent::Failed { disposition }),
        }
    }
}

fn terminal_outcome(
    decoder: &dyn WireStreamDecoder,
    stream_decode_error: Option<StreamDecodeErrorKind>,
) -> BrokerExecutionOutcome {
    BrokerExecutionOutcome::stream(
        decoder
            .terminal_disposition()
            .unwrap_or(InvocationDispositionV1::OutcomeUnknown {
                phase: SendPhase::Receiving,
            }),
        stream_decode_error,
    )
}
