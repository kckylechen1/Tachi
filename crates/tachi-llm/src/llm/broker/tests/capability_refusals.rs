//! The capability gate, one axis at a time.
//!
//! Each test narrows the adapter to a deployment that lacks exactly one
//! capability, sends a request that needs exactly that capability, and asserts
//! the *specific* missing axis comes back. Asserting only "it was refused"
//! would pass for an adapter that refuses everything, which is the failure mode
//! a capability gate is most likely to have after someone "fixes" it.
//!
//! The load-bearing one is streaming: the protocol law says a `stream: enabled`
//! request that cannot be streamed must be **refused**, never quietly answered
//! with a whole body. There is no adapter behaviour that silently drops
//! `stream`, and this suite is what keeps it that way.

use super::*;

fn full_capabilities() -> WireCapabilities {
    OpenAiCompatWire::ceiling()
}

fn adapter_without(mutate: impl FnOnce(&mut WireCapabilities)) -> OpenAiCompatWire {
    let mut caps = full_capabilities();
    mutate(&mut caps);
    OpenAiCompatWire::narrowed_to(caps)
}

#[track_caller]
fn refusal(
    adapter: &OpenAiCompatWire,
    parts: CanonicalInvocationRequestParts,
) -> BeforeSendRefusal {
    let request = CanonicalInvocationRequest::new(parts).expect("request must be valid");
    adapter
        .build_request(&request, api_key_lease())
        .expect_err("the adapter must refuse this request")
}

#[test]
fn a_streaming_request_is_refused_not_silently_downgraded() {
    // The dialect ships with `streaming: false` in this slice, so the plain
    // adapter must already refuse.
    let mut parts = minimal_parts();
    parts.stream = StreamSelection::Enabled;
    assert_eq!(
        refusal(&OpenAiCompatWire::new(), parts.clone()),
        BeforeSendRefusal::UnsupportedCapability {
            missing: vec![UnsupportedCapability::Streaming],
        }
    );

    // And the same request without the stream flag must succeed — otherwise
    // the test above would pass for an adapter that refuses everything.
    let ok_parts = minimal_parts();
    assert!(OpenAiCompatWire::new()
        .build_request(
            &CanonicalInvocationRequest::new(ok_parts).expect("valid"),
            api_key_lease()
        )
        .is_ok());

    // The body must never carry `stream` on the refused path — there is no
    // "refuse but send anyway" branch. Belt two is the decoder:
    assert!(matches!(
        OpenAiCompatWire::new().new_stream_decoder(),
        Err(StreamDecoderUnavailable {
            reason: StreamDecoderUnavailableReason::NotImplementedYet,
            dialect: "openai_compat",
        })
    ));
}

#[test]
fn a_declared_streaming_capability_cannot_widen_the_dialect() {
    // A catalog row (or a careless test) claiming the deployment streams must
    // not be able to talk this adapter into emitting `stream: true` while the
    // decoder does not exist. Narrowing is an intersection, never a union.
    let optimistic = WireCapabilities {
        streaming: true,
        ..full_capabilities()
    };
    let adapter = OpenAiCompatWire::narrowed_to(optimistic);
    assert!(
        !adapter.capabilities().streaming,
        "narrowed_to must intersect with the dialect ceiling, not adopt the claim"
    );

    let mut parts = minimal_parts();
    parts.stream = StreamSelection::Enabled;
    assert_eq!(
        refusal(&adapter, parts),
        BeforeSendRefusal::UnsupportedCapability {
            missing: vec![UnsupportedCapability::Streaming],
        }
    );
}

#[test]
fn a_deployment_that_cannot_call_tools_refuses_a_tool_request() {
    let adapter = adapter_without(|caps| caps.tools = false);
    let mut parts = minimal_parts();
    parts.tools = vec![ToolDeclaration {
        name: "search".to_string(),
        description: None,
        parameters: json!({"type": "object"}),
    }];
    assert_eq!(
        refusal(&adapter, parts),
        BeforeSendRefusal::UnsupportedCapability {
            missing: vec![UnsupportedCapability::Tools],
        }
    );
}

#[test]
fn a_deployment_without_structured_output_refuses_a_json_object_request() {
    let adapter = adapter_without(|caps| {
        caps.structured_output = false;
        caps.json_schema = false;
    });
    let mut parts = minimal_parts();
    parts.response_format = ResponseFormat::JsonObject;
    assert_eq!(
        refusal(&adapter, parts),
        BeforeSendRefusal::UnsupportedCapability {
            missing: vec![UnsupportedCapability::StructuredOutput],
        }
    );
}

#[test]
fn a_json_object_only_deployment_refuses_a_json_schema_request() {
    // The split that exists because collapsing the two axes is wrong in both
    // directions: this deployment can do `json_object` and cannot do
    // `json_schema`, and a single `structured_output` bool would either refuse
    // a request that works or send one that gets rejected at spend time.
    let adapter = adapter_without(|caps| caps.json_schema = false);
    assert!(adapter.capabilities().structured_output);

    let mut ok_parts = minimal_parts();
    ok_parts.response_format = ResponseFormat::JsonObject;
    assert!(
        adapter
            .build_request(
                &CanonicalInvocationRequest::new(ok_parts).expect("valid"),
                api_key_lease()
            )
            .is_ok(),
        "json_object must still be accepted by a json_object-capable deployment"
    );

    let mut parts = minimal_parts();
    parts.response_format = ResponseFormat::JsonSchema {
        name: "answer".to_string(),
        strict: true,
        schema: json!({"type": "object"}),
    };
    assert_eq!(
        refusal(&adapter, parts),
        BeforeSendRefusal::UnsupportedCapability {
            missing: vec![UnsupportedCapability::JsonSchema],
        }
    );
}

#[test]
fn a_text_only_deployment_refuses_an_image_part() {
    let adapter = adapter_without(|caps| caps.media = false);
    let mut parts = minimal_parts();
    parts.messages = vec![CanonicalMessage {
        role: MessageRole::User,
        content: MessageContent::Parts {
            parts: vec![ContentPart::ImageUrl {
                url: "https://provider.test/cat.png".to_string(),
                detail: None,
            }],
        },
        tool_call_id: None,
        name: None,
    }];
    assert_eq!(
        refusal(&adapter, parts),
        BeforeSendRefusal::UnsupportedCapability {
            missing: vec![UnsupportedCapability::Media],
        }
    );
}

#[test]
fn a_non_chat_deployment_refuses_a_chat_request() {
    let adapter = adapter_without(|caps| caps.chat = false);
    assert_eq!(
        refusal(&adapter, minimal_parts()),
        BeforeSendRefusal::UnsupportedCapability {
            missing: vec![UnsupportedCapability::Chat],
        }
    );
}

#[test]
fn every_missing_capability_is_reported_at_once() {
    // One refusal, all the axes — a caller who learns one missing capability
    // per round trip pays an admission and a resolution for each.
    let adapter = adapter_without(|caps| {
        caps.tools = false;
        caps.structured_output = false;
        caps.json_schema = false;
        caps.media = false;
    });
    let mut parts = minimal_parts();
    parts.stream = StreamSelection::Enabled;
    parts.tools = vec![ToolDeclaration {
        name: "search".to_string(),
        description: None,
        parameters: json!({"type": "object"}),
    }];
    parts.response_format = ResponseFormat::JsonSchema {
        name: "answer".to_string(),
        strict: true,
        schema: json!({"type": "object"}),
    };
    parts.messages = vec![CanonicalMessage {
        role: MessageRole::User,
        content: MessageContent::Parts {
            parts: vec![ContentPart::ImageUrl {
                url: "https://provider.test/cat.png".to_string(),
                detail: None,
            }],
        },
        tool_call_id: None,
        name: None,
    }];
    assert_eq!(
        refusal(&adapter, parts),
        BeforeSendRefusal::UnsupportedCapability {
            missing: vec![
                UnsupportedCapability::Tools,
                UnsupportedCapability::Streaming,
                UnsupportedCapability::StructuredOutput,
                UnsupportedCapability::JsonSchema,
                UnsupportedCapability::Media,
            ],
        },
        "missing capabilities must come back in UnsupportedCapability::ALL order"
    );
}

#[test]
fn an_unresolved_alias_is_refused_rather_than_guessed() {
    let mut parts = minimal_parts();
    parts.target = InvocationTarget::ModelAlias {
        alias: ModelAliasRef::new("fast-cheap").expect("valid alias"),
    };
    assert_eq!(
        refusal(&OpenAiCompatWire::new(), parts),
        BeforeSendRefusal::UnresolvedTarget,
        "an adapter must not invent a deployment for an unresolved alias"
    );
}

#[test]
fn a_lease_holding_no_material_is_refused_before_send() {
    // Sending an unauthenticated request to learn what the type already said
    // costs a round trip and, on some providers, a rate-limit slot.
    let request = minimal_request();
    assert_eq!(
        OpenAiCompatWire::new()
            .build_request(&request, AuthMaterialRef::none())
            .expect_err("a dialect that needs a bearer must refuse an empty lease"),
        BeforeSendRefusal::AuthMaterialUnsupported {
            offered: "none".to_string(),
        }
    );
}

#[test]
fn a_capability_set_claiming_json_schema_without_structured_output_is_inconsistent() {
    let bad = WireCapabilities {
        structured_output: false,
        json_schema: true,
        ..WireCapabilities::default()
    };
    assert!(!bad.is_consistent());
    // ...and narrowing repairs it rather than propagating the contradiction.
    assert!(OpenAiCompatWire::narrowed_to(bad)
        .capabilities()
        .is_consistent());
    assert!(
        !OpenAiCompatWire::narrowed_to(bad)
            .capabilities()
            .json_schema
    );
}

#[test]
fn refusals_carry_no_provider_or_request_content() {
    // A refusal is a diagnostic that reaches logs and receipts. It must not
    // become a path for prompt text or endpoint URLs to get there.
    let mut parts = minimal_parts();
    parts.messages = vec![user_message("secret prompt text nobody should log")];
    parts.stream = StreamSelection::Enabled;
    let rendered = format!("{:?}", refusal(&OpenAiCompatWire::new(), parts));
    for forbidden in ["secret prompt", "provider.test", "lease-fixture"] {
        assert!(
            !rendered.contains(forbidden),
            "refusal debug output leaked {forbidden:?}: {rendered}"
        );
    }
}
