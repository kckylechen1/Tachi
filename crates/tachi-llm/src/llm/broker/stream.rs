//! The canonical stream-event vocabulary and the decoder contract.
//!
//! # A freedom, not a constraint
//!
//! The census behind #1682 found **zero** streaming, tool-call, or
//! incremental-parsing code in this crate. So "preserve streaming semantics"
//! pins an empty set: there is no existing consumer to stay compatible with,
//! and defining this vocabulary is a design decision made once, now, rather
//! than a migration. That is why it is frozen in this slice even though the
//! decoder that produces it is the next one — the expensive mistake would be
//! to let each provider's grammar leak into a different event shape.
//!
//! # Why the decoder has four inputs, not one
//!
//! A push-bytes-only decoder cannot express the cases the fixture corpus is
//! built from: a stream that ends without its terminator, a connection that
//! dies mid-event, a caller who cancels between events. Each of those is a
//! *terminal input* with its own correct answer, so the trait names all of
//! them ([`WireStreamDecoder::finish`],
//! [`WireStreamDecoder::on_transport_error`],
//! [`WireStreamDecoder::on_cancel`]) and requires a decoder to end in a
//! terminal disposition rather than merely stopping.
//!
//! # Chunk boundaries are not event boundaries
//!
//! The invariant every decoder must hold is
//! `decode(rechunk(bytes, any_split)) == decode(bytes)`: an event split across
//! two network reads must decode identically to the same bytes delivered
//! whole. It is stated here and fuzzed against the transcript corpus in
//! `tests::stream_chunk_invariance`, because it is the property that a
//! hand-rolled line reader almost always gets wrong.
//!
//! # Not one grammar
//!
//! "SSE transcript per provider" is wrong for one of the six: an Ollama-shaped
//! deployment streams NDJSON, not SSE, and an Anthropic-shaped one uses typed
//! `content_block_delta` / `tool_use` events rather than OpenAI's `delta`
//! shape. The fixture families are therefore per *grammar*, not per provider —
//! which is exactly the test of whether this vocabulary is OpenAI-biased.
//!
//! **This module is the vocabulary and the contract; no decoder lives here.**
//! The `delta` and Anthropic-event grammars are implemented against this trait
//! in [`super::openai_stream`] and [`super::anthropic_stream`], and
//! [`ProviderWire::new_stream_decoder`](super::ProviderWire::new_stream_decoder)
//! still answers with a typed [`StreamDecoderUnavailable`] — rather than a
//! panic or a silent non-streaming downgrade — for any surface that has no
//! decoder to hand back.

use serde::{Deserialize, Serialize};

use super::disposition::{CompletionKindV1, InvocationDispositionV1};
use super::usage::UsageObservationV1;
use super::wire::ProviderResponseMetadata;

/// An incremental tool-call fragment as reconstructed from a stream.
///
/// `arguments_delta` is a *fragment of a JSON document*, not a JSON document:
/// providers split argument objects across events at arbitrary byte offsets,
/// so a decoder must accumulate and may only parse at the call's end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallFragment {
    /// Position of this call within the assistant turn.
    pub index: u32,
    /// Provider-assigned call id, when it has been seen yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Tool name, when it has been seen yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Raw argument bytes seen in this event.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub arguments_delta: String,
}

/// One canonical event in an incremental response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CanonicalStreamEvent {
    /// The provider accepted the request and began generating.
    #[serde(rename = "started")]
    Started {
        /// Provider-side identity of the response, when it announces one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<ProviderResponseMetadata>,
    },
    /// A fragment of assistant text.
    #[serde(rename = "text_delta")]
    TextDelta {
        /// The fragment, exactly as received.
        text: String,
    },
    /// A fragment of a tool call.
    #[serde(rename = "tool_call_delta")]
    ToolCallDelta {
        /// The fragment.
        fragment: ToolCallFragment,
    },
    /// A tool call is complete and its arguments parse.
    #[serde(rename = "tool_call_completed")]
    ToolCallCompleted {
        /// Which call.
        index: u32,
    },
    /// A usage report arrived mid- or end-of-stream.
    #[serde(rename = "usage")]
    Usage {
        /// The observation.
        usage: UsageObservationV1,
    },
    /// The generation ended cleanly.
    #[serde(rename = "completed")]
    Completed {
        /// How it ended.
        completion: CompletionKindV1,
    },
    /// The stream ended in a typed failure. Terminal.
    #[serde(rename = "failed")]
    Failed {
        /// The terminal disposition.
        disposition: InvocationDispositionV1,
    },
}

impl CanonicalStreamEvent {
    /// The fieldless tag for this event.
    pub fn kind(&self) -> CanonicalStreamEventKind {
        match self {
            Self::Started { .. } => CanonicalStreamEventKind::Started,
            Self::TextDelta { .. } => CanonicalStreamEventKind::TextDelta,
            Self::ToolCallDelta { .. } => CanonicalStreamEventKind::ToolCallDelta,
            Self::ToolCallCompleted { .. } => CanonicalStreamEventKind::ToolCallCompleted,
            Self::Usage { .. } => CanonicalStreamEventKind::Usage,
            Self::Completed { .. } => CanonicalStreamEventKind::Completed,
            Self::Failed { .. } => CanonicalStreamEventKind::Failed,
        }
    }

    /// Whether no further event may follow this one.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed { .. } | Self::Failed { .. })
    }
}

/// The fieldless tag vocabulary of [`CanonicalStreamEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CanonicalStreamEventKind {
    /// See [`CanonicalStreamEvent::Started`].
    #[serde(rename = "started")]
    Started,
    /// See [`CanonicalStreamEvent::TextDelta`].
    #[serde(rename = "text_delta")]
    TextDelta,
    /// See [`CanonicalStreamEvent::ToolCallDelta`].
    #[serde(rename = "tool_call_delta")]
    ToolCallDelta,
    /// See [`CanonicalStreamEvent::ToolCallCompleted`].
    #[serde(rename = "tool_call_completed")]
    ToolCallCompleted,
    /// See [`CanonicalStreamEvent::Usage`].
    #[serde(rename = "usage")]
    Usage,
    /// See [`CanonicalStreamEvent::Completed`].
    #[serde(rename = "completed")]
    Completed,
    /// See [`CanonicalStreamEvent::Failed`].
    #[serde(rename = "failed")]
    Failed,
}

impl CanonicalStreamEventKind {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::TextDelta => "text_delta",
            Self::ToolCallDelta => "tool_call_delta",
            Self::ToolCallCompleted => "tool_call_completed",
            Self::Usage => "usage",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "started" => Self::Started,
            "text_delta" => Self::TextDelta,
            "tool_call_delta" => Self::ToolCallDelta,
            "tool_call_completed" => Self::ToolCallCompleted,
            "usage" => Self::Usage,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [CanonicalStreamEventKind] = &[
        Self::Started,
        Self::TextDelta,
        Self::ToolCallDelta,
        Self::ToolCallCompleted,
        Self::Usage,
        Self::Completed,
        Self::Failed,
    ];
}

/// Which decode rule a byte sequence broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StreamDecodeErrorKind {
    /// A framed event was not valid UTF-8.
    #[serde(rename = "invalid_utf8")]
    InvalidUtf8,
    /// The transport framing (SSE field syntax, NDJSON line shape) was broken.
    #[serde(rename = "malformed_frame")]
    MalformedFrame,
    /// A frame parsed as JSON but did not match the dialect's event schema.
    #[serde(rename = "unknown_event_shape")]
    UnknownEventShape,
    /// Events arrived in an order the grammar forbids (a delta after the
    /// terminator, a tool argument for a call that never started).
    #[serde(rename = "illegal_sequence")]
    IllegalSequence,
    /// The stream ended before its grammar's terminator.
    #[serde(rename = "unterminated_stream")]
    UnterminatedStream,
    /// A single event exceeded the decoder's buffer ceiling. A stream is
    /// attacker-influenced input; an unbounded accumulator is a memory bug
    /// waiting for a hostile provider.
    #[serde(rename = "event_too_large")]
    EventTooLarge,
}

impl StreamDecodeErrorKind {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidUtf8 => "invalid_utf8",
            Self::MalformedFrame => "malformed_frame",
            Self::UnknownEventShape => "unknown_event_shape",
            Self::IllegalSequence => "illegal_sequence",
            Self::UnterminatedStream => "unterminated_stream",
            Self::EventTooLarge => "event_too_large",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "invalid_utf8" => Self::InvalidUtf8,
            "malformed_frame" => Self::MalformedFrame,
            "unknown_event_shape" => Self::UnknownEventShape,
            "illegal_sequence" => Self::IllegalSequence,
            "unterminated_stream" => Self::UnterminatedStream,
            "event_too_large" => Self::EventTooLarge,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [StreamDecodeErrorKind] = &[
        Self::InvalidUtf8,
        Self::MalformedFrame,
        Self::UnknownEventShape,
        Self::IllegalSequence,
        Self::UnterminatedStream,
        Self::EventTooLarge,
    ];
}

/// A decode failure, with a body-free detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StreamDecodeError {
    /// Which rule was broken.
    pub kind: StreamDecodeErrorKind,
    /// A static description. Static on purpose: provider stream bytes are
    /// untrusted, and an error string is the classic path by which they end up
    /// in a log.
    pub detail: &'static str,
}

/// How a stream's byte source ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StreamEof {
    /// The body ended where the transport said it would.
    #[serde(rename = "clean")]
    Clean,
    /// The body ended early (short read, truncated chunked encoding).
    #[serde(rename = "truncated")]
    Truncated,
}

impl StreamEof {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Truncated => "truncated",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "clean" => Self::Clean,
            "truncated" => Self::Truncated,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [StreamEof] = &[Self::Clean, Self::Truncated];
}

/// How the transport under a stream failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransportErrorKind {
    /// The connection was reset or closed by the peer mid-stream.
    #[serde(rename = "connection_reset")]
    ConnectionReset,
    /// No byte arrived within the read deadline.
    #[serde(rename = "read_timeout")]
    ReadTimeout,
    /// The TLS session failed mid-stream.
    #[serde(rename = "tls_failure")]
    TlsFailure,
    /// Anything else the transport reported.
    #[serde(rename = "other")]
    Other,
}

impl TransportErrorKind {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConnectionReset => "connection_reset",
            Self::ReadTimeout => "read_timeout",
            Self::TlsFailure => "tls_failure",
            Self::Other => "other",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "connection_reset" => Self::ConnectionReset,
            "read_timeout" => Self::ReadTimeout,
            "tls_failure" => Self::TlsFailure,
            "other" => Self::Other,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [TransportErrorKind] = &[
        Self::ConnectionReset,
        Self::ReadTimeout,
        Self::TlsFailure,
        Self::Other,
    ];
}

/// Why a dialect handed back no stream decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StreamDecoderUnavailableReason {
    /// No streaming grammar exists on the negotiated surface — the
    /// intersection of what the dialect implements and what the deployment
    /// admits. Both halves reach it: a dialect with no streaming grammar at
    /// all, and a dialect that has one for a deployment narrowed to
    /// `streaming: false`, which may not be handed a decoder it was told it
    /// must not use.
    ///
    /// The distinction that matters to the caller is the one against
    /// [`Self::NotImplementedYet`] — *nothing here streams* versus *the
    /// grammar exists and this Broker has not written it* — because only the
    /// second is a gap this Broker can close. The variant name and its wire
    /// spelling are frozen and describe the first half of the reading above;
    /// this note is the whole of it.
    #[serde(rename = "dialect_does_not_stream")]
    DialectDoesNotStream,
    /// The grammar exists and this Broker has not implemented it yet. A
    /// *typed* answer, so the executor turns it into a refusal with a truthful
    /// reason instead of a panic or a silent non-streaming downgrade.
    #[serde(rename = "not_implemented_yet")]
    NotImplementedYet,
}

impl StreamDecoderUnavailableReason {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DialectDoesNotStream => "dialect_does_not_stream",
            Self::NotImplementedYet => "not_implemented_yet",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "dialect_does_not_stream" => Self::DialectDoesNotStream,
            "not_implemented_yet" => Self::NotImplementedYet,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [StreamDecoderUnavailableReason] =
        &[Self::DialectDoesNotStream, Self::NotImplementedYet];
}

/// A dialect could not supply a stream decoder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StreamDecoderUnavailable {
    /// Why.
    pub reason: StreamDecoderUnavailableReason,
    /// The dialect that could not supply one.
    pub dialect: &'static str,
}

impl StreamDecoderUnavailable {
    /// The before-send refusal this becomes when a caller asked to stream.
    ///
    /// Always a refusal, never a downgrade: answering a `stream: enabled`
    /// request with a non-streaming body is the silent-degradation bug the
    /// protocol law exists to forbid.
    pub fn into_refusal(self) -> super::disposition::BeforeSendRefusal {
        super::disposition::BeforeSendRefusal::UnsupportedCapability {
            missing: vec![super::disposition::UnsupportedCapability::Streaming],
        }
    }
}

/// Turns a provider's stream bytes into canonical events.
///
/// Sans-IO: a decoder is pushed bytes, it never reads them. It is a state
/// machine over a byte sequence, which is what makes the fixture corpus — raw
/// recorded chunks in, expected event sequence out — a complete test of it.
///
/// Implementations must hold:
///
/// - **Chunk-boundary invariance.** Re-chunking the same bytes at any split
///   produces the same event sequence.
/// - **Terminality.** After a terminal event, every further input yields no
///   further events and [`Self::terminal_disposition`] stays fixed.
/// - **Boundedness.** A single accumulated event is capped
///   ([`StreamDecodeErrorKind::EventTooLarge`]); provider bytes are untrusted
///   input.
///
/// Not implemented in this slice — see the module note.
pub trait WireStreamDecoder: Send {
    /// Feed the next chunk of body bytes, in order.
    fn push_bytes(&mut self, chunk: &[u8]) -> Result<Vec<CanonicalStreamEvent>, StreamDecodeError>;

    /// The byte source ended. A decoder that has not seen its grammar's
    /// terminator must report [`StreamDecodeErrorKind::UnterminatedStream`]
    /// rather than pretending the response completed.
    fn finish(&mut self, eof: StreamEof) -> Result<Vec<CanonicalStreamEvent>, StreamDecodeError>;

    /// The transport failed mid-stream. Returns any events the decoder can
    /// still justify plus its terminal failure event; infallible, because
    /// there is nothing left to fail into.
    fn on_transport_error(&mut self, error: TransportErrorKind) -> Vec<CanonicalStreamEvent>;

    /// The caller cancelled. Whether this ends as
    /// `CancelledAfterPartial` or `CancelledOutcomeUnknown` depends on whether
    /// the decoder has already emitted visible output — which only the decoder
    /// knows, which is why this is its call and not the executor's.
    fn on_cancel(&mut self) -> Vec<CanonicalStreamEvent>;

    /// The terminal disposition, once the decoder has reached one.
    fn terminal_disposition(&self) -> Option<InvocationDispositionV1>;
}
