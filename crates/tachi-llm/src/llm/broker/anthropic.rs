//! The Anthropic Messages wire adapter.
//!
//! The stream grammar already lives in [`super::anthropic_stream`]; this file
//! adds the other three pure functions so Anthropic becomes a production
//! [`ProviderWire`](super::ProviderWire) rather than a decoder-only divergence
//! probe.

use serde::Serialize;
use serde_json::Value;

use super::anthropic_stream::AnthropicEventStreamDecoder;
use super::canonical::{
    CanonicalInvocationRequest, CanonicalMessage, ContentPart, MessageContent, MessageRole,
    ResponseFormat, ToolChoice, ToolDeclaration,
};
use super::disposition::{BeforeSendRefusal, CompletionKindV1, ProtocolViolation};
use super::stream::{StreamDecoderUnavailable, StreamDecoderUnavailableReason, WireStreamDecoder};
use super::usage::UsageObservationV1;
use super::wire::{
    AuthMaterialKind, AuthMaterialRef, AuthPlacement, CanonicalAssistantMessage, HttpMethod,
    ProviderErrorClass, ProviderErrorClassification, ProviderResponseMetadata, ProviderWire,
    ResponseHeaders, ToolCallV1, WireCapabilities, WireHeader, WireHttpRequest, WireOutcome,
};

/// Matches `WireDialect::Anthropic`.
pub const ANTHROPIC_DIALECT: &str = "anthropic";

/// Anthropic's request header version, required on every request.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The most this adapter can do.
const DIALECT_CEILING: WireCapabilities = WireCapabilities {
    chat: true,
    embeddings: false,
    tools: true,
    streaming: true,
    structured_output: false,
    json_schema: false,
    media: true,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnthropicWire {
    capabilities: WireCapabilities,
}

impl Default for AnthropicWire {
    fn default() -> Self {
        Self::new()
    }
}

impl AnthropicWire {
    pub const fn new() -> Self {
        Self {
            capabilities: DIALECT_CEILING,
        }
    }

    pub const fn narrowed_to(declared: WireCapabilities) -> Self {
        Self {
            capabilities: WireCapabilities {
                chat: declared.chat && DIALECT_CEILING.chat,
                embeddings: declared.embeddings && DIALECT_CEILING.embeddings,
                tools: declared.tools && DIALECT_CEILING.tools,
                streaming: declared.streaming && DIALECT_CEILING.streaming,
                structured_output: declared.structured_output && DIALECT_CEILING.structured_output,
                json_schema: declared.json_schema && DIALECT_CEILING.json_schema,
                media: declared.media && DIALECT_CEILING.media,
            },
        }
    }

    pub const fn ceiling() -> WireCapabilities {
        DIALECT_CEILING
    }
}

#[derive(Serialize)]
struct MessagesBody<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<Vec<AnthropicContentBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: &'static str,
    content: Vec<AnthropicContentBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text {
        text: String,
    },
    Image {
        source: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Serialize)]
struct AnthropicTool<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    input_schema: &'a Value,
}

fn text_block(text: impl Into<String>) -> AnthropicContentBlock {
    AnthropicContentBlock::Text { text: text.into() }
}

fn user_content_blocks(content: &MessageContent) -> Vec<AnthropicContentBlock> {
    match content {
        MessageContent::Text { text } => vec![text_block(text.clone())],
        MessageContent::Parts { parts } => parts
            .iter()
            .map(|part| match part {
                ContentPart::Text { text } => text_block(text.clone()),
                ContentPart::ImageUrl { url, .. } => AnthropicContentBlock::Image {
                    source: serde_json::json!({"type": "url", "url": url}),
                },
            })
            .collect(),
    }
}

fn text_content_blocks(
    content: &MessageContent,
    image_detail: &'static str,
) -> Result<Vec<AnthropicContentBlock>, BeforeSendRefusal> {
    match content {
        MessageContent::Text { text } => Ok(vec![text_block(text.clone())]),
        MessageContent::Parts { parts } => parts
            .iter()
            .map(|part| match part {
                ContentPart::Text { text } => Ok(text_block(text.clone())),
                ContentPart::ImageUrl { .. } => Err(BeforeSendRefusal::UnrepresentableRequest {
                    detail: image_detail,
                }),
            })
            .collect(),
    }
}

fn request_messages(
    messages: &[CanonicalMessage],
) -> Result<Vec<AnthropicMessage>, BeforeSendRefusal> {
    let mut out = Vec::new();
    for message in messages {
        if message.name.is_some() {
            return Err(BeforeSendRefusal::UnrepresentableRequest {
                detail: "Anthropic Messages has no slot for CanonicalMessage.name",
            });
        }
        let blocks = match message.role {
            MessageRole::System => {
                text_content_blocks(
                    &message.content,
                    "Anthropic system content cannot contain images",
                )?;
                continue;
            }
            MessageRole::Tool => vec![AnthropicContentBlock::ToolResult {
                tool_use_id: message.tool_call_id.clone().ok_or(
                    BeforeSendRefusal::UnrepresentableRequest {
                        detail: "a tool result message named no tool call id",
                    },
                )?,
                content: match &message.content {
                    MessageContent::Text { text } => text.clone(),
                    MessageContent::Parts { .. } => {
                        return Err(BeforeSendRefusal::UnrepresentableRequest {
                            detail: "Anthropic tool results are text-only in this broker slice",
                        })
                    }
                },
            }],
            MessageRole::User => user_content_blocks(&message.content),
            MessageRole::Assistant => text_content_blocks(
                &message.content,
                "Anthropic assistant content cannot contain images",
            )?,
        };
        out.push(AnthropicMessage {
            role: match message.role {
                MessageRole::User | MessageRole::Tool => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::System => unreachable!(),
            },
            content: blocks,
        });
    }
    Ok(out)
}

fn system_blocks(
    messages: &[CanonicalMessage],
) -> Result<Option<Vec<AnthropicContentBlock>>, BeforeSendRefusal> {
    let mut out = Vec::new();
    for message in messages
        .iter()
        .filter(|message| message.role == MessageRole::System)
    {
        out.extend(text_content_blocks(
            &message.content,
            "Anthropic system content cannot contain images",
        )?);
    }
    Ok((!out.is_empty()).then_some(out))
}

fn tools_value(tools: &[ToolDeclaration]) -> Option<Vec<AnthropicTool<'_>>> {
    if tools.is_empty() {
        return None;
    }
    Some(
        tools
            .iter()
            .map(|tool| AnthropicTool {
                name: &tool.name,
                description: tool.description.as_deref(),
                input_schema: &tool.parameters,
            })
            .collect(),
    )
}

fn tool_choice_value(choice: &ToolChoice, has_tools: bool) -> Option<Value> {
    match choice {
        ToolChoice::Auto => None,
        ToolChoice::None if !has_tools => None,
        ToolChoice::None => Some(serde_json::json!({"type": "none"})),
        ToolChoice::Any => Some(serde_json::json!({"type": "any"})),
        ToolChoice::Required { tool } => Some(serde_json::json!({"type": "tool", "name": tool})),
    }
}

fn completion_kind(stop_reason: Option<&str>) -> CompletionKindV1 {
    match stop_reason {
        Some("end_turn") | Some("stop_sequence") => CompletionKindV1::Complete,
        Some("max_tokens") => CompletionKindV1::Truncated,
        Some("tool_use") | Some("pause_turn") => CompletionKindV1::ToolCalls,
        Some("refusal") => CompletionKindV1::ContentFiltered,
        _ => CompletionKindV1::Unknown,
    }
}

fn parse_usage(usage: Option<&Value>) -> UsageObservationV1 {
    let token = |key: &str| {
        usage
            .and_then(|value| value.get(key))
            .and_then(Value::as_u64)
    };
    UsageObservationV1::provider_authoritative(
        token("input_tokens"),
        token("output_tokens"),
        token("total_tokens"),
    )
}

fn parse_tool_call(block: &Value) -> Result<ToolCallV1, &'static str> {
    let id = block
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or("/content/0/id")?;
    let name = block
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .ok_or("/content/0/name")?;
    let input = block
        .get("input")
        .filter(|input| input.is_object())
        .ok_or("/content/0/input")?;
    Ok(ToolCallV1 {
        id: id.to_string(),
        name: name.to_string(),
        arguments: serde_json::to_string(input).map_err(|_| "/content/0/input")?,
    })
}

impl ProviderWire for AnthropicWire {
    fn dialect(&self) -> &'static str {
        ANTHROPIC_DIALECT
    }

    fn capabilities(&self) -> WireCapabilities {
        self.capabilities
    }

    fn build_request(
        &self,
        request: &CanonicalInvocationRequest,
        auth: AuthMaterialRef<'_>,
    ) -> Result<WireHttpRequest, BeforeSendRefusal> {
        let Some(target) = request.target().resolved() else {
            return Err(BeforeSendRefusal::UnresolvedTarget);
        };
        let missing = self
            .capabilities
            .missing_for(&request.required_capabilities());
        if !missing.is_empty() {
            return Err(BeforeSendRefusal::UnsupportedCapability { missing });
        }
        if !matches!(request.response_format(), ResponseFormat::Text) {
            return Err(BeforeSendRefusal::UnsupportedCapability {
                missing: vec![super::disposition::UnsupportedCapability::StructuredOutput],
            });
        }
        let max_tokens = request.sampling().max_output_tokens.ok_or(
            BeforeSendRefusal::UnrepresentableRequest {
                detail: "Anthropic requires max_output_tokens to build a Messages request",
            },
        )?;
        let auth_placement = match auth.kind() {
            AuthMaterialKind::ApiKey => AuthPlacement::Header {
                name: "x-api-key",
                prefix: "",
            },
            _ => {
                return Err(BeforeSendRefusal::AuthMaterialUnsupported {
                    offered: auth.kind().as_str().to_string(),
                })
            }
        };
        let stop_sequences = if request.sampling().stop.is_empty() {
            None
        } else {
            Some(request.sampling().stop.as_slice())
        };
        let body = MessagesBody {
            model: target.provider_model_id(),
            max_tokens,
            messages: request_messages(request.messages())?,
            system: system_blocks(request.messages())?,
            tools: tools_value(request.tools()),
            tool_choice: tool_choice_value(request.tool_choice(), !request.tools().is_empty()),
            temperature: request.sampling().temperature,
            top_p: request.sampling().top_p,
            stop_sequences,
            stream: request.stream().is_enabled().then_some(true),
        };
        let body =
            serde_json::to_vec(&body).map_err(|_| BeforeSendRefusal::UnrepresentableRequest {
                detail: "request body could not be serialized",
            })?;
        WireHttpRequest::new(
            HttpMethod::Post,
            target.endpoint().as_str(),
            vec![
                WireHeader::new("content-type", "application/json"),
                WireHeader::new("anthropic-version", ANTHROPIC_VERSION),
            ],
            auth_placement,
            body,
        )
    }

    fn parse_response(&self, status: u16, headers: &ResponseHeaders, body: &[u8]) -> WireOutcome {
        if !(200..300).contains(&status) {
            let excerpt = String::from_utf8_lossy(&body).into_owned();
            return WireOutcome::Rejected {
                status,
                classification: self.classify_error(status, headers, &excerpt),
            };
        }
        let Ok(json) = serde_json::from_slice::<Value>(body) else {
            return WireOutcome::ProtocolViolation {
                violation: ProtocolViolation::MalformedBody {
                    detail: "response body was not valid JSON",
                },
            };
        };
        let Some(content) = json.get("content").and_then(Value::as_array) else {
            return WireOutcome::ProtocolViolation {
                violation: ProtocolViolation::SchemaViolation {
                    pointer: "/content",
                },
            };
        };

        let mut text = String::new();
        let mut tool_calls = Vec::new();
        for block in content {
            let Some(kind) = block.get("type").and_then(Value::as_str) else {
                return WireOutcome::ProtocolViolation {
                    violation: ProtocolViolation::SchemaViolation {
                        pointer: "/content/0/type",
                    },
                };
            };
            match kind {
                "text" => {
                    if let Some(seen) = block.get("text").and_then(Value::as_str) {
                        text.push_str(seen);
                    }
                }
                "tool_use" => {
                    let call = match parse_tool_call(block) {
                        Ok(call) => call,
                        Err(pointer) => {
                            return WireOutcome::ProtocolViolation {
                                violation: ProtocolViolation::SchemaViolation { pointer },
                            };
                        }
                    };
                    tool_calls.push(call);
                }
                _ => {}
            }
        }

        let message = CanonicalAssistantMessage {
            text: (!text.is_empty()).then_some(text),
            tool_calls,
        };
        let stop_reason = json.get("stop_reason").and_then(Value::as_str);
        if message.is_empty() {
            return WireOutcome::ProtocolViolation {
                violation: ProtocolViolation::empty_assistant_content(stop_reason),
            };
        }
        WireOutcome::Completed {
            message,
            completion: completion_kind(stop_reason),
            usage: parse_usage(json.get("usage")),
            metadata: ProviderResponseMetadata {
                effective_model: super::openai_compat::non_blank(json.get("model")),
                effective_version: None,
                provider_request_id: super::openai_compat::non_blank(json.get("id")),
            },
        }
    }

    fn new_stream_decoder(&self) -> Result<Box<dyn WireStreamDecoder>, StreamDecoderUnavailable> {
        if !self.capabilities.streaming {
            return Err(StreamDecoderUnavailable {
                reason: StreamDecoderUnavailableReason::DialectDoesNotStream,
                dialect: ANTHROPIC_DIALECT,
            });
        }
        Ok(Box::new(AnthropicEventStreamDecoder::new()))
    }

    fn classify_error(
        &self,
        status: u16,
        headers: &ResponseHeaders,
        _body_excerpt: &str,
    ) -> ProviderErrorClassification {
        let class = match status {
            401 | 403 => ProviderErrorClass::AuthInvalid,
            429 => ProviderErrorClass::RateLimited,
            500..=599 => ProviderErrorClass::ServerError,
            200..=299 => ProviderErrorClass::Protocol,
            _ => ProviderErrorClass::BadRequest,
        };
        ProviderErrorClassification::new(class, headers.retry_after())
    }
}
