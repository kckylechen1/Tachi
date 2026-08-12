//! The two decoder properties a transcript fixture cannot state: what is
//! bounded, and what a terminal decoder does when it is pushed again.
//!
//! # Why these are not fixtures
//!
//! A ceiling fixture would have to carry the bytes that reach the ceiling —
//! megabytes of one character checked into the repository — and a terminality
//! fixture would have to describe calls made *after* the terminal input, which
//! the transcript format deliberately has no room for (a transcript ends). Both
//! are built here instead, in code, from the same public trait the executor
//! will use.
//!
//! # The accumulators that are not the frame
//!
//! The frame ceiling is the obvious one and the corpus pins it. The two that a
//! reviewer should insist on are the ones a per-frame bound does not cover: a
//! thousand well-formed frames each carrying a kilobyte of tool-call arguments
//! are individually legal and jointly unbounded, and an index is
//! provider-chosen, so a stream can open as many accumulators as it likes with
//! every single frame inside every single limit.

use super::*;

/// One SSE data frame carrying a JSON payload.
fn frame(payload: &Value) -> Vec<u8> {
    format!("data: {payload}\n\n").into_bytes()
}

/// A fresh decoder, obtained the way the executor will.
fn decoder() -> Box<dyn WireStreamDecoder> {
    OpenAiCompatWire::new()
        .new_stream_decoder()
        .expect("the dialect streams")
}

/// One `tool_calls` delta frame.
fn tool_frame(index: u32, id: Option<&str>, name: Option<&str>, arguments: &str) -> Vec<u8> {
    let mut call = serde_json::Map::new();
    call.insert("index".to_string(), json!(index));
    if let Some(id) = id {
        call.insert("id".to_string(), json!(id));
    }
    let mut function = serde_json::Map::new();
    if let Some(name) = name {
        function.insert("name".to_string(), json!(name));
    }
    function.insert("arguments".to_string(), json!(arguments));
    call.insert("function".to_string(), Value::Object(function));
    frame(&json!({
        "choices": [{"index": 0, "delta": {"tool_calls": [Value::Object(call)]}}]
    }))
}

/// The disposition a decode failure of `rule` produces.
fn decode_fault(rule: &str) -> Value {
    json!({
        "disposition": "protocol_error",
        "violation": {"kind": "stream_decode", "rule": rule},
    })
}

/// Drives a decoder until it reports a failure, or gives up.
///
/// A failure detected behind delivered events is reported on the *next* call,
/// so "push until it errors" is the honest way to ask, not "check the push that
/// crossed the line".
fn push_until_failure(
    decoder: &mut dyn WireStreamDecoder,
    frames: impl Iterator<Item = Vec<u8>>,
) -> Option<StreamDecodeErrorKind> {
    for frame in frames {
        if let Err(failure) = decoder.push_bytes(&frame) {
            return Some(failure.kind);
        }
    }
    decoder.push_bytes(&[]).err().map(|failure| failure.kind)
}

#[test]
fn tool_call_arguments_are_bounded_across_events() {
    // Every frame here is well under the frame ceiling; only their sum is over
    // the argument ceiling. A decoder that bounded the frame and not the
    // accumulator would pass the corpus and still let a hostile provider grow
    // one string without limit.
    let mut decoder = decoder();
    decoder
        .push_bytes(&tool_frame(0, Some("call_1"), Some("search"), "{"))
        .expect("the opening fragment is legal");

    let blob = "a".repeat(64 * 1024);
    let failure = push_until_failure(
        decoder.as_mut(),
        (0..32).map(|_| tool_frame(0, None, None, &blob)),
    );
    assert_eq!(
        failure,
        Some(StreamDecodeErrorKind::EventTooLarge),
        "accumulated tool-call arguments must hit a ceiling"
    );
    assert_eq!(
        decoder
            .terminal_disposition()
            .map(|disposition| serde_json::to_value(disposition).expect("serializes")),
        Some(decode_fault("event_too_large"))
    );
}

#[test]
fn one_turn_cannot_open_unbounded_tool_calls() {
    // Each call is tiny and legal; there are simply too many of them. The index
    // is the provider's to choose, so without a ceiling this is an unbounded
    // number of accumulators.
    let mut decoder = decoder();
    let failure = push_until_failure(
        decoder.as_mut(),
        (0..512u32).map(|index| tool_frame(index, Some("call"), Some("search"), "{}")),
    );
    assert_eq!(
        failure,
        Some(StreamDecodeErrorKind::EventTooLarge),
        "a turn must not be able to open unbounded tool calls"
    );
}

/// A complete, clean stream, as bytes.
fn clean_stream() -> Vec<u8> {
    let mut bytes = frame(&json!({
        "id": "chatcmpl-1",
        "model": "test-model-1",
        "choices": [{"index": 0, "delta": {"content": "Hi"}, "finish_reason": null}]
    }));
    bytes.extend(frame(&json!({
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
    })));
    bytes.extend(b"data: [DONE]\n\n");
    bytes
}

#[test]
fn a_terminal_decoder_answers_stably_whatever_it_is_asked_next() {
    // Terminality, stated as the executor will meet it: whichever terminal
    // input arrives after the answer — a second finish, a cancel that lost the
    // race, a connection that dropped while nobody was reading — the answer
    // does not move and no further event appears. A decoder that re-answered
    // here would let a late cancel turn a completed, already-billed invocation
    // into a cancellation.
    let completed = json!({"disposition": "completed", "completion": "complete"});

    let mut decoder = decoder();
    let events = decoder.push_bytes(&clean_stream()).expect("decodes");
    assert_eq!(events.len(), 3, "started, one text delta, completed");
    assert_eq!(
        serde_json::to_value(decoder.terminal_disposition()).expect("serializes"),
        completed
    );

    assert!(decoder.finish(StreamEof::Clean).expect("stable").is_empty());
    assert!(decoder
        .finish(StreamEof::Truncated)
        .expect("a truncation after the answer is not a lost outcome")
        .is_empty());
    assert!(decoder.on_cancel().is_empty());
    assert!(decoder
        .on_transport_error(TransportErrorKind::ConnectionReset)
        .is_empty());
    assert!(decoder.push_bytes(&[]).expect("stable").is_empty());
    assert_eq!(
        serde_json::to_value(decoder.terminal_disposition()).expect("serializes"),
        completed,
        "a terminal disposition must not move"
    );
}

#[test]
fn a_truncated_body_after_the_terminator_is_still_a_completion() {
    // The discrimination that makes `StreamEof` two-valued worth having, in the
    // direction that is easy to get wrong: the bytes stopped early, but they
    // stopped *after* the grammar's terminator, so nothing was lost. Reporting
    // `outcome_unknown` here would send a completed invocation down the
    // retry-on-caller-opt-in path.
    let mut decoder = decoder();
    decoder.push_bytes(&clean_stream()).expect("decodes");
    assert!(decoder
        .finish(StreamEof::Truncated)
        .expect("stable")
        .is_empty());
    assert_eq!(
        serde_json::to_value(decoder.terminal_disposition()).expect("serializes"),
        json!({"disposition": "completed", "completion": "complete"})
    );
}

#[test]
fn a_stream_that_never_delivered_a_byte_says_so() {
    // The phase is not decoration: `awaiting_response` means the provider was
    // working and nothing came back, `receiving` means output had started. They
    // carry the same retry posture today and they are different facts, and the
    // one place that can tell them apart is the decoder that either did or did
    // not see a byte.
    let mut silent = decoder();
    let events = silent.on_transport_error(TransportErrorKind::ReadTimeout);
    assert_eq!(
        serde_json::to_value(&events).expect("serializes"),
        json!([{
            "event": "failed",
            "disposition": {"disposition": "outcome_unknown", "phase": "awaiting_response"},
        }])
    );

    let mut cancelled = decoder();
    assert_eq!(
        serde_json::to_value(cancelled.on_cancel()).expect("serializes"),
        json!([{
            "event": "failed",
            "disposition": {"disposition": "cancelled_outcome_unknown"},
        }]),
        "nothing visible was emitted, so the weaker cancellation terminal applies"
    );

    let mut empty_body = decoder();
    assert_eq!(
        empty_body
            .finish(StreamEof::Clean)
            .expect_err("an empty body is not a completion")
            .kind,
        StreamDecodeErrorKind::UnterminatedStream
    );
}
