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
}
