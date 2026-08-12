//! The OpenAI-compatible chat dialect.
//!
//! # A fresh implementation, not a refactor
//!
//! `chat_lanes::lane_calls::call_provider_tier` serves all four shipped lanes
//! and is not touched by this module: refactoring it into an adapter would
//! torch fifteen-plus consumers for a leaf whose explicit non-goal is consumer
//! cutover. What *is* carried over is its **classification semantics** — the
//! 429 / 401 / 403-billing / 5xx / empty-content decisions it has accumulated
//! — reimplemented here and pinned by the parity suite in
//! `tests::classification_parity`, which feeds one fixture corpus to both and
//! requires the answers to agree. The old code keeps its behaviour and its
//! callers; this code is proven not to have drifted from it.
//!
//! # Two behaviours deliberately *not* carried over
//!
//! 1. **`enable_thinking: false` for SiliconFlow/Qwen.** That is a
//!    provider-and-model-keyed body patch driven by an environment variable
//!    (`should_disable_thinking`). Per-deployment body quirks belong in the
//!    #1681 catalog, not compiled into a dialect, so the broker path does not
//!    reproduce it. A deployment that needs it gets it from its catalog entry
//!    when that lands; until then this dialect sends a clean body.
//! 2. **Trimming assistant text.** The legacy lane returns
//!    `content.trim()`. Leading and trailing whitespace is meaningful in code
//!    and diff output, so the text is returned verbatim here. The *emptiness
//!    test* still trims — a whitespace-only answer is still an empty answer —
//!    so the empty-content decision, which is the part the parity suite
//!    covers, is unchanged.
//!
//! # Streaming is implemented, and a deployment can still be without it
//!
//! Slice-1 declared `streaming: false` because there was no decoder; slice-2
//! lands [`OpenAiCompatStreamDecoder`] and the ceiling says so. The flip is one
//! change, not two: the capability bit and the decoder must move together, or
//! the dialect is either refusing what it can do or promising what it cannot.
//!
//! What did **not** change is the law underneath. A deployment whose catalog
//! entry says it cannot stream still narrows the adapter
//! ([`OpenAiCompatWire::narrowed_to`] intersects, never unions), and a
//! `stream: enabled` request against it is still refused before send with
//! [`UnsupportedCapability::Streaming`](super::UnsupportedCapability) rather
//! than quietly answered with a whole body. The failure mode that rule exists
//! for — the caller believes it streamed and cannot tell that it did not — is
//! silent, so it is guarded structurally rather than by remembering.
//!
//! # What streaming here does not include
//!
//! `stream_options: {"include_usage": true}` is **not** sent. It is an extra
//! request field several OpenAI-compatible servers reject outright, so adding
//! it is a wire change with its own golden and its own per-deployment question
//! (#1681's catalog), not a side effect of landing a decoder. The consequence
//! is honest and visible: a streamed invocation reports usage only when the
//! provider volunteers it, and `UsageObservationV1::unknown` — not zeros — when
//! it does not.

use serde::Serialize;
use serde_json::Value;

use super::canonical::{
    CanonicalInvocationRequest, ContentPart, MessageContent, ResponseFormat, SamplingParams,
    ToolChoice, ToolDeclaration,
};
use super::disposition::{BeforeSendRefusal, CompletionKindV1, ProtocolViolation};
use super::openai_stream::OpenAiCompatStreamDecoder;
use super::stream::{StreamDecoderUnavailable, StreamDecoderUnavailableReason, WireStreamDecoder};
use super::usage::UsageObservationV1;
use super::wire::{
    AuthMaterialKind, AuthMaterialRef, AuthPlacement, CanonicalAssistantMessage, HttpMethod,
    ProviderErrorClass, ProviderErrorClassification, ProviderResponseMetadata, ProviderWire,
    ResponseHeaders, ToolCallV1, WireCapabilities, WireHeader, WireHttpRequest, WireOutcome,
};

/// The dialect's frozen name, matching the seam's `WireDialect::OpenAiCompat`
/// spelling exactly (not serde's `snake_case` of the variant, which would be
/// `open_ai_compat`).
pub const OPENAI_COMPAT_DIALECT: &str = "openai_compat";

/// How many body bytes classification is allowed to look at.
///
/// A provider body is untrusted input. Classification only ever searches for a
/// handful of well-known billing markers, all of which appear early, so a
/// bounded prefix is enough — and a bound means a hostile or broken provider
/// cannot make this process scan an unbounded string.
const BODY_EXCERPT_BYTES: usize = 4096;

/// The most this dialect can do, before a deployment narrows it.
const DIALECT_CEILING: WireCapabilities = WireCapabilities {
    chat: true,
    embeddings: false,
    tools: true,
    // Slice-2: `new_stream_decoder` hands back a real decoder, so the dialect
    // says it streams. The bit and the decoder move in the same change on
    // purpose — a ceiling that disagrees with what the code can do is wrong in
    // whichever direction it disagrees.
    streaming: true,
    structured_output: true,
    json_schema: true,
    media: true,
};

/// The OpenAI-compatible chat adapter.
///
/// Sans-IO: it holds a capability set and nothing else — no client, no
/// endpoint, no credential. The endpoint travels on the request's resolved
/// target so one adapter instance serves every OpenAI-compatible deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenAiCompatWire {
    capabilities: WireCapabilities,
}

impl Default for OpenAiCompatWire {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAiCompatWire {
    /// The dialect at its full capability ceiling.
    pub const fn new() -> Self {
        Self {
            capabilities: DIALECT_CEILING,
        }
    }

    /// The dialect narrowed to a deployment's declared capabilities.
    ///
    /// **Narrowing only.** The result is the intersection with
    /// [`DIALECT_CEILING`], so a catalog entry (or a test) cannot claim a
    /// capability this code does not actually implement — a deployment row
    /// saying `streaming: true` must not be able to talk this adapter into
    /// emitting `stream: true` with no decoder behind it.
    pub const fn narrowed_to(declared: WireCapabilities) -> Self {
        Self {
            capabilities: WireCapabilities {
                chat: declared.chat && DIALECT_CEILING.chat,
                embeddings: declared.embeddings && DIALECT_CEILING.embeddings,
                tools: declared.tools && DIALECT_CEILING.tools,
                streaming: declared.streaming && DIALECT_CEILING.streaming,
                structured_output: declared.structured_output && DIALECT_CEILING.structured_output,
                json_schema: declared.json_schema
                    && DIALECT_CEILING.json_schema
                    && declared.structured_output,
                media: declared.media && DIALECT_CEILING.media,
            },
        }
    }

    /// The dialect's ceiling, for tests and for the catalog to intersect with.
    pub const fn ceiling() -> WireCapabilities {
        DIALECT_CEILING
    }

    /// A bounded, UTF-8-safe prefix of a response body, for classification.
    pub fn body_excerpt(body: &[u8]) -> String {
        let end = body.len().min(BODY_EXCERPT_BYTES);
        String::from_utf8_lossy(&body[..end]).into_owned()
    }
}

// ---------------------------------------------------------------------------
// Request body
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ChatBody<'a> {
    model: &'a str,
    messages: Vec<WireMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<WireTool<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'static str,
    content: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
}

#[derive(Serialize)]
struct WireTool<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireFunction<'a>,
}

#[derive(Serialize)]
struct WireFunction<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    parameters: &'a Value,
}

fn content_value(content: &MessageContent) -> Value {
    match content {
        MessageContent::Text { text } => Value::String(text.clone()),
        MessageContent::Parts { parts } => Value::Array(
            parts
                .iter()
                .map(|part| match part {
                    ContentPart::Text { text } => {
                        serde_json::json!({"type": "text", "text": text})
                    }
                    ContentPart::ImageUrl { url, detail } => {
                        let mut image = serde_json::Map::new();
                        image.insert("url".to_string(), Value::String(url.clone()));
                        if let Some(detail) = detail {
                            image.insert("detail".to_string(), Value::String(detail.clone()));
                        }
                        serde_json::json!({"type": "image_url", "image_url": Value::Object(image)})
                    }
                })
                .collect(),
        ),
    }
}

fn tools_value(tools: &[ToolDeclaration]) -> Option<Vec<WireTool<'_>>> {
    if tools.is_empty() {
        return None;
    }
    Some(
        tools
            .iter()
            .map(|tool| WireTool {
                kind: "function",
                function: WireFunction {
                    name: &tool.name,
                    description: tool.description.as_deref(),
                    parameters: &tool.parameters,
                },
            })
            .collect(),
    )
}

fn tool_choice_value(choice: &ToolChoice, has_tools: bool) -> Option<Value> {
    match choice {
        // The provider default is `auto`; omitting it keeps the body minimal
        // and identical to what the legacy lane sends.
        ToolChoice::Auto => None,
        ToolChoice::None if !has_tools => None,
        ToolChoice::None => Some(Value::String("none".to_string())),
        ToolChoice::Any => Some(Value::String("required".to_string())),
        ToolChoice::Required { tool } => {
            Some(serde_json::json!({"type": "function", "function": {"name": tool}}))
        }
    }
}

fn response_format_value(format: &ResponseFormat) -> Option<Value> {
    match format {
        ResponseFormat::Text => None,
        ResponseFormat::JsonObject => Some(serde_json::json!({"type": "json_object"})),
        // Passthrough: the caller's schema is forwarded byte-for-byte. This
        // dialect does not rewrite, prune or "fix" a schema — a silently
        // altered schema produces output the caller did not ask for and cannot
        // detect.
        ResponseFormat::JsonSchema {
            name,
            strict,
            schema,
        } => Some(serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": name,
                "schema": schema,
                "strict": strict,
            }
        })),
    }
}

#[allow(clippy::type_complexity)]
fn sampling_fields(
    sampling: &SamplingParams,
) -> (
    Option<f32>,
    Option<f32>,
    Option<u32>,
    Option<&[String]>,
    Option<i64>,
) {
    (
        sampling.temperature,
        sampling.top_p,
        sampling.max_output_tokens,
        if sampling.stop.is_empty() {
            None
        } else {
            Some(sampling.stop.as_slice())
        },
        sampling.seed,
    )
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

/// The dialect's finish-reason vocabulary, mapped once.
///
/// `pub(super)` so the streaming decoder calls it instead of transcribing it:
/// the streamed `finish_reason` and the non-streamed one are the same field
/// with the same meanings, and two copies of "which reasons mean truncated"
/// would drift the first time a provider adds one.
pub(super) fn completion_kind(finish_reason: Option<&str>) -> CompletionKindV1 {
    match finish_reason {
        Some("stop") => CompletionKindV1::Complete,
        Some("length") => CompletionKindV1::Truncated,
        Some("tool_calls") | Some("function_call") => CompletionKindV1::ToolCalls,
        Some("content_filter") => CompletionKindV1::ContentFiltered,
        _ => CompletionKindV1::Unknown,
    }
}

/// The dialect's usage block, read once.
///
/// `pub(super)` for the same reason as [`completion_kind`]: a streamed usage
/// report is the same object in the same shape, and a second reader of it would
/// be a second place for the "unreadable is unobserved" rule below to be
/// forgotten.
pub(super) fn parse_usage(usage: Option<&Value>) -> UsageObservationV1 {
    // `as_u64`, not `as_i64`: a negative token count is not a small number of
    // tokens, it is a number this process could not read, and a provider that
    // sends `-1` must not get it stamped `provider_authoritative` in a durable
    // receipt where a spend ceiling will later read it as fact. Same for a
    // float or a string — unreadable is unobserved.
    let token = |key: &str| {
        usage
            .and_then(|value| value.get(key))
            .and_then(Value::as_u64)
    };
    // The constructor demotes an all-unreadable observation to `unknown()`:
    // claiming authority over three `None`s would let an unknown be mistaken
    // for a report of nothing.
    UsageObservationV1::provider_authoritative(
        token("prompt_tokens"),
        token("completion_tokens"),
        token("total_tokens"),
    )
}

fn parse_tool_calls(choice: &Value) -> Vec<ToolCallV1> {
    choice
        .get("message")
        .and_then(|m| m.get("tool_calls"))
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .filter_map(|call| {
                    let function = call.get("function")?;
                    Some(ToolCallV1 {
                        id: call
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        name: function.get("name").and_then(Value::as_str)?.to_string(),
                        arguments: function
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Mirrors the legacy lane's choice selection: scan the choices for the first
/// one whose content is non-blank. Providers that return `n = 1` — every
/// caller in this repo — take the first choice either way.
fn select_choice(choices: &[Value]) -> Option<&Value> {
    choices
        .iter()
        .find(|choice| {
            choice
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_str)
                .is_some_and(|text| !text.trim().is_empty())
        })
        .or_else(|| choices.first())
}

// ---------------------------------------------------------------------------
// The adapter
// ---------------------------------------------------------------------------

impl ProviderWire for OpenAiCompatWire {
    fn dialect(&self) -> &'static str {
        OPENAI_COMPAT_DIALECT
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

        // This dialect authenticates with a bearer header and nothing else. A
        // lease holding some other material is refused before send rather than
        // sent unauthenticated (which would spend a round trip to learn what
        // the type already said).
        let auth_placement = match auth.kind() {
            AuthMaterialKind::ApiKey | AuthMaterialKind::BearerToken => AuthPlacement::Header {
                name: "authorization",
                prefix: "Bearer ",
            },
            AuthMaterialKind::None => {
                return Err(BeforeSendRefusal::AuthMaterialUnsupported {
                    offered: AuthMaterialKind::None.as_str().to_string(),
                })
            }
        };

        let (temperature, top_p, max_tokens, stop, seed) = sampling_fields(request.sampling());
        let body = ChatBody {
            model: target.provider_model_id(),
            messages: request
                .messages()
                .iter()
                .map(|message| WireMessage {
                    role: message.role.as_str(),
                    content: content_value(&message.content),
                    name: message.name.as_deref(),
                    tool_call_id: message.tool_call_id.as_deref(),
                })
                .collect(),
            tools: tools_value(request.tools()),
            tool_choice: tool_choice_value(request.tool_choice(), !request.tools().is_empty()),
            response_format: response_format_value(request.response_format()),
            temperature,
            top_p,
            max_tokens,
            stop,
            seed,
            stream: request.stream().is_enabled().then_some(true),
        };

        let body = serde_json::to_vec(&body).map_err(|_| {
            // Unreachable for this body shape (every field is a plain value or
            // a caller-supplied `Value` that already deserialized), but a
            // typed refusal beats an unwrap on the spend path.
            BeforeSendRefusal::UnrepresentableRequest {
                detail: "request body could not be serialized",
            }
        })?;

        WireHttpRequest::new(
            HttpMethod::Post,
            target.endpoint().as_str(),
            vec![WireHeader::new("content-type", "application/json")],
            auth_placement,
            body,
        )
    }

    fn parse_response(&self, status: u16, headers: &ResponseHeaders, body: &[u8]) -> WireOutcome {
        if !(200..300).contains(&status) {
            return WireOutcome::Rejected {
                status,
                classification: self.classify_error(status, headers, &Self::body_excerpt(body)),
            };
        }

        let Ok(json) = serde_json::from_slice::<Value>(body) else {
            return WireOutcome::ProtocolViolation {
                violation: ProtocolViolation::MalformedBody {
                    detail: "response body was not valid JSON",
                },
            };
        };

        let Some(choices) = json.get("choices").and_then(Value::as_array) else {
            return WireOutcome::ProtocolViolation {
                violation: ProtocolViolation::SchemaViolation {
                    pointer: "/choices",
                },
            };
        };
        let Some(choice) = select_choice(choices) else {
            return WireOutcome::ProtocolViolation {
                violation: ProtocolViolation::SchemaViolation {
                    pointer: "/choices/0",
                },
            };
        };

        let finish_reason = choice.get("finish_reason").and_then(Value::as_str);
        let completion = completion_kind(finish_reason);
        let text = choice
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let message = CanonicalAssistantMessage {
            text,
            tool_calls: parse_tool_calls(choice),
        };

        if message.is_empty() {
            return WireOutcome::ProtocolViolation {
                // Constructor, not the struct literal: it bounds and sanitizes
                // the one provider-controlled string in the violation
                // vocabulary.
                violation: ProtocolViolation::empty_assistant_content(finish_reason),
            };
        }

        WireOutcome::Completed {
            message,
            completion,
            usage: parse_usage(json.get("usage")),
            metadata: ProviderResponseMetadata {
                effective_model: non_blank(json.get("model")),
                effective_version: non_blank(json.get("system_fingerprint")),
                provider_request_id: non_blank(json.get("id")),
            },
        }
    }

    fn new_stream_decoder(&self) -> Result<Box<dyn WireStreamDecoder>, StreamDecoderUnavailable> {
        // A deployment narrowed to `streaming: false` gets the typed
        // unavailable rather than a decoder it was told it may not use. The
        // reason is `DialectDoesNotStream` — for *this* deployment it does
        // not — and never `NotImplementedYet`, which would now be a lie: the
        // grammar is implemented, this deployment is just not allowed it.
        if !self.capabilities.streaming {
            return Err(StreamDecoderUnavailable {
                reason: StreamDecoderUnavailableReason::DialectDoesNotStream,
                dialect: OPENAI_COMPAT_DIALECT,
            });
        }
        Ok(Box::new(OpenAiCompatStreamDecoder::new()))
    }

    fn classify_error(
        &self,
        status: u16,
        headers: &ResponseHeaders,
        body_excerpt: &str,
    ) -> ProviderErrorClassification {
        // These arms are the semantics carried over from
        // `lane_calls::failure_class_for_status` / `chat_auth_failure_class`,
        // with two members split out (billing from rate-limit, protocol from
        // bad-request). `tests::classification_parity` asserts that projecting
        // this answer back onto the legacy four-member class reproduces the
        // legacy decision for every fixture.
        let class = match status {
            401 => ProviderErrorClass::AuthInvalid,
            403 if is_retriable_billing_failure(body_excerpt) => ProviderErrorClass::BillingOrQuota,
            403 => ProviderErrorClass::AuthInvalid,
            429 => ProviderErrorClass::RateLimited,
            500..=599 => ProviderErrorClass::ServerError,
            // A success status reaching the classifier means the body was
            // unusable — the provider answered, in the wrong grammar.
            200..=299 => ProviderErrorClass::Protocol,
            _ => ProviderErrorClass::BadRequest,
        };
        ProviderErrorClassification::new(class, headers.retry_after())
    }
}

/// A trimmed, non-empty string field, or nothing.
pub(super) fn non_blank(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// The legacy lane's billing/quota body test, carried over verbatim.
///
/// Kept byte-identical to `lane_calls::is_retriable_billing_failure` on
/// purpose: it is the one place where classification depends on provider prose
/// rather than on a status code, so any divergence would be a silent
/// behavioural change that only shows up on a customer's dead account. The
/// parity suite pins it against the original's source text.
pub(super) fn is_retriable_billing_failure(resp_text: &str) -> bool {
    let lower = resp_text.to_ascii_lowercase();
    (lower.contains("balance") && lower.contains("insufficient"))
        || lower.contains("insufficient balance")
        || lower.contains("billing")
        || lower.contains("quota exceeded")
        || lower.contains("余额不足")
}
