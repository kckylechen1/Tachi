//! The parts of a stream decoder that must not differ between grammars.
//!
//! # What is shared and why exactly this much
//!
//! Two things in a decoder are *not* grammar-specific, and both are the kind of
//! thing that silently diverges when written twice:
//!
//! 1. **What a terminal input means.** "The transport died", "the caller
//!    cancelled", "the body ended early" have one right answer each, and that
//!    answer is a claim about someone else's billing — whether the provider did
//!    work, whether a retry duplicates spend. Two decoders that answer it
//!    differently would make the disposition depend on which provider was
//!    called, which is exactly the leak the canonical vocabulary exists to
//!    prevent.
//! 2. **Tool-call reconstruction.** Both grammars split a call's arguments
//!    across events at arbitrary byte offsets; both therefore need the same
//!    accumulate-then-validate rules and the same ceilings. Only *where the
//!    fragments come from* differs.
//!
//! Everything above that — event names, terminators, how a delta is spelled —
//! stays in the grammar module, because pretending those are shared is how a
//! trait ends up OpenAI-shaped.
//!
//! # Cancellation is the decoder's call, not the executor's
//!
//! [`StreamLifecycle::on_cancel`] answers `CancelledAfterPartial` or
//! `CancelledOutcomeUnknown` depending on whether the decoder has already
//! emitted visible output. Only the decoder knows that — the executor sees a
//! byte stream, not whether those bytes turned into text the caller has already
//! acted on — and the difference is load-bearing: one is retry-forbidden, the
//! other is retry-on-caller-opt-in.

use serde_json::Value;

use super::disposition::{InvocationDispositionV1, ProtocolViolation, SendPhase};
use super::stream::{
    CanonicalStreamEvent, StreamDecodeError, StreamDecodeErrorKind, ToolCallFragment,
};

/// The most argument bytes one tool call may accumulate across events.
///
/// Separate from the per-frame ceiling and necessary because of it: a thousand
/// well-formed frames of a kilobyte each are individually legal and together
/// unbounded, so a decoder that only capped the frame would still let a hostile
/// provider grow one accumulator without limit.
pub(super) const MAX_TOOL_CALL_ARGUMENT_BYTES: usize = 1 << 20;

/// The most tool calls one assistant turn may open.
///
/// The index is provider-chosen, so without a ceiling a stream can open as many
/// accumulators as it likes even if each one stays small.
pub(super) const MAX_TOOL_CALLS_PER_TURN: usize = 128;

/// A decode failure with a body-free detail.
///
/// Every failure in the decoders is built here, so no call site can invent an
/// error carrying provider bytes: `detail` is `&'static str` by type, and this
/// keeps it that way by construction rather than by review.
pub(super) fn decode_error(kind: StreamDecodeErrorKind, detail: &'static str) -> StreamDecodeError {
    StreamDecodeError { kind, detail }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Where a stream is in its life, and how it ended.
#[derive(Debug, Default)]
pub(super) struct StreamLifecycle {
    /// Whether the `started` event has been emitted.
    started: bool,
    /// Whether any body byte has arrived at all.
    saw_bytes: bool,
    /// Whether output the caller can already have acted on was emitted.
    saw_visible_output: bool,
    /// Whether the grammar's end-of-generation signal has been seen
    /// (`finish_reason`, `stop_reason`) — which is *not* the end of the stream.
    generation_ended: bool,
    /// The terminal disposition, once reached. First writer wins.
    terminal: Option<InvocationDispositionV1>,
}

impl StreamLifecycle {
    /// Whether the stream has reached a terminal disposition.
    pub(super) fn is_terminal(&self) -> bool {
        self.terminal.is_some()
    }

    /// The terminal disposition, once reached.
    pub(super) fn terminal(&self) -> Option<InvocationDispositionV1> {
        self.terminal.clone()
    }

    /// Records that body bytes arrived.
    pub(super) fn note_bytes(&mut self) {
        self.saw_bytes = true;
    }

    /// Records that output the caller can see was emitted.
    pub(super) fn note_visible_output(&mut self) {
        self.saw_visible_output = true;
    }

    /// Marks the stream started, answering whether this was the first time.
    pub(super) fn mark_started(&mut self) -> bool {
        if self.started {
            return false;
        }
        self.started = true;
        true
    }

    /// Whether the grammar's end-of-generation signal has been seen.
    pub(super) fn generation_ended(&self) -> bool {
        self.generation_ended
    }

    /// Records the grammar's end-of-generation signal.
    pub(super) fn end_generation(&mut self) {
        self.generation_ended = true;
    }

    /// Fixes the terminal disposition. The **first** call wins.
    ///
    /// That ordering is the terminality rule: once a stream has answered, a
    /// later event — including a protocol error in bytes that arrived after the
    /// terminator — must not be able to rewrite the answer the caller already
    /// has.
    pub(super) fn seal(&mut self, disposition: InvocationDispositionV1) {
        if self.terminal.is_none() {
            self.terminal = Some(disposition);
        }
    }

    /// Seals the disposition a decode failure produces.
    pub(super) fn seal_decode_error(&mut self, kind: StreamDecodeErrorKind) {
        self.seal(InvocationDispositionV1::ProtocolError {
            violation: ProtocolViolation::StreamDecode { rule: kind },
        });
    }

    /// How far the invocation got, for the dispositions that record it.
    fn phase(&self) -> SendPhase {
        if self.saw_bytes {
            SendPhase::Receiving
        } else {
            SendPhase::AwaitingResponse
        }
    }

    /// The stream stopped being observable: the transport failed, or the body
    /// ended before the transport said it would.
    ///
    /// Both are the same claim — *this process stopped being able to observe an
    /// invocation the provider had already accepted* — so both answer
    /// `OutcomeUnknown`, which is an honest tier and not a failure: the provider
    /// may well have finished the generation and will bill for it.
    pub(super) fn on_observation_lost(&mut self) -> Vec<CanonicalStreamEvent> {
        if self.is_terminal() {
            return Vec::new();
        }
        let disposition = InvocationDispositionV1::OutcomeUnknown {
            phase: self.phase(),
        };
        self.seal(disposition.clone());
        vec![CanonicalStreamEvent::Failed { disposition }]
    }

    /// The caller cancelled.
    pub(super) fn on_cancel(&mut self) -> Vec<CanonicalStreamEvent> {
        if self.is_terminal() {
            return Vec::new();
        }
        let disposition = if self.saw_visible_output {
            InvocationDispositionV1::CancelledAfterPartial
        } else {
            InvocationDispositionV1::CancelledOutcomeUnknown
        };
        self.seal(disposition.clone());
        vec![CanonicalStreamEvent::Failed { disposition }]
    }

    /// The provider's own stream said the invocation failed.
    pub(super) fn fail_with(&mut self, violation: ProtocolViolation) -> CanonicalStreamEvent {
        let disposition = InvocationDispositionV1::ProtocolError { violation };
        self.seal(disposition.clone());
        CanonicalStreamEvent::Failed { disposition }
    }
}

// ---------------------------------------------------------------------------
// Tool calls
// ---------------------------------------------------------------------------

/// One tool call being reconstructed from fragments.
#[derive(Debug)]
struct TrackedCall {
    /// The call's position in the assistant turn.
    index: u32,
    /// The provider-assigned id, once a usable one has been seen.
    id: Option<String>,
    /// The tool name, once a usable one has been seen.
    name: Option<String>,
    /// Argument bytes accumulated so far. A *fragment of* a JSON document, not
    /// a document, until the call closes.
    arguments: String,
    /// Whether the call has been validated and closed.
    closed: bool,
}

/// Reconstructs tool calls from the fragments a stream splits them into.
#[derive(Debug, Default)]
pub(super) struct ToolCallTracker {
    /// Calls in the order they were opened.
    calls: Vec<TrackedCall>,
    /// The highest index opened so far, for the ordering rule.
    highest_opened: Option<u32>,
}

impl ToolCallTracker {
    /// Opens a call at `index`.
    ///
    /// Indices must open in ascending order. A lower index appearing after a
    /// higher one means fragments were reordered somewhere between the model
    /// and here, and silently accepting that is how one call's arguments end up
    /// stapled onto another call's name.
    pub(super) fn open(
        &mut self,
        index: u32,
        id: Option<&str>,
        name: Option<&str>,
    ) -> Result<(), StreamDecodeError> {
        if self.position(index).is_some() {
            return Err(decode_error(
                StreamDecodeErrorKind::IllegalSequence,
                "a tool call was opened twice at the same index",
            ));
        }
        if self.highest_opened.is_some_and(|highest| index <= highest) {
            return Err(decode_error(
                StreamDecodeErrorKind::IllegalSequence,
                "tool call indices must open in ascending order",
            ));
        }
        if self.calls.len() >= MAX_TOOL_CALLS_PER_TURN {
            return Err(decode_error(
                StreamDecodeErrorKind::EventTooLarge,
                "one assistant turn opened more tool calls than the decoder admits",
            ));
        }
        self.highest_opened = Some(index);
        self.calls.push(TrackedCall {
            index,
            id: id.map(str::to_string),
            name: name.map(str::to_string),
            arguments: String::new(),
            closed: false,
        });
        Ok(())
    }

    /// Appends argument bytes to an open call.
    pub(super) fn append(&mut self, index: u32, fragment: &str) -> Result<(), StreamDecodeError> {
        let Some(position) = self.position(index) else {
            return Err(decode_error(
                StreamDecodeErrorKind::IllegalSequence,
                "a tool-call argument fragment arrived for a call that never started",
            ));
        };
        if self.calls[position].closed {
            return Err(decode_error(
                StreamDecodeErrorKind::IllegalSequence,
                "a fragment arrived for a tool call that had already ended",
            ));
        }
        if self.calls[position]
            .arguments
            .len()
            .saturating_add(fragment.len())
            > MAX_TOOL_CALL_ARGUMENT_BYTES
        {
            return Err(decode_error(
                StreamDecodeErrorKind::EventTooLarge,
                "one tool call's arguments exceeded the decoder's buffer ceiling",
            ));
        }
        self.calls[position].arguments.push_str(fragment);
        Ok(())
    }

    /// Applies one OpenAI-shaped fragment, which may carry any combination of
    /// identity and argument bytes.
    ///
    /// `id` and `name` are the *usable* values — a provider that repeats an
    /// empty string on every continuation fragment is saying "no new identity
    /// here", not "the id is now empty".
    pub(super) fn observe(
        &mut self,
        index: u32,
        id: Option<&str>,
        name: Option<&str>,
        arguments: Option<&str>,
    ) -> Result<(), StreamDecodeError> {
        match self.position(index) {
            None => {
                if id.is_none() && name.is_none() {
                    return Err(decode_error(
                        StreamDecodeErrorKind::IllegalSequence,
                        "a tool-call argument fragment arrived for a call that never started",
                    ));
                }
                self.open(index, id, name)?;
            }
            Some(position) => {
                if self.calls[position].closed {
                    return Err(decode_error(
                        StreamDecodeErrorKind::IllegalSequence,
                        "a fragment arrived for a tool call that had already ended",
                    ));
                }
                if let Some(id) = id {
                    match &self.calls[position].id {
                        Some(existing) if existing != id => {
                            return Err(decode_error(
                                StreamDecodeErrorKind::IllegalSequence,
                                "a second call id arrived for an open tool call",
                            ))
                        }
                        Some(_) => {}
                        None => self.calls[position].id = Some(id.to_string()),
                    }
                }
                if let Some(name) = name {
                    match &self.calls[position].name {
                        Some(existing) if existing != name => {
                            return Err(decode_error(
                                StreamDecodeErrorKind::IllegalSequence,
                                "a second function name arrived for an open tool call",
                            ))
                        }
                        Some(_) => {}
                        None => self.calls[position].name = Some(name.to_string()),
                    }
                }
            }
        }
        if let Some(arguments) = arguments {
            self.append(index, arguments)?;
        }
        Ok(())
    }

    /// Closes one call, validating what was reconstructed.
    ///
    /// Answers `Some(index)` when the call closed here and `None` when it was
    /// already closed, so a grammar that closes explicitly (Anthropic's
    /// `content_block_stop`) and one that closes at the terminator (OpenAI's
    /// `finish_reason`) both emit exactly one completion event per call.
    pub(super) fn close(&mut self, index: u32) -> Result<Option<u32>, StreamDecodeError> {
        let Some(position) = self.position(index) else {
            return Err(decode_error(
                StreamDecodeErrorKind::IllegalSequence,
                "a tool call ended that had never started",
            ));
        };
        if self.calls[position].closed {
            return Ok(None);
        }
        validate(&self.calls[position])?;
        self.calls[position].closed = true;
        Ok(Some(index))
    }

    /// Closes every open call, in the order they were opened.
    pub(super) fn close_all(&mut self) -> Result<Vec<u32>, StreamDecodeError> {
        let indices: Vec<u32> = self
            .calls
            .iter()
            .filter(|call| !call.closed)
            .map(|call| call.index)
            .collect();
        let mut closed = Vec::new();
        for index in indices {
            if let Some(index) = self.close(index)? {
                closed.push(index);
            }
        }
        Ok(closed)
    }

    /// The position of a tracked call, by its canonical index.
    fn position(&self, index: u32) -> Option<usize> {
        self.calls.iter().position(|call| call.index == index)
    }
}

/// Whether what was reconstructed is a call at all.
///
/// The two failures are deliberately different kinds, because they are
/// different accidents:
///
/// - arguments that do not parse mean the stream **ended mid-document** — the
///   terminator arrived while the model was still writing, which is an ordering
///   fault ([`StreamDecodeErrorKind::IllegalSequence`]);
/// - arguments that parse but are not an object mean the provider sent a
///   complete document of the wrong **shape**
///   ([`StreamDecodeErrorKind::UnknownEventShape`]).
///
/// Empty arguments are not a failure: a zero-argument tool sends no argument
/// bytes at all, and treating that as truncation would break every call to a
/// tool that takes nothing.
fn validate(call: &TrackedCall) -> Result<(), StreamDecodeError> {
    if call.name.is_none() {
        return Err(decode_error(
            StreamDecodeErrorKind::UnknownEventShape,
            "a tool call ended without ever naming a function",
        ));
    }
    if call.arguments.trim().is_empty() {
        return Ok(());
    }
    let Ok(parsed) = serde_json::from_str::<Value>(&call.arguments) else {
        return Err(decode_error(
            StreamDecodeErrorKind::IllegalSequence,
            "a tool call ended before its arguments formed a complete JSON document",
        ));
    };
    if !parsed.is_object() {
        return Err(decode_error(
            StreamDecodeErrorKind::UnknownEventShape,
            "a tool call's arguments parsed but were not a JSON object",
        ));
    }
    Ok(())
}

/// Builds the canonical fragment for one observed piece of a tool call.
///
/// Shared so the two grammars cannot disagree about the one distinction the
/// vocabulary makes here: a field that was **not seen** is absent, a field seen
/// as an empty string is present and empty.
pub(super) fn fragment(
    index: u32,
    id: Option<&str>,
    name: Option<&str>,
    arguments_delta: &str,
) -> ToolCallFragment {
    ToolCallFragment {
        index,
        id: id.map(str::to_string),
        name: name.map(str::to_string),
        arguments_delta: arguments_delta.to_string(),
    }
}
