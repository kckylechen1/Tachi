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
//! The frame ceiling is the obvious one and the corpus pins it. The ones a
//! reviewer should insist on are those a per-frame bound does not cover: a
//! thousand well-formed frames each carrying a kilobyte of tool-call arguments
//! are individually legal and jointly unbounded, and an index is
//! provider-chosen, so a stream can open as many accumulators as it likes with
//! every single frame inside every single limit.
//!
//! That question has to be asked of **each grammar's own bookkeeping**, not
//! just of the shared driver's. The tool-call ceiling says nothing about
//! Anthropic's content blocks, which a text-only stream opens and closes
//! without ever touching a tool call — so the two decoders are exercised here,
//! not one.

use super::super::sse::{SseFramer, MAX_FRAME_BYTES};
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

/// One named Anthropic event frame.
fn anthropic_frame(name: &str, payload: &Value) -> Vec<u8> {
    format!("event: {name}\ndata: {payload}\n\n").into_bytes()
}

/// One complete Anthropic text block: opened, then closed.
fn anthropic_text_block(index: u32) -> Vec<u8> {
    let mut bytes = anthropic_frame(
        "content_block_start",
        &json!({
            "type": "content_block_start",
            "index": index,
            "content_block": {"type": "text", "text": ""},
        }),
    );
    bytes.extend(anthropic_frame(
        "content_block_stop",
        &json!({"type": "content_block_stop", "index": index}),
    ));
    bytes
}

#[test]
fn one_message_cannot_open_unbounded_content_blocks() {
    // The Anthropic grammar's own accumulator, which no ceiling in the shared
    // driver covers: these blocks are text, so not one tool call is opened and
    // the tool ceiling never fires. Every frame is tiny and legal, every block
    // is closed politely, and the list they grow is scanned on every lookup —
    // so without a ceiling this is unbounded retained memory *and* quadratic
    // work, both chosen by the provider.
    let mut decoder: Box<dyn WireStreamDecoder> = Box::new(AnthropicEventStreamDecoder::new());
    decoder
        .push_bytes(&anthropic_frame(
            "message_start",
            &json!({
                "type": "message_start",
                "message": {"id": "msg_1", "type": "message", "role": "assistant"},
            }),
        ))
        .expect("the message opens legally");

    let failure = push_until_failure(decoder.as_mut(), (0..4096u32).map(anthropic_text_block));
    assert_eq!(
        failure,
        Some(StreamDecodeErrorKind::EventTooLarge),
        "one message must not be able to open unbounded content blocks"
    );
    assert_eq!(
        decoder
            .terminal_disposition()
            .map(|disposition| serde_json::to_value(disposition).expect("serializes")),
        Some(decode_fault("event_too_large"))
    );
}

/// A frame carrying a raw (not necessarily JSON) `data:` payload.
fn raw_data_frame(name: &str, data: &str) -> Vec<u8> {
    format!("event: {name}\ndata: {data}\n\n").into_bytes()
}

/// A well-formed `error` event, sufficient to seal the stream's terminal
/// disposition without opening or closing any content block.
fn anthropic_error() -> Vec<u8> {
    anthropic_frame(
        "error",
        &json!({"type": "error", "error": {"type": "overloaded_error", "message": "x"}}),
    )
}

/// A well-formed `message_start`, opening the message before the `error`.
fn anthropic_message_start() -> Vec<u8> {
    anthropic_frame(
        "message_start",
        &json!({
            "type": "message_start",
            "message": {"id": "msg_1", "type": "message", "role": "assistant"},
        }),
    )
}

#[test]
fn a_malformed_terminator_after_error_is_a_typed_violation_not_a_free_pass() {
    // #1682/CP2 regression: once an `error` event has already sealed the
    // terminal disposition, this grammar reads and drops a trailing
    // `message_stop` rather than rejecting it as garbage — providers
    // routinely close politely with one right after `error`. Before this
    // fix, that exception checked only the SSE `event:` *name*: any frame
    // merely named `message_stop` was waved through unread, data and all, so
    // a truncated proxy tail or a corrupted closing frame decoded as a clean,
    // silent completion instead of surfacing the corruption. Each malformed
    // variant below must fail exactly the way the *same* data/JSON/type
    // checks fail for an ordinary, non-terminal frame — and a genuine
    // terminator must still be accepted and dropped, not turned into
    // `IllegalSequence` trailing garbage.

    // No `data:` field at all. The fault surfaces on the *next* call — see
    // the module note on why a failure detected mid-chunk is stashed rather
    // than returned from the push that found it.
    let mut decoder = AnthropicEventStreamDecoder::new();
    decoder
        .push_bytes(&anthropic_message_start())
        .expect("the message opens legally");
    decoder
        .push_bytes(&anthropic_error())
        .expect("the error seals the terminal disposition");
    assert!(decoder
        .push_bytes(b"event: message_stop\n\n")
        .expect("a stashed failure is not returned from the push that found it")
        .is_empty());
    assert_eq!(
        decoder
            .push_bytes(&[])
            .expect_err("a data-free terminator must not be waved through")
            .kind,
        StreamDecodeErrorKind::UnknownEventShape,
    );

    // A `data:` field that is not valid JSON at all.
    let mut decoder = AnthropicEventStreamDecoder::new();
    decoder
        .push_bytes(&anthropic_message_start())
        .expect("the message opens legally");
    decoder
        .push_bytes(&anthropic_error())
        .expect("the error seals the terminal disposition");
    assert!(decoder
        .push_bytes(&raw_data_frame("message_stop", "not json at all"))
        .expect("a stashed failure is not returned from the push that found it")
        .is_empty());
    assert_eq!(
        decoder
            .push_bytes(&[])
            .expect_err("an unparseable terminator payload must not be waved through")
            .kind,
        StreamDecodeErrorKind::MalformedFrame,
    );

    // A `data:` field that parses as JSON, but whose own `type` disagrees
    // with the SSE event name — the same proxy-rewrite case the ordinary
    // frame path already guards against.
    let mut decoder = AnthropicEventStreamDecoder::new();
    decoder
        .push_bytes(&anthropic_message_start())
        .expect("the message opens legally");
    decoder
        .push_bytes(&anthropic_error())
        .expect("the error seals the terminal disposition");
    assert!(decoder
        .push_bytes(&anthropic_frame("message_stop", &json!({"type": "ping"})))
        .expect("a stashed failure is not returned from the push that found it")
        .is_empty());
    assert_eq!(
        decoder
            .push_bytes(&[])
            .expect_err(
                "a terminator whose payload disagrees with its own name must not be \
                 waved through"
            )
            .kind,
        StreamDecodeErrorKind::UnknownEventShape,
    );

    // Control: a genuine, well-formed terminator after `error` is still read
    // and dropped, exactly as before this fix.
    let mut decoder = AnthropicEventStreamDecoder::new();
    decoder
        .push_bytes(&anthropic_message_start())
        .expect("the message opens legally");
    decoder
        .push_bytes(&anthropic_error())
        .expect("the error seals the terminal disposition");
    assert_eq!(
        decoder
            .push_bytes(&anthropic_frame(
                "message_stop",
                &json!({"type": "message_stop"}),
            ))
            .expect("a well-formed message_stop after error is still read politely"),
        Vec::new(),
        "a genuine terminator after the disposition is already sealed answers nothing new"
    );
}

#[test]
fn the_frame_ceiling_counts_every_field_of_the_frame() {
    // The ceiling names the *frame*, and a frame is more than its data. Two
    // retained fields, each comfortably inside every per-line check, that
    // together pass the ceiling: a decoder charging only `data:` accepts this
    // and holds roughly twice what the constant says it will. The corpus
    // cannot state it — fixture 21 is one oversized *line*, which the per-line
    // check catches on its own and which therefore proves nothing about
    // whether the fields are counted together.
    let half = MAX_FRAME_BYTES / 2 + 1024;
    let mut bytes = b"event: ".to_vec();
    bytes.extend(vec![b'e'; half]);
    bytes.extend(b"\ndata: ");
    bytes.extend(vec![b'd'; half]);
    bytes.extend(b"\n\n");
    assert!(
        half < MAX_FRAME_BYTES,
        "neither line may reach the ceiling on its own, or this test passes for the wrong reason"
    );

    let mut decoder = decoder();
    let failure = push_until_failure(decoder.as_mut(), std::iter::once(bytes));
    assert_eq!(
        failure,
        Some(StreamDecodeErrorKind::EventTooLarge),
        "one frame's retained fields must be bounded together, not one at a time"
    );
    assert_eq!(
        decoder
            .terminal_disposition()
            .map(|disposition| serde_json::to_value(disposition).expect("serializes")),
        Some(decode_fault("event_too_large"))
    );
}

#[test]
fn a_multiline_data_frame_landing_exactly_on_the_ceiling_dispatches() {
    // The regression this pins: a `data:` field is charged its own bytes for
    // its first line, and `bytes + 1` for every line after — the `+ 1` is the
    // `\n` [`SseFramer::consume_line`] actually inserts when it joins a
    // second-or-later line onto the frame's stored `data` string. Charging
    // that `+ 1` on the *first* line too (as if a joiner were about to be
    // written before any bytes exist) bills the ceiling for a byte the frame
    // never retains, so a multi-line payload sized to land exactly on
    // `MAX_FRAME_BYTES` was rejected one byte early.
    //
    // Split roughly in half across two `data:` lines rather than one short
    // line plus one line near `MAX_FRAME_BYTES` on its own: [`SseFramer::push`]
    // also bounds the raw, unterminated line buffer by the same constant, and
    // a near-ceiling single line would trip *that* check instead of the frame
    // charge this fixture exists to pin — proving nothing about the miscount.
    let half = MAX_FRAME_BYTES / 2;
    let first_line = vec![b'a'; half];
    // `+ (half + 1) == MAX_FRAME_BYTES` exactly: first line costs `half`,
    // second line costs `second.len() + 1` for the joining `\n`.
    let second_line = vec![b'b'; MAX_FRAME_BYTES - half - 1];
    let mut bytes = b"data: ".to_vec();
    bytes.extend(&first_line);
    bytes.extend(b"\ndata: ");
    bytes.extend(&second_line);
    bytes.extend(b"\n\n");

    let mut framer = SseFramer::new();
    let (frames, error) = framer.push(&bytes);
    assert_eq!(
        error, None,
        "a multi-line data payload landing exactly on the ceiling must dispatch, \
         not fault"
    );
    assert_eq!(
        frames.len(),
        1,
        "the blank line must close exactly one frame"
    );
    let data = frames[0]
        .data
        .as_ref()
        .expect("the dispatched frame carried the joined data");
    assert_eq!(
        data.len(),
        MAX_FRAME_BYTES,
        "the joined payload (both lines plus the newline joining them) sits exactly \
         at the ceiling"
    );
}

#[test]
fn a_multiline_data_frame_one_byte_over_the_ceiling_is_rejected() {
    // The other side of the same fixture: one more byte on the second line —
    // still a legal split across two lines, still nowhere near the per-line
    // ceiling — and the frame ceiling must still catch it. A fix that merely
    // stopped over-charging (and now under-charges, or charges by luck) would
    // pass the first test and fail this one.
    let half = MAX_FRAME_BYTES / 2;
    let first_line = vec![b'a'; half];
    let second_line = vec![b'b'; MAX_FRAME_BYTES - half];
    let mut bytes = b"data: ".to_vec();
    bytes.extend(&first_line);
    bytes.extend(b"\ndata: ");
    bytes.extend(&second_line);
    bytes.extend(b"\n\n");

    let mut framer = SseFramer::new();
    let (_frames, error) = framer.push(&bytes);
    assert_eq!(
        error.map(|failure| failure.kind),
        Some(StreamDecodeErrorKind::EventTooLarge),
        "one byte over the ceiling, spread across a second data line, must still fault"
    );
}

#[test]
fn a_stashed_failure_is_announced_once_and_then_stays_put() {
    // The window fixtures 29 and 30 pin, carried past where a transcript can
    // follow: a transcript ends at its terminal input, and the question here is
    // what the *next* calls say. A decoder that announced the stash again would
    // report one fault twice; one that cleared the `Err` on announcing it would
    // make the failure visible or invisible depending on which call the caller
    // happened to make first, which is the same bug as depending on where the
    // network split the bytes.
    let mut decoder = decoder();
    let events = decoder
        .push_bytes(b"data: {\"choices\":\"nope\"}\n\n")
        .expect("a failure detected mid-chunk is stashed, not returned");
    assert!(events.is_empty(), "the failing frame produced no events");

    assert_eq!(
        serde_json::to_value(decoder.on_cancel()).expect("serializes"),
        json!([{"event": "failed", "disposition": decode_fault("unknown_event_shape")}]),
        "an infallible terminal input has no Err to return, so it must announce \
         the stash on the event channel"
    );
    assert!(decoder.on_cancel().is_empty(), "one fault, announced once");
    assert!(
        decoder
            .on_transport_error(TransportErrorKind::ConnectionReset)
            .is_empty(),
        "and not announced again by the other terminal input either"
    );
    assert_eq!(
        decoder
            .push_bytes(&[])
            .expect_err("the failure is still in hand for a caller that pushes")
            .kind,
        StreamDecodeErrorKind::UnknownEventShape
    );
    assert_eq!(
        decoder
            .finish(StreamEof::Clean)
            .expect_err("and for one that finishes")
            .kind,
        StreamDecodeErrorKind::UnknownEventShape
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
