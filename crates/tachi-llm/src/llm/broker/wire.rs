//! [`ProviderWire`] — the sans-IO adapter trait — and the wire types it maps
//! between.
//!
//! # What "sans-IO" buys, concretely
//!
//! An adapter is four pure functions. It cannot open a socket, cannot sleep
//! for a backoff, cannot reach a credential and cannot record health. Those
//! are not restrictions the reviewer has to enforce; they are things the
//! signatures make impossible. What follows from that:
//!
//! - **One HTTP pool, structurally.** The pooled client (with `no_proxy` from
//!   #1621 and the self-healing rebuild) lives in the executor. An adapter has
//!   nowhere to put a second one.
//! - **One cancellation point.** The token is selected against in the executor,
//!   once, not once per provider.
//! - **Conformance testing is fixture testing.** The fake adapter and the real
//!   one are the same code; only the executor differs.
//!
//! # What sans-IO does *not* cover — say it out loud
//!
//! The fixture matrix proves the *wire mapping*. It proves nothing about
//! connect failures, stalled bodies, mid-stream drops, key-pool rotation, or
//! health side effects, all of which live in the executor and need their own
//! fault-injection suite. Mistaking a green fixture matrix for coverage of the
//! invocation path is the specific error this paragraph exists to prevent.
//!
//! # Credentials never arrive here
//!
//! [`AuthMaterialRef`] carries a *kind* and an opaque lease reference, never
//! material. An adapter declares an [`AuthPlacement`] — "put a bearer token in
//! the `authorization` header" — and the executor, which holds the lease, does
//! the placing. So a [`WireHttpRequest`] is secret-free by construction rather
//! than by redaction, and a golden of one can be checked into the repository
//! without a scrubbing step that someone will eventually forget to run.

use serde::{Deserialize, Serialize};

use super::canonical::{CanonicalInvocationRequest, RequiredCapabilities};
use super::disposition::{
    BeforeSendRefusal, CompletionKindV1, InvocationDispositionV1, ProtocolViolation, RetryPosture,
    UnsupportedCapability,
};
use super::stream::{StreamDecoderUnavailable, WireStreamDecoder};
use super::usage::UsageObservationV1;
use crate::ProviderInvocationFailureClass;

// ---------------------------------------------------------------------------
// HTTP shapes
// ---------------------------------------------------------------------------

/// The HTTP method of a wire request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HttpMethod {
    /// The only method any current dialect uses for an invocation.
    #[serde(rename = "POST")]
    Post,
    /// Model/catalog discovery.
    #[serde(rename = "GET")]
    Get,
}

impl HttpMethod {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Post => "POST",
            Self::Get => "GET",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "POST" => Self::Post,
            "GET" => Self::Get,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [HttpMethod] = &[Self::Post, Self::Get];
}

/// One non-secret request header an adapter set.
///
/// There is no secret variant, and that absence is the design: an adapter
/// never holds material, so it cannot construct one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireHeader {
    /// Lowercase header name.
    pub name: String,
    /// Header value.
    pub value: String,
}

/// Where the executor must inject the leased auth material.
///
/// Note what is *not* here: a query-parameter placement. Some providers accept
/// a key in the URL, and supporting that means every URL in every log, receipt
/// and error string becomes secret-bearing. Adding it needs a URL redaction
/// rule first, so it is deliberately absent rather than "not needed yet".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthPlacement {
    /// The deployment needs no material (a local runtime, typically).
    #[serde(rename = "none")]
    None,
    /// Put the material in a header, after `prefix`.
    #[serde(rename = "header")]
    Header {
        /// Lowercase header name.
        name: &'static str,
        /// Literal prefix before the material, `"Bearer "` and the like.
        /// Empty for raw-key headers.
        prefix: &'static str,
    },
}

/// What kind of material the executor's lease holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuthMaterialKind {
    /// No material at all.
    #[serde(rename = "none")]
    None,
    /// A long-lived provider API key.
    #[serde(rename = "api_key")]
    ApiKey,
    /// A short-lived bearer token (the #1684 OAuth path).
    #[serde(rename = "bearer_token")]
    BearerToken,
}

impl AuthMaterialKind {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ApiKey => "api_key",
            Self::BearerToken => "bearer_token",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "none" => Self::None,
            "api_key" => Self::ApiKey,
            "bearer_token" => Self::BearerToken,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [AuthMaterialKind] = &[Self::None, Self::ApiKey, Self::BearerToken];
}

/// What an adapter is told about auth: a kind, and an opaque lease reference.
///
/// **It carries no material, and there is no accessor that returns any.** The
/// adapter needs the kind so it can refuse before send when a dialect cannot
/// place what the lease holds; it needs the reference only to echo provenance.
/// These do not compile:
///
/// ```compile_fail
/// use tachi_llm::llm::broker::AuthMaterialRef;
///
/// let auth: AuthMaterialRef<'_> = unimplemented!();
/// let _ = auth.secret();
/// ```
///
/// ```compile_fail
/// use tachi_llm::llm::broker::AuthMaterialRef;
///
/// let auth: AuthMaterialRef<'_> = unimplemented!();
/// let _ = auth.material();
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthMaterialRef<'a> {
    kind: AuthMaterialKind,
    lease_ref: &'a str,
}

impl AuthMaterialRef<'static> {
    /// No material is held.
    pub fn none() -> Self {
        Self {
            kind: AuthMaterialKind::None,
            lease_ref: "",
        }
    }
}

impl<'a> AuthMaterialRef<'a> {
    /// A lease of `kind`, identified by an opaque reference.
    pub fn leased(kind: AuthMaterialKind, lease_ref: &'a str) -> Self {
        Self { kind, lease_ref }
    }

    /// What kind of material the lease holds.
    pub fn kind(&self) -> AuthMaterialKind {
        self.kind
    }

    /// The opaque lease reference. Not material, and not usable as one.
    pub fn lease_ref(&self) -> &'a str {
        self.lease_ref
    }
}

/// A fully-built provider request — everything except the credential.
///
/// Private fields with read-only accessors so no later caller can staple a
/// header onto a request after the adapter built it (which is how a
/// secret-free-by-construction guarantee usually dies).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireHttpRequest {
    method: HttpMethod,
    url: String,
    headers: Vec<WireHeader>,
    auth_placement: AuthPlacement,
    body: Vec<u8>,
}

impl WireHttpRequest {
    /// Build a wire request. Header names are lowercased so the executor and
    /// the goldens agree on one spelling.
    pub fn new(
        method: HttpMethod,
        url: impl Into<String>,
        headers: Vec<WireHeader>,
        auth_placement: AuthPlacement,
        body: Vec<u8>,
    ) -> Self {
        Self {
            method,
            url: url.into(),
            headers: headers
                .into_iter()
                .map(|h| WireHeader {
                    name: h.name.to_ascii_lowercase(),
                    value: h.value,
                })
                .collect(),
            auth_placement,
            body,
        }
    }

    /// The method.
    pub fn method(&self) -> HttpMethod {
        self.method
    }

    /// The absolute URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The non-secret headers, in the order the adapter set them.
    pub fn headers(&self) -> &[WireHeader] {
        &self.headers
    }

    /// Where the executor must inject the leased material.
    pub fn auth_placement(&self) -> &AuthPlacement {
        &self.auth_placement
    }

    /// The request body bytes.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The body as UTF-8, for goldens and for tests. `None` when the dialect
    /// produced non-UTF-8 bytes (no current dialect does).
    pub fn body_utf8(&self) -> Option<&str> {
        std::str::from_utf8(&self.body).ok()
    }
}

/// A read-only, case-insensitive view of a provider's response headers.
///
/// It exists because classification needs headers: `Retry-After` is the
/// provider telling us how long to wait, and a classifier that only sees the
/// status and the body has to invent a backoff instead. Header names are
/// lowercased once at construction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResponseHeaders {
    entries: Vec<(String, String)>,
}

impl ResponseHeaders {
    /// An empty header set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from name/value pairs.
    pub fn from_pairs<N, V, I>(pairs: I) -> Self
    where
        N: AsRef<str>,
        V: AsRef<str>,
        I: IntoIterator<Item = (N, V)>,
    {
        Self {
            entries: pairs
                .into_iter()
                .map(|(n, v)| {
                    (
                        n.as_ref().trim().to_ascii_lowercase(),
                        v.as_ref().trim().to_string(),
                    )
                })
                .collect(),
        }
    }

    /// First value for a header name, matched case-insensitively.
    pub fn get(&self, name: &str) -> Option<&str> {
        let needle = name.to_ascii_lowercase();
        self.entries
            .iter()
            .find(|(n, _)| *n == needle)
            .map(|(_, v)| v.as_str())
    }

    /// The provider's `Retry-After` directive, if any.
    ///
    /// HTTP allows a delta-seconds form and an HTTP-date form. The legacy lane
    /// parses only the first and drops the second on the floor; both are
    /// preserved here, because "wait until Tuesday 09:00" is a real answer and
    /// discarding it means backing off by a guess instead.
    pub fn retry_after(&self) -> Option<RetryAfter> {
        let raw = self.get("retry-after")?;
        if raw.is_empty() {
            return None;
        }
        match raw.parse::<u64>() {
            Ok(seconds) => Some(RetryAfter::Seconds(seconds)),
            Err(_) => Some(RetryAfter::At(raw.to_string())),
        }
    }
}

/// A `Retry-After` directive as read off the wire.
///
/// The local counterpart of the seam's `RetryAfter`; spellings are identical
/// on purpose so the two reconcile without a translation table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum RetryAfter {
    /// `Retry-After: 120` — delta seconds.
    #[serde(rename = "seconds")]
    Seconds(u64),
    /// `Retry-After: <HTTP-date>` — preserved as the received string.
    #[serde(rename = "at")]
    At(String),
}

impl RetryAfter {
    /// The delta-seconds form, when the provider used it.
    ///
    /// Exactly what the legacy lane's `retry-after` read produces, so the two
    /// can be compared fixture for fixture.
    pub fn as_seconds(&self) -> Option<u64> {
        match self {
            Self::Seconds(seconds) => Some(*seconds),
            Self::At(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Error classification
// ---------------------------------------------------------------------------

/// What the executor should do about a failed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RetryAdvice {
    /// Re-send to the same deployment is reasonable.
    #[serde(rename = "retry_same_deployment")]
    RetrySameDeployment,
    /// This credential is the problem; another key or account may work.
    #[serde(rename = "retry_other_credential")]
    RetryOtherCredential,
    /// This deployment is the problem; a fallback candidate may work.
    #[serde(rename = "try_fallback_deployment")]
    TryFallbackDeployment,
    /// Nothing to retry — the request itself is wrong.
    #[serde(rename = "do_not_retry")]
    DoNotRetry,
}

impl RetryAdvice {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RetrySameDeployment => "retry_same_deployment",
            Self::RetryOtherCredential => "retry_other_credential",
            Self::TryFallbackDeployment => "try_fallback_deployment",
            Self::DoNotRetry => "do_not_retry",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "retry_same_deployment" => Self::RetrySameDeployment,
            "retry_other_credential" => Self::RetryOtherCredential,
            "try_fallback_deployment" => Self::TryFallbackDeployment,
            "do_not_retry" => Self::DoNotRetry,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [RetryAdvice] = &[
        Self::RetrySameDeployment,
        Self::RetryOtherCredential,
        Self::TryFallbackDeployment,
        Self::DoNotRetry,
    ];
}

/// Why a provider refused, classified from status + headers + body.
///
/// Six members where the legacy lane has four, and the two extra splits are
/// the ones that were costing information:
///
/// - **billing/quota vs rate limit.** The legacy lane maps both onto
///   `ProviderExhausted`, but a 403 "insufficient balance" needs a human and a
///   429 needs a clock. The account-health side effects are different, and
///   flattening them means a dead account looks like a busy one forever.
/// - **protocol vs bad request.** Both are legacy `LaneOutage`, but one is our
///   request being wrong and the other is the provider's answer being wrong;
///   only the first is fixable by the caller.
///
/// [`ProviderErrorClass::to_legacy_failure_class`] is the projection back onto
/// the four legacy members, and the parity suite asserts that the projection
/// of this classifier's answer equals what `lane_calls` would have said for
/// the same status and body. The richer split is therefore additive: it cannot
/// change a single existing decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderErrorClass {
    /// 401, or a 403 that is not a billing failure. The credential is bad.
    #[serde(rename = "auth_invalid")]
    AuthInvalid,
    /// A 403 whose body says the account is out of money or over quota.
    #[serde(rename = "billing_or_quota")]
    BillingOrQuota,
    /// 429. The provider is asking us to slow down.
    #[serde(rename = "rate_limited")]
    RateLimited,
    /// 5xx. The provider is broken right now.
    #[serde(rename = "server_error")]
    ServerError,
    /// Any other non-success status. Our request was wrong.
    #[serde(rename = "bad_request")]
    BadRequest,
    /// A success status carrying an answer that is not the provider's own
    /// grammar.
    #[serde(rename = "protocol")]
    Protocol,
}

impl ProviderErrorClass {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthInvalid => "auth_invalid",
            Self::BillingOrQuota => "billing_or_quota",
            Self::RateLimited => "rate_limited",
            Self::ServerError => "server_error",
            Self::BadRequest => "bad_request",
            Self::Protocol => "protocol",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "auth_invalid" => Self::AuthInvalid,
            "billing_or_quota" => Self::BillingOrQuota,
            "rate_limited" => Self::RateLimited,
            "server_error" => Self::ServerError,
            "bad_request" => Self::BadRequest,
            "protocol" => Self::Protocol,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [ProviderErrorClass] = &[
        Self::AuthInvalid,
        Self::BillingOrQuota,
        Self::RateLimited,
        Self::ServerError,
        Self::BadRequest,
        Self::Protocol,
    ];

    /// The projection onto the legacy four-member class the shipped lanes and
    /// the durable health surface already speak.
    ///
    /// Total by construction (no wildcard arm), so adding a member to this
    /// enum without deciding its legacy meaning is a compile error rather than
    /// a silent `LaneOutage`.
    pub fn to_legacy_failure_class(self) -> ProviderInvocationFailureClass {
        match self {
            Self::AuthInvalid => ProviderInvocationFailureClass::AuthFailed,
            Self::BillingOrQuota | Self::RateLimited => {
                ProviderInvocationFailureClass::ProviderExhausted
            }
            Self::ServerError => ProviderInvocationFailureClass::Transient,
            Self::BadRequest | Self::Protocol => ProviderInvocationFailureClass::LaneOutage,
        }
    }

    /// Whether the executor may re-send after this class.
    pub fn retry_posture(self) -> RetryPosture {
        match self {
            // The provider rejected the request before doing work, so nothing
            // was spent and a retry (with another key, another deployment, or
            // after the retry-after) duplicates nothing.
            Self::AuthInvalid
            | Self::BillingOrQuota
            | Self::RateLimited
            | Self::ServerError
            | Self::BadRequest => RetryPosture::Safe,
            // A protocol violation means the provider *did* work and answered
            // unusably. Re-sending buys the same answer at the same price.
            Self::Protocol => RetryPosture::Forbidden,
        }
    }

    /// The executor's default next move for this class.
    pub fn default_retry_advice(self) -> RetryAdvice {
        match self {
            Self::AuthInvalid | Self::BillingOrQuota => RetryAdvice::RetryOtherCredential,
            Self::RateLimited | Self::ServerError => RetryAdvice::RetrySameDeployment,
            Self::BadRequest => RetryAdvice::DoNotRetry,
            Self::Protocol => RetryAdvice::TryFallbackDeployment,
        }
    }
}

/// A classified provider error, with the directives that came with it.
///
/// A struct rather than a bare [`ProviderErrorClass`] because the classifier
/// is handed response headers precisely so `Retry-After` survives
/// classification; returning only the enum would read the header and then
/// throw it away, which is the bug this shape exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderErrorClassification {
    /// What went wrong.
    pub class: ProviderErrorClass,
    /// The provider's own wait directive, preserved verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<RetryAfter>,
    /// What the executor should do next.
    pub advice: RetryAdvice,
}

impl ProviderErrorClassification {
    /// Classify with the class's default advice.
    pub fn new(class: ProviderErrorClass, retry_after: Option<RetryAfter>) -> Self {
        Self {
            class,
            retry_after,
            advice: class.default_retry_advice(),
        }
    }
}

// ---------------------------------------------------------------------------
// Response shapes
// ---------------------------------------------------------------------------

/// A tool call the model asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallV1 {
    /// Provider-assigned call id, echoed back with the tool result.
    pub id: String,
    /// Which tool.
    pub name: String,
    /// Arguments as the provider emitted them — a JSON *string*, not a parsed
    /// value, because providers emit invalid JSON here often enough that
    /// parsing at the wire boundary would turn a recoverable answer into a
    /// protocol failure.
    pub arguments: String,
}

/// Provider-side identity of a response. Non-secret by construction: model,
/// version and request id only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderResponseMetadata {
    /// The model the provider says actually served the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_model: Option<String>,
    /// A build/version fingerprint, when the provider sends one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_version: Option<String>,
    /// The provider's own request id, for support tickets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<String>,
}

/// The assistant turn a provider returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalAssistantMessage {
    /// Assistant text, when there was any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Tool calls, when the model asked for any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallV1>,
}

impl CanonicalAssistantMessage {
    /// Whether the turn carried nothing usable at all — neither text nor a
    /// tool call. The legacy lane's "empty content" case.
    ///
    /// The text test **trims**, matching `lane_calls`'s
    /// `.map(str::trim).filter(|s| !s.is_empty())`: a whitespace-only answer is
    /// an empty answer on both sides. That is what keeps the empty-content
    /// decision — the one the classification parity suite covers — unchanged
    /// while the returned text itself stays untrimmed.
    pub fn is_empty(&self) -> bool {
        self.text
            .as_deref()
            .is_none_or(|text| text.trim().is_empty())
            && self.tool_calls.is_empty()
    }
}

/// What an adapter made of a non-streaming provider response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WireOutcome {
    /// A usable answer.
    #[serde(rename = "completed")]
    Completed {
        /// The assistant turn.
        message: CanonicalAssistantMessage,
        /// How the generation ended.
        completion: CompletionKindV1,
        /// Token usage, with provenance.
        usage: UsageObservationV1,
        /// Provider-side identity.
        metadata: ProviderResponseMetadata,
    },
    /// The provider said no.
    #[serde(rename = "rejected")]
    Rejected {
        /// The HTTP status.
        status: u16,
        /// The classification.
        classification: ProviderErrorClassification,
    },
    /// The provider answered outside its own grammar.
    #[serde(rename = "protocol_violation")]
    ProtocolViolation {
        /// Which rule was broken.
        violation: ProtocolViolation,
    },
}

impl WireOutcome {
    /// The disposition this outcome ends the invocation with.
    pub fn disposition(&self) -> InvocationDispositionV1 {
        match self {
            Self::Completed { completion, .. } => InvocationDispositionV1::Completed {
                completion: *completion,
            },
            Self::Rejected {
                status,
                classification,
            } => InvocationDispositionV1::ProviderRejected {
                class: classification.class,
                status: *status,
                retry_after: classification.retry_after.clone(),
            },
            Self::ProtocolViolation { violation } => InvocationDispositionV1::ProtocolError {
                violation: violation.clone(),
            },
        }
    }

    /// Usage, when any was observed. A rejected or malformed response has
    /// none — deliberately `None` rather than zeros, since "the provider
    /// charged nothing" is a claim we cannot make.
    pub fn usage(&self) -> Option<&UsageObservationV1> {
        match self {
            Self::Completed { usage, .. } => Some(usage),
            Self::Rejected { .. } | Self::ProtocolViolation { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

/// What a dialect/deployment pair can do.
///
/// The axes mirror the seam's `DeploymentCapabilities` one for one, plus
/// `json_schema`: several OpenAI-compatible providers accept
/// `response_format: json_object` and reject `json_schema`, and collapsing the
/// two means either refusing requests that would have worked or sending
/// requests that will be rejected at spend time. `json_schema` implies
/// `structured_output`; [`WireCapabilities::is_consistent`] pins that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WireCapabilities {
    /// Chat/completions.
    pub chat: bool,
    /// Embeddings.
    pub embeddings: bool,
    /// Tool/function calling.
    pub tools: bool,
    /// Incremental streaming.
    pub streaming: bool,
    /// Any output-shape constraint.
    pub structured_output: bool,
    /// Strict JSON Schema specifically.
    pub json_schema: bool,
    /// Non-text attachments.
    pub media: bool,
}

impl WireCapabilities {
    /// Whether the capability set is internally coherent.
    pub fn is_consistent(self) -> bool {
        !self.json_schema || self.structured_output
    }

    /// Every capability the request needs that this set lacks, in
    /// [`UnsupportedCapability::ALL`] order.
    ///
    /// Returns *all* of them rather than the first: a caller who learns one
    /// missing capability per round trip will make several, and each one costs
    /// an admission and a resolution.
    pub fn missing_for(self, required: &RequiredCapabilities) -> Vec<UnsupportedCapability> {
        let mut missing = Vec::new();
        if required.chat && !self.chat {
            missing.push(UnsupportedCapability::Chat);
        }
        if required.tools && !self.tools {
            missing.push(UnsupportedCapability::Tools);
        }
        if required.streaming && !self.streaming {
            missing.push(UnsupportedCapability::Streaming);
        }
        if required.structured_output && !self.structured_output {
            missing.push(UnsupportedCapability::StructuredOutput);
        }
        if required.json_schema && !self.json_schema {
            missing.push(UnsupportedCapability::JsonSchema);
        }
        if required.media && !self.media {
            missing.push(UnsupportedCapability::Media);
        }
        missing
    }
}

// ---------------------------------------------------------------------------
// The trait
// ---------------------------------------------------------------------------

/// One provider grammar, as four pure functions.
///
/// Every method is `&self` and side-effect-free. An implementation is expected
/// to be a zero-sized or configuration-only type; if one ever needs a
/// connection, a clock or a credential, the design is wrong and the fix is in
/// the executor, not here.
pub trait ProviderWire: Send + Sync {
    /// The dialect's frozen name, matching the seam's `WireDialect` spelling.
    fn dialect(&self) -> &'static str;

    /// What this dialect can do.
    fn capabilities(&self) -> WireCapabilities;

    /// Map a canonical request onto provider bytes, or refuse before send.
    ///
    /// `auth` carries a kind and a lease reference, never material: the
    /// returned request declares an [`AuthPlacement`] and the executor injects.
    fn build_request(
        &self,
        request: &CanonicalInvocationRequest,
        auth: AuthMaterialRef<'_>,
    ) -> Result<WireHttpRequest, BeforeSendRefusal>;

    /// Map a non-streaming provider response onto a canonical outcome,
    /// including usage extraction.
    ///
    /// Total: every input produces a [`WireOutcome`], because "the provider
    /// sent something we cannot read" is an outcome
    /// ([`WireOutcome::ProtocolViolation`]) rather than an error to propagate.
    fn parse_response(&self, status: u16, headers: &ResponseHeaders, body: &[u8]) -> WireOutcome;

    /// A fresh decoder for this dialect's streaming grammar.
    ///
    /// Fallible on purpose. A dialect that does not stream, or one whose
    /// grammar this Broker has not implemented yet, answers with a typed
    /// [`StreamDecoderUnavailable`] — never a panic, and never a silent
    /// downgrade to a non-streaming response.
    fn new_stream_decoder(&self) -> Result<Box<dyn WireStreamDecoder>, StreamDecoderUnavailable>;

    /// Classify a failed response.
    ///
    /// `headers` is present so `Retry-After` survives classification;
    /// `body_excerpt` is a bounded prefix, since classification only ever
    /// inspects the body for well-known markers and the full body is untrusted
    /// input that must not be retained.
    fn classify_error(
        &self,
        status: u16,
        headers: &ResponseHeaders,
        body_excerpt: &str,
    ) -> ProviderErrorClassification;
}
