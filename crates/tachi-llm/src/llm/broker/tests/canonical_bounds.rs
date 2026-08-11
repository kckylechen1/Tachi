//! The canonical request's bounds, on **both** construction paths.
//!
//! Each bound is asserted twice: once through
//! [`CanonicalInvocationRequest::new`] and once through `serde_json::from_str`.
//! That pairing is the whole point of the parts shadow — a validation that only
//! runs on the constructor is a validation a JSON body walks straight past, and
//! the JSON body is the one that arrives from outside this process.

use super::*;

/// Round-trips parts through JSON and back, so the deserialize path runs the
/// same validation the constructor does.
fn deserialize(
    parts: &CanonicalInvocationRequestParts,
) -> Result<CanonicalInvocationRequest, String> {
    // Parts is deserialize-only, so it is projected to JSON by hand here; using
    // the *validated* type's Serialize would beg the question by requiring a
    // valid request to test an invalid one.
    let raw = serde_json::to_string(&PartsProjection(parts)).expect("parts must project");
    serde_json::from_str::<CanonicalInvocationRequest>(&raw).map_err(|err| err.to_string())
}

/// A serialize-side view of [`CanonicalInvocationRequestParts`], matching its
/// field names exactly.
struct PartsProjection<'a>(&'a CanonicalInvocationRequestParts);

impl serde::Serialize for PartsProjection<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let p = self.0;
        let value = json!({
            "target": p.target,
            "messages": p.messages,
            "tools": p.tools,
            "tool_choice": p.tool_choice,
            "response_format": p.response_format,
            "stream": p.stream,
            "sampling": p.sampling,
            "data_policy": p.data_policy,
            "budget": p.budget,
            "idempotency_key": p.idempotency_key,
            "deadline": p.deadline,
            "cancellation": p.cancellation,
            "admitted": p.admitted,
        });
        value.serialize(serializer)
    }
}

/// Asserts a bad `parts` is refused by the constructor with `expected`, and
/// that its JSON form is refused too.
#[track_caller]
fn assert_refused_on_both_paths(
    parts: CanonicalInvocationRequestParts,
    expected: RequestError,
    what: &str,
) {
    assert_eq!(
        CanonicalInvocationRequest::new(parts.clone()),
        Err(expected.clone()),
        "{what}: constructor accepted a value it must refuse"
    );
    let deserialized = deserialize(&parts);
    assert!(
        deserialized.is_err(),
        "{what}: the deserialize path minted a request the constructor refuses \
         — the validation is bypassable"
    );
    let message = deserialized.unwrap_err();
    assert!(
        message.contains(&expected.to_string()),
        "{what}: deserialize rejected for the wrong reason\n  got:      {message}\n  expected: {expected}"
    );
}

#[test]
fn the_minimal_request_is_accepted_on_both_paths() {
    // The negative tests below are worthless unless the baseline passes: a
    // constructor that refuses everything would satisfy every one of them.
    let parts = minimal_parts();
    assert!(CanonicalInvocationRequest::new(parts.clone()).is_ok());
    assert!(deserialize(&parts).is_ok());
}

#[test]
fn an_empty_message_list_is_refused() {
    let mut parts = minimal_parts();
    parts.messages.clear();
    assert_refused_on_both_paths(parts, RequestError::NoMessages, "empty messages");
}

#[test]
fn the_message_count_cap_holds() {
    let mut parts = minimal_parts();
    parts.messages = (0..=MAX_MESSAGES).map(|_| user_message("x")).collect();
    assert_refused_on_both_paths(
        parts,
        RequestError::TooManyMessages {
            len: MAX_MESSAGES + 1,
        },
        "message count",
    );
}

#[test]
fn the_single_message_size_cap_holds() {
    let mut parts = minimal_parts();
    parts.messages = vec![user_message(&"x".repeat(MAX_MESSAGE_CONTENT_BYTES + 1))];
    assert_refused_on_both_paths(
        parts,
        RequestError::MessageTooLarge {
            index: 0,
            bytes: MAX_MESSAGE_CONTENT_BYTES + 1,
        },
        "single message size",
    );
}

#[test]
fn the_total_content_cap_holds_even_when_every_message_is_legal() {
    // The discriminative case: five messages, each comfortably under the
    // per-message cap, summing past the total. A per-message-only check passes
    // this and lets an unbounded prompt through in slices.
    let each = MAX_MESSAGE_CONTENT_BYTES;
    let count = MAX_TOTAL_CONTENT_BYTES / each + 1;
    let mut parts = minimal_parts();
    parts.messages = (0..count)
        .map(|_| user_message(&"x".repeat(each)))
        .collect();
    assert_refused_on_both_paths(
        parts,
        RequestError::TotalContentTooLarge {
            bytes: each * count,
        },
        "total content size",
    );
}

#[test]
fn the_content_part_cap_holds() {
    let mut parts = minimal_parts();
    parts.messages = vec![CanonicalMessage {
        role: MessageRole::User,
        content: MessageContent::Parts {
            parts: (0..=MAX_CONTENT_PARTS)
                .map(|_| ContentPart::Text {
                    text: "x".to_string(),
                })
                .collect(),
        },
        tool_call_id: None,
        name: None,
    }];
    assert_refused_on_both_paths(
        parts,
        RequestError::TooManyContentParts {
            index: 0,
            len: MAX_CONTENT_PARTS + 1,
        },
        "content parts",
    );
}

fn tool(name: &str) -> ToolDeclaration {
    ToolDeclaration {
        name: name.to_string(),
        description: Some("a tool".to_string()),
        parameters: json!({"type": "object", "properties": {}}),
    }
}

#[test]
fn the_tool_count_cap_holds() {
    let mut parts = minimal_parts();
    parts.tools = (0..=MAX_TOOLS).map(|i| tool(&format!("t{i}"))).collect();
    assert_refused_on_both_paths(
        parts,
        RequestError::TooManyTools { len: MAX_TOOLS + 1 },
        "tool count",
    );
}

#[test]
fn a_duplicate_tool_name_is_refused() {
    // Providers key a tool result by name; two tools sharing one makes the
    // result unattributable, so this is refused rather than deduplicated.
    let mut parts = minimal_parts();
    parts.tools = vec![tool("search"), tool("search")];
    assert_refused_on_both_paths(
        parts,
        RequestError::DuplicateToolName {
            tool: "search".to_string(),
        },
        "duplicate tool",
    );
}

#[test]
fn a_non_object_tool_schema_is_refused() {
    let mut parts = minimal_parts();
    parts.tools = vec![ToolDeclaration {
        name: "search".to_string(),
        description: None,
        parameters: json!("not an object"),
    }];
    assert_refused_on_both_paths(
        parts,
        RequestError::ToolSchemaNotObject {
            tool: "search".to_string(),
        },
        "non-object tool schema",
    );
}

#[test]
fn an_oversized_tool_schema_is_refused() {
    let mut parts = minimal_parts();
    let padding = "x".repeat(MAX_TOOL_SCHEMA_BYTES);
    let schema = json!({"type": "object", "description": padding});
    let bytes = serde_json::to_vec(&schema)
        .expect("schema serializes")
        .len();
    parts.tools = vec![ToolDeclaration {
        name: "search".to_string(),
        description: None,
        parameters: schema,
    }];
    assert_refused_on_both_paths(
        parts,
        RequestError::ToolSchemaTooLarge {
            tool: "search".to_string(),
            bytes,
        },
        "oversized tool schema",
    );
}

#[test]
fn a_tool_choice_naming_an_undeclared_tool_is_refused() {
    let mut parts = minimal_parts();
    parts.tools = vec![tool("search")];
    parts.tool_choice = ToolChoice::Required {
        tool: "delete_everything".to_string(),
    };
    assert_refused_on_both_paths(
        parts,
        RequestError::ToolChoiceUnknownTool {
            tool: "delete_everything".to_string(),
        },
        "unknown tool choice",
    );
}

#[test]
fn demanding_a_tool_call_without_tools_is_refused() {
    let mut parts = minimal_parts();
    parts.tool_choice = ToolChoice::Any;
    assert_refused_on_both_paths(
        parts,
        RequestError::ToolChoiceWithoutTools,
        "tool choice without tools",
    );
}

#[test]
fn sampling_ranges_are_enforced() {
    for bad in [-0.1_f32, 2.1] {
        let mut parts = minimal_parts();
        parts.sampling.temperature = Some(bad);
        assert_refused_on_both_paths(
            parts,
            RequestError::InvalidTemperature,
            "temperature out of range",
        );
    }
    for bad in [-0.1_f32, 1.1] {
        let mut parts = minimal_parts();
        parts.sampling.top_p = Some(bad);
        assert_refused_on_both_paths(parts, RequestError::InvalidTopP, "top_p out of range");
    }

    // NaN and infinity are checked on the constructor path only, and that is
    // not a gap: JSON has no literal for either, so the deserialize path cannot
    // express them — which is exactly why the finiteness check has to live in
    // the constructor, where an in-process caller can reach it.
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut parts = minimal_parts();
        parts.sampling.temperature = Some(bad);
        assert_eq!(
            CanonicalInvocationRequest::new(parts),
            Err(RequestError::InvalidTemperature),
            "non-finite temperature {bad} must be refused"
        );
        let mut parts = minimal_parts();
        parts.sampling.top_p = Some(bad);
        assert_eq!(
            CanonicalInvocationRequest::new(parts),
            Err(RequestError::InvalidTopP),
            "non-finite top_p {bad} must be refused"
        );
    }

    let mut parts = minimal_parts();
    parts.sampling.max_output_tokens = Some(0);
    assert_refused_on_both_paths(
        parts,
        RequestError::InvalidMaxOutputTokens,
        "zero output budget",
    );
    let mut parts = minimal_parts();
    parts.sampling.stop = (0..=MAX_STOP_SEQUENCES).map(|i| i.to_string()).collect();
    assert_refused_on_both_paths(
        parts,
        RequestError::TooManyStopSequences {
            len: MAX_STOP_SEQUENCES + 1,
        },
        "stop sequences",
    );
}

#[test]
fn boundary_values_are_accepted_not_rejected() {
    // An off-by-one in the wrong direction is just as wrong: the caps are
    // inclusive, so exactly-at-the-cap must pass.
    let mut parts = minimal_parts();
    parts.sampling.temperature = Some(2.0);
    parts.sampling.top_p = Some(1.0);
    parts.sampling.max_output_tokens = Some(1);
    parts.sampling.stop = (0..MAX_STOP_SEQUENCES).map(|i| i.to_string()).collect();
    assert!(CanonicalInvocationRequest::new(parts.clone()).is_ok());
    assert!(deserialize(&parts).is_ok());

    let mut parts = minimal_parts();
    parts.messages = (0..MAX_MESSAGES).map(|_| user_message("x")).collect();
    assert!(CanonicalInvocationRequest::new(parts).is_ok());
}

#[test]
fn a_blank_response_schema_name_and_a_non_object_schema_are_refused() {
    let mut parts = minimal_parts();
    parts.response_format = ResponseFormat::JsonSchema {
        name: "   ".to_string(),
        strict: true,
        schema: json!({"type": "object"}),
    };
    assert_refused_on_both_paths(
        parts,
        RequestError::ResponseSchemaNameBlank,
        "blank schema name",
    );

    let mut parts = minimal_parts();
    parts.response_format = ResponseFormat::JsonSchema {
        name: "answer".to_string(),
        strict: true,
        schema: json!([1, 2, 3]),
    };
    assert_refused_on_both_paths(
        parts,
        RequestError::ResponseSchemaNotObject,
        "non-object schema",
    );
}

#[test]
fn opaque_refs_reject_blank_control_and_oversized_values() {
    assert_eq!(
        ModelAliasRef::new("  "),
        Err(RequestError::EmptyField {
            field: "model_alias"
        })
    );
    assert_eq!(
        ModelAliasRef::new("has space"),
        Err(RequestError::RefNotOpaque {
            field: "model_alias"
        })
    );
    assert_eq!(
        ModelAliasRef::new("has\nnewline"),
        Err(RequestError::RefNotOpaque {
            field: "model_alias"
        })
    );
    assert!(matches!(
        ModelAliasRef::new("x".repeat(257)),
        Err(RequestError::RefTooLong { .. })
    ));
    assert!(ModelAliasRef::new("gpt-4o-mini").is_ok());

    // The deserialize path runs the identical check.
    assert!(serde_json::from_str::<ModelAliasRef>("\"has space\"").is_err());
    assert!(serde_json::from_str::<IdempotencyKey>("\"  \"").is_err());
}

#[test]
fn an_endpoint_must_be_an_absolute_http_url_with_a_host() {
    for bad in [
        "file:///etc/passwd",
        "ftp://provider.test/v1",
        "/v1/chat/completions",
        "provider.test/v1",
        "https://",
    ] {
        assert!(
            EndpointUrl::new(bad).is_err(),
            "endpoint {bad:?} must be refused: an adapter that is handed a \
             non-addressable endpoint has no honest answer"
        );
    }
    assert!(EndpointUrl::new("http://127.0.0.1:11434/v1/chat/completions").is_ok());
    assert!(EndpointUrl::new(TEST_ENDPOINT).is_ok());
}

#[test]
fn a_blank_caller_ref_is_refused() {
    // Admission refs are recorded provenance; a blank one records nothing and
    // would put an empty string in a durable receipt.
    let mut parts = minimal_parts();
    parts.admitted.caller_ref = "  ".to_string();
    assert_refused_on_both_paths(
        parts,
        RequestError::EmptyField {
            field: "caller_ref",
        },
        "blank caller ref",
    );
}

#[test]
fn required_capabilities_are_derived_from_the_payload() {
    let plain = minimal_request().required_capabilities();
    assert_eq!(
        plain,
        RequiredCapabilities {
            chat: true,
            tools: false,
            streaming: false,
            structured_output: false,
            json_schema: false,
            media: false,
        }
    );

    let mut parts = minimal_parts();
    parts.tools = vec![tool("search")];
    parts.stream = StreamSelection::Enabled;
    parts.response_format = ResponseFormat::JsonSchema {
        name: "answer".to_string(),
        strict: true,
        schema: json!({"type": "object"}),
    };
    parts.messages = vec![CanonicalMessage {
        role: MessageRole::User,
        content: MessageContent::Parts {
            parts: vec![
                ContentPart::Text {
                    text: "what is this".to_string(),
                },
                ContentPart::ImageUrl {
                    url: "https://provider.test/cat.png".to_string(),
                    detail: Some("low".to_string()),
                },
            ],
        },
        tool_call_id: None,
        name: None,
    }];
    let rich = CanonicalInvocationRequest::new(parts)
        .expect("rich request is valid")
        .required_capabilities();
    assert_eq!(
        rich,
        RequiredCapabilities {
            chat: true,
            tools: true,
            streaming: true,
            structured_output: true,
            json_schema: true,
            media: true,
        },
        "a json_schema format must require structured_output too, or a \
         deployment that only does json_object would pass the gate"
    );
}

#[test]
fn a_valid_request_round_trips_through_its_own_serialization() {
    let mut parts = minimal_parts();
    parts.idempotency_key = Some(IdempotencyKey::new("idem-42").expect("valid key"));
    parts.tools = vec![tool("search")];
    parts.tool_choice = ToolChoice::Required {
        tool: "search".to_string(),
    };
    parts.sampling.temperature = Some(0.25);
    parts.sampling.stop = vec!["\n\n".to_string()];
    let request = CanonicalInvocationRequest::new(parts).expect("valid");

    let raw = serde_json::to_string(&request).expect("request serializes");
    let restored: CanonicalInvocationRequest =
        serde_json::from_str(&raw).expect("request round-trips");
    assert_eq!(request, restored);
}
