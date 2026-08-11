//! The canonical invocation request — the Broker's provider-independent
//! request shape, and the vocabulary it is built from.
//!
//! # Two entry points, one type
//!
//! The frozen design gives the gateway two ways in: a caller may name a
//! [`ModelAliasRef`] and let the resolver pick a deployment, or hand over an
//! already-resolved deployment. [`InvocationTarget`] is that fork. A sans-IO
//! adapter can only build bytes for the resolved arm — asked to build from an
//! unresolved alias it refuses *typed*
//! ([`BeforeSendRefusal::UnresolvedTarget`](super::BeforeSendRefusal), never a
//! panic and never a guessed default), because "the resolver has not run yet"
//! is a real state the executor must be able to report.
//!
//! # Bounded by construction
//!
//! Every list and every payload here is capped, and the cap is checked in one
//! fallible constructor that the `Deserialize` path also runs (via
//! [`CanonicalInvocationRequestParts`] and `#[serde(try_from = ...)]`), so a
//! JSON body cannot mint a request the constructor would have refused. An
//! unbounded canonical request would move a denial-of-service surface from the
//! provider — who at least bills for it — to this process.
//!
//! # No credential-shaped field exists here
//!
//! See the module-level two-gate note. This is the type-level half of it: a
//! caller who has passed admission can describe *what* to ask for and *under
//! what constraints*, and has no vocabulary at all for *which credential* to
//! spend. There is no key id, key env, vault alias, rotation member, or
//! account selector field — not optional, not `Option`, not present.

use serde::{Deserialize, Serialize};

/// Maximum messages in one canonical request.
pub const MAX_MESSAGES: usize = 512;
/// Maximum UTF-8 bytes of content in a single message.
pub const MAX_MESSAGE_CONTENT_BYTES: usize = 1_048_576;
/// Maximum UTF-8 bytes of content summed across every message.
pub const MAX_TOTAL_CONTENT_BYTES: usize = 4_194_304;
/// Maximum multi-part fragments in a single message.
pub const MAX_CONTENT_PARTS: usize = 64;
/// Maximum tool declarations in one canonical request.
pub const MAX_TOOLS: usize = 128;
/// Maximum serialized bytes of a single tool's parameter schema.
pub const MAX_TOOL_SCHEMA_BYTES: usize = 65_536;
/// Maximum stop sequences.
pub const MAX_STOP_SEQUENCES: usize = 8;
/// Maximum characters of an opaque reference string (alias, lease, caller).
const MAX_REF_CHARS: usize = 256;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a canonical request (or one of its validated components) was refused.
///
/// Every variant is reachable from *both* a constructor call and a
/// `Deserialize` of the same type — that identity is the point of the parts
/// shadow, and the `*_deserialize_rejects_*` tests assert it payload by
/// payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    /// A required opaque reference string was empty or whitespace-only.
    EmptyField {
        /// The field that was blank, in its wire spelling.
        field: &'static str,
    },
    /// An opaque reference string exceeded [`MAX_REF_CHARS`].
    RefTooLong {
        /// The field that was too long, in its wire spelling.
        field: &'static str,
        /// The rejected length in characters.
        chars: usize,
    },
    /// An opaque reference string contained control or whitespace characters,
    /// which would make it unsafe to echo into a header or a log line.
    RefNotOpaque {
        /// The field that carried the illegal character.
        field: &'static str,
    },
    /// An endpoint was not an absolute `http`/`https` URL with a host.
    InvalidEndpoint {
        /// Why the URL was rejected. Never contains the URL itself: an
        /// endpoint may carry a query-string credential in the wild, so it is
        /// treated as untrusted for logging purposes.
        reason: &'static str,
    },
    /// A request carried no messages. There is no such thing as an empty
    /// chat invocation; an empty list is a caller bug, not a cheap call.
    NoMessages,
    /// More than [`MAX_MESSAGES`] messages.
    TooManyMessages {
        /// The rejected length.
        len: usize,
    },
    /// A single message exceeded [`MAX_MESSAGE_CONTENT_BYTES`].
    MessageTooLarge {
        /// Index of the offending message.
        index: usize,
        /// The rejected size in bytes.
        bytes: usize,
    },
    /// Summed message content exceeded [`MAX_TOTAL_CONTENT_BYTES`].
    TotalContentTooLarge {
        /// The rejected size in bytes.
        bytes: usize,
    },
    /// A message carried more than [`MAX_CONTENT_PARTS`] fragments.
    TooManyContentParts {
        /// Index of the offending message.
        index: usize,
        /// The rejected length.
        len: usize,
    },
    /// More than [`MAX_TOOLS`] tool declarations.
    TooManyTools {
        /// The rejected length.
        len: usize,
    },
    /// A tool's parameter schema exceeded [`MAX_TOOL_SCHEMA_BYTES`].
    ToolSchemaTooLarge {
        /// The tool whose schema was too large.
        tool: String,
        /// The rejected size in bytes.
        bytes: usize,
    },
    /// A tool's parameter schema was not a JSON object. Every wire dialect
    /// this Broker speaks expects an object schema; anything else would be
    /// forwarded verbatim and rejected by the provider at spend time.
    ToolSchemaNotObject {
        /// The tool whose schema was ill-shaped.
        tool: String,
    },
    /// Two tool declarations shared a name. Providers key tool results by
    /// name, so a duplicate makes a returned result unattributable.
    DuplicateToolName {
        /// The repeated name.
        tool: String,
    },
    /// [`ToolChoice::Required`] named a tool the request did not declare.
    ToolChoiceUnknownTool {
        /// The named tool.
        tool: String,
    },
    /// A tool choice was requested without declaring any tool.
    ToolChoiceWithoutTools,
    /// More than [`MAX_STOP_SEQUENCES`] stop sequences.
    TooManyStopSequences {
        /// The rejected length.
        len: usize,
    },
    /// `temperature` was not a finite value in `0.0..=2.0`.
    InvalidTemperature,
    /// `top_p` was not a finite value in `0.0..=1.0`.
    InvalidTopP,
    /// `max_output_tokens` was zero. An explicit zero-token budget is a
    /// request that cannot succeed, so it is refused before it can be spent.
    InvalidMaxOutputTokens,
    /// A structured-output response format carried a schema that was not a
    /// JSON object.
    ResponseSchemaNotObject,
    /// A structured-output response format carried a blank schema name.
    ResponseSchemaNameBlank,
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyField { field } => write!(f, "field `{field}` was blank"),
            Self::RefTooLong { field, chars } => {
                write!(f, "field `{field}` was {chars} chars, cap {MAX_REF_CHARS}")
            }
            Self::RefNotOpaque { field } => {
                write!(f, "field `{field}` contained control or whitespace")
            }
            Self::InvalidEndpoint { reason } => write!(f, "endpoint rejected: {reason}"),
            Self::NoMessages => write!(f, "request carried no messages"),
            Self::TooManyMessages { len } => write!(f, "{len} messages, cap {MAX_MESSAGES}"),
            Self::MessageTooLarge { index, bytes } => write!(
                f,
                "message {index} was {bytes} bytes, cap {MAX_MESSAGE_CONTENT_BYTES}"
            ),
            Self::TotalContentTooLarge { bytes } => {
                write!(f, "{bytes} content bytes, cap {MAX_TOTAL_CONTENT_BYTES}")
            }
            Self::TooManyContentParts { index, len } => {
                write!(f, "message {index} had {len} parts, cap {MAX_CONTENT_PARTS}")
            }
            Self::TooManyTools { len } => write!(f, "{len} tools, cap {MAX_TOOLS}"),
            Self::ToolSchemaTooLarge { tool, bytes } => write!(
                f,
                "tool `{tool}` schema was {bytes} bytes, cap {MAX_TOOL_SCHEMA_BYTES}"
            ),
            Self::ToolSchemaNotObject { tool } => {
                write!(f, "tool `{tool}` schema was not a JSON object")
            }
            Self::DuplicateToolName { tool } => write!(f, "duplicate tool name `{tool}`"),
            Self::ToolChoiceUnknownTool { tool } => {
                write!(f, "tool choice named undeclared tool `{tool}`")
            }
            Self::ToolChoiceWithoutTools => write!(f, "tool choice set with no tools declared"),
            Self::TooManyStopSequences { len } => {
                write!(f, "{len} stop sequences, cap {MAX_STOP_SEQUENCES}")
            }
            Self::InvalidTemperature => write!(f, "temperature must be finite in 0.0..=2.0"),
            Self::InvalidTopP => write!(f, "top_p must be finite in 0.0..=1.0"),
            Self::InvalidMaxOutputTokens => write!(f, "max_output_tokens must be non-zero"),
            Self::ResponseSchemaNotObject => {
                write!(f, "response schema was not a JSON object")
            }
            Self::ResponseSchemaNameBlank => write!(f, "response schema name was blank"),
        }
    }
}

impl std::error::Error for RequestError {}

/// Validates an opaque reference string: non-blank, capped, and free of
/// control/whitespace characters so it can be echoed into a receipt or a
/// header without a second sanitization pass.
fn validated_ref(field: &'static str, raw: &str) -> Result<String, RequestError> {
    if raw.trim().is_empty() {
        return Err(RequestError::EmptyField { field });
    }
    let chars = raw.chars().count();
    if chars > MAX_REF_CHARS {
        return Err(RequestError::RefTooLong { field, chars });
    }
    if raw
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '\u{feff}')
    {
        return Err(RequestError::RefNotOpaque { field });
    }
    Ok(raw.to_string())
}

// ---------------------------------------------------------------------------
// Opaque newtypes
// ---------------------------------------------------------------------------

/// A caller-facing model alias, opaque to this crate.
///
/// The counterpart of the seam's `ModelRef`. It is deliberately *not* a
/// provider model id: resolving an alias to a deployment is #1681's job, and
/// an adapter never sees this arm of [`InvocationTarget`] resolved.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct ModelAliasRef(String);

impl ModelAliasRef {
    /// Validate and wrap an alias.
    pub fn new(raw: impl AsRef<str>) -> Result<Self, RequestError> {
        Ok(Self(validated_ref("model_alias", raw.as_ref())?))
    }

    /// The alias as written by the caller.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ModelAliasRef {
    type Error = RequestError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::new(raw)
    }
}

/// A caller-supplied idempotency key.
///
/// Present so a retry after an *outcome-unknown* disposition can be made safe
/// by a provider that honours the key. It is caller-scoped provenance, not a
/// credential and not a cache key this process interprets.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Validate and wrap an idempotency key.
    pub fn new(raw: impl AsRef<str>) -> Result<Self, RequestError> {
        Ok(Self(validated_ref("idempotency_key", raw.as_ref())?))
    }

    /// The key as written by the caller.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for IdempotencyKey {
    type Error = RequestError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::new(raw)
    }
}

/// An absolute provider endpoint an adapter may build a request against.
///
/// The seam's `ResolvedDeployment::endpoint_ref` is deliberately *opaque* —
/// turning that reference into a concrete URL is the executor's job in a later
/// slice. This type is the already-concrete end of that: validated at
/// construction to be an absolute `http`/`https` URL with a host, so an
/// adapter never has to decide whether the string it was handed is addressable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct EndpointUrl(String);

impl EndpointUrl {
    /// Validate and wrap an endpoint URL.
    pub fn new(raw: impl AsRef<str>) -> Result<Self, RequestError> {
        let raw = raw.as_ref();
        if raw.trim().is_empty() {
            return Err(RequestError::EmptyField { field: "endpoint" });
        }
        let parsed = reqwest::Url::parse(raw).map_err(|_| RequestError::InvalidEndpoint {
            reason: "not a parseable absolute URL",
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(RequestError::InvalidEndpoint {
                reason: "scheme must be http or https",
            });
        }
        if parsed.host_str().is_none_or(str::is_empty) {
            return Err(RequestError::InvalidEndpoint {
                reason: "URL carried no host",
            });
        }
        Ok(Self(raw.to_string()))
    }

    /// The endpoint as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for EndpointUrl {
    type Error = RequestError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::new(raw)
    }
}

// ---------------------------------------------------------------------------
// Target
// ---------------------------------------------------------------------------

/// The wire-level facts an adapter needs in order to build bytes.
///
/// A narrow projection of the seam's `ResolvedDeployment` on purpose: an
/// adapter has no business with the account reference, the pricing reference
/// or the bounds — those are the executor's and the receipt's. Handing the
/// adapter only the endpoint, the provider-side model id and a provenance id
/// is what makes "the adapter cannot select a credential" true by inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ResolvedWireTargetParts")]
pub struct ResolvedWireTarget {
    deployment_id: String,
    endpoint: EndpointUrl,
    provider_model_id: String,
}

/// Constructor input for [`ResolvedWireTarget`], and its deserialization
/// shadow. One type serves both roles so the two paths cannot drift.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResolvedWireTargetParts {
    /// Catalog-controlled deployment identity, opaque here.
    pub deployment_id: String,
    /// The concrete endpoint to address.
    pub endpoint: EndpointUrl,
    /// The provider's own model id, sent on the wire verbatim.
    pub provider_model_id: String,
}

impl ResolvedWireTarget {
    /// Validate and build a resolved target.
    pub fn new(parts: ResolvedWireTargetParts) -> Result<Self, RequestError> {
        Ok(Self {
            deployment_id: validated_ref("deployment_id", &parts.deployment_id)?,
            endpoint: parts.endpoint,
            provider_model_id: validated_ref("provider_model_id", &parts.provider_model_id)?,
        })
    }

    /// Catalog-controlled deployment identity.
    pub fn deployment_id(&self) -> &str {
        &self.deployment_id
    }

    /// The concrete endpoint to address.
    pub fn endpoint(&self) -> &EndpointUrl {
        &self.endpoint
    }

    /// The provider's own model id.
    pub fn provider_model_id(&self) -> &str {
        &self.provider_model_id
    }
}

impl TryFrom<ResolvedWireTargetParts> for ResolvedWireTarget {
    type Error = RequestError;

    fn try_from(parts: ResolvedWireTargetParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}

/// Which of the gateway's two entry points a request came in through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InvocationTarget {
    /// The caller named an alias; the resolver has not run yet.
    #[serde(rename = "model_alias")]
    ModelAlias {
        /// The caller's alias.
        alias: ModelAliasRef,
    },
    /// The caller handed over an already-resolved deployment.
    #[serde(rename = "resolved")]
    Resolved {
        /// The resolved wire facts.
        target: ResolvedWireTarget,
    },
}

impl InvocationTarget {
    /// The resolved target, or `None` when resolution has not happened yet.
    pub fn resolved(&self) -> Option<&ResolvedWireTarget> {
        match self {
            Self::ModelAlias { .. } => None,
            Self::Resolved { target } => Some(target),
        }
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Who authored a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MessageRole {
    /// Operator instructions.
    #[serde(rename = "system")]
    System,
    /// End-user turn.
    #[serde(rename = "user")]
    User,
    /// Model turn.
    #[serde(rename = "assistant")]
    Assistant,
    /// A tool's result, answering an assistant tool call.
    #[serde(rename = "tool")]
    Tool,
}

impl MessageRole {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "system" => Self::System,
            "user" => Self::User,
            "assistant" => Self::Assistant,
            "tool" => Self::Tool,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [MessageRole] =
        &[Self::System, Self::User, Self::Assistant, Self::Tool];
}

/// One fragment of a multi-part message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContentPart {
    /// Plain text.
    #[serde(rename = "text")]
    Text {
        /// The text.
        text: String,
    },
    /// A referenced image. Requires the `media` capability; an adapter whose
    /// deployment lacks it refuses typed rather than dropping the part, since
    /// silently dropping an image changes the question being asked.
    #[serde(rename = "image_url")]
    ImageUrl {
        /// Where the image lives.
        url: String,
        /// Provider-specific detail hint, passed through when present.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

impl ContentPart {
    /// UTF-8 bytes this part contributes to the request's content budget.
    fn content_bytes(&self) -> usize {
        match self {
            Self::Text { text } => text.len(),
            Self::ImageUrl { url, detail } => {
                url.len() + detail.as_ref().map(String::len).unwrap_or(0)
            }
        }
    }

    /// Whether this part needs the `media` capability.
    fn is_media(&self) -> bool {
        matches!(self, Self::ImageUrl { .. })
    }
}

/// A message's payload: either plain text or an ordered part list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageContent {
    /// A single text body.
    #[serde(rename = "text")]
    Text {
        /// The text.
        text: String,
    },
    /// An ordered list of fragments.
    #[serde(rename = "parts")]
    Parts {
        /// The fragments, in order.
        parts: Vec<ContentPart>,
    },
}

impl MessageContent {
    /// UTF-8 bytes this content contributes to the request's budget.
    pub fn content_bytes(&self) -> usize {
        match self {
            Self::Text { text } => text.len(),
            Self::Parts { parts } => parts.iter().map(ContentPart::content_bytes).sum(),
        }
    }

    /// Whether any fragment needs the `media` capability.
    pub fn has_media(&self) -> bool {
        match self {
            Self::Text { .. } => false,
            Self::Parts { parts } => parts.iter().any(ContentPart::is_media),
        }
    }

    fn part_count(&self) -> usize {
        match self {
            Self::Text { .. } => 1,
            Self::Parts { parts } => parts.len(),
        }
    }
}

/// One canonical message.
///
/// No standalone invariant — every bound it participates in
/// ([`MAX_MESSAGE_CONTENT_BYTES`], [`MAX_TOTAL_CONTENT_BYTES`],
/// [`MAX_CONTENT_PARTS`]) is a property of the *request*, checked in
/// [`CanonicalInvocationRequest::new`]. Fields therefore stay public.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalMessage {
    /// Who authored it.
    pub role: MessageRole,
    /// What it says.
    pub content: MessageContent,
    /// For [`MessageRole::Tool`], which tool call this answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Optional participant name, passed through when the dialect has a slot
    /// for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

// ---------------------------------------------------------------------------
// Tools / response format / stream
// ---------------------------------------------------------------------------

/// A tool the model may call.
///
/// No standalone invariant (its caps are request-level), so fields are public.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDeclaration {
    /// Tool name, unique within a request.
    pub name: String,
    /// Human-readable description handed to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON-Schema object describing the parameters.
    pub parameters: serde_json::Value,
}

/// How hard the caller is asking for a tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolChoice {
    /// No preference; the default.
    #[serde(rename = "auto")]
    Auto,
    /// The model must not call a tool.
    #[serde(rename = "none")]
    None,
    /// The model must call some tool.
    #[serde(rename = "any")]
    Any,
    /// The model must call this specific declared tool.
    #[serde(rename = "required")]
    Required {
        /// The tool that must be called.
        tool: String,
    },
}

impl ToolChoice {
    /// Whether this choice expresses a demand for tool calling (and therefore
    /// requires the `tools` capability even when it names nothing).
    fn demands_tools(&self) -> bool {
        !matches!(self, Self::Auto | Self::None)
    }
}

/// The output shape the caller is asking for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponseFormat {
    /// Free text; the default.
    #[serde(rename = "text")]
    Text,
    /// Any syntactically valid JSON object.
    #[serde(rename = "json_object")]
    JsonObject,
    /// A specific JSON Schema, passed through to the provider verbatim.
    #[serde(rename = "json_schema")]
    JsonSchema {
        /// Schema name the provider echoes back.
        name: String,
        /// Whether the provider must enforce the schema strictly.
        strict: bool,
        /// The schema itself, forwarded unmodified.
        schema: serde_json::Value,
    },
}

/// Whether the caller asked for incremental output.
///
/// A separate two-variant enum rather than a `bool` because "the caller asked
/// for streaming and the deployment cannot" is a *typed refusal*, and a bool
/// invites the silent-downgrade bug the protocol law forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamSelection {
    /// One response, one body.
    #[serde(rename = "disabled")]
    Disabled,
    /// Incremental events.
    #[serde(rename = "enabled")]
    Enabled,
}

impl StreamSelection {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Enabled => "enabled",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "disabled" => Self::Disabled,
            "enabled" => Self::Enabled,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [StreamSelection] = &[Self::Disabled, Self::Enabled];

    /// Whether streaming was requested.
    pub fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

/// Sampling knobs, all optional.
///
/// No standalone invariant beyond the ranges, which are checked in
/// [`CanonicalInvocationRequest::new`] so that the deserialize path checks them
/// too; fields stay public.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SamplingParams {
    /// `0.0..=2.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// `0.0..=1.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Non-zero cap on generated tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Up to [`MAX_STOP_SEQUENCES`] stop strings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    /// Provider-side determinism hint, forwarded when the dialect has a slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
}

impl SamplingParams {
    /// Every knob unset — the "provider default" request.
    pub fn none() -> Self {
        Self {
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            stop: Vec::new(),
            seed: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Constraints / context
// ---------------------------------------------------------------------------

/// What the caller demands of how their data is handled.
///
/// These are *caller demands*, and absence of a demand is not a grant: org
/// policy lives in the resolver's data-policy exclusion axis, not here. There
/// is deliberately no `Default`, so a caller (or a future gateway projection)
/// cannot get a permissive posture by forgetting to state one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataPolicyConstraint {
    /// The deployment must not train on this input.
    pub prohibit_training_on_input: bool,
    /// The deployment must not retain this input beyond the call.
    pub prohibit_retention: bool,
    /// An opaque residency requirement the resolver interprets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_residency: Option<String>,
}

/// The spend ceiling this invocation must stay under.
///
/// `pricing_snapshot_ref` is the #1681 coupling point and is opaque here: this
/// slice freezes the *structure* and abstains from cost arithmetic entirely
/// until the catalog lands. An estimate computed against a pricing table that
/// does not exist would be a fabricated number in a durable receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetConstraint {
    /// Ceiling on prompt + completion tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_tokens: Option<u32>,
    /// Ceiling in micro-units of account currency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_micros: Option<u64>,
    /// Opaque reference to the pricing snapshot the ceiling was priced against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_snapshot_ref: Option<String>,
}

impl BudgetConstraint {
    /// No stated ceiling.
    pub fn unbounded() -> Self {
        Self {
            max_total_tokens: None,
            max_cost_micros: None,
            pricing_snapshot_ref: None,
        }
    }
}

/// Wall-clock budget for the invocation.
///
/// Declarative on purpose: a sans-IO adapter holds no clock and no timer, so
/// the deadline travels as data and the executor is the only thing that can
/// enforce it. Exceeding `total_ms` after the request has been accepted is
/// exactly the *outcome-unknown* disposition, never a plain failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadlineContext {
    /// Whole-invocation budget in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_ms: Option<u64>,
    /// Connect-phase budget in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_ms: Option<u64>,
    /// First-byte budget in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_byte_ms: Option<u64>,
}

impl DeadlineContext {
    /// No stated deadline; the executor falls back to its own ceiling.
    pub fn unbounded() -> Self {
        Self {
            total_ms: None,
            connect_ms: None,
            first_byte_ms: None,
        }
    }
}

/// How the caller's cancellation scope binds to this invocation.
///
/// `scope_ref` is opaque: the executor binds a real `CancellationToken` to it
/// at a single point. An adapter can neither hold nor observe a token, which is
/// what keeps cancellation from acquiring a per-provider implementation.
///
/// `retry_on_outcome_unknown` has no `Default` impl for a reason — the safe
/// answer is `false` (a retry after an unknown outcome can double-spend and
/// double-act), and a `Default` that says so is one `..Default::default()`
/// away from being flipped by accident.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancellationContext {
    /// Opaque handle for the caller's cancellation scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_ref: Option<String>,
    /// Whether the caller permits an automatic retry after an *outcome-unknown*
    /// disposition.
    pub retry_on_outcome_unknown: bool,
}

impl CancellationContext {
    /// No scope bound, and no permission to retry an unknown outcome.
    pub fn none() -> Self {
        Self {
            scope_ref: None,
            retry_on_outcome_unknown: false,
        }
    }
}

/// Who the admission layer said is calling.
///
/// **Recorded, never authorizing.** Admission is the UDS peer credential (a
/// later slice); these refs exist so a receipt can say who asked, and are read
/// by nothing that makes an allow/deny decision. Putting them in the request
/// body is safe precisely because nothing downstream treats a body field as
/// authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmittedRefs {
    /// Opaque caller identity established at admission.
    pub caller_ref: String,
    /// Opaque host identity, when the caller declared one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_ref: Option<String>,
    /// Opaque task identity, when the caller declared one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_ref: Option<String>,
}

// ---------------------------------------------------------------------------
// Required capabilities
// ---------------------------------------------------------------------------

/// What a request *needs* a deployment to be able to do.
///
/// Derived from the request's own content rather than declared beside it: a
/// declared requirement can disagree with the payload, and when it does the
/// capability gate checks the wrong thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RequiredCapabilities {
    /// Always true for a chat invocation.
    pub chat: bool,
    /// Tools were declared, or a tool choice demanded one.
    pub tools: bool,
    /// `stream: enabled`.
    pub streaming: bool,
    /// A non-text response format.
    pub structured_output: bool,
    /// Specifically a JSON-Schema response format.
    pub json_schema: bool,
    /// A message carried a non-text part.
    pub media: bool,
}

// ---------------------------------------------------------------------------
// The request
// ---------------------------------------------------------------------------

/// The Broker's provider-independent chat invocation request.
///
/// Private fields, one fallible constructor, read-only accessors, and a
/// `Deserialize` routed through that same constructor — a JSON body cannot
/// mint a request [`CanonicalInvocationRequest::new`] would have refused.
///
/// # No credential-shaped field exists
///
/// A caller cannot name a credential, because there is no field to name one
/// in — the two-gate law's type-level half. These do not compile:
///
/// ```compile_fail
/// use tachi_llm::llm::broker::CanonicalInvocationRequestParts;
///
/// let mut parts: CanonicalInvocationRequestParts = unimplemented!();
/// parts.api_key_env = "TACHI_SILICONFLOW_API_KEY".to_string();
/// ```
///
/// ```compile_fail
/// use tachi_llm::llm::broker::CanonicalInvocationRequestParts;
///
/// let mut parts: CanonicalInvocationRequestParts = unimplemented!();
/// parts.vault_alias = "vault:openai#2".to_string();
/// ```
///
/// ```compile_fail
/// use tachi_llm::llm::broker::CanonicalInvocationRequest;
///
/// let req: CanonicalInvocationRequest = unimplemented!();
/// let _ = req.credential();
/// ```
///
/// Nor can the fields be forged past the constructor:
///
/// ```compile_fail
/// use tachi_llm::llm::broker::{CanonicalInvocationRequest, InvocationTarget};
///
/// let req: CanonicalInvocationRequest = unimplemented!();
/// let forged = CanonicalInvocationRequest {
///     target: req.target().clone(),
///     ..req
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "CanonicalInvocationRequestParts")]
pub struct CanonicalInvocationRequest {
    target: InvocationTarget,
    messages: Vec<CanonicalMessage>,
    tools: Vec<ToolDeclaration>,
    tool_choice: ToolChoice,
    response_format: ResponseFormat,
    stream: StreamSelection,
    sampling: SamplingParams,
    data_policy: DataPolicyConstraint,
    budget: BudgetConstraint,
    #[serde(skip_serializing_if = "Option::is_none")]
    idempotency_key: Option<IdempotencyKey>,
    deadline: DeadlineContext,
    cancellation: CancellationContext,
    admitted: AdmittedRefs,
}

/// Constructor input for [`CanonicalInvocationRequest`], and its
/// deserialization shadow.
///
/// One type serves both roles deliberately: it *is* the wire shape, so
/// `CanonicalInvocationRequest::new(parts)` and
/// `serde_json::from_str::<CanonicalInvocationRequest>` run the identical
/// validation, and there is no second field list to drift.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CanonicalInvocationRequestParts {
    /// Alias or resolved deployment.
    pub target: InvocationTarget,
    /// The conversation, in order. At least one, at most [`MAX_MESSAGES`].
    pub messages: Vec<CanonicalMessage>,
    /// Declared tools, at most [`MAX_TOOLS`].
    #[serde(default)]
    pub tools: Vec<ToolDeclaration>,
    /// Tool-calling pressure.
    #[serde(default = "default_tool_choice")]
    pub tool_choice: ToolChoice,
    /// Requested output shape.
    #[serde(default = "default_response_format")]
    pub response_format: ResponseFormat,
    /// Incremental output selection.
    #[serde(default = "default_stream_selection")]
    pub stream: StreamSelection,
    /// Sampling knobs.
    #[serde(default = "SamplingParams::none")]
    pub sampling: SamplingParams,
    /// Caller data-handling demands.
    pub data_policy: DataPolicyConstraint,
    /// Caller spend ceiling.
    #[serde(default = "BudgetConstraint::unbounded")]
    pub budget: BudgetConstraint,
    /// Optional idempotency key.
    #[serde(default)]
    pub idempotency_key: Option<IdempotencyKey>,
    /// Wall-clock budget.
    #[serde(default = "DeadlineContext::unbounded")]
    pub deadline: DeadlineContext,
    /// Cancellation binding.
    #[serde(default = "CancellationContext::none")]
    pub cancellation: CancellationContext,
    /// Admission-recorded refs.
    pub admitted: AdmittedRefs,
}

fn default_tool_choice() -> ToolChoice {
    ToolChoice::Auto
}

fn default_response_format() -> ResponseFormat {
    ResponseFormat::Text
}

fn default_stream_selection() -> StreamSelection {
    StreamSelection::Disabled
}

impl CanonicalInvocationRequest {
    /// Validate and build a canonical request.
    ///
    /// Every bound in this module is checked here, and only here, so the
    /// constructor and the deserialize path cannot drift apart.
    pub fn new(parts: CanonicalInvocationRequestParts) -> Result<Self, RequestError> {
        let CanonicalInvocationRequestParts {
            target,
            messages,
            tools,
            tool_choice,
            response_format,
            stream,
            sampling,
            data_policy,
            budget,
            idempotency_key,
            deadline,
            cancellation,
            admitted,
        } = parts;

        if messages.is_empty() {
            return Err(RequestError::NoMessages);
        }
        if messages.len() > MAX_MESSAGES {
            return Err(RequestError::TooManyMessages {
                len: messages.len(),
            });
        }
        let mut total_bytes = 0usize;
        for (index, message) in messages.iter().enumerate() {
            let parts_len = message.content.part_count();
            if parts_len > MAX_CONTENT_PARTS {
                return Err(RequestError::TooManyContentParts {
                    index,
                    len: parts_len,
                });
            }
            let bytes = message.content.content_bytes();
            if bytes > MAX_MESSAGE_CONTENT_BYTES {
                return Err(RequestError::MessageTooLarge { index, bytes });
            }
            total_bytes = total_bytes.saturating_add(bytes);
        }
        if total_bytes > MAX_TOTAL_CONTENT_BYTES {
            return Err(RequestError::TotalContentTooLarge { bytes: total_bytes });
        }

        if tools.len() > MAX_TOOLS {
            return Err(RequestError::TooManyTools { len: tools.len() });
        }
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for tool in &tools {
            let name = validated_ref("tool_name", &tool.name)?;
            if !tool.parameters.is_object() {
                return Err(RequestError::ToolSchemaNotObject { tool: name });
            }
            let bytes = serde_json::to_vec(&tool.parameters)
                .map(|v| v.len())
                .unwrap_or(usize::MAX);
            if bytes > MAX_TOOL_SCHEMA_BYTES {
                return Err(RequestError::ToolSchemaTooLarge { tool: name, bytes });
            }
            if !seen.insert(tool.name.as_str()) {
                return Err(RequestError::DuplicateToolName { tool: name });
            }
        }
        if tool_choice.demands_tools() && tools.is_empty() {
            return Err(RequestError::ToolChoiceWithoutTools);
        }
        if let ToolChoice::Required { tool } = &tool_choice {
            if !tools.iter().any(|declared| &declared.name == tool) {
                return Err(RequestError::ToolChoiceUnknownTool { tool: tool.clone() });
            }
        }

        if let ResponseFormat::JsonSchema { name, schema, .. } = &response_format {
            if name.trim().is_empty() {
                return Err(RequestError::ResponseSchemaNameBlank);
            }
            if !schema.is_object() {
                return Err(RequestError::ResponseSchemaNotObject);
            }
        }

        if let Some(temperature) = sampling.temperature {
            if !temperature.is_finite() || !(0.0..=2.0).contains(&temperature) {
                return Err(RequestError::InvalidTemperature);
            }
        }
        if let Some(top_p) = sampling.top_p {
            if !top_p.is_finite() || !(0.0..=1.0).contains(&top_p) {
                return Err(RequestError::InvalidTopP);
            }
        }
        if sampling.max_output_tokens == Some(0) {
            return Err(RequestError::InvalidMaxOutputTokens);
        }
        if sampling.stop.len() > MAX_STOP_SEQUENCES {
            return Err(RequestError::TooManyStopSequences {
                len: sampling.stop.len(),
            });
        }

        let admitted = AdmittedRefs {
            caller_ref: validated_ref("caller_ref", &admitted.caller_ref)?,
            host_ref: admitted
                .host_ref
                .as_deref()
                .map(|raw| validated_ref("host_ref", raw))
                .transpose()?,
            task_ref: admitted
                .task_ref
                .as_deref()
                .map(|raw| validated_ref("task_ref", raw))
                .transpose()?,
        };

        Ok(Self {
            target,
            messages,
            tools,
            tool_choice,
            response_format,
            stream,
            sampling,
            data_policy,
            budget,
            idempotency_key,
            deadline,
            cancellation,
            admitted,
        })
    }

    /// Alias or resolved deployment.
    pub fn target(&self) -> &InvocationTarget {
        &self.target
    }

    /// The conversation, in order.
    pub fn messages(&self) -> &[CanonicalMessage] {
        &self.messages
    }

    /// Declared tools.
    pub fn tools(&self) -> &[ToolDeclaration] {
        &self.tools
    }

    /// Tool-calling pressure.
    pub fn tool_choice(&self) -> &ToolChoice {
        &self.tool_choice
    }

    /// Requested output shape.
    pub fn response_format(&self) -> &ResponseFormat {
        &self.response_format
    }

    /// Incremental output selection.
    pub fn stream(&self) -> StreamSelection {
        self.stream
    }

    /// Sampling knobs.
    pub fn sampling(&self) -> &SamplingParams {
        &self.sampling
    }

    /// Caller data-handling demands.
    pub fn data_policy(&self) -> &DataPolicyConstraint {
        &self.data_policy
    }

    /// Caller spend ceiling.
    pub fn budget(&self) -> &BudgetConstraint {
        &self.budget
    }

    /// Optional idempotency key.
    pub fn idempotency_key(&self) -> Option<&IdempotencyKey> {
        self.idempotency_key.as_ref()
    }

    /// Wall-clock budget.
    pub fn deadline(&self) -> DeadlineContext {
        self.deadline
    }

    /// Cancellation binding.
    pub fn cancellation(&self) -> &CancellationContext {
        &self.cancellation
    }

    /// Admission-recorded refs. Recorded, never authorizing.
    pub fn admitted(&self) -> &AdmittedRefs {
        &self.admitted
    }

    /// What this request needs the deployment to be able to do, derived from
    /// the payload rather than declared beside it.
    pub fn required_capabilities(&self) -> RequiredCapabilities {
        RequiredCapabilities {
            chat: true,
            tools: !self.tools.is_empty() || self.tool_choice.demands_tools(),
            streaming: self.stream.is_enabled(),
            structured_output: !matches!(self.response_format, ResponseFormat::Text),
            json_schema: matches!(self.response_format, ResponseFormat::JsonSchema { .. }),
            media: self.messages.iter().any(|m| m.content.has_media()),
        }
    }
}

impl TryFrom<CanonicalInvocationRequestParts> for CanonicalInvocationRequest {
    type Error = RequestError;

    fn try_from(parts: CanonicalInvocationRequestParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
