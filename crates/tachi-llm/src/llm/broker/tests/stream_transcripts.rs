//! Recorded provider streams in, canonical events out.
//!
//! # What a transcript fixture is
//!
//! Raw body bytes exactly as a provider sent them — chunked the way the network
//! chunked them — plus the terminal input that ended the stream, plus the
//! canonical event sequence, decode failure and terminal disposition that must
//! come back. Under sans-IO that is the *whole* decoder: it is a state machine
//! over a byte sequence, so a corpus of byte sequences with expected outputs is
//! a complete test of it, with no network and no fake to keep in sync.
//!
//! # The corpus is per grammar, not per provider
//!
//! Six providers, three grammars. OpenAI/xAI/OpenRouter/generic-compat share
//! the `delta` grammar; Anthropic's typed `content_block_delta` events are a
//! second; an Ollama-shaped deployment's NDJSON is a third. Loading every
//! grammar through *this one harness* is the test of whether the canonical
//! vocabulary is OpenAI-biased: if it were, the second grammar would need its
//! own comparison shape, and it does not.
//!
//! # Cross-grammar fixtures are the discrimination that matters
//!
//! Every grammar's corpus carries at least one transcript from a *different*
//! grammar, which must come back as a typed refusal. A decoder that half-reads
//! a stream it does not understand produces plausible nonsense, and the caller
//! has no way to tell that from a real answer — which is precisely why "NDJSON
//! fed to an SSE decoder" is a fixture and not a comment.
//!
//! # Why the harness ends with an empty push
//!
//! A decoder delivers the events that legitimately preceded a failure *before*
//! the failure — it has to, or the observable sequence would depend on where
//! the network split the bytes — so a failure detected in the last chunk is
//! still in hand when the chunks run out. The drain is what makes the recorded
//! transcript complete under every chunking, and it doubles as an assertion
//! that a terminal decoder answers stably rather than panicking when pushed
//! again.
//!
//! # Why the terminal input is delivered even after a push has failed
//!
//! Which channel a stashed failure leaves on is part of the decoder's answer,
//! and the corpus pins both halves of it: a stash closed by `finish` rides out
//! as the `Err` and produces no event (fixtures 15, 22, 26 and a dozen more),
//! while a stash closed by an *infallible* terminal input — a cancel, a dead
//! transport — is announced as a `failed` event, because there is no `Err` for
//! it to ride (fixtures 29 and 30). The canonical event sequence is therefore a
//! function of the bytes **and the terminal input together**, never of the
//! bytes alone.
//!
//! So the terminal input is delivered on every run, including the runs where a
//! `push_bytes` already answered `Err`. Whether such a push exists is pure
//! chunking: the failure is stashed at the byte that completes the broken
//! frame, and whether one more read follows that byte is the network's
//! business — `split_at` manufactures one from a cut on the last byte, and a
//! cut on the end manufactures a zero-length one. A harness that stopped at the
//! first `Err` would drop the transcript's terminal input on exactly those
//! chunkings, then compare a run of *(bytes, cancel)* against a run of
//! *(bytes)* and report the missing `failed` event as a decoder that lost a
//! failure. The first failure still wins, because that is the one the decoder
//! sealed.

use super::*;

/// The `delta`-grammar corpus.
const OPENAI_SSE_DIR: &str = "openai_compat_stream";

/// The Anthropic event-grammar corpus.
const ANTHROPIC_SSE_DIR: &str = "anthropic_stream";

/// Every corpus directory, with the grammar each one speaks.
pub(super) const STREAM_CORPORA: &[(&str, &str)] = &[
    (OPENAI_SSE_DIR, "openai_compat_sse"),
    (ANTHROPIC_SSE_DIR, "anthropic_sse"),
];

/// What a fixture does once its chunks are exhausted.
#[derive(Debug, Clone, Copy)]
pub(super) enum TerminalInput {
    /// The byte source ended, cleanly or short.
    Finish(StreamEof),
    /// The transport failed mid-stream.
    Transport(TransportErrorKind),
    /// The caller cancelled.
    Cancel,
    /// Nothing — the fixture is about what the bytes alone produce.
    None,
}

/// One decoded run of a transcript.
#[derive(Debug, PartialEq)]
pub(super) struct DecodedTranscript {
    /// Every canonical event, in order, serialized.
    pub(super) events: Vec<Value>,
    /// The decode failure's frozen spelling, when there was one.
    pub(super) error: Option<String>,
    /// The terminal disposition, serialized.
    pub(super) disposition: Option<Value>,
}

/// A fresh decoder for a grammar named by a fixture.
///
/// Built through [`ProviderWire::new_stream_decoder`] rather than by naming the
/// concrete type: that is the path the executor will take, so a dialect that
/// stopped handing back a decoder would fail the whole corpus here rather than
/// pass it and fail in production.
pub(super) fn decoder_for(grammar: &str) -> Box<dyn WireStreamDecoder> {
    match grammar {
        "openai_compat_sse" => OpenAiCompatWire::new()
            .new_stream_decoder()
            .expect("the OpenAI-compat dialect streams and must hand back a decoder"),
        // Constructed directly, because there is no Anthropic `ProviderWire`
        // yet: slice-2 builds this grammar to answer whether the canonical
        // vocabulary is OpenAI-shaped, not to ship a second dialect. The
        // request/response/classification halves are a later leaf, and saying
        // so here is cheaper than letting a reader infer a dialect that is not
        // there.
        "anthropic_sse" => Box::new(AnthropicEventStreamDecoder::new()),
        other => panic!("fixture names a grammar this harness does not know: {other}"),
    }
}

/// Runs one transcript to its terminal state.
pub(super) fn decode_transcript(
    grammar: &str,
    chunks: &[Vec<u8>],
    terminal: TerminalInput,
) -> DecodedTranscript {
    let mut decoder = decoder_for(grammar);
    let mut events = Vec::new();
    let mut error: Option<StreamDecodeErrorKind> = None;

    for chunk in chunks {
        match decoder.push_bytes(chunk) {
            Ok(produced) => events.extend(produced.iter().map(serialize_event)),
            Err(failure) => {
                error = Some(failure.kind);
                break;
            }
        }
    }

    // Unconditional. See the module note: the stream ended the way the fixture
    // records, and a `push_bytes` that answered `Err` first is a fact about the
    // chunking, not about how the stream ended.
    match terminal {
        TerminalInput::Finish(eof) => match decoder.finish(eof) {
            Ok(produced) => events.extend(produced.iter().map(serialize_event)),
            Err(failure) => error = error.or(Some(failure.kind)),
        },
        TerminalInput::Transport(kind) => {
            let produced = decoder.on_transport_error(kind);
            events.extend(produced.iter().map(serialize_event));
        }
        TerminalInput::Cancel => {
            let produced = decoder.on_cancel();
            events.extend(produced.iter().map(serialize_event));
        }
        TerminalInput::None => {}
    }

    // The drain. See the module note: a failure detected behind delivered
    // events is reported on the next call, and this is that call.
    if error.is_none() {
        match decoder.push_bytes(&[]) {
            Ok(produced) => assert!(
                produced.is_empty(),
                "a decoder produced events from an empty push after its terminal input"
            ),
            Err(failure) => error = Some(failure.kind),
        }
    }

    DecodedTranscript {
        events,
        error: error.map(|kind| kind.as_str().to_string()),
        disposition: decoder
            .terminal_disposition()
            .map(|disposition| serde_json::to_value(disposition).expect("serializes")),
    }
}

/// Serializes one canonical event for comparison against a golden.
fn serialize_event(event: &CanonicalStreamEvent) -> Value {
    serde_json::to_value(event).expect("a canonical event serializes")
}

/// The byte chunks a fixture records.
///
/// Three spellings, because a transcript has to be able to hold bytes that are
/// not text: `text` for the readable common case, `hex` for a byte sequence
/// that is deliberately not UTF-8, and a `repeat` count so a fixture about a
/// buffer ceiling does not put a megabyte of one character in the repository.
pub(super) fn transcript_chunks(fixture: &Value, name: &str) -> Vec<Vec<u8>> {
    let entries = fixture
        .get("chunks")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("fixture {name} has no chunks array"));
    assert!(
        !entries.is_empty(),
        "fixture {name} records no bytes at all"
    );
    entries
        .iter()
        .map(|entry| {
            let repeat = entry
                .get("repeat")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .try_into()
                .unwrap_or_else(|_| panic!("fixture {name}: repeat count is absurd"));
            let unit = if let Some(text) = entry.get("text").and_then(Value::as_str) {
                text.as_bytes().to_vec()
            } else if let Some(hex) = entry.get("hex").and_then(Value::as_str) {
                decode_hex(hex, name)
            } else {
                panic!("fixture {name}: a chunk carries neither text nor hex")
            };
            unit.repeat(repeat)
        })
        .collect()
}

/// Decodes a fixture's `hex` chunk.
fn decode_hex(hex: &str, name: &str) -> Vec<u8> {
    let digits: Vec<char> = hex.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        digits.len().is_multiple_of(2),
        "fixture {name}: a hex chunk has an odd number of digits"
    );
    digits
        .chunks(2)
        .map(|pair| {
            let byte: String = pair.iter().collect();
            u8::from_str_radix(&byte, 16)
                .unwrap_or_else(|_| panic!("fixture {name}: {byte} is not a hex byte"))
        })
        .collect()
}

/// The terminal input a fixture ends with.
pub(super) fn transcript_terminal(fixture: &Value, name: &str) -> TerminalInput {
    match fixture_str(fixture, name, "/terminal_input/kind") {
        "finish" => {
            let raw = fixture_str(fixture, name, "/terminal_input/eof");
            TerminalInput::Finish(
                StreamEof::parse(raw)
                    .unwrap_or_else(|| panic!("fixture {name}: {raw} is not an eof spelling")),
            )
        }
        "transport_error" => {
            let raw = fixture_str(fixture, name, "/terminal_input/error");
            TerminalInput::Transport(TransportErrorKind::parse(raw).unwrap_or_else(|| {
                panic!("fixture {name}: {raw} is not a transport-error spelling")
            }))
        }
        "cancel" => TerminalInput::Cancel,
        "none" => TerminalInput::None,
        other => panic!("fixture {name}: {other} is not a terminal input"),
    }
}

/// Every transcript fixture, across every grammar's corpus.
fn every_transcript() -> Vec<(String, String, Value)> {
    let mut out = Vec::new();
    for (dir, grammar) in STREAM_CORPORA {
        for (name, fixture) in load_fixtures(dir) {
            let declared = fixture_str(&fixture, &name, "/grammar");
            assert_eq!(
                declared, *grammar,
                "fixture {name} sits in the {grammar} corpus but declares {declared}"
            );
            out.push((name, (*grammar).to_string(), fixture));
        }
    }
    out
}

#[test]
fn every_transcript_decodes_to_its_golden() {
    let fixtures = every_transcript();
    assert!(
        fixtures.len() >= 47,
        "the transcript corpus shrank to {} fixtures",
        fixtures.len()
    );
    assert!(
        STREAM_CORPORA.len() >= 2,
        "one grammar cannot answer whether the vocabulary is shaped around it"
    );

    for (name, grammar, fixture) in &fixtures {
        assert!(
            !fixture_str(fixture, name, "/why").trim().is_empty(),
            "fixture {name} has no rationale — a golden whose motivation is lost \
             gets 'updated' the first time it fails"
        );

        let decoded = decode_transcript(
            grammar,
            &transcript_chunks(fixture, name),
            transcript_terminal(fixture, name),
        );

        let expected_events = fixture_value(fixture, name, "/expected/events")
            .as_array()
            .unwrap_or_else(|| panic!("fixture {name}: expected.events is not an array"));
        assert_eq!(
            &decoded.events,
            expected_events,
            "fixture {name}: the canonical event sequence drifted\n  actual:   {}\n  expected: {}",
            serde_json::to_string_pretty(&decoded.events).unwrap_or_default(),
            serde_json::to_string_pretty(expected_events).unwrap_or_default(),
        );

        let expected_error = fixture_value(fixture, name, "/expected/error");
        match expected_error {
            Value::Null => assert_eq!(
                decoded.error, None,
                "fixture {name}: expected a clean decode, got {:?}",
                decoded.error
            ),
            Value::String(kind) => {
                assert!(
                    StreamDecodeErrorKind::parse(kind).is_some(),
                    "fixture {name}: {kind} is not a decode-error spelling"
                );
                assert_eq!(
                    decoded.error.as_deref(),
                    Some(kind.as_str()),
                    "fixture {name}: the decode failure drifted"
                );
            }
            other => panic!("fixture {name}: expected.error must be a spelling or null: {other}"),
        }

        let expected_disposition = fixture_value(fixture, name, "/expected/terminal_disposition");
        match (&decoded.disposition, expected_disposition) {
            (None, Value::Null) => {}
            (Some(actual), expected) => assert_eq!(
                actual, expected,
                "fixture {name}: the terminal disposition drifted"
            ),
            (None, expected) => panic!(
                "fixture {name}: the decoder reached no terminal disposition, expected {expected}"
            ),
        }
    }
}

#[test]
fn a_decoded_transcript_never_carries_provider_prose() {
    // The stream is untrusted input and its decoded form reaches logs, receipts
    // and dispositions. A fixture that records a provider's own error message
    // states here that none of it may survive decoding — which is what keeps
    // `detail: &'static str` from being quietly relaxed into a formatted
    // string carrying the body.
    let mut checked = 0;
    for (name, grammar, fixture) in every_transcript() {
        let Some(forbidden) = fixture.get("must_not_leak").and_then(Value::as_array) else {
            continue;
        };
        let chunks = transcript_chunks(&fixture, &name);
        let raw = String::from_utf8_lossy(&chunks.concat()).into_owned();
        let decoded = decode_transcript(&grammar, &chunks, transcript_terminal(&fixture, &name));
        let rendered = format!("{decoded:?}");
        for needle in forbidden {
            let needle = needle.as_str().expect("must_not_leak holds strings");
            assert!(
                !rendered.contains(needle),
                "fixture {name}: decoded output leaked provider prose {needle:?}"
            );
            // ...and the sanity half: the bytes really did carry it, so this is
            // not passing because the fixture forgot to include it.
            assert!(
                raw.contains(needle),
                "fixture {name}: {needle:?} is not in the recorded bytes, so \
                 asserting it does not leak proves nothing"
            );
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "no fixture declares must_not_leak, so this test asserts nothing"
    );
}

#[test]
fn the_corpus_reaches_every_decode_error_kind() {
    // A vocabulary member no fixture produces is a member nothing tests. This
    // is the coverage assertion the `StreamDecodeErrorKind` compile-time guard
    // cannot make: the guard catches a *new* variant, this catches an existing
    // one that the decoder has no path to.
    let reached: Vec<String> = every_transcript()
        .iter()
        .filter_map(|(name, _, fixture)| {
            fixture
                .pointer("/expected/error")
                .and_then(Value::as_str)
                .map(|kind| {
                    assert!(
                        StreamDecodeErrorKind::parse(kind).is_some(),
                        "fixture {name}: {kind} is not a decode-error spelling"
                    );
                    kind.to_string()
                })
        })
        .collect();
    for kind in StreamDecodeErrorKind::ALL.iter().copied() {
        assert!(
            reached.iter().any(|seen| seen == kind.as_str()),
            "no transcript fixture produces {kind:?}"
        );
    }
}

#[test]
fn the_corpus_reaches_every_canonical_stream_event() {
    // The same coverage question on the output side. `started` and `completed`
    // are easy; `usage`, `tool_call_completed` and `failed` are the ones a thin
    // corpus quietly never exercises.
    let mut reached: Vec<String> = Vec::new();
    for (name, _, fixture) in every_transcript() {
        for event in fixture_value(&fixture, &name, "/expected/events")
            .as_array()
            .unwrap_or_else(|| panic!("fixture {name}: expected.events is not an array"))
        {
            let tag = event
                .get("event")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("fixture {name}: an expected event has no tag"));
            assert!(
                CanonicalStreamEventKind::parse(tag).is_some(),
                "fixture {name}: {tag} is not an event spelling"
            );
            reached.push(tag.to_string());
        }
    }
    for kind in CanonicalStreamEventKind::ALL.iter().copied() {
        assert!(
            reached.iter().any(|seen| seen == kind.as_str()),
            "no transcript fixture produces {kind:?}"
        );
    }
}

/// One named fixture from a corpus.
fn transcript_named(dir: &str, file: &str) -> Value {
    load_fixtures(dir)
        .into_iter()
        .find(|(name, _)| name == file)
        .unwrap_or_else(|| panic!("fixture {file} is missing from {dir}"))
        .1
}

/// The tool-call story a decoded transcript tells, with everything
/// provider-specific projected away.
///
/// Ids are dropped on purpose: they are the provider's own opaque handles and
/// the two grammars will never agree on them. Everything else — the event
/// sequence, the function name, the reassembled argument document, how the turn
/// ended — is canonical, and must agree exactly.
fn tool_story(events: &[Value]) -> Value {
    let mut kinds: Vec<&str> = Vec::new();
    let mut name: Option<String> = None;
    let mut arguments = String::new();
    let mut completion: Option<String> = None;
    for event in events {
        match event.get("event").and_then(Value::as_str) {
            Some("tool_call_delta") => {
                kinds.push("tool_call_delta");
                assert_eq!(
                    event.pointer("/fragment/index").and_then(Value::as_u64),
                    Some(0),
                    "both fixtures describe the turn's first tool call"
                );
                if let Some(seen) = event.pointer("/fragment/name").and_then(Value::as_str) {
                    name = Some(seen.to_string());
                }
                if let Some(seen) = event
                    .pointer("/fragment/arguments_delta")
                    .and_then(Value::as_str)
                {
                    arguments.push_str(seen);
                }
            }
            Some("tool_call_completed") => kinds.push("tool_call_completed"),
            Some("completed") => {
                kinds.push("completed");
                completion = event
                    .get("completion")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            _ => {}
        }
    }
    json!({
        "kinds": kinds,
        "name": name,
        "arguments": arguments,
        "completion": completion,
    })
}

#[test]
fn two_grammars_describe_the_same_turn_in_the_same_vocabulary() {
    // The frozen design's actual question, asked directly. One turn — a model
    // calling `search` with `{"q":"cats"}` — arrives as OpenAI `delta` chunks
    // with identity and arguments packed into repeated deltas, and as Anthropic
    // content blocks that open, accumulate and close explicitly. Different
    // frames, different event names, different index numbering, different
    // terminators. If the canonical vocabulary were shaped around one of them,
    // the other's story would come out differently here.
    let openai = transcript_named(
        "openai_compat_stream",
        "07_tool_call_reassembled_across_events.json",
    );
    let anthropic = transcript_named("anthropic_stream", "02_tool_use_after_a_text_block.json");

    let openai = decode_transcript(
        "openai_compat_sse",
        &transcript_chunks(&openai, "openai tool call"),
        transcript_terminal(&openai, "openai tool call"),
    );
    let anthropic = decode_transcript(
        "anthropic_sse",
        &transcript_chunks(&anthropic, "anthropic tool call"),
        transcript_terminal(&anthropic, "anthropic tool call"),
    );
    assert_eq!(openai.error, None);
    assert_eq!(anthropic.error, None);

    let story = tool_story(&openai.events);
    assert_eq!(
        story,
        tool_story(&anthropic.events),
        "the two grammars tell different canonical stories about the same turn"
    );
    // ...and the story is not vacuous.
    assert_eq!(
        story,
        json!({
            "kinds": ["tool_call_delta", "tool_call_delta", "tool_call_delta",
                      "tool_call_completed", "completed"],
            "name": "search",
            "arguments": "{\"q\":\"cats\"}",
            "completion": "tool_calls",
        })
    );
    assert_eq!(
        openai.disposition, anthropic.disposition,
        "the same turn must reach the same terminal disposition in both grammars"
    );
}
