//! Ollama's NDJSON chat stream grammar.
//!
//! Ollama streams newline-delimited JSON rather than SSE. The decoder is still
//! pure: bytes in, canonical events out, with the same terminal-input rules as
//! the SSE grammars.

use serde_json::Value;

use super::disposition::{CompletionKindV1, InvocationDispositionV1, ProtocolViolation};
use super::openai_compat::non_blank;
use super::sse::MAX_FRAME_BYTES;
use super::stream::{
    CanonicalStreamEvent, StreamDecodeError, StreamDecodeErrorKind, StreamEof, TransportErrorKind,
    WireStreamDecoder,
};
use super::stream_grammar::{decode_error, fragment, StreamLifecycle, ToolCallTracker};
use super::usage::UsageObservationV1;
use super::wire::ProviderResponseMetadata;

#[derive(Debug, Default)]
pub struct OllamaNdjsonStreamDecoder {
    line: Vec<u8>,
    after_cr: bool,
    lifecycle: StreamLifecycle,
    tools: ToolCallTracker,
    stashed: Option<StreamDecodeError>,
    stash_announced: bool,
}

impl OllamaNdjsonStreamDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    fn stash(&mut self, error: StreamDecodeError) {
        self.lifecycle.seal_decode_error(error.kind);
        if self.stashed.is_none() {
            self.stashed = Some(error);
        }
    }

    fn announce_stash(&mut self) -> Vec<CanonicalStreamEvent> {
        if self.stashed.is_none() || self.stash_announced {
            return Vec::new();
        }
        self.stash_announced = true;
        self.lifecycle
            .terminal()
            .map(|disposition| vec![CanonicalStreamEvent::Failed { disposition }])
            .unwrap_or_default()
    }

    fn take_line(&mut self) -> Result<Option<Vec<u8>>, StreamDecodeError> {
        if self.line.is_empty() {
            return Ok(None);
        }
        let mut line = std::mem::take(&mut self.line);
        let out = std::mem::take(&mut line);
        self.line = line;
        Ok(Some(out))
    }

    fn parse_line(&mut self, line: &[u8]) -> Result<Vec<CanonicalStreamEvent>, StreamDecodeError> {
        let Ok(text) = std::str::from_utf8(line) else {
            return Err(decode_error(
                StreamDecodeErrorKind::InvalidUtf8,
                "an NDJSON line was not valid UTF-8",
            ));
        };
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        if self.lifecycle.is_terminal() {
            return Err(decode_error(
                StreamDecodeErrorKind::IllegalSequence,
                "a JSON line arrived after the stream's terminal record",
            ));
        }
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return Err(decode_error(
                StreamDecodeErrorKind::MalformedFrame,
                "an NDJSON line was not valid JSON",
            ));
        };
        let Some(object) = value.as_object() else {
            return Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "an NDJSON line parsed as JSON but was not an object",
            ));
        };

        let mut events = Vec::new();
        if self.lifecycle.mark_started() {
            let metadata = ProviderResponseMetadata {
                effective_model: non_blank(value.get("model")),
                effective_version: None,
                provider_request_id: None,
            };
            events.push(CanonicalStreamEvent::Started {
                metadata: (metadata != ProviderResponseMetadata::default()).then_some(metadata),
            });
        }

        if let Some(error) = object.get("error").filter(|error| !error.is_null()) {
            if !error.is_string() {
                return Err(decode_error(
                    StreamDecodeErrorKind::UnknownEventShape,
                    "an `error` field was present but was not a string",
                ));
            }
            events.push(
                self.lifecycle
                    .fail_with(ProtocolViolation::SchemaViolation { pointer: "/error" }),
            );
            return Ok(events);
        }

        if let Some(message) = object.get("message").filter(|message| !message.is_null()) {
            let Some(message) = message.as_object() else {
                return Err(decode_error(
                    StreamDecodeErrorKind::UnknownEventShape,
                    "a line's message field was not an object",
                ));
            };
            if let Some(content) = message.get("content").filter(|content| !content.is_null()) {
                let Some(text) = content.as_str() else {
                    return Err(decode_error(
                        StreamDecodeErrorKind::UnknownEventShape,
                        "a message content field was not a string",
                    ));
                };
                if !text.is_empty() {
                    self.lifecycle.note_visible_output();
                    events.push(CanonicalStreamEvent::TextDelta {
                        text: text.to_string(),
                    });
                }
            }

            if let Some(calls) = message.get("tool_calls").filter(|calls| !calls.is_null()) {
                let Some(calls) = calls.as_array() else {
                    return Err(decode_error(
                        StreamDecodeErrorKind::UnknownEventShape,
                        "a message tool_calls field was not an array",
                    ));
                };
                for (index, call) in calls.iter().enumerate() {
                    let Some(function) = call.get("function").and_then(Value::as_object) else {
                        return Err(decode_error(
                            StreamDecodeErrorKind::UnknownEventShape,
                            "a tool call carried no function object",
                        ));
                    };
                    let Some(name) = function.get("name").and_then(Value::as_str) else {
                        return Err(decode_error(
                            StreamDecodeErrorKind::UnknownEventShape,
                            "a tool call named no function",
                        ));
                    };
                    let arguments = match function.get("arguments") {
                        None | Some(Value::Null) => String::new(),
                        Some(Value::String(arguments)) => arguments.clone(),
                        Some(arguments) => serde_json::to_string(arguments).map_err(|_| {
                            decode_error(
                                StreamDecodeErrorKind::UnknownEventShape,
                                "a tool call arguments value could not be serialized",
                            )
                        })?,
                    };
                    let index = index as u32;
                    self.tools.observe(
                        index,
                        None,
                        Some(name),
                        (!arguments.is_empty()).then_some(arguments.as_str()),
                    )?;
                    events.push(CanonicalStreamEvent::ToolCallDelta {
                        fragment: fragment(index, None, Some(name), arguments.as_str()),
                    });
                    self.lifecycle.note_visible_output();
                    if let Some(index) = self.tools.close(index)? {
                        events.push(CanonicalStreamEvent::ToolCallCompleted { index });
                    }
                }
            }
        } else if let Some(content) = object.get("response").filter(|content| !content.is_null()) {
            let Some(text) = content.as_str() else {
                return Err(decode_error(
                    StreamDecodeErrorKind::UnknownEventShape,
                    "a response field was present but was not a string",
                ));
            };
            if !text.is_empty() {
                self.lifecycle.note_visible_output();
                events.push(CanonicalStreamEvent::TextDelta {
                    text: text.to_string(),
                });
            }
        } else if object.get("done").and_then(Value::as_bool) != Some(true) {
            return Err(decode_error(
                StreamDecodeErrorKind::UnknownEventShape,
                "a line carried neither content, tool calls, an error, nor the terminal done flag",
            ));
        }

        let usage = parse_usage(&value);
        if usage != UsageObservationV1::unknown() {
            events.push(CanonicalStreamEvent::Usage { usage });
        }

        if object.get("done").and_then(Value::as_bool) == Some(true) {
            if !self.lifecycle.saw_visible_output() {
                return Err(decode_error(
                    StreamDecodeErrorKind::IllegalSequence,
                    "the stream terminated without assistant content or a tool call",
                ));
            }
            let completion = completion_kind(
                object.get("done_reason").and_then(Value::as_str),
                object
                    .get("message")
                    .and_then(Value::as_object)
                    .and_then(|message| message.get("tool_calls"))
                    .is_some_and(|calls| !calls.is_null()),
            );
            for index in self.tools.close_all()? {
                events.push(CanonicalStreamEvent::ToolCallCompleted { index });
            }
            events.push(CanonicalStreamEvent::Completed { completion });
            self.lifecycle.end_generation();
            self.lifecycle
                .seal(InvocationDispositionV1::Completed { completion });
        }

        Ok(events)
    }
}

impl WireStreamDecoder for OllamaNdjsonStreamDecoder {
    fn push_bytes(&mut self, chunk: &[u8]) -> Result<Vec<CanonicalStreamEvent>, StreamDecodeError> {
        if let Some(stashed) = self.stashed.clone() {
            return Err(stashed);
        }
        if !chunk.is_empty() {
            self.lifecycle.note_bytes();
        }
        let mut events = Vec::new();
        for byte in chunk {
            let after_cr = std::mem::take(&mut self.after_cr);
            match *byte {
                b'\n' if after_cr => continue,
                b'\n' | b'\r' => {
                    self.after_cr = *byte == b'\r';
                    if let Some(line) = self.take_line()? {
                        match self.parse_line(&line) {
                            Ok(mut produced) => events.append(&mut produced),
                            Err(error) => {
                                self.stash(error);
                                return Ok(events);
                            }
                        }
                    }
                }
                byte => {
                    if self.line.len() >= MAX_FRAME_BYTES {
                        self.stash(decode_error(
                            StreamDecodeErrorKind::EventTooLarge,
                            "one NDJSON line exceeded the decoder's buffer ceiling",
                        ));
                        return Ok(events);
                    }
                    self.line.push(byte);
                }
            }
        }
        Ok(events)
    }

    fn finish(&mut self, eof: StreamEof) -> Result<Vec<CanonicalStreamEvent>, StreamDecodeError> {
        if let Some(stashed) = self.stashed.clone() {
            return Err(stashed);
        }
        let mut events = Vec::new();
        match eof {
            StreamEof::Clean => {
                if let Some(line) = self.take_line()? {
                    match self.parse_line(&line) {
                        Ok(mut produced) => events.append(&mut produced),
                        Err(error) => {
                            self.stash(error.clone());
                            return Err(error);
                        }
                    }
                }
                if self.lifecycle.is_terminal() {
                    Ok(events)
                } else {
                    let error = decode_error(
                        StreamDecodeErrorKind::UnterminatedStream,
                        "the body ended cleanly without an Ollama done=true record",
                    );
                    self.stash(error.clone());
                    Err(error)
                }
            }
            StreamEof::Truncated => {
                self.line.clear();
                events.append(&mut self.lifecycle.on_observation_lost());
                Ok(events)
            }
        }
    }

    fn on_transport_error(&mut self, _error: TransportErrorKind) -> Vec<CanonicalStreamEvent> {
        if self.stashed.is_some() {
            return self.announce_stash();
        }
        self.lifecycle.on_observation_lost()
    }

    fn on_cancel(&mut self) -> Vec<CanonicalStreamEvent> {
        if self.stashed.is_some() {
            return self.announce_stash();
        }
        self.lifecycle.on_cancel()
    }

    fn terminal_disposition(&self) -> Option<InvocationDispositionV1> {
        self.lifecycle.terminal()
    }
}

fn parse_usage(value: &Value) -> UsageObservationV1 {
    let token = |key: &str| value.get(key).and_then(Value::as_u64);
    UsageObservationV1::provider_authoritative(
        token("prompt_eval_count"),
        token("eval_count"),
        token("total_tokens"),
    )
}

fn completion_kind(done_reason: Option<&str>, saw_tool_calls: bool) -> CompletionKindV1 {
    match done_reason {
        Some("stop") => {
            if saw_tool_calls {
                CompletionKindV1::ToolCalls
            } else {
                CompletionKindV1::Complete
            }
        }
        Some("length") => CompletionKindV1::Truncated,
        Some("tool_calls") => CompletionKindV1::ToolCalls,
        Some("content_filter") => CompletionKindV1::ContentFiltered,
        _ if saw_tool_calls => CompletionKindV1::ToolCalls,
        _ => CompletionKindV1::Unknown,
    }
}
