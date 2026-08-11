// broker.rs — the Model Broker's sans-IO wire layer (#1682 slice-1).
//
//! Sans-IO provider wire adapters and the canonical invocation vocabulary.
//!
//! # What this module is
//!
//! #1682's frozen design (D2) puts every provider-specific *wire mapping* —
//! how a canonical request becomes bytes, how bytes become a canonical
//! outcome, how an error response is classified — behind one trait,
//! [`ProviderWire`], whose implementations are **pure functions**. An adapter
//! holds no HTTP client, opens no connection, sleeps for no retry, and reads
//! no credential material. The single executor (a later slice) owns the
//! connection, the pool, the retry clock and the cancellation token; the
//! adapter owns only the grammar.
//!
//! Three properties fall out of that, and they are the reason for the shape:
//!
//! 1. **A fake adapter and the real adapter are the same code.** Conformance
//!    testing is fixture testing, so a provider's marginal cost is its fixture
//!    cost — see the golden corpus under `broker/fixtures/`.
//! 2. **There cannot be a second HTTP client pool.** An adapter has nowhere to
//!    put one. The "one pooled client" rule is structural here, not a review
//!    convention.
//! 3. **An adapter cannot leak a credential**, because one never reaches it —
//!    see the two-gate note below.
//!
//! # The two-gate law, at the type level
//!
//! Gate 1 is admission (the UDS listener, a later slice). Gate 2 is the lease:
//! the executor obtains auth material through the #748/#1680 opaque-auth-ref
//! interface. The law is that *passing gate 1 must not let a caller select a
//! credential*, and here that is enforced by the type system rather than by a
//! runtime check:
//!
//! - [`CanonicalInvocationRequest`] has **no field that can name a credential
//!   member** — no key id, no key env, no vault alias, no account selector.
//!   The compile-fail doctests on it pin that; `request_schema_*` tests pin
//!   the serialized field list.
//! - [`AuthMaterialRef`] — the only auth-shaped thing an adapter is handed —
//!   carries a *kind* and an opaque lease reference, and **no material**. An
//!   adapter therefore cannot embed a secret in the bytes it returns; it
//!   declares an [`AuthPlacement`] and the executor injects. That is why
//!   [`WireHttpRequest`] goldens are secret-free by construction rather than
//!   by redaction.
//!
//! # Relationship to the #1681/#1682 shared seam
//!
//! The five shared seam types (`ModelRef`, `ResolvedDeployment`,
//! `ResolutionOutcome`, `OperationalResolver`, `HealthObservation`) live in
//! `memcore` and are frozen by a separate PR. This slice deliberately does not
//! depend on them: the canonical vocabulary below is tachi-llm-local and
//! self-held, so the two chains can land in either order. Where a local type
//! is the counterpart of a seam type — [`RetryAfter`], [`WireCapabilities`],
//! [`ResolvedWireTarget`] — its spelling and its axes are kept identical to
//! the seam's on purpose, and its doc comment names the seam type it will be
//! reconciled with. Wiring the resolver in is a later slice, not this one.
//!
//! # Spelling discipline (inherited from the seam's PR-review rework)
//!
//! Every fieldless enum declares its wire spelling once, next to the variant
//! (`#[serde(rename = ...)]`), returns the same string from `as_str()`, parses
//! it back in `parse()`, and lists every variant in `ALL`. The
//! `*_spelling_*` tests assert serde's output equals `as_str()` *and* equals a
//! literal golden, because a round-trip test alone cannot catch serde's
//! `rename_all = "snake_case"` emitting `open_ai_compat` while `as_str()` says
//! `openai_compat`. Enums whose variants carry payloads get **no** `as_str()`
//! — a tag string that silently drops the payload is a lie; they expose a
//! separate fieldless `*Kind` enum instead.
//!
//! # What is deliberately not here (slice boundaries)
//!
//! - **No stream decoder implementation.** [`WireStreamDecoder`] is defined,
//!   with the explicit terminal inputs the golden corpus needs (`finish`,
//!   `on_transport_error`, `on_cancel`); the OpenAI-compat adapter returns a
//!   typed [`StreamDecoderUnavailable`] rather than panicking. SSE/NDJSON
//!   decoding, tool-call reconstruction and the transcript fixtures are the
//!   next slice.
//! - **No executor, no cancellation state machine, no HTTP.** The disposition
//!   vocabulary that the state machine will drive is frozen here
//!   ([`InvocationDispositionV1`]); the machine that walks it is not.
//! - **No gateway, no listener, no admission.** [`AdmittedRefs`] is the
//!   recorded-never-authorizing shape those will fill in.
//! - **No consumer cutover.** `chat_lanes::lane_calls` keeps serving all four
//!   lanes untouched. This module copies its *classification semantics* and
//!   pins the copy with a parity test; it changes nothing there.

mod canonical;
mod disposition;
mod openai_compat;
mod stream;
mod usage;
mod wire;

#[cfg(test)]
mod tests;

pub use canonical::{
    AdmittedRefs, BudgetConstraint, CancellationContext, CanonicalInvocationRequest,
    CanonicalInvocationRequestParts, CanonicalMessage, ContentPart, DataPolicyConstraint,
    DeadlineContext, EndpointUrl, IdempotencyKey, InvocationTarget, MessageContent, MessageRole,
    ModelAliasRef, RequestError, RequiredCapabilities, ResolvedWireTarget, ResolvedWireTargetParts,
    ResponseFormat, SamplingParams, StreamSelection, ToolChoice, ToolDeclaration,
    MAX_CONTENT_PARTS, MAX_MESSAGES, MAX_MESSAGE_CONTENT_BYTES, MAX_STOP_SEQUENCES, MAX_TOOLS,
    MAX_TOOL_SCHEMA_BYTES, MAX_TOTAL_CONTENT_BYTES,
};
pub use disposition::{
    BeforeSendRefusal, BeforeSendRefusalKind, CancellationEvidence, CompletionKindV1,
    InvocationDispositionKind, InvocationDispositionV1, ProtocolViolation, ProtocolViolationKind,
    RetryPosture, SendPhase, UnsupportedCapability, MAX_FINISH_REASON_CHARS,
};
pub use stream::{
    CanonicalStreamEvent, CanonicalStreamEventKind, StreamDecodeError, StreamDecodeErrorKind,
    StreamDecoderUnavailable, StreamDecoderUnavailableReason, StreamEof, ToolCallFragment,
    TransportErrorKind, WireStreamDecoder,
};
pub use usage::{UsageObservationV1, UsageProvenanceV1};
pub use wire::{
    AuthMaterialKind, AuthMaterialRef, AuthPlacement, CanonicalAssistantMessage, HttpMethod,
    ProviderErrorClass, ProviderErrorClassification, ProviderResponseMetadata, ProviderWire,
    ResponseHeaders, RetryAdvice, RetryAfter, ToolCallV1, WireCapabilities, WireHeader,
    WireHttpRequest, WireOutcome,
};

pub use openai_compat::{OpenAiCompatWire, OPENAI_COMPAT_DIALECT};
