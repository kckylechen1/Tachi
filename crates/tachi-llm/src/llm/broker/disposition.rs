//! The typed disposition vocabulary — every way an invocation can end.
//!
//! # Why a full spectrum, frozen now
//!
//! Today the only shipped completion vocabulary is
//! [`CompletionStatusV1`](crate::CompletionStatusV1) (`Complete` / `Truncated`
//! / `Unknown`), which describes *how a body ended* and cannot describe an
//! invocation that never got a body. Everything else collapses into a string
//! error, and a string error cannot answer the two questions that actually
//! matter after a failure: **did the provider do work we will be billed for**,
//! and **is a retry safe**.
//!
//! So the spectrum is closed and typed:
//!
//! | Disposition | Provider did work? | Retry |
//! |---|---|---|
//! | [`RefusedBeforeSend`](InvocationDispositionV1::RefusedBeforeSend) | no | safe |
//! | [`CancelledBeforeSend`](InvocationDispositionV1::CancelledBeforeSend) | no | safe |
//! | [`ProviderRejected`](InvocationDispositionV1::ProviderRejected) | rejected it | per error class |
//! | [`OutcomeUnknown`](InvocationDispositionV1::OutcomeUnknown) | **unknowable** | only on caller opt-in |
//! | [`CancelledOutcomeUnknown`](InvocationDispositionV1::CancelledOutcomeUnknown) | **unknowable** | only on caller opt-in |
//! | [`CancelledConfirmed`](InvocationDispositionV1::CancelledConfirmed) | no (proven) | safe |
//! | [`CancelledAfterPartial`](InvocationDispositionV1::CancelledAfterPartial) | yes, partially emitted | forbidden |
//! | [`ProtocolError`](InvocationDispositionV1::ProtocolError) | yes | forbidden by default |
//! | [`Completed`](InvocationDispositionV1::Completed) | yes | n/a |
//!
//! The load-bearing row is *outcome-unknown*. A timeout after the request was
//! accepted does not mean the call failed — it means this process stopped
//! being able to observe it. Reporting that as a failure and retrying is how a
//! deadline turns into double spend and a double side-effect. `Unknown` is an
//! honest tier here, not a degraded one, and `CancelledConfirmed` is reachable
//! **only with protocol evidence** ([`CancellationEvidence`]).
//!
//! # Relationship to the durable receipt (#1519)
//!
//! This vocabulary does **not** touch `CompletionStatusV1` and does not amend
//! the `model-invocation-v1` schema, whose bytes stay unchanged and whose
//! authoring authority is #1519's. [`CompletionKindV1::to_completion_status_v1`]
//! is the lossy projection onto the existing three-value status; the richer
//! disposition reaches durable storage through the broker-side wrapper the
//! #1519 owner amendment permits. A mapping-table proposal goes to #1519; this
//! module does not decide it.

use serde::{Deserialize, Serialize};

use super::stream::StreamDecodeErrorKind;
use super::wire::{ProviderErrorClass, RetryAfter};
use crate::CompletionStatusV1;

// ---------------------------------------------------------------------------
// Before-send refusal
// ---------------------------------------------------------------------------

/// One capability a deployment lacks for a request that needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UnsupportedCapability {
    /// The deployment does not serve chat.
    #[serde(rename = "chat")]
    Chat,
    /// The deployment cannot call tools.
    #[serde(rename = "tools")]
    Tools,
    /// The deployment cannot stream. Refusing here is the protocol law: a
    /// `stream: enabled` request must never be silently answered non-streaming.
    #[serde(rename = "streaming")]
    Streaming,
    /// The deployment cannot constrain output shape at all.
    #[serde(rename = "structured_output")]
    StructuredOutput,
    /// The deployment supports `json_object` but not strict JSON Schema.
    #[serde(rename = "json_schema")]
    JsonSchema,
    /// The deployment cannot accept non-text parts.
    #[serde(rename = "media")]
    Media,
}

impl UnsupportedCapability {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Tools => "tools",
            Self::Streaming => "streaming",
            Self::StructuredOutput => "structured_output",
            Self::JsonSchema => "json_schema",
            Self::Media => "media",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "chat" => Self::Chat,
            "tools" => Self::Tools,
            "streaming" => Self::Streaming,
            "structured_output" => Self::StructuredOutput,
            "json_schema" => Self::JsonSchema,
            "media" => Self::Media,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [UnsupportedCapability] = &[
        Self::Chat,
        Self::Tools,
        Self::Streaming,
        Self::StructuredOutput,
        Self::JsonSchema,
        Self::Media,
    ];
}

/// Why an adapter refused to build a request, before any byte was sent.
///
/// Every variant here is a *safe* terminal: nothing was sent, so nothing was
/// spent and a fallback candidate may be tried immediately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BeforeSendRefusal {
    /// The request still names an alias; the resolver has not run. An adapter
    /// will not guess a deployment.
    #[serde(rename = "unresolved_target")]
    UnresolvedTarget,
    /// The deployment cannot do something this request needs.
    #[serde(rename = "unsupported_capability")]
    UnsupportedCapability {
        /// Every missing capability, in [`UnsupportedCapability::ALL`] order,
        /// so the caller learns all of them from one refusal rather than one
        /// per round trip.
        missing: Vec<UnsupportedCapability>,
    },
    /// The auth material the executor holds is not a kind this dialect can
    /// place (for example an OAuth token for a dialect that only takes an API
    /// key header).
    #[serde(rename = "auth_material_unsupported")]
    AuthMaterialUnsupported {
        /// The kind that was offered, in its frozen spelling.
        offered: String,
    },
    /// The request's shape is legal canonically but has no representation in
    /// this dialect.
    #[serde(rename = "unrepresentable_request")]
    UnrepresentableRequest {
        /// A static, body-free description of what could not be represented.
        detail: &'static str,
    },
    /// The request would exceed the caller's own stated budget before it is
    /// sent.
    #[serde(rename = "budget_exceeded")]
    BudgetExceeded {
        /// A static, body-free description of which ceiling was hit.
        detail: &'static str,
    },
}

impl BeforeSendRefusal {
    /// The fieldless tag for this refusal.
    pub fn kind(&self) -> BeforeSendRefusalKind {
        match self {
            Self::UnresolvedTarget => BeforeSendRefusalKind::UnresolvedTarget,
            Self::UnsupportedCapability { .. } => BeforeSendRefusalKind::UnsupportedCapability,
            Self::AuthMaterialUnsupported { .. } => BeforeSendRefusalKind::AuthMaterialUnsupported,
            Self::UnrepresentableRequest { .. } => BeforeSendRefusalKind::UnrepresentableRequest,
            Self::BudgetExceeded { .. } => BeforeSendRefusalKind::BudgetExceeded,
        }
    }
}

/// The fieldless tag vocabulary of [`BeforeSendRefusal`].
///
/// [`BeforeSendRefusal`] carries payloads and therefore has no `as_str()` — a
/// tag string that drops the payload is a lie. This is the honest tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BeforeSendRefusalKind {
    /// See [`BeforeSendRefusal::UnresolvedTarget`].
    #[serde(rename = "unresolved_target")]
    UnresolvedTarget,
    /// See [`BeforeSendRefusal::UnsupportedCapability`].
    #[serde(rename = "unsupported_capability")]
    UnsupportedCapability,
    /// See [`BeforeSendRefusal::AuthMaterialUnsupported`].
    #[serde(rename = "auth_material_unsupported")]
    AuthMaterialUnsupported,
    /// See [`BeforeSendRefusal::UnrepresentableRequest`].
    #[serde(rename = "unrepresentable_request")]
    UnrepresentableRequest,
    /// See [`BeforeSendRefusal::BudgetExceeded`].
    #[serde(rename = "budget_exceeded")]
    BudgetExceeded,
}

impl BeforeSendRefusalKind {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)` and
    /// the matching [`BeforeSendRefusal`] variant's tag.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnresolvedTarget => "unresolved_target",
            Self::UnsupportedCapability => "unsupported_capability",
            Self::AuthMaterialUnsupported => "auth_material_unsupported",
            Self::UnrepresentableRequest => "unrepresentable_request",
            Self::BudgetExceeded => "budget_exceeded",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "unresolved_target" => Self::UnresolvedTarget,
            "unsupported_capability" => Self::UnsupportedCapability,
            "auth_material_unsupported" => Self::AuthMaterialUnsupported,
            "unrepresentable_request" => Self::UnrepresentableRequest,
            "budget_exceeded" => Self::BudgetExceeded,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [BeforeSendRefusalKind] = &[
        Self::UnresolvedTarget,
        Self::UnsupportedCapability,
        Self::AuthMaterialUnsupported,
        Self::UnrepresentableRequest,
        Self::BudgetExceeded,
    ];
}

// ---------------------------------------------------------------------------
// Protocol violation
// ---------------------------------------------------------------------------

/// The provider answered, but not in its own grammar.
///
/// Distinct from a provider *rejection*: a rejection is the provider working
/// correctly and saying no; a violation means the response cannot be trusted
/// to mean anything, so it is never retried automatically (the same malformed
/// answer is the likely outcome, at the same price).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProtocolViolation {
    /// The body was not valid JSON.
    #[serde(rename = "malformed_body")]
    MalformedBody {
        /// Body-free description of the parse failure.
        detail: &'static str,
    },
    /// The body parsed but a required field was missing or ill-typed.
    #[serde(rename = "schema_violation")]
    SchemaViolation {
        /// The JSON pointer of the offending field.
        pointer: &'static str,
    },
    /// A success status carried no assistant content at all. The legacy lane
    /// treats this as a retriable lane fault; the disposition keeps the fact
    /// that the provider *answered* rather than hiding it in a generic error.
    #[serde(rename = "empty_assistant_content")]
    EmptyAssistantContent {
        /// The provider's own `finish_reason`, when it sent one — bounded to
        /// [`MAX_FINISH_REASON_CHARS`]. Build it with
        /// [`ProtocolViolation::empty_assistant_content`] rather than by hand:
        /// this is the only provider-controlled string in the whole violation
        /// vocabulary (every other detail is `&'static str`), so it is the only
        /// place an unbounded response body can reach a durable disposition.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finish_reason: Option<String>,
    },
    /// A stream decoder rejected the byte sequence.
    #[serde(rename = "stream_decode")]
    StreamDecode {
        /// Which decode rule was broken. Named `rule` because the variant's
        /// serde payload lives beside the internal `kind` tag — a field named
        /// `kind` would collide with the tag itself.
        rule: StreamDecodeErrorKind,
    },
}

/// The most `finish_reason` characters a violation retains.
///
/// Every real value is a short enum-like token (`stop`, `length`,
/// `tool_calls`, `content_filter`), so this is generous for the honest case and
/// still a bound for the hostile one. It has to be a bound: the disposition is
/// durable, and a provider that answers with a megabyte-long `finish_reason`
/// must not be able to choose how much of this process's storage it consumes.
pub const MAX_FINISH_REASON_CHARS: usize = 64;

impl ProtocolViolation {
    /// The empty-content violation, with the provider's `finish_reason`
    /// bounded and control characters stripped.
    ///
    /// The bounding lives here, not at each call site, so a second adapter
    /// cannot reintroduce the unbounded path by constructing the variant
    /// directly with what it read off the wire.
    pub fn empty_assistant_content(finish_reason: Option<&str>) -> Self {
        Self::EmptyAssistantContent {
            finish_reason: finish_reason.map(bounded_provider_token),
        }
    }

    /// The fieldless tag for this violation.
    pub fn kind(&self) -> ProtocolViolationKind {
        match self {
            Self::MalformedBody { .. } => ProtocolViolationKind::MalformedBody,
            Self::SchemaViolation { .. } => ProtocolViolationKind::SchemaViolation,
            Self::EmptyAssistantContent { .. } => ProtocolViolationKind::EmptyAssistantContent,
            Self::StreamDecode { .. } => ProtocolViolationKind::StreamDecode,
        }
    }
}

/// Bounds and sanitizes one short provider-controlled token.
///
/// Control characters are replaced rather than dropped: a `finish_reason` of
/// `"stop\n[llm] fabricated log line"` must not be able to forge a log record
/// downstream, and replacing keeps the length honest while removing the
/// framing character that does the forging.
fn bounded_provider_token(raw: &str) -> String {
    raw.chars()
        .take(MAX_FINISH_REASON_CHARS)
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect()
}

/// The fieldless tag vocabulary of [`ProtocolViolation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProtocolViolationKind {
    /// See [`ProtocolViolation::MalformedBody`].
    #[serde(rename = "malformed_body")]
    MalformedBody,
    /// See [`ProtocolViolation::SchemaViolation`].
    #[serde(rename = "schema_violation")]
    SchemaViolation,
    /// See [`ProtocolViolation::EmptyAssistantContent`].
    #[serde(rename = "empty_assistant_content")]
    EmptyAssistantContent,
    /// See [`ProtocolViolation::StreamDecode`].
    #[serde(rename = "stream_decode")]
    StreamDecode,
}

impl ProtocolViolationKind {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MalformedBody => "malformed_body",
            Self::SchemaViolation => "schema_violation",
            Self::EmptyAssistantContent => "empty_assistant_content",
            Self::StreamDecode => "stream_decode",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "malformed_body" => Self::MalformedBody,
            "schema_violation" => Self::SchemaViolation,
            "empty_assistant_content" => Self::EmptyAssistantContent,
            "stream_decode" => Self::StreamDecode,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [ProtocolViolationKind] = &[
        Self::MalformedBody,
        Self::SchemaViolation,
        Self::EmptyAssistantContent,
        Self::StreamDecode,
    ];
}

// ---------------------------------------------------------------------------
// Phases, evidence, completion
// ---------------------------------------------------------------------------

/// How far an invocation got before it stopped being observable.
///
/// The distinction is the whole point: a request that never left is safe to
/// retry, one that was accepted is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SendPhase {
    /// The connection was never established.
    #[serde(rename = "connecting")]
    Connecting,
    /// Bytes were being written; the provider may or may not have a complete
    /// request.
    #[serde(rename = "sending")]
    Sending,
    /// The request was fully written and accepted; the provider is working.
    #[serde(rename = "awaiting_response")]
    AwaitingResponse,
    /// Response bytes were arriving.
    #[serde(rename = "receiving")]
    Receiving,
}

impl SendPhase {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Sending => "sending",
            Self::AwaitingResponse => "awaiting_response",
            Self::Receiving => "receiving",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "connecting" => Self::Connecting,
            "sending" => Self::Sending,
            "awaiting_response" => Self::AwaitingResponse,
            "receiving" => Self::Receiving,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [SendPhase] = &[
        Self::Connecting,
        Self::Sending,
        Self::AwaitingResponse,
        Self::Receiving,
    ];

    /// Whether the provider is known never to have received a complete
    /// request. Only [`SendPhase::Connecting`] proves that.
    pub fn provider_definitely_untouched(self) -> bool {
        matches!(self, Self::Connecting)
    }
}

/// Protocol evidence that a cancellation really did prevent completion.
///
/// [`InvocationDispositionV1::CancelledConfirmed`] is unreachable without one
/// of these. "The connection dropped" is *not* on this list, and that omission
/// is deliberate: a dropped connection proves only that we stopped listening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CancellationEvidence {
    /// The provider acknowledged the cancellation in-band.
    #[serde(rename = "provider_acknowledged")]
    ProviderAcknowledged,
    /// The provider's own terminal event says the generation was aborted
    /// before any output.
    #[serde(rename = "terminal_event_aborted")]
    TerminalEventAborted,
    /// The request was never accepted, proven at the transport layer.
    #[serde(rename = "never_accepted")]
    NeverAccepted,
}

impl CancellationEvidence {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderAcknowledged => "provider_acknowledged",
            Self::TerminalEventAborted => "terminal_event_aborted",
            Self::NeverAccepted => "never_accepted",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "provider_acknowledged" => Self::ProviderAcknowledged,
            "terminal_event_aborted" => Self::TerminalEventAborted,
            "never_accepted" => Self::NeverAccepted,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [CancellationEvidence] = &[
        Self::ProviderAcknowledged,
        Self::TerminalEventAborted,
        Self::NeverAccepted,
    ];
}

/// How a completed generation ended.
///
/// Richer than [`CompletionStatusV1`] on purpose — `tool_calls` and
/// `content_filter` are real, distinguishable endings that the durable schema
/// currently flattens to `Unknown`. [`CompletionKindV1::to_completion_status_v1`]
/// is that flattening, kept explicit and tested so the richer vocabulary can
/// never widen the durable schema by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CompletionKindV1 {
    /// The model stopped on its own.
    #[serde(rename = "complete")]
    Complete,
    /// The output hit a token ceiling.
    #[serde(rename = "truncated")]
    Truncated,
    /// The model ended its turn by calling tools.
    #[serde(rename = "tool_calls")]
    ToolCalls,
    /// The provider stopped generation on a content policy.
    #[serde(rename = "content_filtered")]
    ContentFiltered,
    /// The provider reported no usable reason.
    #[serde(rename = "unknown")]
    Unknown,
}

impl CompletionKindV1 {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Truncated => "truncated",
            Self::ToolCalls => "tool_calls",
            Self::ContentFiltered => "content_filtered",
            Self::Unknown => "unknown",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "complete" => Self::Complete,
            "truncated" => Self::Truncated,
            "tool_calls" => Self::ToolCalls,
            "content_filtered" => Self::ContentFiltered,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [CompletionKindV1] = &[
        Self::Complete,
        Self::Truncated,
        Self::ToolCalls,
        Self::ContentFiltered,
        Self::Unknown,
    ];

    /// The lossy projection onto the durable #1519 status.
    ///
    /// Only an explicit stop is `Complete` and only a token ceiling is
    /// `Truncated`; every other ending stays `Unknown`, exactly as
    /// `completion_status_from_finish_reason` has always behaved. The
    /// projection exists so the broker's richer vocabulary never widens the
    /// frozen schema.
    pub fn to_completion_status_v1(self) -> CompletionStatusV1 {
        match self {
            Self::Complete => CompletionStatusV1::Complete,
            Self::Truncated => CompletionStatusV1::Truncated,
            Self::ToolCalls | Self::ContentFiltered | Self::Unknown => CompletionStatusV1::Unknown,
        }
    }
}

// ---------------------------------------------------------------------------
// Retry posture
// ---------------------------------------------------------------------------

/// Whether the executor may re-send after a disposition.
///
/// A three-value answer, not a bool, because "only if the caller said so" is a
/// real and common state and collapsing it into either `true` or `false` is a
/// bug in one direction or the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RetryPosture {
    /// Nothing was spent and nothing happened; retry freely.
    #[serde(rename = "safe")]
    Safe,
    /// The provider may have acted. Retry only with explicit caller consent
    /// ([`CancellationContext::retry_on_outcome_unknown`](super::CancellationContext)),
    /// ideally under an idempotency key.
    #[serde(rename = "caller_opt_in")]
    CallerOptIn,
    /// Retrying would duplicate observable work or spend. Do not.
    #[serde(rename = "forbidden")]
    Forbidden,
    /// The call succeeded; there is nothing to retry.
    #[serde(rename = "not_applicable")]
    NotApplicable,
}

impl RetryPosture {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::CallerOptIn => "caller_opt_in",
            Self::Forbidden => "forbidden",
            Self::NotApplicable => "not_applicable",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "safe" => Self::Safe,
            "caller_opt_in" => Self::CallerOptIn,
            "forbidden" => Self::Forbidden,
            "not_applicable" => Self::NotApplicable,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [RetryPosture] = &[
        Self::Safe,
        Self::CallerOptIn,
        Self::Forbidden,
        Self::NotApplicable,
    ];
}

// ---------------------------------------------------------------------------
// The disposition
// ---------------------------------------------------------------------------

/// How one invocation ended. The closed spectrum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum InvocationDispositionV1 {
    /// The adapter refused to build a request. Nothing was sent.
    #[serde(rename = "refused_before_send")]
    RefusedBeforeSend {
        /// Why.
        refusal: BeforeSendRefusal,
    },
    /// The provider received the request and said no.
    #[serde(rename = "provider_rejected")]
    ProviderRejected {
        /// The classified reason.
        class: ProviderErrorClass,
        /// The HTTP status that carried it.
        status: u16,
        /// The provider's own retry directive, when it sent one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after: Option<RetryAfter>,
    },
    /// Observation stopped after the request may have been accepted. Whether
    /// the provider completed the work is **unknowable from here**.
    #[serde(rename = "outcome_unknown")]
    OutcomeUnknown {
        /// How far it got.
        phase: SendPhase,
    },
    /// The provider answered outside its own grammar.
    #[serde(rename = "protocol_error")]
    ProtocolError {
        /// Which rule was broken.
        violation: ProtocolViolation,
    },
    /// The caller cancelled before anything was sent.
    #[serde(rename = "cancelled_before_send")]
    CancelledBeforeSend,
    /// The caller cancelled and the protocol proved non-completion.
    #[serde(rename = "cancelled_confirmed")]
    CancelledConfirmed {
        /// The proof. Without one this variant is unreachable.
        evidence: CancellationEvidence,
    },
    /// The caller cancelled after the request was accepted and no proof of
    /// non-completion arrived. The default cancellation terminal.
    #[serde(rename = "cancelled_outcome_unknown")]
    CancelledOutcomeUnknown,
    /// The caller cancelled after output had already been emitted.
    #[serde(rename = "cancelled_after_partial")]
    CancelledAfterPartial,
    /// The invocation completed and produced a body.
    #[serde(rename = "completed")]
    Completed {
        /// How the generation ended.
        completion: CompletionKindV1,
    },
}

impl InvocationDispositionV1 {
    /// The fieldless tag for this disposition.
    pub fn kind(&self) -> InvocationDispositionKind {
        match self {
            Self::RefusedBeforeSend { .. } => InvocationDispositionKind::RefusedBeforeSend,
            Self::ProviderRejected { .. } => InvocationDispositionKind::ProviderRejected,
            Self::OutcomeUnknown { .. } => InvocationDispositionKind::OutcomeUnknown,
            Self::ProtocolError { .. } => InvocationDispositionKind::ProtocolError,
            Self::CancelledBeforeSend => InvocationDispositionKind::CancelledBeforeSend,
            Self::CancelledConfirmed { .. } => InvocationDispositionKind::CancelledConfirmed,
            Self::CancelledOutcomeUnknown => InvocationDispositionKind::CancelledOutcomeUnknown,
            Self::CancelledAfterPartial => InvocationDispositionKind::CancelledAfterPartial,
            Self::Completed { .. } => InvocationDispositionKind::Completed,
        }
    }

    /// Whether the provider is known to have done no work at all.
    ///
    /// Conservative by construction: anything unknowable answers `false`. A
    /// `true` here is a claim about someone else's billing, so it is only made
    /// where the protocol proves it.
    pub fn provider_did_no_work(&self) -> bool {
        match self {
            Self::RefusedBeforeSend { .. } | Self::CancelledBeforeSend => true,
            Self::CancelledConfirmed { evidence } => matches!(
                evidence,
                CancellationEvidence::NeverAccepted | CancellationEvidence::TerminalEventAborted
            ),
            Self::OutcomeUnknown { phase } => phase.provider_definitely_untouched(),
            Self::ProviderRejected { .. }
            | Self::ProtocolError { .. }
            | Self::CancelledOutcomeUnknown
            | Self::CancelledAfterPartial
            | Self::Completed { .. } => false,
        }
    }

    /// Whether the executor may re-send. See [`RetryPosture`].
    pub fn retry_posture(&self) -> RetryPosture {
        match self {
            Self::RefusedBeforeSend { .. }
            | Self::CancelledBeforeSend
            | Self::CancelledConfirmed { .. } => RetryPosture::Safe,
            Self::ProviderRejected { class, .. } => class.retry_posture(),
            Self::OutcomeUnknown { phase } => {
                if phase.provider_definitely_untouched() {
                    RetryPosture::Safe
                } else {
                    RetryPosture::CallerOptIn
                }
            }
            Self::CancelledOutcomeUnknown => RetryPosture::CallerOptIn,
            Self::ProtocolError { .. } | Self::CancelledAfterPartial => RetryPosture::Forbidden,
            Self::Completed { .. } => RetryPosture::NotApplicable,
        }
    }

    /// Whether a *different* candidate deployment may be tried. Weaker than
    /// [`Self::retry_posture`]: a fallback is only free when nothing was sent
    /// to anyone.
    pub fn fallback_eligible(&self) -> bool {
        matches!(self.retry_posture(), RetryPosture::Safe)
    }
}

/// The fieldless tag vocabulary of [`InvocationDispositionV1`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InvocationDispositionKind {
    /// See [`InvocationDispositionV1::RefusedBeforeSend`].
    #[serde(rename = "refused_before_send")]
    RefusedBeforeSend,
    /// See [`InvocationDispositionV1::ProviderRejected`].
    #[serde(rename = "provider_rejected")]
    ProviderRejected,
    /// See [`InvocationDispositionV1::OutcomeUnknown`].
    #[serde(rename = "outcome_unknown")]
    OutcomeUnknown,
    /// See [`InvocationDispositionV1::ProtocolError`].
    #[serde(rename = "protocol_error")]
    ProtocolError,
    /// See [`InvocationDispositionV1::CancelledBeforeSend`].
    #[serde(rename = "cancelled_before_send")]
    CancelledBeforeSend,
    /// See [`InvocationDispositionV1::CancelledConfirmed`].
    #[serde(rename = "cancelled_confirmed")]
    CancelledConfirmed,
    /// See [`InvocationDispositionV1::CancelledOutcomeUnknown`].
    #[serde(rename = "cancelled_outcome_unknown")]
    CancelledOutcomeUnknown,
    /// See [`InvocationDispositionV1::CancelledAfterPartial`].
    #[serde(rename = "cancelled_after_partial")]
    CancelledAfterPartial,
    /// See [`InvocationDispositionV1::Completed`].
    #[serde(rename = "completed")]
    Completed,
}

impl InvocationDispositionKind {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)` and
    /// the matching [`InvocationDispositionV1`] variant's tag.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RefusedBeforeSend => "refused_before_send",
            Self::ProviderRejected => "provider_rejected",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::ProtocolError => "protocol_error",
            Self::CancelledBeforeSend => "cancelled_before_send",
            Self::CancelledConfirmed => "cancelled_confirmed",
            Self::CancelledOutcomeUnknown => "cancelled_outcome_unknown",
            Self::CancelledAfterPartial => "cancelled_after_partial",
            Self::Completed => "completed",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "refused_before_send" => Self::RefusedBeforeSend,
            "provider_rejected" => Self::ProviderRejected,
            "outcome_unknown" => Self::OutcomeUnknown,
            "protocol_error" => Self::ProtocolError,
            "cancelled_before_send" => Self::CancelledBeforeSend,
            "cancelled_confirmed" => Self::CancelledConfirmed,
            "cancelled_outcome_unknown" => Self::CancelledOutcomeUnknown,
            "cancelled_after_partial" => Self::CancelledAfterPartial,
            "completed" => Self::Completed,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [InvocationDispositionKind] = &[
        Self::RefusedBeforeSend,
        Self::ProviderRejected,
        Self::OutcomeUnknown,
        Self::ProtocolError,
        Self::CancelledBeforeSend,
        Self::CancelledConfirmed,
        Self::CancelledOutcomeUnknown,
        Self::CancelledAfterPartial,
        Self::Completed,
    ];
}
