//! The single writer for `model_deployment_health` (tachi#1681 D4, PR-C).
//!
//! # One writer, one authority
//!
//! [`record_deployment_outcome`] is to `model_deployment_health` what
//! [`crate::vault::health::record_key_outcome`] (#1680 D6) is to
//! `vault_key_health`: the only function that turns an observed outcome into a
//! row. Every channel classifies into [`DeploymentOutcome`], names its
//! [`EvidenceKind`], and hands both here; persistence stays with whoever owns
//! the connection ([`crate::db::model_catalog::record_model_deployment_outcome`]).
//!
//! # What this authority may not say
//!
//! The four health authorities (#1681 D4) are separate on purpose, and the
//! separation is made **structural** here rather than left to review:
//!
//! 1. **An auth failure cannot be expressed as a deployment outcome.**
//!    [`DeploymentOutcome`] has no auth variant. A caller holding a 401/403
//!    cannot construct a value to pass in — not "is rejected at runtime",
//!    *cannot be written down*. [`DeploymentOutcome::classify`] returns `None`
//!    for those statuses for the same reason, so the classifier and the type
//!    agree by construction. A 401/403 says something about the credential and
//!    the account, and nothing whatsoever about the deployment; letting it land
//!    here would cool down every sibling that shares a broken key.
//! 2. **This writer's whole type face is deployment-shaped.** Inputs are a
//!    deployment id, a [`DeploymentOutcome`], an [`EvidenceKind`] and an
//!    instant; outputs are a [`ModelDeploymentHealth`] row and one
//!    [`NewModelDeploymentEvent`]. No credential, account, alias, pricing or
//!    deployment-metadata type appears in either direction, so no outcome
//!    recording path can reach another authority's table even by accident
//!    (#1681 discriminations 5 and 11).
//! 3. **Provider text never becomes durable provenance.** Unlike
//!    `record_key_outcome`, this writer accepts **no caller-supplied reason**
//!    at all. `last_error` is generated from the closed outcome vocabulary plus
//!    a numeric HTTP status, so the `types.rs:252-267` rule (collapse provider
//!    error text at the durable boundary) holds without every call site
//!    remembering it.
//!
//! # Cooldown
//!
//! A cooldown is a state of the *health row*, never a column on the catalog
//! row: prices and capabilities describe what a deployment is, a cooldown
//! describes how it is behaving this minute (#1681 D1's table boundary).
//!
//! `Retry-After` is honoured in both forms RFC 9110 §10.2.3 allows — a
//! delta-seconds count and an HTTP-date — and both are resolved to an
//! **instant** ([`RetryAfter::cooldown_until`]). Comparing or sorting the
//! rendered strings would get a date-form header wrong in exactly the way
//! `ModelDeployment::freshness_at` documents for `expires_at`. A header we
//! cannot parse falls back to the class default rather than being guessed at,
//! and every cooldown is clamped: an unbounded provider-supplied value must not
//! be able to park a deployment forever, and a date already in the past must
//! not produce a negative cooldown that reads as "no cooldown at all".

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::{json, Map, Value};

use super::{DeploymentEventKind, ModelDeploymentHealth, NewModelDeploymentEvent};
use crate::vault::health::EvidenceKind;

/// `model_deployment_health.state` vocabulary. Persisted strings, so they are
/// constants here rather than literals at each site — the same discipline
/// `vault_key_health`'s status ladder follows.
pub const DEPLOYMENT_HEALTH_STATE_OK: &str = "ok";
pub const DEPLOYMENT_HEALTH_STATE_COOLDOWN: &str = "cooldown";
pub const DEPLOYMENT_HEALTH_STATE_ERROR: &str = "error";

/// `metadata` JSON field carrying the [`DeploymentOutcome`] label.
pub const HEALTH_OUTCOME_FIELD: &str = "outcome";
/// `metadata` JSON field carrying the HTTP status, when the outcome had one.
pub const HEALTH_STATUS_FIELD: &str = "status";
/// `metadata` JSON field carrying the cooldown this outcome bought, in seconds.
pub const HEALTH_COOLDOWN_SECS_FIELD: &str = "cooldown_secs";

/// Cooldown a throttle report buys when the provider named no `Retry-After`.
const DEFAULT_THROTTLE_COOLDOWN_SECS: u64 = 60;
/// Clamp. Deliberately this module's own constants rather than a reuse of
/// `vault::health`'s: the two authorities are separate by design (#1681 D4),
/// and sharing one constant would make a future tweak to the credential
/// cooldown silently move every deployment's cooldown with it.
const MIN_COOLDOWN_SECS: u64 = 1;
const MAX_COOLDOWN_SECS: u64 = 3600;

// ─── Retry-After ─────────────────────────────────────────────────────────────

/// A parsed `Retry-After` header, in both forms RFC 9110 §10.2.3 allows.
///
/// Parsed, not stored as text, because the two forms answer the same question
/// in different units and only an instant can be compared: `120` means two
/// minutes from now, `Wed, 21 Oct 2026 07:28:00 GMT` means a fixed moment that
/// may already have passed by the time it is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAfter {
    /// `Retry-After: 120`
    DeltaSeconds(u64),
    /// `Retry-After: Wed, 21 Oct 2026 07:28:00 GMT` (IMF-fixdate).
    HttpDate(DateTime<Utc>),
}

impl RetryAfter {
    /// Parse a raw header value, or `None` if it is neither form.
    ///
    /// The obsolete RFC 850 and asctime date formats are **not** accepted:
    /// guessing at a format the provider is not supposed to send would turn a
    /// malformed header into a confident wrong instant, and the class default
    /// is the honest answer. `None` here is not an error — the caller falls
    /// back to the default for the outcome class.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        if let Ok(seconds) = raw.parse::<u64>() {
            return Some(Self::DeltaSeconds(seconds));
        }
        // IMF-fixdate is an RFC 2822 date-time whose zone is `GMT`.
        DateTime::parse_from_rfc2822(raw)
            .ok()
            .map(|parsed| Self::HttpDate(parsed.with_timezone(&Utc)))
    }

    /// The cooldown this header buys, in seconds from `now`, clamped.
    ///
    /// A date already in the past clamps up to [`MIN_COOLDOWN_SECS`] rather
    /// than to zero: the provider said "not yet", and a zero-length cooldown is
    /// indistinguishable from never having been throttled.
    pub fn cooldown_secs_from(self, now: DateTime<Utc>) -> u64 {
        let raw = match self {
            Self::DeltaSeconds(seconds) => seconds,
            Self::HttpDate(until) => (until - now).num_seconds().max(0) as u64,
        };
        raw.clamp(MIN_COOLDOWN_SECS, MAX_COOLDOWN_SECS)
    }

    /// When the deployment becomes selectable again.
    pub fn cooldown_until(self, now: DateTime<Utc>) -> DateTime<Utc> {
        now + Duration::seconds(self.cooldown_secs_from(now) as i64)
    }
}

/// The cooldown a throttle report buys after defaulting and clamping. Exposed
/// so a caller that wants to log or mirror the value cannot disagree with the
/// row this module writes.
pub fn throttle_cooldown_secs(retry_after: Option<RetryAfter>, now: DateTime<Utc>) -> u64 {
    match retry_after {
        Some(retry_after) => retry_after.cooldown_secs_from(now),
        None => DEFAULT_THROTTLE_COOLDOWN_SECS.clamp(MIN_COOLDOWN_SECS, MAX_COOLDOWN_SECS),
    }
}

// ─── the outcome vocabulary ──────────────────────────────────────────────────

/// What a lane observed, as far as the *deployment* authority is concerned.
///
/// Note what has no variant: authentication. See the module header — that
/// absence is the enforcement mechanism for #1681 D4's attribution rule, not a
/// convenience.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentOutcome {
    /// The deployment answered with a usable response.
    Served,
    /// The deployment throttled us (`429`) or reported the quota behind it
    /// spent (`402`). The one outcome that buys a cooldown by default.
    Throttled { retry_after: Option<RetryAfter> },
    /// No response at all: a timeout, a connect failure, a dropped body.
    Unreachable,
    /// The deployment itself failed (`5xx`). Cools down **only** when the
    /// provider named a `Retry-After`: honouring an explicit instruction is the
    /// provider's policy, whereas inventing a backoff for every 5xx would be
    /// selection policy, which is the resolver's (#1681 D5/PR-D) to own.
    ServerError {
        status: u16,
        retry_after: Option<RetryAfter>,
    },
    /// The deployment answered, and the answer was unusable: an unparseable
    /// body, an empty completion, or a refusal that is neither auth nor
    /// throttling (`400`, `404`, `422`).
    UnusableResponse { status: Option<u16> },
}

/// What a lane saw at the transport/HTTP boundary — the input
/// [`DeploymentOutcome::classify`] reads.
///
/// Typed rather than "an `Option<u16>` plus a `bool`" so that "no status
/// because the transport died" and "no status because we did not look" cannot
/// be spelled the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderResponseSignal {
    /// The request never produced a status line.
    NoResponse,
    /// The provider answered with this status.
    Status(u16),
    /// A success status whose body could not be used.
    UnusableBody,
}

impl DeploymentOutcome {
    /// Classify what a lane saw for the deployment authority.
    ///
    /// `None` means "not this authority's business": today that is exactly
    /// `401` and `403`, which belong to the credential and account surfaces and
    /// are recorded there by the existing paths, unchanged (#1681 D4).
    ///
    /// The runtime half of the attribution rule; the compile-time half is that
    /// there is no auth variant for this function to return even if it wanted
    /// to.
    pub fn classify(
        signal: ProviderResponseSignal,
        retry_after: Option<RetryAfter>,
    ) -> Option<Self> {
        match signal {
            ProviderResponseSignal::NoResponse => Some(Self::Unreachable),
            ProviderResponseSignal::UnusableBody => Some(Self::UnusableResponse { status: None }),
            ProviderResponseSignal::Status(401 | 403) => None,
            ProviderResponseSignal::Status(429 | 402) => Some(Self::Throttled { retry_after }),
            ProviderResponseSignal::Status(status) if (500..600).contains(&status) => {
                Some(Self::ServerError {
                    status,
                    retry_after,
                })
            }
            ProviderResponseSignal::Status(status) if (200..300).contains(&status) => {
                Some(Self::Served)
            }
            ProviderResponseSignal::Status(status) => Some(Self::UnusableResponse {
                status: Some(status),
            }),
        }
    }

    /// Stable label written into `metadata.outcome` and into the event
    /// evidence.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Served => "served",
            Self::Throttled { .. } => "throttled",
            Self::Unreachable => "unreachable",
            Self::ServerError { .. } => "server_error",
            Self::UnusableResponse { .. } => "unusable_response",
        }
    }

    /// The `model_deployment_health.state` this outcome puts the row in.
    pub fn state(self) -> &'static str {
        match self {
            Self::Served => DEPLOYMENT_HEALTH_STATE_OK,
            Self::Throttled { .. } => DEPLOYMENT_HEALTH_STATE_COOLDOWN,
            Self::Unreachable | Self::ServerError { .. } | Self::UnusableResponse { .. } => {
                DEPLOYMENT_HEALTH_STATE_ERROR
            }
        }
    }

    /// The event kind this outcome appends to `model_deployment_events`.
    pub fn event_kind(self) -> DeploymentEventKind {
        match self {
            Self::Served => DeploymentEventKind::HealthServed,
            Self::Throttled { .. } => DeploymentEventKind::HealthCooldown,
            Self::Unreachable | Self::ServerError { .. } | Self::UnusableResponse { .. } => {
                DeploymentEventKind::HealthError
            }
        }
    }

    /// The HTTP status this outcome carries, if it had one. Numeric and
    /// therefore public-safe — unlike a provider's response body, which this
    /// writer never accepts.
    pub fn status(self) -> Option<u16> {
        match self {
            Self::ServerError { status, .. } => Some(status),
            Self::UnusableResponse { status } => status,
            Self::Served | Self::Throttled { .. } | Self::Unreachable => None,
        }
    }

    /// When this outcome says the deployment becomes selectable again.
    fn cooldown_until(self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Self::Throttled { retry_after } => {
                Some(now + Duration::seconds(throttle_cooldown_secs(retry_after, now) as i64))
            }
            // See the variant note: an explicit instruction is honoured, an
            // invented backoff is not.
            Self::ServerError { retry_after, .. } => {
                retry_after.map(|retry_after| retry_after.cooldown_until(now))
            }
            Self::Served | Self::Unreachable | Self::UnusableResponse { .. } => None,
        }
    }

    /// The `last_error` text this outcome carries. Generated from the closed
    /// vocabulary and a numeric status — never from provider-supplied text,
    /// which this writer has no parameter to receive.
    fn generated_error(self, cooldown_secs: Option<u64>) -> Option<String> {
        match self {
            Self::Served => None,
            Self::Throttled { .. } => Some(format!(
                "throttled by the provider; retry after {}s",
                cooldown_secs.unwrap_or(DEFAULT_THROTTLE_COOLDOWN_SECS)
            )),
            Self::Unreachable => Some("no response from the provider".to_string()),
            Self::ServerError { status, .. } => Some(format!("provider returned HTTP {status}")),
            Self::UnusableResponse {
                status: Some(status),
            } => Some(format!("unusable response (HTTP {status})")),
            Self::UnusableResponse { status: None } => Some("unusable response".to_string()),
        }
    }
}

// ─── the writer ──────────────────────────────────────────────────────────────

/// What [`record_deployment_outcome`] decided, for the caller that owns the
/// connection.
///
/// Both halves travel together on purpose: a health row that moved without its
/// event would make the fold (#1681 discrimination 12) disagree with the table
/// it projects, and the only way to make that hard is to hand the caller one
/// value it cannot half-persist without noticing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentOutcomeWrite {
    /// The health row to persist.
    pub health: ModelDeploymentHealth,
    /// The append-only event that records the same transition.
    pub event: NewModelDeploymentEvent,
    /// Set only when this outcome bought a cooldown.
    pub cooldown_until: Option<DateTime<Utc>>,
    /// Set only by [`DeploymentOutcome::Served`]: any cooldown this deployment
    /// carried is now void.
    pub clear_cooldown: bool,
}

/// **The** `model_deployment_health` writer (#1681 D4).
///
/// `existing` is the row as last read (`None` = no row yet). `catalog_revision`
/// is the deployment's current `model_deployments.revision`, carried onto the
/// event because that column is `NOT NULL`: a health event does not *advance*
/// the catalog revision, it records the revision it observed, so a fold over
/// the log still reports the latest catalog revision (#1681 discrimination 12).
/// `now` is injected rather than read here so every field of one write shares a
/// single instant and tests can pin it.
///
/// Nothing is persisted — the caller owns the connection, exactly as
/// `record_key_outcome` leaves persistence to its caller.
pub fn record_deployment_outcome(
    existing: Option<&ModelDeploymentHealth>,
    deployment_id: &str,
    catalog_revision: i64,
    outcome: DeploymentOutcome,
    evidence: EvidenceKind,
    now: DateTime<Utc>,
) -> DeploymentOutcomeWrite {
    let now_iso = now.to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut health = match existing {
        Some(existing) => {
            let mut health = existing.clone();
            // The write names its own identity: a stale id on the supplied row
            // can never redirect it at another deployment.
            health.deployment_id = deployment_id.to_string();
            health
        }
        None => new_deployment_health(deployment_id, &now_iso),
    };

    let cooldown_until = outcome.cooldown_until(now);
    let cooldown_secs = cooldown_until.map(|until| (until - now).num_seconds().max(0) as u64);
    let clear_cooldown = matches!(outcome, DeploymentOutcome::Served);

    health.state = outcome.state().to_string();
    match outcome {
        DeploymentOutcome::Served => {
            health.cooldown_until = None;
            health.last_success_at = Some(now_iso.clone());
            health.last_error = None;
            health.error_count = 0;
        }
        _ => {
            // A cooldown this outcome did not set is left exactly as found:
            // a 5xx arriving while a 429 cooldown is still running must not
            // quietly make the deployment selectable again.
            if let Some(until) = cooldown_until {
                health.cooldown_until = Some(until.to_rfc3339_opts(SecondsFormat::Millis, true));
            }
            health.last_error = outcome.generated_error(cooldown_secs);
            health.error_count += 1;
        }
    }

    health.evidence_kind = Some(evidence);
    health.last_attempt_at = Some(now_iso.clone());
    health.observed_at = now_iso.clone();
    health.updated_at = now_iso.clone();
    health.metadata = stamp_outcome(&health.metadata, outcome, cooldown_secs);

    let event = NewModelDeploymentEvent::new(deployment_id, catalog_revision, outcome.event_kind())
        .with_evidence(
            event_evidence(&health, outcome, evidence, cooldown_secs, &now_iso).to_string(),
        );

    DeploymentOutcomeWrite {
        health,
        event,
        cooldown_until,
        clear_cooldown,
    }
}

/// A fresh, healthy row for a deployment nothing has been recorded about yet.
pub fn new_deployment_health(deployment_id: &str, now_iso: &str) -> ModelDeploymentHealth {
    ModelDeploymentHealth {
        deployment_id: deployment_id.to_string(),
        state: DEPLOYMENT_HEALTH_STATE_OK.to_string(),
        cooldown_until: None,
        last_success_at: None,
        last_attempt_at: None,
        last_error: None,
        error_count: 0,
        evidence_kind: None,
        observed_at: now_iso.to_string(),
        metadata: "{}".to_string(),
        updated_at: now_iso.to_string(),
    }
}

/// Merge this outcome's stamp into the row's `metadata` object, preserving
/// keys a future writer may have put there. Metadata that is not a JSON object
/// is replaced rather than parsed — an unreadable blob is not something to
/// append to (the `vault::health::stamp_evidence` precedent).
fn stamp_outcome(metadata: &str, outcome: DeploymentOutcome, cooldown_secs: Option<u64>) -> String {
    let mut object = match serde_json::from_str::<Value>(metadata) {
        Ok(Value::Object(object)) => object,
        _ => Map::new(),
    };
    object.insert(
        HEALTH_OUTCOME_FIELD.to_string(),
        Value::String(outcome.as_str().to_string()),
    );
    match outcome.status() {
        Some(status) => {
            object.insert(HEALTH_STATUS_FIELD.to_string(), json!(status));
        }
        None => {
            object.remove(HEALTH_STATUS_FIELD);
        }
    }
    match cooldown_secs {
        Some(secs) => {
            object.insert(HEALTH_COOLDOWN_SECS_FIELD.to_string(), json!(secs));
        }
        None => {
            object.remove(HEALTH_COOLDOWN_SECS_FIELD);
        }
    }
    Value::Object(object).to_string()
}

/// The event's evidence payload: labels, a numeric status, counts and
/// timestamps. Public-safe by construction — there is no provider text in
/// scope to leak.
fn event_evidence(
    health: &ModelDeploymentHealth,
    outcome: DeploymentOutcome,
    evidence: EvidenceKind,
    cooldown_secs: Option<u64>,
    now_iso: &str,
) -> Value {
    json!({
        "outcome": outcome.as_str(),
        "state": health.state,
        "evidence_kind": evidence.as_str(),
        "observed_at": now_iso,
        "status": outcome.status(),
        "cooldown_until": health.cooldown_until,
        "cooldown_secs": cooldown_secs,
        "error_count": health.error_count,
    })
}

#[cfg(test)]
mod tests;
