//! The disposition and usage vocabularies: serialized shape, and the safety
//! semantics that shape is there to carry.
//!
//! Two kinds of assertion live here and they fail for different reasons.
//!
//! 1. **Shape goldens.** Every [`InvocationDispositionV1`] variant is pinned
//!    against a *literal* JSON document. These are durable bytes — a
//!    broker-side wrapper writes them next to a `model-invocation-v1` receipt —
//!    so a rename is a storage migration, not a refactor, and the golden is
//!    what makes that visible in review.
//! 2. **Semantic goldens.** For every variant, the three questions that decide
//!    whether money gets spent twice: did the provider do work, may we re-send,
//!    may we try a different deployment. A serialization test alone would stay
//!    green while `CancelledOutcomeUnknown` quietly became retry-safe.
//!
//! Both tables are checked for exhaustiveness against
//! [`InvocationDispositionKind::ALL`], so a new variant fails these tests until
//! someone decides its bytes *and* its safety answer. That is the point: a new
//! terminal state that nobody classified would otherwise default to whatever
//! the `match` arm nearest to it happens to say.

use super::*;
use crate::CompletionStatusV1;

/// One disposition, its golden bytes, and its safety answers.
struct Row {
    kind: InvocationDispositionKind,
    disposition: InvocationDispositionV1,
    golden: Value,
    /// Is the provider *proven* to have done nothing?
    provider_did_no_work: bool,
    /// May the executor re-send?
    posture: RetryPosture,
}

/// Every disposition, in declaration order.
///
/// One table rather than one test per variant, because the property that
/// matters most — *nothing is missing* — is a property of the list.
fn disposition_table() -> Vec<Row> {
    vec![
        Row {
            kind: InvocationDispositionKind::RefusedBeforeSend,
            disposition: InvocationDispositionV1::RefusedBeforeSend {
                refusal: BeforeSendRefusal::UnsupportedCapability {
                    missing: vec![UnsupportedCapability::Streaming],
                },
            },
            golden: json!({
                "disposition": "refused_before_send",
                "refusal": {"kind": "unsupported_capability", "missing": ["streaming"]},
            }),
            provider_did_no_work: true,
            posture: RetryPosture::Safe,
        },
        Row {
            kind: InvocationDispositionKind::ProviderRejected,
            disposition: InvocationDispositionV1::ProviderRejected {
                class: ProviderErrorClass::RateLimited,
                status: 429,
                retry_after: Some(RetryAfter::Seconds(30)),
            },
            golden: json!({
                "disposition": "provider_rejected",
                "class": "rate_limited",
                "status": 429,
                "retry_after": {"kind": "seconds", "value": 30},
            }),
            provider_did_no_work: false,
            posture: RetryPosture::Safe,
        },
        Row {
            kind: InvocationDispositionKind::OutcomeUnknown,
            disposition: InvocationDispositionV1::OutcomeUnknown {
                phase: SendPhase::AwaitingResponse,
            },
            golden: json!({"disposition": "outcome_unknown", "phase": "awaiting_response"}),
            provider_did_no_work: false,
            posture: RetryPosture::CallerOptIn,
        },
        Row {
            kind: InvocationDispositionKind::ProtocolError,
            disposition: InvocationDispositionV1::ProtocolError {
                violation: ProtocolViolation::SchemaViolation {
                    pointer: "/choices",
                },
            },
            golden: json!({
                "disposition": "protocol_error",
                "violation": {"kind": "schema_violation", "pointer": "/choices"},
            }),
            provider_did_no_work: false,
            posture: RetryPosture::Forbidden,
        },
        Row {
            kind: InvocationDispositionKind::CancelledBeforeSend,
            disposition: InvocationDispositionV1::CancelledBeforeSend,
            golden: json!({"disposition": "cancelled_before_send"}),
            provider_did_no_work: true,
            posture: RetryPosture::Safe,
        },
        Row {
            kind: InvocationDispositionKind::CancelledConfirmed,
            disposition: InvocationDispositionV1::CancelledConfirmed {
                evidence: CancellationEvidence::NeverAccepted,
            },
            golden: json!({"disposition": "cancelled_confirmed", "evidence": "never_accepted"}),
            provider_did_no_work: true,
            posture: RetryPosture::Safe,
        },
        Row {
            kind: InvocationDispositionKind::CancelledOutcomeUnknown,
            disposition: InvocationDispositionV1::CancelledOutcomeUnknown,
            golden: json!({"disposition": "cancelled_outcome_unknown"}),
            provider_did_no_work: false,
            posture: RetryPosture::CallerOptIn,
        },
        Row {
            kind: InvocationDispositionKind::CancelledAfterPartial,
            disposition: InvocationDispositionV1::CancelledAfterPartial,
            golden: json!({"disposition": "cancelled_after_partial"}),
            provider_did_no_work: false,
            posture: RetryPosture::Forbidden,
        },
        Row {
            kind: InvocationDispositionKind::Completed,
            disposition: InvocationDispositionV1::Completed {
                completion: CompletionKindV1::Truncated,
            },
            golden: json!({"disposition": "completed", "completion": "truncated"}),
            provider_did_no_work: false,
            posture: RetryPosture::NotApplicable,
        },
    ]
}

#[test]
fn every_disposition_variant_has_frozen_bytes() {
    let table = disposition_table();
    let covered: Vec<InvocationDispositionKind> = table.iter().map(|row| row.kind).collect();
    assert_eq!(
        covered,
        InvocationDispositionKind::ALL.to_vec(),
        "the golden table is missing a disposition, or is out of declaration order — \
         a terminal state with no golden is a durable byte sequence nobody reviewed"
    );

    for row in &table {
        assert_eq!(
            row.disposition.kind(),
            row.kind,
            "table row {:?} holds a disposition of a different kind",
            row.kind
        );
        assert_golden(
            row.kind.as_str(),
            "disposition",
            &row.disposition,
            &row.golden,
        );
    }
}

#[test]
fn every_disposition_variant_has_a_frozen_safety_answer() {
    for row in disposition_table() {
        let kind = row.kind;
        assert_eq!(
            row.disposition.provider_did_no_work(),
            row.provider_did_no_work,
            "{kind:?}: provider_did_no_work drifted — this answer is a claim about \
             someone else's billing and may only be `true` where the protocol proves it"
        );
        assert_eq!(
            row.disposition.retry_posture(),
            row.posture,
            "{kind:?}: retry_posture drifted — this answer decides whether a retry \
             double-spends"
        );
        assert_eq!(
            row.disposition.fallback_eligible(),
            row.posture == RetryPosture::Safe,
            "{kind:?}: fallback eligibility must track the Safe posture exactly — a \
             fallback candidate is only free when nothing reached anyone"
        );
    }
}

#[test]
fn outcome_unknown_is_only_safe_when_the_provider_was_never_reached() {
    // The load-bearing row of the whole spectrum. A deadline that expired after
    // the request was accepted is not a failure; it is the end of our ability
    // to observe. Only `Connecting` proves the provider never got a request.
    for phase in SendPhase::ALL.iter().copied() {
        let disposition = InvocationDispositionV1::OutcomeUnknown { phase };
        let expected = if phase == SendPhase::Connecting {
            RetryPosture::Safe
        } else {
            RetryPosture::CallerOptIn
        };
        assert_eq!(
            disposition.retry_posture(),
            expected,
            "phase {phase:?}: a retry after an unobservable outcome needs caller consent \
             unless the transport proved the request never landed"
        );
        assert_eq!(
            disposition.provider_did_no_work(),
            phase == SendPhase::Connecting
        );
    }

    // `Sending` is the trap: bytes were written, so the provider may hold a
    // complete request even though we never saw a response.
    assert_eq!(
        InvocationDispositionV1::OutcomeUnknown {
            phase: SendPhase::Sending,
        }
        .retry_posture(),
        RetryPosture::CallerOptIn,
        "a half-written request may still have been received in full"
    );
}

#[test]
fn a_dropped_connection_is_not_evidence_of_cancellation() {
    // `CancelledConfirmed` is reachable only through `CancellationEvidence`,
    // and the vocabulary deliberately has no "connection dropped" member: a
    // dropped connection proves only that we stopped listening.
    let spellings: Vec<&str> = CancellationEvidence::ALL
        .iter()
        .map(|evidence| evidence.as_str())
        .collect();
    for absent in [
        "connection_dropped",
        "connection_reset",
        "timeout",
        "client_gave_up",
    ] {
        assert!(
            !spellings.contains(&absent),
            "{absent:?} became cancellation evidence — that is an inference from our \
             own silence, not a protocol proof"
        );
    }

    // And only evidence that speaks about the *provider's* state clears it of
    // having done work.
    for evidence in CancellationEvidence::ALL.iter().copied() {
        let disposition = InvocationDispositionV1::CancelledConfirmed { evidence };
        let expected = matches!(
            evidence,
            CancellationEvidence::NeverAccepted | CancellationEvidence::TerminalEventAborted
        );
        assert_eq!(
            disposition.provider_did_no_work(),
            expected,
            "{evidence:?}: an acknowledgement means the provider heard us, not that it \
             had not already started"
        );
    }
}

#[test]
fn completion_kind_projects_onto_the_frozen_receipt_status_without_widening_it() {
    // #1519 owns `CompletionStatusV1`, and the broker's richer vocabulary must
    // not widen it by accident. The projection is the same rule
    // `completion_status_from_finish_reason` has always applied: only an
    // explicit stop is Complete, only a token ceiling is Truncated, everything
    // else is honestly Unknown.
    let expected = [
        (CompletionKindV1::Complete, CompletionStatusV1::Complete),
        (CompletionKindV1::Truncated, CompletionStatusV1::Truncated),
        (CompletionKindV1::ToolCalls, CompletionStatusV1::Unknown),
        (
            CompletionKindV1::ContentFiltered,
            CompletionStatusV1::Unknown,
        ),
        (CompletionKindV1::Unknown, CompletionStatusV1::Unknown),
    ];
    let covered: Vec<CompletionKindV1> = expected.iter().map(|(kind, _)| *kind).collect();
    assert_eq!(
        covered,
        CompletionKindV1::ALL.to_vec(),
        "a completion kind has no declared projection onto the durable status"
    );
    for (kind, status) in expected {
        assert_eq!(
            kind.to_completion_status_v1(),
            status,
            "{kind:?} projects onto the wrong durable status"
        );
    }

    // The projection is lossy in exactly one direction: three broker kinds
    // collapse onto Unknown, and none of them may become Complete.
    let complete: Vec<CompletionKindV1> = CompletionKindV1::ALL
        .iter()
        .copied()
        .filter(|kind| kind.to_completion_status_v1() == CompletionStatusV1::Complete)
        .collect();
    assert_eq!(
        complete,
        vec![CompletionKindV1::Complete],
        "only an explicit stop may be reported as a complete generation"
    );
}

#[test]
fn unknown_usage_is_absent_numbers_not_zeros() {
    // Zero tokens and unknown tokens are different claims, and a receipt that
    // says "0 prompt tokens" is a lie that a spend ceiling will happily
    // enforce against.
    let unknown = UsageObservationV1::unknown();
    assert_golden(
        "usage",
        "unknown observation",
        &unknown,
        &json!({"provenance": "unknown"}),
    );
    assert!(!unknown.has_any_number());
    assert!(!unknown.provenance.is_billable_authority());

    let serialized = serde_json::to_value(&unknown).expect("usage serializes");
    for key in ["prompt_tokens", "completion_tokens", "total_tokens"] {
        assert!(
            serialized.get(key).is_none(),
            "an unknown observation emitted {key} — absent and zero must not be the same bytes"
        );
    }
}

#[test]
fn a_provider_reported_observation_keeps_its_numbers_and_its_authority() {
    let reported = UsageObservationV1::provider_authoritative(Some(12), Some(34), Some(46));
    assert_golden(
        "usage",
        "provider-authoritative observation",
        &reported,
        &json!({
            "prompt_tokens": 12,
            "completion_tokens": 34,
            "total_tokens": 46,
            "provenance": "provider_authoritative",
        }),
    );

    // A partial report stays partial: the total is deliberately not derived,
    // because several providers bill a total that is not the sum.
    let partial = UsageObservationV1::provider_authoritative(Some(12), None, None);
    assert_golden(
        "usage",
        "partial observation",
        &partial,
        &json!({"prompt_tokens": 12, "provenance": "provider_authoritative"}),
    );
    assert!(partial.has_any_number());

    // Round trip, since this shape is read back out of durable storage.
    let round_tripped: UsageObservationV1 =
        serde_json::from_value(serde_json::to_value(&reported).expect("serializes"))
            .expect("usage deserializes");
    assert_eq!(round_tripped, reported);
}

#[test]
fn only_the_provider_reported_tier_may_be_billed_against() {
    let billable: Vec<UsageProvenanceV1> = UsageProvenanceV1::ALL
        .iter()
        .copied()
        .filter(|provenance| provenance.is_billable_authority())
        .collect();
    assert_eq!(
        billable,
        vec![UsageProvenanceV1::ProviderAuthoritative],
        "an estimated or calculated number was promoted to invoice authority"
    );
}

#[test]
fn this_slice_computes_no_cost() {
    // #1681 owns pricing. A cost computed against a catalog that does not exist
    // would be a fabricated number in a durable receipt, so the structure
    // carries an opaque forward reference and abstains.
    let usage = UsageObservationV1::provider_authoritative(Some(10), Some(20), Some(30));
    assert!(usage.pricing_snapshot_ref.is_none());
    let serialized = serde_json::to_value(&usage).expect("serializes");
    let keys: Vec<&str> = serialized
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    for cost_shaped in ["cost", "cost_micros", "price", "amount", "currency"] {
        assert!(
            !keys.contains(&cost_shaped),
            "usage grew a {cost_shaped} field before the pricing catalog exists"
        );
    }
}

#[test]
fn a_wire_outcome_lands_on_exactly_one_disposition() {
    let completed = WireOutcome::Completed {
        message: CanonicalAssistantMessage {
            text: Some("hi".to_string()),
            tool_calls: Vec::new(),
        },
        completion: CompletionKindV1::Complete,
        usage: UsageObservationV1::provider_authoritative(Some(1), Some(2), Some(3)),
        metadata: ProviderResponseMetadata::default(),
    };
    assert_eq!(
        completed.disposition(),
        InvocationDispositionV1::Completed {
            completion: CompletionKindV1::Complete,
        }
    );
    assert!(completed.usage().is_some());

    let rejected = WireOutcome::Rejected {
        status: 503,
        classification: ProviderErrorClassification::new(
            ProviderErrorClass::ServerError,
            Some(RetryAfter::Seconds(5)),
        ),
    };
    assert_eq!(
        rejected.disposition(),
        InvocationDispositionV1::ProviderRejected {
            class: ProviderErrorClass::ServerError,
            status: 503,
            retry_after: Some(RetryAfter::Seconds(5)),
        },
        "the provider's own wait directive must survive into the disposition"
    );
    assert!(
        rejected.usage().is_none(),
        "a rejected attempt reports no usage — `None`, not zeros, because \
         'the provider charged nothing' is a claim we cannot make"
    );

    let violated = WireOutcome::ProtocolViolation {
        violation: ProtocolViolation::MalformedBody {
            detail: "response body was not valid JSON",
        },
    };
    assert_eq!(
        violated.disposition(),
        InvocationDispositionV1::ProtocolError {
            violation: ProtocolViolation::MalformedBody {
                detail: "response body was not valid JSON",
            },
        }
    );
    assert!(violated.usage().is_none());
}

#[test]
fn a_provider_controlled_finish_reason_is_bounded_and_defanged() {
    // The one provider-controlled string in the violation vocabulary — every
    // other detail is `&'static str`. It is durable, so its length must be our
    // decision rather than the provider's.
    let hostile = format!("stop{}", "A".repeat(10_000));
    let violation = ProtocolViolation::empty_assistant_content(Some(&hostile));
    let ProtocolViolation::EmptyAssistantContent { finish_reason } = &violation else {
        panic!("constructor built the wrong variant");
    };
    let kept = finish_reason.as_deref().expect("a reason was supplied");
    assert_eq!(
        kept.chars().count(),
        MAX_FINISH_REASON_CHARS,
        "an unbounded provider string reached a durable disposition"
    );
    assert!(kept.starts_with("stop"));

    // Control characters are the log-forging path, so they do not survive.
    let forging =
        ProtocolViolation::empty_assistant_content(Some("stop\n[llm] fabricated log line\r\n"));
    let ProtocolViolation::EmptyAssistantContent { finish_reason } = &forging else {
        panic!("constructor built the wrong variant");
    };
    let kept = finish_reason.as_deref().expect("a reason was supplied");
    assert!(
        !kept.contains('\n') && !kept.contains('\r'),
        "a newline survived into a disposition field: {kept:?}"
    );

    // The honest case is untouched, and absence stays absent.
    assert_eq!(
        ProtocolViolation::empty_assistant_content(Some("content_filter")),
        ProtocolViolation::EmptyAssistantContent {
            finish_reason: Some("content_filter".to_string()),
        }
    );
    assert_golden(
        "violation",
        "empty content with no reason",
        &ProtocolViolation::empty_assistant_content(None),
        &json!({"kind": "empty_assistant_content"}),
    );
}

#[test]
fn every_before_send_refusal_is_a_safe_terminal() {
    // Nothing was sent, so nothing was spent: every refusal must be free to
    // retry and free to fall back. A refusal that reported otherwise would
    // strand a caller on a deployment that never received a byte.
    let refusals = [
        BeforeSendRefusal::UnresolvedTarget,
        BeforeSendRefusal::UnsupportedCapability {
            missing: vec![UnsupportedCapability::Streaming],
        },
        BeforeSendRefusal::AuthMaterialUnsupported {
            offered: "none".to_string(),
        },
        BeforeSendRefusal::UnrepresentableRequest {
            detail: "no representation",
        },
        BeforeSendRefusal::BudgetExceeded {
            detail: "max_total_tokens",
        },
    ];
    assert_eq!(
        refusals.len(),
        BeforeSendRefusalKind::ALL.len(),
        "a refusal kind has no safety assertion here"
    );
    for refusal in refusals {
        let disposition = InvocationDispositionV1::RefusedBeforeSend {
            refusal: refusal.clone(),
        };
        assert!(disposition.provider_did_no_work(), "{refusal:?}");
        assert_eq!(
            disposition.retry_posture(),
            RetryPosture::Safe,
            "{refusal:?}"
        );
        assert!(disposition.fallback_eligible(), "{refusal:?}");
    }
}

#[test]
fn a_stream_decoder_that_is_unavailable_becomes_a_refusal_not_a_downgrade() {
    // The silent-degradation guard, stated as a disposition: a caller who asked
    // to stream and got a whole body has no way to notice.
    let unavailable = StreamDecoderUnavailable {
        reason: StreamDecoderUnavailableReason::NotImplementedYet,
        dialect: OPENAI_COMPAT_DIALECT,
    };
    let refusal = unavailable.into_refusal();
    assert_eq!(
        refusal,
        BeforeSendRefusal::UnsupportedCapability {
            missing: vec![UnsupportedCapability::Streaming],
        }
    );
    assert!(InvocationDispositionV1::RefusedBeforeSend { refusal }.fallback_eligible());
}

#[test]
fn a_disposition_never_carries_provider_body_text() {
    // Dispositions reach logs and durable storage. Every detail string in the
    // vocabulary is `&'static str` except the bounded finish reason, and this
    // asserts the whole spectrum stays free of the response body.
    let body_text = "PROVIDER-SECRET-BODY-MARKER";
    let outcome = OpenAiCompatWire::new().parse_response(
        200,
        &ResponseHeaders::new(),
        format!(r#"{{"choices":[{{"message":{{"content":""}},"finish_reason":"{body_text}"}}]}}"#)
            .as_bytes(),
    );
    let disposition = outcome.disposition();
    let rendered = serde_json::to_string(&disposition).expect("serializes");

    // The finish reason is the one field that echoes the provider, and it is
    // the field the bound applies to — so it *is* present, deliberately, and
    // nothing else is.
    assert!(
        rendered.contains(body_text),
        "sanity: this fixture uses the reason field"
    );

    let hostile_body = r#"{"choices":[{"message":{"content":""},"finish_reason":"stop"}],"secret":"LEAKED-BODY-FIELD"}"#;
    let hostile = OpenAiCompatWire::new()
        .parse_response(200, &ResponseHeaders::new(), hostile_body.as_bytes())
        .disposition();
    let rendered = serde_json::to_string(&hostile).expect("serializes");
    assert!(
        !rendered.contains("LEAKED-BODY-FIELD"),
        "a response body field reached the disposition: {rendered}"
    );

    let malformed = OpenAiCompatWire::new()
        .parse_response(
            200,
            &ResponseHeaders::new(),
            b"not json at all LEAKED-BODY-FIELD",
        )
        .disposition();
    let rendered = serde_json::to_string(&malformed).expect("serializes");
    assert!(
        !rendered.contains("LEAKED-BODY-FIELD"),
        "a malformed body reached the disposition: {rendered}"
    );
}
