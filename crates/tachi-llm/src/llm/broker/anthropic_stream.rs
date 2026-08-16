//! The Anthropic event streaming grammar — the divergence goldens' subject.
//!
//! # Why this exists in a slice about the OpenAI dialect
//!
//! The frozen design's question about the canonical vocabulary is whether it is
//! OpenAI-shaped. That question cannot be answered by writing more OpenAI
//! fixtures: it is answered by decoding a grammar that disagrees with OpenAI
//! about nearly everything and seeing whether the same event vocabulary, the
//! same tool-call reconstruction and the same terminal-input semantics still
//! fit. This module is that experiment, and under sans-IO its cost is roughly
//! the cost of the fixtures it enables.
//!
//! # Four places this grammar disagrees with the delta grammar
//!
//! 1. **Events are named on the SSE frame**, not typed inside the payload — and
//!    the name and the payload's own `type` must agree, which is a cheap and
//!    real integrity check the delta grammar cannot make.
//! 2. **The terminator is an event** (`message_stop`), not a sentinel string.
//! 3. **Content blocks are explicit**: a call opens with `content_block_start`,
//!    accumulates `input_json_delta` fragments and closes with
//!    `content_block_stop`, where OpenAI packs identity and arguments into
//!    repeated deltas and closes everything at `finish_reason`.
//! 4. **Usage is reported twice** — input tokens at `message_start`, output
//!    tokens at `message_delta`.
//!
//! # The index that is not the index
//!
//! Anthropic's `index` counts *content blocks*: with one text block before it,
//! the first tool call is block 1. The canonical `ToolCallFragment.index` is the
//! call's position among the turn's tool calls, so the same call is canonical
//! index 0. Passing the block index straight through would put a hole in the
//! numbering the moment a model wrote a sentence before calling a tool — and it
//! would do so silently, since nothing downstream can tell 1-of-1 from 1-of-2.
//! The mapping is what the divergence goldens pin.
//!
//! # What is deliberately not here
//!
//! An Anthropic request/response adapter lives next door now
//! ([`super::anthropic::AnthropicWire`]); this module stays stream-grammar
//! only so the request/response mapping and the event-state machine cannot
//! quietly diverge. What still is deliberately absent is a canonical event for
//! reasoning traces: `thinking_delta` / `signature_delta` fragments are read
//! and dropped, because inventing one would be a vocabulary change, not a
//! decoder change.

use serde_json::Value;

use super::disposition::{CompletionKindV1, InvocationDispositionV1, ProtocolViolation};
use super::openai_compat::non_blank;
use super::sse::SseFrame;
use super::stream::{
    CanonicalStreamEvent, StreamDecodeError, StreamDecodeErrorKind, StreamEof, TransportErrorKind,
    WireStreamDecoder,
};
use super::stream_grammar::{
    decode_error, fragment, SseDecoder, SseGrammar, StreamLifecycle, ToolCallTracker,
};
use super::usage::UsageObservationV1;
use super::wire::ProviderResponseMetadata;

/// The most content blocks one message may open.
///
/// The tool-call ceiling in [`super::stream_grammar`] does not cover this, and
/// that is the whole point of stating it separately: a text or thinking block
/// opens no tool call, so a run of tiny, individually legal
/// `content_block_start`/`content_block_stop` pairs passes every other limit
/// while growing this list without bound — and since every event looks its
/// block up by scanning the list, the work is quadratic in bytes the provider
/// chose to send. Both halves are the provider's to choose, which is what
/// makes it a ceiling and not a nicety.
///
/// Larger than the tool ceiling because a block is a far cheaper object than a
/// reconstructed call, and because an interleaved-thinking turn legitimately
/// opens many of them: a thought, a sentence, another thought, a tool call.
const MAX_CONTENT_BLOCKS_PER_MESSAGE: usize = 1024;

/// The event that ends an Anthropic stream.
///
/// Named because it is read in two places — the dispatch table and the
/// after-the-answer rule — and those two must mean the same event.
const MESSAGE_STOP_EVENT: &str = "message_stop";

/// Decodes an Anthropic event stream into canonical events.
#[derive(Debug, Default)]
pub struct AnthropicEventStreamDecoder(SseDecoder<AnthropicEventGrammar>);

impl AnthropicEventStreamDecoder {
    /// A decoder with no bytes seen.
    pub fn new() -> Self {
        Self(SseDecoder::new())
    }
}

impl WireStreamDecoder for AnthropicEventStreamDecoder {
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

/// What kind of content block an index holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    /// Assistant text.
    Text,
    /// A tool call, and its position among the turn's tool calls.
    Tool {
        /// The canonical index — *not* the block index. See the module note.
        ordinal: u32,
    },
    /// A block this vocabulary has no event for (reasoning traces), read and
    /// dropped rather than refused: refusing would fail every extended-thinking
    /// response, and inventing an event would change the frozen vocabulary.
    Ignored,
}

/// One content block seen in the stream.
#[derive(Debug)]
struct TrackedBlock {
    /// The provider's block index.
    index: u32,
    /// What it holds.
    kind: BlockKind,
    /// Whether `content_block_stop` has arrived for it.
    closed: bool,
}

/// The Anthropic grammar's own state.
#[derive(Debug, Default)]
pub(super) struct AnthropicEventGrammar {
    /// Tool calls under reconstruction, keyed by canonical index.
    tools: ToolCallTracker,
    /// Content blocks seen, in the order they opened.
    blocks: Vec<TrackedBlock>,
    /// The canonical index the next tool call will take.
    next_ordinal: u32,
    /// The completion kind the generation ended with, once seen.
    completion: Option<CompletionKindV1>,
}

/// Validates a frame's payload for the event named `name`: a `data` field
/// present, parseable as JSON, an object, and self-declaring the same `type`
/// the SSE `event:` field already named.
///
/// Shared rather than inlined per call site because this grammar checks it
/// twice — once for every ordinary frame, and once for the `message_stop`
/// this grammar still accepts after the stream is already sealed (see the
/// terminal branch of [`AnthropicEventGrammar::handle_frame`]). A malformed
/// payload must fail the same way under either name, and a copy-pasted check
/// is exactly how the two drift.
fn validated_event_payload(frame: &SseFrame, name: &str) -> Result<Value, StreamDecodeError> {
    let Some(data) = frame.data.as_deref() else {
        return Err(decode_error(
            StreamDecodeErrorKind::UnknownEventShape,
            "an event carried no data field",
        ));
    };
    let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
        return Err(decode_error(
            StreamDecodeErrorKind::MalformedFrame,
            "an event payload was not the grammar's JSON serialization",
        ));
    };
    if !value.is_object() {
        return Err(decode_error(
            StreamDecodeErrorKind::UnknownEventShape,
            "an event payload parsed as JSON but was not an object",
        ));
    }
    // The frame says what it is twice. Requiring the two to agree costs
    // nothing and catches the case where a proxy rewrites one of them.
    let Some(declared) = value.get("type").and_then(Value::as_str) else {
        return Err(decode_error(
            StreamDecodeErrorKind::UnknownEventShape,
            "an event payload carried no type",
        ));
    };
    if declared != name {
        return Err(decode_error(
            StreamDecodeErrorKind::UnknownEventShape,
            "the SSE event name and the payload's own type disagree",
        ));
    }
    Ok(value)
}

impl SseGrammar for AnthropicEventGrammar {
    fn handle_frame(
        &mut self,
        lifecycle: &mut StreamLifecycle,
        frame: SseFrame,
    ) -> Result<Vec<CanonicalStreamEvent>, StreamDecodeError> {
        if lifecycle.is_terminal() {
            // The same exception the delta grammar makes for `[DONE]`, which
            // is why it is worth stating twice: this grammar's own terminator,
            // closing a stream whose answer is already sealed, is the provider
            // closing politely — an `error` event is normally followed by
            // `message_stop` — and not trailing garbage. It answers nothing
            // and cannot move the sealed disposition.
            //
            // "Politely" still means an actual `message_stop`, not merely a
            // frame that happens to be named one: it gets the exact same
            // data/JSON/type discipline every other frame in this grammar
            // gets, via the same validator, so a truncated proxy tail or a
            // provider bug that mangles the closing frame cannot ride through
            // silently just because the `event:` name matches. Anything named
            // something else is unambiguously trailing garbage and stays
            // `IllegalSequence` without inspecting its payload at all.
            return if frame.event.as_deref() == Some(MESSAGE_STOP_EVENT) {
                validated_event_payload(&frame, MESSAGE_STOP_EVENT).map(|_| Vec::new())
            } else {
                Err(decode_error(
                    StreamDecodeErrorKind::IllegalSequence,
                    "a frame arrived after the stream's terminator",
                ))
            };
        }
        let Some(name) = frame.event.as_deref() else {
            return Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "this grammar names every event; an unnamed frame belongs to a \
                 different grammar",
            ));
        };
        let value = validated_event_payload(&frame, name)?;

        match name {
            "message_start" => self.message_start(lifecycle, &value),
            "content_block_start" => self.content_block_start(lifecycle, &value),
            "content_block_delta" => self.content_block_delta(lifecycle, &value),
            "content_block_stop" => self.content_block_stop(&value),
            "message_delta" => self.message_delta(lifecycle, &value),
            MESSAGE_STOP_EVENT => self.message_stop(lifecycle),
            // A keep-alive. It must cost nothing: a decoder that refused one
            // would fail on live traffic while every hand-written fixture
            // passed.
            "ping" => Ok(Vec::new()),
            // The provider reporting a failure in its own grammar — the event
            // channel, not the decode-error channel, and carrying none of the
            // prose that came with it.
            "error" => {
                Ok(vec![lifecycle.fail_with(
                    ProtocolViolation::SchemaViolation { pointer: "/error" },
                )])
            }
            _ => Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "an event name outside this grammar",
            )),
        }
    }

    fn unterminated_detail(&self) -> &'static str {
        "the body ended cleanly without the grammar's message_stop terminator"
    }
}

impl AnthropicEventGrammar {
    /// `message_start`: identity, and the input-token half of usage.
    fn message_start(
        &mut self,
        lifecycle: &mut StreamLifecycle,
        value: &Value,
    ) -> Result<Vec<CanonicalStreamEvent>, StreamDecodeError> {
        if !lifecycle.mark_started() {
            return Err(decode_error(
                StreamDecodeErrorKind::IllegalSequence,
                "a second message_start arrived for one stream",
            ));
        }
        let message = value.get("message");
        let metadata = ProviderResponseMetadata {
            effective_model: non_blank(message.and_then(|message| message.get("model"))),
            effective_version: None,
            provider_request_id: non_blank(message.and_then(|message| message.get("id"))),
        };
        let mut events = vec![CanonicalStreamEvent::Started {
            metadata: (metadata != ProviderResponseMetadata::default()).then_some(metadata),
        }];
        if let Some(usage) = message
            .and_then(|message| message.get("usage"))
            .and_then(usage_of)
        {
            events.push(CanonicalStreamEvent::Usage { usage });
        }
        Ok(events)
    }

    /// `content_block_start`: a text block, or a tool call opening.
    fn content_block_start(
        &mut self,
        lifecycle: &mut StreamLifecycle,
        value: &Value,
    ) -> Result<Vec<CanonicalStreamEvent>, StreamDecodeError> {
        if !lifecycle.started() {
            return Err(decode_error(
                StreamDecodeErrorKind::IllegalSequence,
                "a content block started before the message did",
            ));
        }
        if lifecycle.generation_ended() {
            return Err(decode_error(
                StreamDecodeErrorKind::IllegalSequence,
                "a content block started after the generation ended",
            ));
        }
        // Checked before anything is opened, so a refusal leaves no half-opened
        // call behind it.
        if self.blocks.len() >= MAX_CONTENT_BLOCKS_PER_MESSAGE {
            return Err(decode_error(
                StreamDecodeErrorKind::EventTooLarge,
                "one message opened more content blocks than the decoder admits",
            ));
        }
        let index = block_index(value)?;
        if self.position(index).is_some() {
            return Err(decode_error(
                StreamDecodeErrorKind::IllegalSequence,
                "two content blocks started at the same index",
            ));
        }
        let Some(block) = value.get("content_block") else {
            return Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "a content_block_start carried no content block",
            ));
        };
        let Some(block_type) = block.get("type").and_then(Value::as_str) else {
            return Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "a content block carried no type",
            ));
        };

        match block_type {
            "text" => {
                self.track(index, BlockKind::Text);
                Ok(Vec::new())
            }
            "tool_use" => {
                let id = non_blank(block.get("id"));
                let Some(name) = non_blank(block.get("name")) else {
                    return Err(decode_error(
                        StreamDecodeErrorKind::UnknownEventShape,
                        "a tool_use block named no function",
                    ));
                };
                // Block index in, canonical index out. See the module note.
                let ordinal = self.next_ordinal;
                self.tools
                    .open(ordinal, id.as_deref(), Some(name.as_str()))?;
                self.next_ordinal += 1;
                self.track(index, BlockKind::Tool { ordinal });
                lifecycle.note_visible_output();
                Ok(vec![CanonicalStreamEvent::ToolCallDelta {
                    fragment: fragment(ordinal, id.as_deref(), Some(name.as_str()), ""),
                }])
            }
            "thinking" | "redacted_thinking" => {
                self.track(index, BlockKind::Ignored);
                Ok(Vec::new())
            }
            _ => Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "a content block of a type outside this grammar",
            )),
        }
    }

    /// `content_block_delta`: text, or a slice of a tool call's arguments.
    fn content_block_delta(
        &mut self,
        lifecycle: &mut StreamLifecycle,
        value: &Value,
    ) -> Result<Vec<CanonicalStreamEvent>, StreamDecodeError> {
        if lifecycle.generation_ended() {
            return Err(decode_error(
                StreamDecodeErrorKind::IllegalSequence,
                "a content delta arrived after the generation ended",
            ));
        }
        let index = block_index(value)?;
        let Some(position) = self.position(index) else {
            return Err(decode_error(
                StreamDecodeErrorKind::IllegalSequence,
                "a delta arrived for a content block that never started",
            ));
        };
        if self.blocks[position].closed {
            return Err(decode_error(
                StreamDecodeErrorKind::IllegalSequence,
                "a delta arrived for a content block that had already ended",
            ));
        }
        let kind = self.blocks[position].kind;

        let Some(delta) = value.get("delta") else {
            return Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "a content_block_delta carried no delta",
            ));
        };
        let Some(delta_type) = delta.get("type").and_then(Value::as_str) else {
            return Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "a delta carried no type",
            ));
        };

        match (kind, delta_type) {
            (BlockKind::Text, "text_delta") => {
                let Some(text) = delta.get("text").and_then(Value::as_str) else {
                    return Err(decode_error(
                        StreamDecodeErrorKind::UnknownEventShape,
                        "a text delta carried no text",
                    ));
                };
                if text.is_empty() {
                    return Ok(Vec::new());
                }
                lifecycle.note_visible_output();
                Ok(vec![CanonicalStreamEvent::TextDelta {
                    text: text.to_string(),
                }])
            }
            (BlockKind::Tool { ordinal }, "input_json_delta") => {
                let Some(partial) = delta.get("partial_json").and_then(Value::as_str) else {
                    return Err(decode_error(
                        StreamDecodeErrorKind::UnknownEventShape,
                        "an input_json_delta carried no partial_json",
                    ));
                };
                self.tools.append(ordinal, partial)?;
                lifecycle.note_visible_output();
                Ok(vec![CanonicalStreamEvent::ToolCallDelta {
                    fragment: fragment(ordinal, None, None, partial),
                }])
            }
            (BlockKind::Ignored, "thinking_delta" | "signature_delta") => Ok(Vec::new()),
            (_, "text_delta" | "input_json_delta" | "thinking_delta" | "signature_delta") => {
                Err(decode_error(
                    StreamDecodeErrorKind::IllegalSequence,
                    "a delta's type does not match the content block it belongs to",
                ))
            }
            _ => Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "a content-block delta of a type outside this grammar",
            )),
        }
    }

    /// `content_block_stop`: the block is complete.
    fn content_block_stop(
        &mut self,
        value: &Value,
    ) -> Result<Vec<CanonicalStreamEvent>, StreamDecodeError> {
        let index = block_index(value)?;
        let Some(position) = self.position(index) else {
            return Err(decode_error(
                StreamDecodeErrorKind::IllegalSequence,
                "a content block ended that never started",
            ));
        };
        if self.blocks[position].closed {
            return Err(decode_error(
                StreamDecodeErrorKind::IllegalSequence,
                "a content block ended twice",
            ));
        }
        self.blocks[position].closed = true;
        let kind = self.blocks[position].kind;
        match kind {
            BlockKind::Tool { ordinal } => Ok(self
                .tools
                .close(ordinal)?
                .map(|index| vec![CanonicalStreamEvent::ToolCallCompleted { index }])
                .unwrap_or_default()),
            BlockKind::Text | BlockKind::Ignored => Ok(Vec::new()),
        }
    }

    /// `message_delta`: how the generation ended, and the output-token half of
    /// usage.
    fn message_delta(
        &mut self,
        lifecycle: &mut StreamLifecycle,
        value: &Value,
    ) -> Result<Vec<CanonicalStreamEvent>, StreamDecodeError> {
        let mut events = Vec::new();
        // Both usage reports are emitted rather than merged. Merging would mean
        // deciding which one wins, and the executor — which sees the whole
        // sequence — is the one that can decide it; a decoder that dropped the
        // first would lose the input-token count on any stream that dies before
        // this event.
        if let Some(usage) = value.get("usage").and_then(usage_of) {
            events.push(CanonicalStreamEvent::Usage { usage });
        }
        if let Some(stop) = value
            .get("delta")
            .and_then(|delta| delta.get("stop_reason"))
            .and_then(Value::as_str)
        {
            if lifecycle.generation_ended() {
                return Err(decode_error(
                    StreamDecodeErrorKind::IllegalSequence,
                    "a second stop_reason arrived for one generation",
                ));
            }
            lifecycle.end_generation();
            self.completion = Some(stop_reason_kind(stop));
        }
        Ok(events)
    }

    /// `message_stop`: the terminator.
    fn message_stop(
        &mut self,
        lifecycle: &mut StreamLifecycle,
    ) -> Result<Vec<CanonicalStreamEvent>, StreamDecodeError> {
        if !lifecycle.saw_visible_output() {
            return Err(decode_error(
                StreamDecodeErrorKind::IllegalSequence,
                "the stream terminated without assistant content or a tool call",
            ));
        }
        let mut events = Vec::new();
        // A block that never got its `content_block_stop` is still a call the
        // model made; closing here validates it rather than dropping it.
        for index in self.tools.close_all()? {
            events.push(CanonicalStreamEvent::ToolCallCompleted { index });
        }
        let completion = self.completion.unwrap_or(CompletionKindV1::Unknown);
        lifecycle.seal(InvocationDispositionV1::Completed { completion });
        events.push(CanonicalStreamEvent::Completed { completion });
        Ok(events)
    }

    /// Records a content block. The list it grows is bounded by
    /// [`MAX_CONTENT_BLOCKS_PER_MESSAGE`], checked where the block opens.
    fn track(&mut self, index: u32, kind: BlockKind) {
        self.blocks.push(TrackedBlock {
            index,
            kind,
            closed: false,
        });
    }

    /// Where a block index sits in [`Self::blocks`].
    fn position(&self, index: u32) -> Option<usize> {
        self.blocks.iter().position(|block| block.index == index)
    }
}

/// The provider's content-block index.
fn block_index(value: &Value) -> Result<u32, StreamDecodeError> {
    let Some(index) = value.get("index").and_then(Value::as_u64) else {
        return Err(decode_error(
            StreamDecodeErrorKind::UnknownEventShape,
            "a content-block event carried no index",
        ));
    };
    u32::try_from(index).map_err(|_| {
        decode_error(
            StreamDecodeErrorKind::UnknownEventShape,
            "a content-block index did not fit the canonical width",
        )
    })
}

/// This grammar's usage block.
///
/// No total: Anthropic does not report one, and deriving `input + output` would
/// manufacture a number with `provider_authoritative` stamped on it — the exact
/// thing the usage vocabulary's provenance tiers exist to prevent.
fn usage_of(value: &Value) -> Option<UsageObservationV1> {
    let input = value.get("input_tokens").and_then(Value::as_u64);
    let output = value.get("output_tokens").and_then(Value::as_u64);
    if input.is_none() && output.is_none() {
        return None;
    }
    Some(UsageObservationV1::provider_authoritative(
        input, output, None,
    ))
}

/// This grammar's end-of-generation vocabulary.
///
/// Its own mapping, not the delta grammar's: `stop_sequence` is a normal stop
/// here, `refusal` is a content-policy ending, and there is no `length`. Sharing
/// one table would mean one of the two grammars reading a reason it never sends.
fn stop_reason_kind(raw: &str) -> CompletionKindV1 {
    match raw {
        "end_turn" | "stop_sequence" => CompletionKindV1::Complete,
        "max_tokens" => CompletionKindV1::Truncated,
        "tool_use" => CompletionKindV1::ToolCalls,
        "refusal" => CompletionKindV1::ContentFiltered,
        _ => CompletionKindV1::Unknown,
    }
}
