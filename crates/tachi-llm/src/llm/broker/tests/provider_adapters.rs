use super::stream_transcripts::{decode_transcript, TerminalInput};
use super::*;

fn anthropic_request() -> CanonicalInvocationRequest {
    let mut parts = minimal_parts();
    parts.messages = vec![
        CanonicalMessage {
            role: MessageRole::System,
            content: MessageContent::Text {
                text: "You are terse.".to_string(),
            },
            tool_call_id: None,
            name: None,
        },
        user_message("Look at this"),
        CanonicalMessage {
            role: MessageRole::User,
            content: MessageContent::Parts {
                parts: vec![
                    ContentPart::ImageUrl {
                        url: "https://example.test/cat.png".to_string(),
                        detail: Some("high".to_string()),
                    },
                    ContentPart::Text {
                        text: "Caption it".to_string(),
                    },
                ],
            },
            tool_call_id: None,
            name: None,
        },
    ];
    parts.tools = vec![ToolDeclaration {
        name: "weather".to_string(),
        description: Some("Read the weather".to_string()),
        parameters: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
    }];
    parts.tool_choice = ToolChoice::Required {
        tool: "weather".to_string(),
    };
    parts.stream = StreamSelection::Enabled;
    parts.sampling = SamplingParams {
        temperature: Some(0.2),
        top_p: Some(0.9),
        max_output_tokens: Some(256),
        stop: vec!["DONE".to_string()],
        seed: None,
    };
    CanonicalInvocationRequest::new(parts).expect("anthropic fixture request must be valid")
}

fn anthropic_request_with_message(message: CanonicalMessage) -> CanonicalInvocationRequest {
    let mut parts = minimal_parts();
    parts.messages = vec![message];
    parts.sampling.max_output_tokens = Some(32);
    CanonicalInvocationRequest::new(parts).expect("Anthropic negative fixture must be canonical")
}

fn image_message(role: MessageRole) -> CanonicalMessage {
    CanonicalMessage {
        role,
        content: MessageContent::Parts {
            parts: vec![ContentPart::ImageUrl {
                url: "https://example.test/forbidden.png".to_string(),
                detail: None,
            }],
        },
        tool_call_id: None,
        name: None,
    }
}

#[test]
fn anthropic_builds_a_messages_request_without_executor_state() {
    let built = AnthropicWire::new()
        .build_request(&anthropic_request(), api_key_lease())
        .expect("anthropic request should build");
    let projected = serde_json::from_slice::<Value>(built.body()).expect("JSON body");
    assert_eq!(built.method(), HttpMethod::Post);
    assert_eq!(built.url(), TEST_ENDPOINT);
    assert_eq!(
        serde_json::to_value(built.auth_placement()).expect("serializes"),
        json!({"kind": "header", "name": "x-api-key", "prefix": ""})
    );
    assert_eq!(
        built
            .headers()
            .iter()
            .map(|header| (header.name(), header.value()))
            .collect::<Vec<_>>(),
        vec![
            ("content-type", "application/json"),
            ("anthropic-version", "2023-06-01"),
        ]
    );
    assert_eq!(
        projected,
        json!({
            "model": "test-model-1",
            "max_tokens": 256,
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "Look at this"}]},
                {"role": "user", "content": [
                    {"type": "image", "source": {"type": "url", "url": "https://example.test/cat.png"}},
                    {"type": "text", "text": "Caption it"}
                ]}
            ],
            "system": [{"type": "text", "text": "You are terse."}],
            "tools": [{
                "name": "weather",
                "description": "Read the weather",
                "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}
            }],
            "tool_choice": {"type": "tool", "name": "weather"},
            "temperature": 0.2,
            "top_p": 0.9,
            "stop_sequences": ["DONE"],
            "stream": true
        })
    );
}

#[test]
fn anthropic_tool_results_refuse_until_history_can_bind_the_assistant_call() {
    let mut parts = minimal_parts();
    parts.messages.push(CanonicalMessage {
        role: MessageRole::Tool,
        content: MessageContent::Text {
            text: "{\"result\":\"warm\"}".to_string(),
        },
        tool_call_id: Some("toolu_1".to_string()),
        name: None,
    });
    parts.sampling.max_output_tokens = Some(32);
    let request = CanonicalInvocationRequest::new(parts).expect("canonical tool result fixture");
    let expected = BeforeSendRefusal::UnrepresentableRequest {
        detail: "canonical history cannot yet bind a tool result to its assistant tool call",
    };

    assert_eq!(
        AnthropicWire::new()
            .build_request(&request, api_key_lease())
            .expect_err("Anthropic must not send an orphan tool result"),
        expected
    );
}

#[test]
fn anthropic_refuses_to_move_a_mid_conversation_system_turn_to_the_prefix() {
    let mut parts = minimal_parts();
    parts.messages.push(CanonicalMessage {
        role: MessageRole::System,
        content: MessageContent::Text {
            text: "From now on, answer in JSON.".to_string(),
        },
        tool_call_id: None,
        name: None,
    });
    parts.sampling.max_output_tokens = Some(32);
    let request =
        CanonicalInvocationRequest::new(parts).expect("canonical mid-conversation system turn");

    assert_eq!(
        AnthropicWire::new()
            .build_request(&request, api_key_lease())
            .expect_err("Anthropic must not reorder a system turn"),
        BeforeSendRefusal::UnrepresentableRequest {
            detail:
                "Anthropic mid-conversation system turns are not supported by every resolved model",
        }
    );
}

#[test]
fn anthropic_refuses_a_final_assistant_prefill_without_a_model_capability() {
    let mut parts = minimal_parts();
    parts.messages.push(CanonicalMessage {
        role: MessageRole::Assistant,
        content: MessageContent::Text {
            text: "The answer is".to_string(),
        },
        tool_call_id: None,
        name: None,
    });
    parts.sampling.max_output_tokens = Some(32);
    let request = CanonicalInvocationRequest::new(parts).expect("canonical assistant prefill");

    assert_eq!(
        AnthropicWire::new()
            .build_request(&request, api_key_lease())
            .expect_err("Anthropic prefill must fail closed"),
        BeforeSendRefusal::UnrepresentableRequest {
            detail: "Anthropic assistant prefill is not supported by every resolved model",
        }
    );
}

#[test]
fn anthropic_requires_a_max_output_cap_and_an_api_key() {
    let mut parts = minimal_parts();
    parts.sampling = SamplingParams::none();
    let request = CanonicalInvocationRequest::new(parts).expect("valid request");
    assert_eq!(
        AnthropicWire::new()
            .build_request(&request, api_key_lease())
            .expect_err("Anthropic requires max_tokens"),
        BeforeSendRefusal::UnrepresentableRequest {
            detail: "Anthropic requires max_output_tokens to build a Messages request",
        }
    );
    let request = anthropic_request();
    assert_eq!(
        AnthropicWire::new()
            .build_request(&request, AuthMaterialRef::none())
            .expect_err("no auth must be refused"),
        BeforeSendRefusal::AuthMaterialUnsupported {
            offered: "none".to_string(),
        }
    );
}

#[test]
fn anthropic_rejects_system_images_before_send() {
    let request = anthropic_request_with_message(image_message(MessageRole::System));
    assert_eq!(
        AnthropicWire::new()
            .build_request(&request, api_key_lease())
            .expect_err("system images have no Anthropic representation"),
        BeforeSendRefusal::UnrepresentableRequest {
            detail: "Anthropic system content cannot contain images",
        }
    );
}

#[test]
fn anthropic_rejects_assistant_images_before_send() {
    let request = anthropic_request_with_message(image_message(MessageRole::Assistant));
    assert_eq!(
        AnthropicWire::new()
            .build_request(&request, api_key_lease())
            .expect_err("assistant images have no Anthropic representation"),
        BeforeSendRefusal::UnrepresentableRequest {
            detail: "Anthropic assistant content cannot contain images",
        }
    );
}

#[test]
fn anthropic_rejects_canonical_message_names_before_send() {
    let mut message = user_message("hello");
    message.name = Some("participant".to_string());
    let request = anthropic_request_with_message(message);
    assert_eq!(
        AnthropicWire::new()
            .build_request(&request, api_key_lease())
            .expect_err("message names have no Anthropic slot"),
        BeforeSendRefusal::UnrepresentableRequest {
            detail: "Anthropic Messages has no slot for CanonicalMessage.name",
        }
    );
}

#[test]
fn anthropic_parses_text_and_tool_use_without_leaking_provider_prose() {
    let outcome = AnthropicWire::new().parse_response(
        200,
        &ResponseHeaders::new(),
        br#"{
            "id":"msg_1",
            "model":"claude-test-1",
            "content":[
                {"type":"text","text":"Searching..."},
                {"type":"tool_use","id":"toolu_1","name":"search","input":{"q":"cats"}}
            ],
            "stop_reason":"tool_use",
            "usage":{"input_tokens":0,"output_tokens":15}
        }"#,
    );
    assert_golden(
        "anthropic response",
        "completed tool use",
        &outcome,
        &json!({
            "outcome": "completed",
            "message": {
                "text": "Searching...",
                "tool_calls": [{
                    "id": "toolu_1",
                    "name": "search",
                    "arguments": "{\"q\":\"cats\"}"
                }]
            },
            "completion": "tool_calls",
            "usage": {
                "prompt_tokens": 0,
                "completion_tokens": 15,
                "provenance": "provider_authoritative"
            },
            "metadata": {
                "effective_model": "claude-test-1",
                "provider_request_id": "msg_1"
            }
        }),
    );
}

#[test]
fn anthropic_empty_content_is_a_protocol_violation_even_with_stop_reason() {
    let outcome = AnthropicWire::new().parse_response(
        200,
        &ResponseHeaders::new(),
        br#"{"id":"msg_2","model":"claude-test-2","content":[],"stop_reason":"refusal","usage":{"input_tokens":3}}"#,
    );
    assert_golden(
        "anthropic response",
        "empty assistant content",
        &outcome,
        &json!({
            "outcome": "protocol_violation",
            "violation": {
                "kind": "empty_assistant_content",
                "finish_reason": "refusal"
            }
        }),
    );
}

fn assert_anthropic_tool_use_schema_violation(body: &[u8], pointer: &'static str) {
    assert_eq!(
        AnthropicWire::new().parse_response(200, &ResponseHeaders::new(), body),
        WireOutcome::ProtocolViolation {
            violation: ProtocolViolation::SchemaViolation { pointer },
        }
    );
}

#[test]
fn anthropic_tool_use_missing_id_is_a_protocol_violation() {
    assert_anthropic_tool_use_schema_violation(
        br#"{"content":[{"type":"tool_use","name":"search","input":{}}]}"#,
        "/content/0/id",
    );
}

#[test]
fn anthropic_tool_use_blank_id_is_a_protocol_violation() {
    assert_anthropic_tool_use_schema_violation(
        br#"{"content":[{"type":"tool_use","id":"  ","name":"search","input":{}}]}"#,
        "/content/0/id",
    );
}

#[test]
fn anthropic_tool_use_blank_name_is_a_protocol_violation() {
    assert_anthropic_tool_use_schema_violation(
        br#"{"content":[{"type":"tool_use","id":"toolu_1","name":"\t","input":{}}]}"#,
        "/content/0/name",
    );
}

#[test]
fn anthropic_tool_use_non_object_input_is_a_protocol_violation() {
    assert_anthropic_tool_use_schema_violation(
        br#"{"content":[{"type":"tool_use","id":"toolu_1","name":"search","input":[]}]}"#,
        "/content/0/input",
    );
}

#[test]
fn family_wrappers_share_the_openai_wire_but_keep_distinct_dialects() {
    let request = minimal_request();
    let auth = api_key_lease();
    let baseline = OpenAiCompatWire::new()
        .build_request(&request, auth)
        .expect("baseline must build");
    let xai = XaiWire::new()
        .build_request(&request, auth)
        .expect("xai request");
    assert_eq!(
        xai.body_utf8(),
        baseline.body_utf8(),
        "{XAI_DIALECT}: request bytes drifted"
    );
    XaiWire::new()
        .new_stream_decoder()
        .expect("xai streams through the shared grammar");

    let open_router = OpenRouterWire::new()
        .build_request(&request, auth)
        .expect("openrouter request");
    assert_eq!(
        open_router.body_utf8(),
        baseline.body_utf8(),
        "{OPEN_ROUTER_DIALECT}: request bytes drifted"
    );
    OpenRouterWire::new()
        .new_stream_decoder()
        .expect("openrouter streams through the shared grammar");

    let generic = GenericCompatWire::new()
        .build_request(&request, auth)
        .expect("generic request");
    assert_eq!(
        generic.body_utf8(),
        baseline.body_utf8(),
        "{GENERIC_COMPAT_DIALECT}: request bytes drifted"
    );
    GenericCompatWire::new()
        .new_stream_decoder()
        .expect("generic streams through the shared grammar");

    let transcript = decode_transcript(
        "openai_compat_sse",
        &[
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n"
                .to_vec(),
            b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n".to_vec(),
            b"data: [DONE]\n\n".to_vec(),
        ],
        TerminalInput::Finish(StreamEof::Clean),
    );
    assert_eq!(
        transcript.error, None,
        "the shared grammar must still decode for every family wrapper"
    );
    assert_eq!(XaiWire::new().dialect(), "xai");
    assert_eq!(OpenRouterWire::new().dialect(), "open_router");
    assert_eq!(GenericCompatWire::new().dialect(), "generic_compat");

    let unavailable = [
        (
            XAI_DIALECT,
            XaiWire::narrowed_to(WireCapabilities::default())
                .new_stream_decoder()
                .err()
                .expect("narrowed xAI must refuse streaming")
                .dialect,
        ),
        (
            OPEN_ROUTER_DIALECT,
            OpenRouterWire::narrowed_to(WireCapabilities::default())
                .new_stream_decoder()
                .err()
                .expect("narrowed OpenRouter must refuse streaming")
                .dialect,
        ),
        (
            GENERIC_COMPAT_DIALECT,
            GenericCompatWire::narrowed_to(WireCapabilities::default())
                .new_stream_decoder()
                .err()
                .expect("narrowed generic compat must refuse streaming")
                .dialect,
        ),
    ];
    for (expected, actual) in unavailable {
        assert_eq!(actual, expected);
    }
}
