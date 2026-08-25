use super::*;

// ---------------------------------------------------------------------------
// 5. HealthObservation
// ---------------------------------------------------------------------------

/// How a health observation was learned. The seam-local, serde-capable twin of
/// the #1680 `vault::health::EvidenceKind{Probed, SelfReported}` pattern —
/// redefined here (rather than reused) so the seam stays self-contained and
/// serde round-trips without coupling to the vault module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObservationEvidence {
    /// Observed by a non-generating probe.
    #[serde(rename = "probed")]
    Probed,
    /// Reported by a consumer of the deployment (the invocation path).
    #[serde(rename = "self_reported")]
    SelfReported,
}

impl ObservationEvidence {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Probed => "probed",
            Self::SelfReported => "self_reported",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "probed" => Self::Probed,
            "self_reported" => Self::SelfReported,
            _ => return None,
        })
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [ObservationEvidence] = &[Self::Probed, Self::SelfReported];
}

/// The class of an invocation error, aligned with the #1681 D4 attribution
/// rules. Closed set: the executor classifies a failure into exactly one of
/// these, and the D4 rule decides which health authority each touches
/// (`AuthInvalid` never touches deployment health; `RateLimited` dual-records;
/// the rest are deployment-only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvocationErrorClass {
    /// 401 / 403 — credential/account surfaces only, never deployment health.
    #[serde(rename = "auth_invalid")]
    AuthInvalid,
    /// 429 / quota — dual-record (deployment cooldown + credential rate-limit).
    #[serde(rename = "rate_limited")]
    RateLimited,
    /// Request timed out.
    #[serde(rename = "timeout")]
    Timeout,
    /// 5xx server error.
    #[serde(rename = "server_error")]
    ServerError,
    /// Malformed / unparseable / protocol-violating response.
    #[serde(rename = "protocol")]
    Protocol,
}

impl InvocationErrorClass {
    /// The frozen wire spelling. Must equal the variant's `serde(rename)`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthInvalid => "auth_invalid",
            Self::RateLimited => "rate_limited",
            Self::Timeout => "timeout",
            Self::ServerError => "server_error",
            Self::Protocol => "protocol",
        }
    }

    /// Parse a frozen wire spelling; `None` for anything unrecognised.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "auth_invalid" => Self::AuthInvalid,
            "rate_limited" => Self::RateLimited,
            "timeout" => Self::Timeout,
            "server_error" => Self::ServerError,
            "protocol" => Self::Protocol,
            _ => return None,
        })
    }

    /// Whether the D4 attribution rule routes this class to deployment health.
    /// `AuthInvalid` is the sole class that never does.
    pub fn touches_deployment_health(self) -> bool {
        !matches!(self, Self::AuthInvalid)
    }

    /// Every variant, in declaration order.
    pub const ALL: &'static [InvocationErrorClass] = &[
        Self::AuthInvalid,
        Self::RateLimited,
        Self::Timeout,
        Self::ServerError,
        Self::Protocol,
    ];
}

/// A `Retry-After` directive as read off the wire. The header survives
/// classification; HTTP delta-seconds and HTTP-date forms are both preserved.
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

/// One invocation outcome reported to the health layer: the precise
/// account/deployment pair, error class, and any `Retry-After`.
///
/// Note the pairing of `account_ref` and `deployment_id`: the D4 attribution
/// rule needs both, because a 401/403 must reach the account surface while
/// never touching deployment health, and a 429 must reach both. Blank refs
/// would make the attribution unroutable, so fields are private and both
/// construction paths validate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "HealthObservationWire")]
pub struct HealthObservation {
    account_ref: String,
    deployment_id: String,
    error_class: InvocationErrorClass,
    retry_after: Option<RetryAfter>,
    observed_at: String,
    evidence: ObservationEvidence,
}

/// Deserialization shadow for [`HealthObservation`].
#[derive(Deserialize)]
struct HealthObservationWire {
    account_ref: String,
    deployment_id: String,
    error_class: InvocationErrorClass,
    retry_after: Option<RetryAfter>,
    observed_at: String,
    evidence: ObservationEvidence,
}

impl TryFrom<HealthObservationWire> for HealthObservation {
    type Error = SeamError;

    fn try_from(wire: HealthObservationWire) -> Result<Self, Self::Error> {
        Self::new(
            wire.account_ref,
            wire.deployment_id,
            wire.error_class,
            wire.retry_after,
            wire.observed_at,
            wire.evidence,
        )
    }
}

impl HealthObservation {
    /// Construct a validated observation.
    ///
    /// # Errors
    ///
    /// [`SeamError::EmptyField`] if `account_ref`, `deployment_id`, or
    /// `observed_at` is blank.
    pub fn new(
        account_ref: impl Into<String>,
        deployment_id: impl Into<String>,
        error_class: InvocationErrorClass,
        retry_after: Option<RetryAfter>,
        observed_at: impl Into<String>,
        evidence: ObservationEvidence,
    ) -> Result<Self, SeamError> {
        let account_ref = account_ref.into();
        let deployment_id = deployment_id.into();
        let observed_at = observed_at.into();
        require_non_empty(&account_ref, "account_ref")?;
        require_non_empty(&deployment_id, "deployment_id")?;
        require_non_empty(&observed_at, "observed_at")?;
        Ok(Self {
            account_ref,
            deployment_id,
            error_class,
            retry_after,
            observed_at,
            evidence,
        })
    }

    /// Opaque #1680 account/credential ref the invocation used.
    pub fn account_ref(&self) -> &str {
        &self.account_ref
    }

    /// The deployment the invocation targeted.
    pub fn deployment_id(&self) -> &str {
        &self.deployment_id
    }

    /// The classified failure.
    pub fn error_class(&self) -> InvocationErrorClass {
        self.error_class
    }

    /// The wire `Retry-After`, if the response carried one.
    pub fn retry_after(&self) -> Option<&RetryAfter> {
        self.retry_after.as_ref()
    }

    /// When the outcome was observed (ISO-8601).
    pub fn observed_at(&self) -> &str {
        &self.observed_at
    }

    /// How the observation was learned.
    pub fn evidence(&self) -> ObservationEvidence {
        self.evidence
    }
}
