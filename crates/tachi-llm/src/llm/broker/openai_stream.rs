//! The OpenAI-compatible `delta` streaming grammar.
//!
//! # The grammar, as this decoder reads it
//!
//! A `chat.completion.chunk` stream is SSE frames whose `data` is a JSON
//! object, ended by the literal frame `data: [DONE]`. Each object carries at
//! most one choice, whose `delta` holds either a slice of assistant text or a
//! slice of a tool call, and whose `finish_reason` — when it finally appears —
//! ends the *generation* but not the *stream*: a usage report may still follow,
//! and the terminator comes after that.
//!
//! Those are two different ends, and conflating them is why a decoder either
//! drops the usage block (ending at `finish_reason`) or reports success on a
//! truncated body (ending at neither).
//! [`StreamLifecycle`](super::stream_grammar::StreamLifecycle) keeps them
//! apart.
//!
//! # What is here and what is not
//!
//! Only the meaning of a frame. Framing is [`super::sse`], and everything that
//! must not vary between providers — terminal inputs, ceilings, tool-call
//! reconstruction, when a failure may be reported — is
//! [`super::stream_grammar`]. This module is the pluggable part, which is the
//! claim the Anthropic grammar exists to test.
//!
//! # Two failure channels, and the rule for which is which
//!
//! - **`Err(StreamDecodeError)` means the decoder could not read the bytes.**
//!   Framing broken, not UTF-8, valid JSON of an unknown shape, events in an
//!   order the grammar forbids.
//! - **A `Failed` event means the decoder read the bytes and they say the
//!   invocation failed.** An in-stream `error` object is the provider speaking
//!   its own grammar correctly to report a failure; calling that a decode error
//!   would blame the wrong party.
//!
//! # What a `[DONE]` seals
//!
//! Once the terminator has been seen the answer is fixed. Bytes arriving after
//! it are a protocol fault and are reported as one
//! ([`StreamDecodeErrorKind::IllegalSequence`]) rather than ignored — silence
//! is how a provider that keeps talking after `[DONE]` stays undiagnosed — but
//! they do **not** change the disposition. Turning a completed invocation into
//! a failure because of trailing garbage would be a far worse bug than the one
//! being reported.
//!
//! The rule is about *garbage* after the answer, and the terminator is not
//! garbage. A stream sealed by something other than `[DONE]` — an in-stream
//! `error` object, which real gateways follow with `data: [DONE]` — is a
//! provider closing the stream it opened, so the terminator arriving there
//! answers nothing, changes nothing and is not a fault. Reporting it would
//! staple an `illegal_sequence` onto every provider-declared failure, which
//! blames the decoder for a provider that behaved correctly.

use serde_json::Value;

use super::disposition::{CompletionKindV1, InvocationDispositionV1, ProtocolViolation};
use super::openai_compat::{completion_kind, non_blank, parse_usage};
use super::sse::SseFrame;
use super::stream::{
    CanonicalStreamEvent, StreamDecodeError, StreamDecodeErrorKind, StreamEof, ToolCallFragment,
    TransportErrorKind, WireStreamDecoder,
};
use super::stream_grammar::{
    decode_error, fragment, SseDecoder, SseGrammar, StreamLifecycle, ToolCallTracker,
};
use super::wire::ProviderResponseMetadata;

/// The frame that ends an OpenAI-compatible stream.
const DONE_SENTINEL: &str = "[DONE]";

/// The only SSE event name this grammar admits.
///
/// The delta grammar carries its type inside the payload and normally sets no
/// event name at all; a *named* event belongs to another grammar (Anthropic's
/// `message_start`, an NDJSON deployment's nothing-at-all), and accepting one
/// silently is how a decoder half-reads a stream it does not understand.
const MESSAGE_EVENT: &str = "message";

/// Decodes an OpenAI-compatible `delta` stream into canonical events.
#[derive(Debug, Default)]
pub struct OpenAiCompatStreamDecoder(SseDecoder<DeltaGrammar>);

impl OpenAiCompatStreamDecoder {
    /// A decoder with no bytes seen.
    pub fn new() -> Self {
        Self(SseDecoder::new())
    }
}

// Forwarding, deliberately: the shared driver owns the transport and the
// terminal-input semantics, and this type exists so the dialect's decoder has a
// name of its own in the public API.
impl WireStreamDecoder for OpenAiCompatStreamDecoder {
    fn push_bytes(&mut self, chunk: &[u8]) -> Result<Vec<CanonicalStreamEvent>, StreamDecodeError> {
        self.0.push_bytes(chunk)
    }

    fn finish(&mut self, eof: StreamEof) -> Result<Vec<CanonicalStreamEvent>, StreamDecodeError> {
        self.0.finish(eof)
    }

    fn on_transport_error(&mut self, error: TransportErrorKind) -> Vec<CanonicalStreamEvent> {
        self.0.on_transport_error(error)
    }

    fn on_cancel(&mut self) -> Vec<CanonicalStreamEvent> {
        self.0.on_cancel()
    }

    fn terminal_disposition(&self) -> Option<InvocationDispositionV1> {
        self.0.terminal_disposition()
    }
}

/// The `delta` grammar's own state: what it is reconstructing, and how the
/// generation ended.
#[derive(Debug, Default)]
pub(super) struct DeltaGrammar {
    /// Tool calls under reconstruction.
    tools: ToolCallTracker,
    /// The completion kind the generation ended with, once seen.
    completion: Option<CompletionKindV1>,
}

impl SseGrammar for DeltaGrammar {
    fn handle_frame(
        &mut self,
        lifecycle: &mut StreamLifecycle,
        frame: SseFrame,
    ) -> Result<Vec<CanonicalStreamEvent>, StreamDecodeError> {
        if lifecycle.is_terminal() {
            // The one frame that is not "trailing garbage": this grammar's own
            // terminator, closing a stream whose answer is already sealed.
            // That is the ordinary shape of a provider-declared failure — real
            // gateways send `{"error": …}` and then `data: [DONE]` — and
            // calling it a decode fault would staple an `illegal_sequence`
            // onto every in-stream error report, blaming the decoder for a
            // provider that closed its stream politely. It answers nothing and
            // changes nothing: the sealed disposition is first-writer-wins and
            // stays exactly as it was.
            if is_terminator(&frame) {
                return Ok(Vec::new());
            }
            return Err(decode_error(
                StreamDecodeErrorKind::IllegalSequence,
                "a frame arrived after the stream's terminator",
            ));
        }
        if let Some(name) = frame.event.as_deref() {
            if name != MESSAGE_EVENT {
                return Err(decode_error(
                    StreamDecodeErrorKind::UnknownEventShape,
                    "the delta grammar carries its type in the payload; \
                     a named SSE event belongs to a different grammar",
                ));
            }
        }
        let Some(data) = frame.data.as_deref() else {
            return Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "an SSE frame carried no data field",
            ));
        };
        let data = data.trim();
        if data == DONE_SENTINEL {
            return self.terminate(lifecycle);
        }

        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return Err(decode_error(
                StreamDecodeErrorKind::MalformedFrame,
                "a data frame was not the dialect's JSON serialization",
            ));
        };
        let Some(object) = value.as_object() else {
            return Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "a data frame parsed as JSON but was not an object",
            ));
        };

        let mut events = Vec::new();
        if lifecycle.mark_started() {
            events.push(CanonicalStreamEvent::Started {
                metadata: metadata_of(&value),
            });
        }

        if let Some(error) = object.get("error").filter(|error| !error.is_null()) {
            // The split between the two failure channels is about *who* is at
            // fault, and it turns on the shape: an error **object** is the
            // provider speaking its own grammar correctly to report a failure,
            // which is the event channel. A `false`, a string, a number or an
            // array under `/error` is none of that — it is a shape this
            // decoder does not know, and admitting it would let any
            // wrong-typed field seal a stream as provider-declared failure and
            // blame the provider for a document nobody can read.
            if !error.is_object() {
                return Err(decode_error(
                    StreamDecodeErrorKind::UnknownEventShape,
                    "an `error` field was present but was not an error object",
                ));
            }
            // The provider's own failure report. Not a decode error — the
            // grammar was spoken correctly — so it becomes the terminal event,
            // pointing at the field that carried it and carrying none of it.
            events.push(
                lifecycle.fail_with(ProtocolViolation::SchemaViolation { pointer: "/error" }),
            );
            return Ok(events);
        }

        let choices = match object.get("choices") {
            Some(Value::Array(choices)) => choices.as_slice(),
            None | Some(Value::Null) => &[][..],
            Some(_) => {
                return Err(decode_error(
                    StreamDecodeErrorKind::UnknownEventShape,
                    "`choices` was present but was not an array",
                ))
            }
        };
        let usage = object.get("usage").filter(|usage| !usage.is_null());
        if choices.is_empty() && usage.is_none() {
            return Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "a chunk carried neither choices nor a usage report",
            ));
        }
        if choices.len() > 1 {
            // In a stream the choices arrive interleaved, so the non-streaming
            // path's "pick the first non-blank choice" has nothing to pick
            // from without buffering the whole response — which is what
            // streaming exists to avoid. A typed refusal beats inventing an
            // interleaving rule the provider never agreed to.
            return Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "a streamed chunk carried more than one choice",
            ));
        }

        if let Some(usage) = usage {
            events.push(CanonicalStreamEvent::Usage {
                usage: parse_usage(Some(usage)),
            });
        }
        if let Some(choice) = choices.first() {
            self.decode_choice(lifecycle, choice, &mut events)?;
        }
        Ok(events)
    }

    fn unterminated_detail(&self) -> &'static str {
        "the body ended cleanly without the delta grammar's [DONE] terminator"
    }
}

impl DeltaGrammar {
    /// Applies one `choices[]` entry.
    fn decode_choice(
        &mut self,
        lifecycle: &mut StreamLifecycle,
        choice: &Value,
        events: &mut Vec<CanonicalStreamEvent>,
    ) -> Result<(), StreamDecodeError> {
        let Some(choice) = choice.as_object() else {
            return Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "a choice was not an object",
            ));
        };

        // Which completion this delta belongs to. A chunk carrying more than
        // one choice is refused above, but that refusal is worth nothing on
        // its own: a provider answering `n > 1` sends each choice in a chunk
        // of its own, every chunk passes the one-choice check, and the deltas
        // of several independent completions arrive interleaved on one stream.
        // Decoding those as *the* answer splices two generations into one
        // sentence, and the caller has no way to see it happened. This decoder
        // reads the first completion and refuses to guess about the rest.
        if let Some(index) = choice.get("index").filter(|index| !index.is_null()) {
            if index.as_u64() != Some(0) {
                return Err(decode_error(
                    StreamDecodeErrorKind::UnknownEventShape,
                    "a streamed chunk carried a choice other than the first",
                ));
            }
        }

        if let Some(delta) = choice.get("delta").filter(|delta| !delta.is_null()) {
            let Some(delta) = delta.as_object() else {
                return Err(decode_error(
                    StreamDecodeErrorKind::UnknownEventShape,
                    "a choice's delta was not an object",
                ));
            };

            if let Some(content) = delta.get("content").filter(|content| !content.is_null()) {
                let Some(text) = content.as_str() else {
                    return Err(decode_error(
                        StreamDecodeErrorKind::UnknownEventShape,
                        "a delta's content was not a string",
                    ));
                };
                // The opening chunk of every OpenAI stream carries
                // `{"role":"assistant","content":""}`. An empty fragment is not
                // a text event; emitting one would put a meaningless delta in
                // front of every single response.
                if !text.is_empty() {
                    if lifecycle.generation_ended() {
                        return Err(decode_error(
                            StreamDecodeErrorKind::IllegalSequence,
                            "a content delta arrived after the generation ended",
                        ));
                    }
                    lifecycle.note_visible_output();
                    events.push(CanonicalStreamEvent::TextDelta {
                        text: text.to_string(),
                    });
                }
            }

            if let Some(calls) = delta.get("tool_calls").filter(|calls| !calls.is_null()) {
                let Some(calls) = calls.as_array() else {
                    return Err(decode_error(
                        StreamDecodeErrorKind::UnknownEventShape,
                        "a delta's tool_calls was not an array",
                    ));
                };
                for call in calls {
                    if lifecycle.generation_ended() {
                        return Err(decode_error(
                            StreamDecodeErrorKind::IllegalSequence,
                            "a tool-call delta arrived after the generation ended",
                        ));
                    }
                    events.push(CanonicalStreamEvent::ToolCallDelta {
                        fragment: self.tool_fragment(call)?,
                    });
                    lifecycle.note_visible_output();
                }
            }
        }

        if let Some(finish) = choice
            .get("finish_reason")
            .filter(|finish| !finish.is_null())
        {
            let Some(finish) = finish.as_str() else {
                return Err(decode_error(
                    StreamDecodeErrorKind::UnknownEventShape,
                    "a finish_reason was present but was not a string",
                ));
            };
            if lifecycle.generation_ended() {
                return Err(decode_error(
                    StreamDecodeErrorKind::IllegalSequence,
                    "a second finish_reason arrived for one generation",
                ));
            }
            lifecycle.end_generation();
            // The same mapping the non-streaming path uses, called rather than
            // copied: two transcriptions of "which finish reasons mean
            // truncated" is one transcription too many.
            self.completion = Some(completion_kind(Some(finish)));
            for index in self.tools.close_all()? {
                events.push(CanonicalStreamEvent::ToolCallCompleted { index });
            }
        }
        Ok(())
    }

    /// Applies one `tool_calls[]` entry, updating the reconstruction.
    fn tool_fragment(&mut self, call: &Value) -> Result<ToolCallFragment, StreamDecodeError> {
        let Some(call) = call.as_object() else {
            return Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "a tool-call delta was not an object",
            ));
        };
        let Some(index) = call.get("index").and_then(Value::as_u64) else {
            return Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "a tool-call delta carried no index",
            ));
        };
        let Ok(index) = u32::try_from(index) else {
            return Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "a tool-call index did not fit the canonical width",
            ));
        };

        // Two readings of the same fields, and the difference is deliberate.
        // The *fragment* reports what the wire said, so a provider that repeats
        // `"id": ""` on every continuation is visible as such. The
        // *reconstruction* only accepts a non-empty value as identity, because
        // an empty repeat is not a second id — treating it as one would make
        // every continuation fragment look like an id conflict.
        let raw_id = call.get("id").and_then(Value::as_str);
        let function = call.get("function").filter(|value| !value.is_null());
        let raw_name = function
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str);
        let arguments = function
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str);

        self.tools.observe(
            index,
            raw_id.filter(|id| !id.is_empty()),
            raw_name.filter(|name| !name.is_empty()),
            arguments,
        )?;
        Ok(fragment(
            index,
            raw_id,
            raw_name,
            arguments.unwrap_or_default(),
        ))
    }

    /// The terminator arrived.
    fn terminate(
        &mut self,
        lifecycle: &mut StreamLifecycle,
    ) -> Result<Vec<CanonicalStreamEvent>, StreamDecodeError> {
        let mut events = Vec::new();
        // A stream may terminate without ever sending `finish_reason`; the
        // calls it opened are still calls, so they are closed and validated
        // here rather than left dangling.
        for index in self.tools.close_all()? {
            events.push(CanonicalStreamEvent::ToolCallCompleted { index });
        }
        let completion = self.completion.unwrap_or(CompletionKindV1::Unknown);
        lifecycle.seal(InvocationDispositionV1::Completed { completion });
        events.push(CanonicalStreamEvent::Completed { completion });
        Ok(events)
    }
}

/// Whether a frame is exactly this grammar's terminator.
///
/// Spelled once and read in two places, so "the frame that ends the stream"
/// and "the frame that may follow the answer" cannot drift into two different
/// notions of what `[DONE]` is.
fn is_terminator(frame: &SseFrame) -> bool {
    frame
        .event
        .as_deref()
        .is_none_or(|name| name == MESSAGE_EVENT)
        && frame.data.as_deref().map(str::trim) == Some(DONE_SENTINEL)
}

/// The provider-side identity a chunk announces, when it announces any.
///
/// `None` rather than an all-empty struct: "the provider named itself" and "the
/// provider named nothing" are different facts, and a receipt that cannot tell
/// them apart records a model attribution nobody made.
fn metadata_of(value: &Value) -> Option<ProviderResponseMetadata> {
    let metadata = ProviderResponseMetadata {
        effective_model: non_blank(value.get("model")),
        effective_version: non_blank(value.get("system_fingerprint")),
        provider_request_id: non_blank(value.get("id")),
    };
    if metadata == ProviderResponseMetadata::default() {
        return None;
    }
    Some(metadata)
}
