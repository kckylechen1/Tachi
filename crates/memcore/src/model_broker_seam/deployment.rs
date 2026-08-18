use super::*;

// ---------------------------------------------------------------------------
// 2. ResolvedDeployment
// ---------------------------------------------------------------------------

/// The wire dialect a deployment speaks. The closed set includes an explicit
/// `Unknown` for a catalogued deployment whose grammar this build cannot
/// operate.
///
/// `Unknown` is deliberate rather than absent — "we catalogued it and cannot
/// speak to it" must be distinguishable from "we never looked", the same rule
/// as `AuthMode::Unsupported`.
///
/// Each variant carries an explicit `rename` rather than relying on
/// `rename_all = "snake_case"`: serde's snake_case of `OpenAiCompat` would be
/// `open_ai_compat`, while the contract spelling is `openai_compat`. The
/// spelling is declared next to the variant and pinned against a literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireDialect {
    /// OpenAI `/v1/chat/completions` grammar (SiliconFlow / DeepSeek / ZAI /
    /// OpenAI itself all speak it today).
    #[serde(rename = "openai_compat")]
    OpenAiCompat,
    /// Anthropic messages grammar (`content_block_delta` / `tool_use`).
    #[serde(rename = "anthropic")]
    Anthropic,
    /// xAI grammar.
    #[serde(rename = "xai")]
    Xai,
    /// OpenRouter grammar.
    #[serde(rename = "open_router")]
    OpenRouter,
    /// A generic OpenAI-compatible endpoint that is none of the named vendors.
    #[serde(rename = "generic_compat")]
    GenericCompat,
    /// Ollama `/api/generate` (NDJSON stream, not SSE).
    #[serde(rename = "ollama")]
    Ollama,
    /// Catalogued but the wire grammar is not operable by this build.
    #[serde(rename = "unknown")]
    Unknown,
}

impl WireDialect {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompat => "openai_compat",
            Self::Anthropic => "anthropic",
            Self::Xai => "xai",
            Self::OpenRouter => "open_router",
            Self::GenericCompat => "generic_compat",
            Self::Ollama => "ollama",
            Self::Unknown => "unknown",
        }
    }

    /// Parse a frozen wire spelling. `None` for anything unrecognised — an
    /// unknown *spelling* is not the same as the `Unknown` *dialect*, and this
    /// seam never collapses the two.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "openai_compat" => Self::OpenAiCompat,
            "anthropic" => Self::Anthropic,
            "xai" => Self::Xai,
            "open_router" => Self::OpenRouter,
            "generic_compat" => Self::GenericCompat,
            "ollama" => Self::Ollama,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [WireDialect] = &[
        Self::OpenAiCompat,
        Self::Anthropic,
        Self::Xai,
        Self::OpenRouter,
        Self::GenericCompat,
        Self::Ollama,
        Self::Unknown,
    ];
}

/// What a deployment can do. The capability axes #1681 D1 lists for the
/// `capabilities` JSON column, restated as a flat plain-data projection.
///
/// No invariant, so the fields stay public: every combination of booleans is a
/// legal deployment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentCapabilities {
    /// Chat/completions lane.
    pub chat: bool,
    /// Embeddings lane.
    pub embeddings: bool,
    /// Tool/function calling.
    pub tools: bool,
    /// Incremental (SSE / NDJSON) streaming.
    pub streaming: bool,
    /// Schema-constrained output.
    pub structured_output: bool,
    /// Non-text attachments.
    pub media: bool,
}

/// A deployment's context / output / attachment bounds.
///
/// `embedding_dimensions` is the #1681 D3 dimension declaration: an embed
/// deployment carries the vector dimensionality it produces, so an override
/// whose dimension mismatches the stored index fails loudly instead of silently
/// corrupting comparability. All fields are optional — a chat deployment has no
/// embedding dimension, a deployment whose bounds are unknown carries `None`.
///
/// Each axis has a matching [`ExclusionReason`], so "the request did not fit"
/// is always attributable to one bound rather than to a generic mismatch.
/// No invariant, so the fields stay public.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentBounds {
    /// Total context window in tokens.
    pub context_window: Option<u32>,
    /// Maximum generated tokens per response.
    pub max_output: Option<u32>,
    /// Maximum attachment payload in bytes.
    pub attachment_bytes: Option<u64>,
    /// Produced embedding dimensionality (#1681 D3 dimension declaration).
    pub embedding_dimensions: Option<u32>,
}

/// Constructor input for [`ResolvedDeployment`], and its deserialization shadow.
///
/// One type serves both roles deliberately: it *is* the wire shape, so
/// `ResolvedDeployment::new(parts)` and `serde_json::from_str::<ResolvedDeployment>`
/// run the identical validation, and there is no second field list to drift.
/// A parts struct rather than an eight-argument constructor keeps the call site
/// readable (and stays under clippy's argument threshold).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResolvedDeploymentParts {
    /// Catalog-controlled deployment identity.
    pub deployment_id: String,
    /// The grammar this deployment speaks.
    pub wire_dialect: WireDialect,
    /// Opaque endpoint reference.
    pub endpoint_ref: String,
    /// The provider's own model id sent on the wire.
    pub provider_model_id: String,
    /// What the deployment can do.
    pub capabilities: DeploymentCapabilities,
    /// Context / output / attachment bounds.
    pub bounds: DeploymentBounds,
    /// Opaque #1680 auth/account reference.
    pub account_ref: String,
    /// Content-addressed pricing snapshot id, or `None`.
    pub pricing_snapshot_ref: Option<String>,
}

/// A single already-resolved deployment — what a resolver hands out for one
/// concrete route.
///
/// This is the *resolved projection*, not the `model_deployments` row: it
/// carries no health columns (health is a separate authority, #1681 D4), no
/// catalog bookkeeping (`fetched_at`/`expires_at`/`status`), and no secret —
/// `account_ref` is the opaque #1680 `auth_ref`, never a Vault member name.
///
/// Fields are private; build one with [`ResolvedDeployment::new`] (or
/// deserialize, which runs the same validation) and read it back through the
/// accessors. [`ResolvedDeployment::into_parts`] gives a mutable copy of the
/// wire shape for consumers that need to vary one field — re-validated on the
/// way back in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ResolvedDeploymentParts")]
pub struct ResolvedDeployment {
    deployment_id: String,
    wire_dialect: WireDialect,
    endpoint_ref: String,
    provider_model_id: String,
    capabilities: DeploymentCapabilities,
    bounds: DeploymentBounds,
    account_ref: String,
    pricing_snapshot_ref: Option<String>,
}

impl TryFrom<ResolvedDeploymentParts> for ResolvedDeployment {
    type Error = SeamError;

    fn try_from(parts: ResolvedDeploymentParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}

impl ResolvedDeployment {
    /// Construct a validated `ResolvedDeployment`.
    ///
    /// # Errors
    ///
    /// [`SeamError::EmptyField`] if `deployment_id`, `endpoint_ref`,
    /// `provider_model_id`, `account_ref`, or a present `pricing_snapshot_ref`
    /// is blank. A *missing* price (`None`) is legal — the catalog may not have
    /// one yet; a blank one is a bug.
    pub fn new(parts: ResolvedDeploymentParts) -> Result<Self, SeamError> {
        require_non_empty(&parts.deployment_id, "deployment_id")?;
        require_non_empty(&parts.endpoint_ref, "endpoint_ref")?;
        require_non_empty(&parts.provider_model_id, "provider_model_id")?;
        require_non_empty(&parts.account_ref, "account_ref")?;
        if let Some(price) = parts.pricing_snapshot_ref.as_deref() {
            require_non_empty(price, "pricing_snapshot_ref")?;
        }
        Ok(Self {
            deployment_id: parts.deployment_id,
            wire_dialect: parts.wire_dialect,
            endpoint_ref: parts.endpoint_ref,
            provider_model_id: parts.provider_model_id,
            capabilities: parts.capabilities,
            bounds: parts.bounds,
            account_ref: parts.account_ref,
            pricing_snapshot_ref: parts.pricing_snapshot_ref,
        })
    }

    /// Catalog-controlled deployment identity (closed vocabulary; the value the
    /// receipt provenance chain records — not provider error text).
    pub fn deployment_id(&self) -> &str {
        &self.deployment_id
    }

    /// The grammar this deployment speaks.
    pub fn wire_dialect(&self) -> WireDialect {
        self.wire_dialect
    }

    /// Opaque endpoint reference (an id/handle, resolved to a base_url by the
    /// executor — not the URL itself at this layer).
    pub fn endpoint_ref(&self) -> &str {
        &self.endpoint_ref
    }

    /// The provider's own model id sent on the wire.
    pub fn provider_model_id(&self) -> &str {
        &self.provider_model_id
    }

    /// What the deployment can do.
    pub fn capabilities(&self) -> DeploymentCapabilities {
        self.capabilities
    }

    /// Context / output / attachment bounds.
    pub fn bounds(&self) -> DeploymentBounds {
        self.bounds
    }

    /// Opaque #1680 auth/account reference. Never a credential member name.
    pub fn account_ref(&self) -> &str {
        &self.account_ref
    }

    /// Content-addressed pricing snapshot id (#1681 D1), or `None` when the
    /// catalog has no price for this deployment yet.
    pub fn pricing_snapshot_ref(&self) -> Option<&str> {
        self.pricing_snapshot_ref.as_deref()
    }

    /// Decompose into the (unvalidated) wire shape, for consumers that need to
    /// vary one field and rebuild through [`ResolvedDeployment::new`].
    pub fn into_parts(self) -> ResolvedDeploymentParts {
        ResolvedDeploymentParts {
            deployment_id: self.deployment_id,
            wire_dialect: self.wire_dialect,
            endpoint_ref: self.endpoint_ref,
            provider_model_id: self.provider_model_id,
            capabilities: self.capabilities,
            bounds: self.bounds,
            account_ref: self.account_ref,
            pricing_snapshot_ref: self.pricing_snapshot_ref,
        }
    }
}
