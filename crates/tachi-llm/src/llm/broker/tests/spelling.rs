//! Every frozen wire spelling, pinned three ways at once.
//!
//! A round-trip test is not enough, and the seam's `open_ai_compat` incident is
//! the proof: serde's `rename_all = "snake_case"` turned `OpenAiCompat` into
//! `open_ai_compat` while `as_str()` said `openai_compat`, and a round-trip
//! test stayed green the whole time because serde was consistent *with itself*.
//!
//! So each enum is checked against all three of:
//!
//! 1. what serde emits,
//! 2. what `as_str()` returns,
//! 3. a **literal** golden list written out below.
//!
//! Any one of the three drifting alone is a failure, and (3) means the golden
//! cannot be "updated" by re-running the code that produced the drift.

use super::*;

/// Asserts serde output == `as_str()` == the literal golden, for every
/// variant, plus `parse` round-trip and `ALL` coverage.
macro_rules! assert_spellings {
    ($ty:ty, $golden:expr) => {{
        let golden: &[&str] = $golden;
        let all = <$ty>::ALL;
        assert_eq!(
            all.len(),
            golden.len(),
            "{}: variant count changed — a new variant needs a golden spelling, \
             not a silently accepted one",
            stringify!($ty)
        );
        for (variant, expected) in all.iter().copied().zip(golden.iter().copied()) {
            assert_eq!(
                variant.as_str(),
                expected,
                "{}: as_str() drifted from the golden",
                stringify!($ty)
            );
            let serialized = serde_json::to_value(variant).expect("variant must serialize");
            assert_eq!(
                serialized,
                json!(expected),
                "{}: serde spelling drifted from the golden",
                stringify!($ty)
            );
            assert_eq!(
                <$ty>::parse(expected),
                Some(variant),
                "{}: parse() does not round-trip its own spelling",
                stringify!($ty)
            );
        }
        assert_eq!(<$ty>::parse("definitely-not-a-variant"), None);
    }};
}

// ---------------------------------------------------------------------------
// Compile-time exhaustiveness guards
// ---------------------------------------------------------------------------
//
// Never called — these exist only to be *compiled*. A `match` with no
// wildcard arm is exhaustive by construction, so the moment either enum
// gains a variant this file stops compiling, before any test runs and before
// anyone has to remember that `ALL`, a golden list, or a coverage assertion
// needs a matching update. The runtime checks elsewhere in this module catch
// the same drift, but only if someone runs the tests; this catches it at
// `cargo check`.

/// Forces a compile error the moment [`CanonicalStreamEventKind`] gains a
/// variant this file has not been taught the golden spelling for.
#[allow(dead_code)]
fn _all_stream_event_kinds_enumerated(kind: CanonicalStreamEventKind) {
    match kind {
        CanonicalStreamEventKind::Started => (),
        CanonicalStreamEventKind::TextDelta => (),
        CanonicalStreamEventKind::ToolCallDelta => (),
        CanonicalStreamEventKind::ToolCallCompleted => (),
        CanonicalStreamEventKind::Usage => (),
        CanonicalStreamEventKind::Completed => (),
        CanonicalStreamEventKind::Failed => (),
    }
}

/// The same guard for [`ProtocolViolationKind`], which had no compile-time
/// exhaustiveness check before this leaf — only the runtime `assert_spellings!`
/// coverage below.
#[allow(dead_code)]
fn _all_protocol_violation_kinds_enumerated(kind: ProtocolViolationKind) {
    match kind {
        ProtocolViolationKind::MalformedBody => (),
        ProtocolViolationKind::SchemaViolation => (),
        ProtocolViolationKind::EmptyAssistantContent => (),
        ProtocolViolationKind::StreamDecode => (),
    }
}

#[test]
fn canonical_vocabulary_spellings_are_frozen() {
    assert_spellings!(MessageRole, &["system", "user", "assistant", "tool"]);
    assert_spellings!(StreamSelection, &["disabled", "enabled"]);
}

#[test]
fn disposition_vocabulary_spellings_are_frozen() {
    assert_spellings!(
        UnsupportedCapability,
        &[
            "chat",
            "tools",
            "streaming",
            "structured_output",
            "json_schema",
            "media"
        ]
    );
    assert_spellings!(
        BeforeSendRefusalKind,
        &[
            "unresolved_target",
            "unsupported_capability",
            "auth_material_unsupported",
            "unrepresentable_request",
            "budget_exceeded"
        ]
    );
    assert_spellings!(
        ProtocolViolationKind,
        &[
            "malformed_body",
            "schema_violation",
            "empty_assistant_content",
            "stream_decode"
        ]
    );
    assert_spellings!(
        SendPhase,
        &["connecting", "sending", "awaiting_response", "receiving"]
    );
    assert_spellings!(
        CancellationEvidence,
        &[
            "provider_acknowledged",
            "terminal_event_aborted",
            "never_accepted"
        ]
    );
    assert_spellings!(
        CompletionKindV1,
        &[
            "complete",
            "truncated",
            "tool_calls",
            "content_filtered",
            "unknown"
        ]
    );
    assert_spellings!(
        RetryPosture,
        &["safe", "caller_opt_in", "forbidden", "not_applicable"]
    );
    assert_spellings!(
        InvocationDispositionKind,
        &[
            "refused_before_send",
            "provider_rejected",
            "outcome_unknown",
            "protocol_error",
            "cancelled_before_send",
            "cancelled_confirmed",
            "cancelled_outcome_unknown",
            "cancelled_after_partial",
            "completed"
        ]
    );
}

#[test]
fn usage_and_wire_vocabulary_spellings_are_frozen() {
    assert_spellings!(
        UsageProvenanceV1,
        &[
            "provider_authoritative",
            "catalog_calculated",
            "estimated",
            "unknown"
        ]
    );
    assert_spellings!(HttpMethod, &["POST", "GET"]);
    assert_spellings!(AuthMaterialKind, &["none", "api_key", "bearer_token"]);
    assert_spellings!(
        RetryAdvice,
        &[
            "retry_same_deployment",
            "retry_other_credential",
            "try_fallback_deployment",
            "do_not_retry"
        ]
    );
    assert_spellings!(
        ProviderErrorClass,
        &[
            "auth_invalid",
            "billing_or_quota",
            "rate_limited",
            "server_error",
            "bad_request",
            "protocol"
        ]
    );
}

#[test]
fn stream_vocabulary_spellings_are_frozen() {
    assert_spellings!(
        CanonicalStreamEventKind,
        &[
            "started",
            "text_delta",
            "tool_call_delta",
            "tool_call_completed",
            "usage",
            "completed",
            "failed"
        ]
    );
    assert_spellings!(
        StreamDecodeErrorKind,
        &[
            "invalid_utf8",
            "malformed_frame",
            "unknown_event_shape",
            "illegal_sequence",
            "unterminated_stream",
            "event_too_large"
        ]
    );
    assert_spellings!(StreamEof, &["clean", "truncated"]);
    assert_spellings!(
        TransportErrorKind,
        &["connection_reset", "read_timeout", "tls_failure", "other"]
    );
    assert_spellings!(
        StreamDecoderUnavailableReason,
        &["dialect_does_not_stream", "not_implemented_yet"]
    );
}

#[test]
fn payload_bearing_enum_tags_match_their_fieldless_kind() {
    // The rule the module doc states: an enum with payloads gets no `as_str()`,
    // it gets a `kind()`. This asserts the two spellings actually agree, which
    // is the thing `as_str()` would otherwise have been asserting.
    let refusals = [
        BeforeSendRefusal::UnresolvedTarget,
        BeforeSendRefusal::UnsupportedCapability {
            missing: vec![UnsupportedCapability::Streaming],
        },
        BeforeSendRefusal::AuthMaterialUnsupported {
            offered: "none".to_string(),
        },
        BeforeSendRefusal::UnrepresentableRequest { detail: "d" },
        BeforeSendRefusal::BudgetExceeded { detail: "d" },
    ];
    assert_eq!(refusals.len(), BeforeSendRefusalKind::ALL.len());
    for refusal in &refusals {
        let tag = serde_json::to_value(refusal).expect("refusal must serialize");
        assert_eq!(
            tag.get("kind").and_then(Value::as_str),
            Some(refusal.kind().as_str()),
            "refusal tag and kind() disagree for {refusal:?}"
        );
    }

    let violations = [
        ProtocolViolation::MalformedBody { detail: "d" },
        ProtocolViolation::SchemaViolation { pointer: "/x" },
        ProtocolViolation::EmptyAssistantContent {
            finish_reason: None,
        },
        ProtocolViolation::StreamDecode {
            rule: StreamDecodeErrorKind::IllegalSequence,
        },
    ];
    assert_eq!(violations.len(), ProtocolViolationKind::ALL.len());
    for violation in &violations {
        let tag = serde_json::to_value(violation).expect("violation must serialize");
        assert_eq!(
            tag.get("kind").and_then(Value::as_str),
            Some(violation.kind().as_str()),
            "violation tag and kind() disagree for {violation:?}"
        );
    }

    let dispositions = [
        InvocationDispositionV1::RefusedBeforeSend {
            refusal: BeforeSendRefusal::UnresolvedTarget,
        },
        InvocationDispositionV1::ProviderRejected {
            class: ProviderErrorClass::RateLimited,
            status: 429,
            retry_after: None,
        },
        InvocationDispositionV1::OutcomeUnknown {
            phase: SendPhase::AwaitingResponse,
        },
        InvocationDispositionV1::ProtocolError {
            violation: ProtocolViolation::MalformedBody { detail: "d" },
        },
        InvocationDispositionV1::CancelledBeforeSend,
        InvocationDispositionV1::CancelledConfirmed {
            evidence: CancellationEvidence::NeverAccepted,
        },
        InvocationDispositionV1::CancelledOutcomeUnknown,
        InvocationDispositionV1::CancelledAfterPartial,
        InvocationDispositionV1::Completed {
            completion: CompletionKindV1::Complete,
        },
    ];
    assert_eq!(
        dispositions.len(),
        InvocationDispositionKind::ALL.len(),
        "every disposition variant must be represented here"
    );
    for (disposition, kind) in dispositions
        .iter()
        .zip(InvocationDispositionKind::ALL.iter().copied())
    {
        assert_eq!(
            disposition.kind(),
            kind,
            "disposition list is out of declaration order"
        );
        let tag = serde_json::to_value(disposition).expect("disposition must serialize");
        assert_eq!(
            tag.get("disposition").and_then(Value::as_str),
            Some(kind.as_str()),
            "disposition tag and kind() disagree for {disposition:?}"
        );
    }
}

#[test]
fn payload_bearing_variants_have_literal_instance_goldens() {
    // Tag parity (above) proves the *discriminant* is spelled right. It says
    // nothing about the payload keys beside the tag: renaming
    // `ProtocolViolation::StreamDecode`'s `rule` field to `decode_rule`, or
    // `TextDelta`'s `text` to `delta`, keeps every tag assertion green while
    // changing the bytes a consumer parses. So each payload-bearing variant is
    // serialized once, as an instance, against a literal.
    let violations = [
        (
            ProtocolViolation::MalformedBody {
                detail: "response body was not valid JSON",
            },
            json!({"kind": "malformed_body", "detail": "response body was not valid JSON"}),
        ),
        (
            ProtocolViolation::SchemaViolation {
                pointer: "/choices",
            },
            json!({"kind": "schema_violation", "pointer": "/choices"}),
        ),
        (
            ProtocolViolation::empty_assistant_content(Some("stop")),
            json!({"kind": "empty_assistant_content", "finish_reason": "stop"}),
        ),
        (
            ProtocolViolation::StreamDecode {
                rule: StreamDecodeErrorKind::IllegalSequence,
            },
            json!({"kind": "stream_decode", "rule": "illegal_sequence"}),
        ),
    ];
    assert_eq!(
        violations.len(),
        ProtocolViolationKind::ALL.len(),
        "a violation variant has no instance golden"
    );
    for (violation, expected) in &violations {
        assert_golden("violation", "instance", violation, expected);
    }

    // The stream event vocabulary is frozen before its decoder exists, which
    // is exactly when a field rename is cheapest and least noticed. Every one
    // of the seven, with a populated payload.
    let events = [
        (
            CanonicalStreamEvent::Started { metadata: None },
            json!({"event": "started"}),
        ),
        (
            CanonicalStreamEvent::Started {
                metadata: Some(ProviderResponseMetadata {
                    effective_model: Some("test-model-1".to_string()),
                    effective_version: None,
                    provider_request_id: Some("req-1".to_string()),
                }),
            },
            json!({
                "event": "started",
                "metadata": {
                    "effective_model": "test-model-1",
                    "provider_request_id": "req-1",
                },
            }),
        ),
        (
            CanonicalStreamEvent::TextDelta {
                text: "the answ".to_string(),
            },
            json!({"event": "text_delta", "text": "the answ"}),
        ),
        (
            CanonicalStreamEvent::ToolCallDelta {
                fragment: ToolCallFragment {
                    index: 0,
                    id: Some("call_1".to_string()),
                    name: Some("search".to_string()),
                    arguments_delta: "{\"q\":".to_string(),
                },
            },
            json!({
                "event": "tool_call_delta",
                "fragment": {
                    "index": 0,
                    "id": "call_1",
                    "name": "search",
                    "arguments_delta": "{\"q\":",
                },
            }),
        ),
        (
            CanonicalStreamEvent::ToolCallCompleted { index: 1 },
            json!({"event": "tool_call_completed", "index": 1}),
        ),
        (
            CanonicalStreamEvent::Usage {
                usage: UsageObservationV1::provider_authoritative(Some(12), Some(34), Some(46)),
            },
            json!({
                "event": "usage",
                "usage": {
                    "prompt_tokens": 12,
                    "completion_tokens": 34,
                    "total_tokens": 46,
                    "provenance": "provider_authoritative",
                },
            }),
        ),
        (
            CanonicalStreamEvent::Completed {
                completion: CompletionKindV1::ToolCalls,
            },
            json!({"event": "completed", "completion": "tool_calls"}),
        ),
        (
            CanonicalStreamEvent::Failed {
                disposition: InvocationDispositionV1::ProtocolError {
                    violation: ProtocolViolation::StreamDecode {
                        rule: StreamDecodeErrorKind::UnterminatedStream,
                    },
                },
            },
            json!({
                "event": "failed",
                "disposition": {
                    "disposition": "protocol_error",
                    "violation": {"kind": "stream_decode", "rule": "unterminated_stream"},
                },
            }),
        ),
    ];
    for (event, expected) in &events {
        assert_golden("stream event", "instance", event, expected);
    }

    // Coverage, so a new variant cannot be added without a golden: the
    // distinct kinds exercised above must be every kind there is, in
    // declaration order.
    let mut covered: Vec<CanonicalStreamEventKind> =
        events.iter().map(|(event, _)| event.kind()).collect();
    covered.dedup();
    assert_eq!(
        covered,
        CanonicalStreamEventKind::ALL.to_vec(),
        "the stream event goldens no longer cover every variant in order"
    );

    // A fragment on its own, because the decoder slice will build these
    // incrementally and the absent-vs-empty distinction is load-bearing: an
    // omitted `id` means "not seen yet", not "empty".
    assert_golden(
        "tool call fragment",
        "nothing seen yet",
        &ToolCallFragment {
            index: 3,
            id: None,
            name: None,
            arguments_delta: String::new(),
        },
        &json!({"index": 3}),
    );
    // The other half of that distinction, pinned literally rather than just
    // implied by the first golden: a provider that has announced an id which
    // happens to be the empty string is a *seen* id, not an unseen one, and
    // must serialize with the key present — `Option::is_none` is the skip
    // condition, not `str::is_empty`. Without this golden, a change that
    // skipped serialization on an empty id too (conflating "seen, empty"
    // with "not seen") would still pass the `id: None` case above.
    assert_golden(
        "tool call fragment",
        "id seen but empty",
        &ToolCallFragment {
            index: 3,
            id: Some(String::new()),
            name: None,
            arguments_delta: String::new(),
        },
        &json!({"index": 3, "id": ""}),
    );
}

#[test]
fn retry_after_keeps_the_seam_spelling() {
    // The seam (memcore) freezes this exact shape; the local copy must stay
    // byte-identical so the two reconcile without a translation table.
    assert_eq!(
        serde_json::to_value(RetryAfter::Seconds(120)).expect("serializes"),
        json!({"kind": "seconds", "value": 120})
    );
    assert_eq!(
        serde_json::to_value(RetryAfter::At("Tue, 12 Aug 2026 09:00:00 GMT".to_string()))
            .expect("serializes"),
        json!({"kind": "at", "value": "Tue, 12 Aug 2026 09:00:00 GMT"})
    );
}

#[test]
fn dialect_name_matches_the_seam_wire_dialect_spelling() {
    // Not serde's snake_case of `OpenAiCompat` (which is `open_ai_compat`).
    assert_eq!(OPENAI_COMPAT_DIALECT, "openai_compat");
    assert_eq!(OpenAiCompatWire::new().dialect(), "openai_compat");
    assert_eq!(ANTHROPIC_DIALECT, "anthropic");
    assert_eq!(AnthropicWire::new().dialect(), "anthropic");
    assert_eq!(XAI_DIALECT, "xai");
    assert_eq!(XaiWire::new().dialect(), "xai");
    assert_eq!(OPEN_ROUTER_DIALECT, "open_router");
    assert_eq!(OpenRouterWire::new().dialect(), "open_router");
    assert_eq!(GENERIC_COMPAT_DIALECT, "generic_compat");
    assert_eq!(GenericCompatWire::new().dialect(), "generic_compat");
}
