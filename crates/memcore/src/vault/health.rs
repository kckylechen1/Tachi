// vault/health.rs — the single writer for `vault_key_health` (tachi#1680 D6).
//
// # Why one writer
//
// Before this module, three independent places decided what one key's health
// row looks like after an outcome: the LLM client's `mark_secret_*` helpers,
// its caller-reported `record_provider_key_result`, and the CLI's
// `build_key_health_result` behind `tachi vault record-key-result` /
// the `vault_record_key_result` MCP tool. Each carried its own status-string
// ladder, its own cooldown arithmetic, and its own idea of which neighbouring
// fields an outcome may clear — which is exactly how a "rate limited" report
// on one channel and the same report on another channel stopped agreeing.
//
// [`record_key_outcome`] is now the only function that turns an outcome into a
// row. Every channel classifies into [`TypedOutcome`], names its
// [`EvidenceKind`], and hands both here; persistence stays with whoever owns
// the connection.
//
// # Evidence kinds
//
// The distinction is *how the outcome was learned*, not who typed it:
//
// - [`EvidenceKind::Probed`] — Tachi made a deliberate, non-generating
//   authentication request to the provider's documented endpoint and read the
//   status code itself (`tachi_llm::llm::auth_probe`).
// - [`EvidenceKind::SelfReported`] — a consumer of the key told us how its own
//   usage went (the LLM invocation path, the MCP tool, the CLI). Truthful, but
//   unverified by us and reportable by anything holding the tool.
//
// The kind lands in `vault_key_health.metadata`, which is a raw `TEXT` column
// that has held the literal `"{}"` at every construction site and that no
// reader parses for a fixed shape — so this is a zero-schema, zero-wire
// change (#1680 D6). Readers who want it can use
// [`EvidenceKind::from_metadata`] or `json_extract(metadata, '$.evidence_kind')`.
//
// # What an outcome may not do
//
// Two invariants are load-bearing and tested:
//
// 1. A write names exactly one `(logical_name, key_id)` — the table's primary
//    key. A 401/403/429 for one pool member can never touch a sibling member,
//    because no code path here can express a row it was not handed.
// 2. [`TypedOutcome::Unknown`] — "an attempt happened, the provider said
//    nothing about this credential" (probe transport failure, refused
//    redirect, unparseable body) — is **non-destructive**: it stamps the
//    attempt and the evidence and leaves the health binding (status,
//    auth_failed, disabled, cooldown, counters, last_success/last_error)
//    exactly as it found it. A probe that could not reach the provider must
//    never be able to clear a real auth failure, nor manufacture one.

use chrono::{DateTime, Duration, Utc};
use serde_json::{Map, Value};

use super::VaultKeyHealth;

/// `vault_key_health.status` vocabulary. These strings are persisted and are
/// read back by `tachi_llm`'s availability mapping and by the status/doctor
/// surfaces, so they are constants here rather than literals at each site.
pub const HEALTH_STATUS_OK: &str = "ok";
pub const HEALTH_STATUS_AUTH_FAILED: &str = "auth_failed";
pub const HEALTH_STATUS_RATE_LIMITED: &str = "rate_limited";
pub const HEALTH_STATUS_EXHAUSTED: &str = "exhausted";
pub const HEALTH_STATUS_ERROR: &str = "error";

/// `metadata` JSON field carrying [`EvidenceKind`].
pub const EVIDENCE_KIND_FIELD: &str = "evidence_kind";
/// `metadata` JSON field carrying the timestamp of the evidence.
pub const EVIDENCE_AT_FIELD: &str = "evidence_at";
/// `metadata` JSON field carrying the [`TypedOutcome`] the evidence produced.
pub const EVIDENCE_OUTCOME_FIELD: &str = "evidence_outcome";

/// Default cooldown when a rate-limit report carries no `Retry-After`.
const DEFAULT_RATE_LIMIT_COOLDOWN_SECS: u64 = 60;
/// Cooldown clamp — an unbounded provider-supplied `Retry-After` must not be
/// able to park a credential forever.
const MIN_RATE_LIMIT_COOLDOWN_SECS: u64 = 1;
const MAX_RATE_LIMIT_COOLDOWN_SECS: u64 = 3600;

/// How an outcome was learned. See the module header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceKind {
    /// Observed by Tachi's own non-generating auth probe.
    Probed,
    /// Reported by a consumer of the credential (invocation path, MCP tool, CLI).
    SelfReported,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Probed => "probed",
            Self::SelfReported => "self_reported",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "probed" => Some(Self::Probed),
            "self_reported" => Some(Self::SelfReported),
            _ => None,
        }
    }

    /// Read the evidence kind back out of a stored `metadata` value. Returns
    /// `None` for the legacy `"{}"` rows, for non-object metadata, and for an
    /// unrecognized kind — never a guess.
    pub fn from_metadata(metadata: &str) -> Option<Self> {
        serde_json::from_str::<Value>(metadata)
            .ok()?
            .get(EVIDENCE_KIND_FIELD)?
            .as_str()
            .and_then(Self::parse)
    }
}

/// The closed outcome vocabulary every health-writing channel classifies into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypedOutcome {
    /// The credential authenticated (2xx / explicit success report).
    Success,
    /// The provider rejected the credential (401/403).
    AuthFailed,
    /// The provider throttled this credential (429 / explicit report).
    RateLimited { retry_after_secs: Option<u64> },
    /// The credential is out of quota/credit (402 / explicit report).
    Exhausted,
    /// A classified failure that says something about this attempt but does
    /// not condemn the credential (unexpected status, provider 5xx reported
    /// through a channel that asked for it to be counted).
    Error,
    /// No verdict: an attempt happened, the provider said nothing usable about
    /// this credential. **Non-destructive** — see the module header.
    Unknown,
}

impl TypedOutcome {
    /// The single classifier for the caller-reported channels, which speak
    /// "HTTP status code plus optional free-text outcome word". Both the LLM
    /// client's `record_provider_key_result` and the CLI's
    /// `record-key-result` used to carry their own copy of this ladder; the
    /// copies had drifted in ordering, so this one is authoritative.
    ///
    /// Ordering note: an explicit `exhausted` word wins over a 401/403 status
    /// (the LLM client's original order), because that combination is a
    /// contradictory report and `exhausted` is the less destructive reading —
    /// it does not raise the `auth_failed` flag that removes a key from
    /// selection until the retry TTL expires.
    pub fn classify(
        status_code: Option<u16>,
        outcome: Option<&str>,
        retry_after_secs: Option<u64>,
    ) -> Self {
        let outcome = outcome.map(|value| value.trim().to_ascii_lowercase());
        let outcome = outcome.as_deref();
        if status_code == Some(429) || matches!(outcome, Some("rate_limited" | "cooldown")) {
            Self::RateLimited { retry_after_secs }
        } else if matches!(outcome, Some("exhausted")) {
            Self::Exhausted
        } else if matches!(status_code, Some(401 | 403)) || matches!(outcome, Some("auth_failed")) {
            Self::AuthFailed
        } else if status_code.is_some_and(|code| (200..300).contains(&code))
            || matches!(outcome, Some("success" | "ok"))
        {
            Self::Success
        } else {
            Self::Error
        }
    }

    /// Stable label written into `metadata.evidence_outcome`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::AuthFailed => "auth_failed",
            Self::RateLimited { .. } => "rate_limited",
            Self::Exhausted => "exhausted",
            Self::Error => "error",
            Self::Unknown => "unknown",
        }
    }

    /// The `last_error` text an outcome carries when the channel supplied no
    /// reason of its own. `None` means "leave whatever is there alone".
    fn default_reason(self) -> Option<String> {
        match self {
            Self::AuthFailed => Some("auth failure".to_string()),
            Self::Exhausted => Some("key exhausted".to_string()),
            Self::RateLimited { retry_after_secs } => Some(format!(
                "rate limited; retry after {}s",
                rate_limit_cooldown_secs(retry_after_secs)
            )),
            Self::Success | Self::Error | Self::Unknown => None,
        }
    }
}

/// The cooldown a rate-limit report actually buys, after defaulting and
/// clamping. Exposed so an in-memory cooldown mirror (`tachi_llm`'s
/// `Instant`-based one) and the persisted `cooldown_until` cannot disagree.
pub fn rate_limit_cooldown_secs(retry_after_secs: Option<u64>) -> u64 {
    retry_after_secs
        .unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN_SECS)
        .clamp(MIN_RATE_LIMIT_COOLDOWN_SECS, MAX_RATE_LIMIT_COOLDOWN_SECS)
}

/// A fresh, healthy row for a credential nothing has been recorded about yet.
/// The one constructor — in-memory provider state and the CLI both used to
/// spell this literal out, `metadata: "{}"` included.
pub fn new_key_health(logical_name: &str, key_id: &str, now: DateTime<Utc>) -> VaultKeyHealth {
    VaultKeyHealth {
        logical_name: logical_name.to_string(),
        key_id: key_id.to_string(),
        status: HEALTH_STATUS_OK.to_string(),
        cooldown_until: None,
        last_success: None,
        last_attempt: None,
        last_error: None,
        error_count: 0,
        auth_failed: false,
        disabled: false,
        metadata: "{}".to_string(),
        updated_at: now.to_rfc3339(),
    }
}

/// What [`record_key_outcome`] decided, for the caller that owns persistence
/// and (in the LLM client's case) an ephemeral in-memory cooldown mirror.
#[derive(Debug, Clone)]
pub struct KeyOutcomeWrite {
    /// The row to persist. Already carries the evidence stamp.
    pub health: VaultKeyHealth,
    /// Set only by [`TypedOutcome::RateLimited`]: when the credential becomes
    /// selectable again.
    pub cooldown_until: Option<DateTime<Utc>>,
    /// Set only by [`TypedOutcome::Success`]: any ephemeral cooldown for this
    /// member is now void.
    pub clear_cooldown: bool,
}

/// **The** `vault_key_health` writer (#1680 D6).
///
/// `existing` is the row as last known (in-memory state for the LLM client, a
/// `vault_get_key_health` read for the CLI); `None` means "no row yet" and
/// starts from [`new_key_health`]. `now` is injected rather than read here so
/// every field of one write shares a single instant and tests can pin it.
///
/// The returned row is not persisted — the caller owns the connection (see
/// the module header on why this is a pure function).
pub fn record_key_outcome(
    existing: Option<&VaultKeyHealth>,
    logical_name: &str,
    key_id: &str,
    outcome: TypedOutcome,
    evidence: EvidenceKind,
    reason: Option<&str>,
    now: DateTime<Utc>,
) -> KeyOutcomeWrite {
    let mut health = match existing {
        Some(existing) => {
            let mut health = existing.clone();
            // The write names its own identity; a stale/blank identity on the
            // supplied row can never redirect it at another member.
            health.logical_name = logical_name.to_string();
            health.key_id = key_id.to_string();
            health
        }
        None => new_key_health(logical_name, key_id, now),
    };
    let now_iso = now.to_rfc3339();
    let reason = reason
        .map(|value| value.to_string())
        .or_else(|| outcome.default_reason());

    let mut cooldown_until = None;
    let mut clear_cooldown = false;

    match outcome {
        TypedOutcome::Success => {
            health.status = HEALTH_STATUS_OK.to_string();
            health.auth_failed = false;
            health.cooldown_until = None;
            health.last_success = Some(now_iso.clone());
            health.last_error = None;
            health.error_count = 0;
            clear_cooldown = true;
        }
        TypedOutcome::AuthFailed => {
            health.status = HEALTH_STATUS_AUTH_FAILED.to_string();
            health.auth_failed = true;
            health.cooldown_until = None;
            health.last_error = reason;
            health.error_count += 1;
        }
        TypedOutcome::RateLimited { retry_after_secs } => {
            let cooldown = rate_limit_cooldown_secs(retry_after_secs);
            let until = now + Duration::seconds(cooldown as i64);
            health.status = HEALTH_STATUS_RATE_LIMITED.to_string();
            health.cooldown_until = Some(until.to_rfc3339());
            health.last_error = reason;
            health.error_count += 1;
            cooldown_until = Some(until);
        }
        TypedOutcome::Exhausted => {
            health.status = HEALTH_STATUS_EXHAUSTED.to_string();
            health.last_error = reason;
            health.error_count += 1;
        }
        TypedOutcome::Error => {
            health.status = HEALTH_STATUS_ERROR.to_string();
            health.last_error = reason;
            health.error_count += 1;
        }
        // Deliberately touches nothing but the attempt and the evidence.
        TypedOutcome::Unknown => {}
    }

    health.last_attempt = Some(now_iso.clone());
    health.updated_at = now_iso.clone();
    health.metadata = stamp_evidence(&health.metadata, outcome, evidence, &now_iso);

    KeyOutcomeWrite {
        health,
        cooldown_until,
        clear_cooldown,
    }
}

/// Merge the evidence stamp into an existing `metadata` object, preserving any
/// other keys a future writer may have put there. Metadata that is not a JSON
/// object (the column is raw `TEXT`) is replaced rather than parsed — an
/// unreadable blob is not something to append to.
fn stamp_evidence(
    metadata: &str,
    outcome: TypedOutcome,
    evidence: EvidenceKind,
    now_iso: &str,
) -> String {
    let mut object = match serde_json::from_str::<Value>(metadata) {
        Ok(Value::Object(object)) => object,
        _ => Map::new(),
    };
    object.insert(
        EVIDENCE_KIND_FIELD.to_string(),
        Value::String(evidence.as_str().to_string()),
    );
    object.insert(
        EVIDENCE_OUTCOME_FIELD.to_string(),
        Value::String(outcome.as_str().to_string()),
    );
    object.insert(
        EVIDENCE_AT_FIELD.to_string(),
        Value::String(now_iso.to_string()),
    );
    Value::Object(object).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + seconds, 0).expect("fixed test instant")
    }

    #[test]
    fn classify_maps_both_caller_channels_onto_one_ladder() {
        assert_eq!(
            TypedOutcome::classify(Some(429), None, Some(30)),
            TypedOutcome::RateLimited {
                retry_after_secs: Some(30)
            }
        );
        assert_eq!(
            TypedOutcome::classify(None, Some("COOLDOWN"), None),
            TypedOutcome::RateLimited {
                retry_after_secs: None
            }
        );
        assert_eq!(
            TypedOutcome::classify(Some(401), None, None),
            TypedOutcome::AuthFailed
        );
        assert_eq!(
            TypedOutcome::classify(Some(403), None, None),
            TypedOutcome::AuthFailed
        );
        assert_eq!(
            TypedOutcome::classify(None, Some("exhausted"), None),
            TypedOutcome::Exhausted
        );
        assert_eq!(
            TypedOutcome::classify(Some(200), None, None),
            TypedOutcome::Success
        );
        assert_eq!(
            TypedOutcome::classify(None, Some("ok"), None),
            TypedOutcome::Success
        );
        assert_eq!(
            TypedOutcome::classify(Some(500), None, None),
            TypedOutcome::Error
        );
        assert_eq!(
            TypedOutcome::classify(None, None, None),
            TypedOutcome::Error
        );
        // The one ordering the two old copies disagreed about.
        assert_eq!(
            TypedOutcome::classify(Some(401), Some("exhausted"), None),
            TypedOutcome::Exhausted
        );
    }

    #[test]
    fn rate_limit_cooldown_is_defaulted_and_clamped() {
        assert_eq!(rate_limit_cooldown_secs(None), 60);
        assert_eq!(rate_limit_cooldown_secs(Some(0)), 1);
        assert_eq!(rate_limit_cooldown_secs(Some(30)), 30);
        assert_eq!(rate_limit_cooldown_secs(Some(u64::MAX)), 3600);
    }

    #[test]
    fn each_outcome_writes_its_documented_row() {
        let write = record_key_outcome(
            None,
            "DEEPSEEK_API_KEY",
            "DEEPSEEK_API_KEY_2",
            TypedOutcome::AuthFailed,
            EvidenceKind::Probed,
            None,
            at(0),
        );
        assert_eq!(write.health.status, HEALTH_STATUS_AUTH_FAILED);
        assert!(write.health.auth_failed);
        assert_eq!(write.health.error_count, 1);
        assert_eq!(write.health.last_error.as_deref(), Some("auth failure"));
        assert_eq!(write.health.cooldown_until, None);
        assert!(write.cooldown_until.is_none());

        let write = record_key_outcome(
            Some(&write.health),
            "DEEPSEEK_API_KEY",
            "DEEPSEEK_API_KEY_2",
            TypedOutcome::RateLimited {
                retry_after_secs: Some(30),
            },
            EvidenceKind::SelfReported,
            None,
            at(1),
        );
        assert_eq!(write.health.status, HEALTH_STATUS_RATE_LIMITED);
        assert_eq!(write.health.error_count, 2);
        assert_eq!(
            write.health.last_error.as_deref(),
            Some("rate limited; retry after 30s")
        );
        assert_eq!(write.cooldown_until, Some(at(31)));
        assert_eq!(
            write.health.cooldown_until.as_deref(),
            Some(at(31).to_rfc3339().as_str())
        );

        let write = record_key_outcome(
            Some(&write.health),
            "DEEPSEEK_API_KEY",
            "DEEPSEEK_API_KEY_2",
            TypedOutcome::Success,
            EvidenceKind::Probed,
            None,
            at(2),
        );
        assert_eq!(write.health.status, HEALTH_STATUS_OK);
        assert!(!write.health.auth_failed);
        assert_eq!(write.health.error_count, 0);
        assert_eq!(write.health.last_error, None);
        assert_eq!(write.health.cooldown_until, None);
        assert_eq!(
            write.health.last_success.as_deref(),
            Some(&*at(2).to_rfc3339())
        );
        assert!(write.clear_cooldown);
    }

    /// #1680 disc-5: a probe that learned nothing must not be able to rewrite
    /// the health binding — in either direction.
    #[test]
    fn unknown_outcome_never_replaces_the_health_binding() {
        let failed = record_key_outcome(
            None,
            "SILICONFLOW_API_KEY",
            "SILICONFLOW_API_KEY_1",
            TypedOutcome::AuthFailed,
            EvidenceKind::SelfReported,
            Some("provider rejected the key"),
            at(0),
        )
        .health;

        let write = record_key_outcome(
            Some(&failed),
            "SILICONFLOW_API_KEY",
            "SILICONFLOW_API_KEY_1",
            TypedOutcome::Unknown,
            EvidenceKind::Probed,
            Some("probe transport failure"),
            at(5),
        );

        assert_eq!(write.health.status, failed.status);
        assert_eq!(write.health.auth_failed, failed.auth_failed);
        assert_eq!(write.health.disabled, failed.disabled);
        assert_eq!(write.health.cooldown_until, failed.cooldown_until);
        assert_eq!(write.health.error_count, failed.error_count);
        assert_eq!(write.health.last_error, failed.last_error);
        assert_eq!(write.health.last_success, failed.last_success);
        assert!(write.cooldown_until.is_none());
        assert!(!write.clear_cooldown);
        // Only the attempt and the evidence moved.
        assert_eq!(
            write.health.last_attempt.as_deref(),
            Some(&*at(5).to_rfc3339())
        );
        assert_eq!(
            EvidenceKind::from_metadata(&write.health.metadata),
            Some(EvidenceKind::Probed)
        );
    }

    /// A disabled or auth-failed credential is never re-enabled as a side
    /// effect of some *other* failure being reported.
    #[test]
    fn failure_outcomes_never_clear_an_operator_disable() {
        let mut disabled = new_key_health("EXA_API_KEY", "EXA_API_KEY", at(0));
        disabled.disabled = true;
        disabled.auth_failed = true;

        for outcome in [
            TypedOutcome::AuthFailed,
            TypedOutcome::Exhausted,
            TypedOutcome::Error,
            TypedOutcome::RateLimited {
                retry_after_secs: None,
            },
            TypedOutcome::Unknown,
        ] {
            let write = record_key_outcome(
                Some(&disabled),
                "EXA_API_KEY",
                "EXA_API_KEY",
                outcome,
                EvidenceKind::SelfReported,
                None,
                at(1),
            );
            assert!(write.health.disabled, "{outcome:?} must not re-enable");
            assert!(
                write.health.auth_failed,
                "{outcome:?} must not clear an auth failure"
            );
        }
    }

    #[test]
    fn evidence_kinds_are_distinguishable_in_storage() {
        let probed = record_key_outcome(
            None,
            "DEEPSEEK_API_KEY",
            "DEEPSEEK_API_KEY_1",
            TypedOutcome::Success,
            EvidenceKind::Probed,
            None,
            at(0),
        )
        .health;
        let reported = record_key_outcome(
            None,
            "DEEPSEEK_API_KEY",
            "DEEPSEEK_API_KEY_1",
            TypedOutcome::Success,
            EvidenceKind::SelfReported,
            None,
            at(0),
        )
        .health;

        assert_ne!(probed.metadata, reported.metadata);
        assert_eq!(
            EvidenceKind::from_metadata(&probed.metadata),
            Some(EvidenceKind::Probed)
        );
        assert_eq!(
            EvidenceKind::from_metadata(&reported.metadata),
            Some(EvidenceKind::SelfReported)
        );
        // Legacy rows stay legible as "no evidence recorded", never as a guess.
        assert_eq!(EvidenceKind::from_metadata("{}"), None);
        assert_eq!(EvidenceKind::from_metadata("not json"), None);
    }

    #[test]
    fn evidence_stamp_preserves_unrelated_metadata_keys() {
        let mut existing = new_key_health("XAI_API_KEY", "XAI_API_KEY", at(0));
        existing.metadata = r#"{"note":"kept"}"#.to_string();

        let write = record_key_outcome(
            Some(&existing),
            "XAI_API_KEY",
            "XAI_API_KEY",
            TypedOutcome::Success,
            EvidenceKind::Probed,
            None,
            at(1),
        );

        let parsed: Value = serde_json::from_str(&write.health.metadata).expect("object metadata");
        assert_eq!(parsed["note"], Value::String("kept".to_string()));
        assert_eq!(parsed[EVIDENCE_KIND_FIELD], Value::String("probed".into()));
        assert_eq!(
            parsed[EVIDENCE_OUTCOME_FIELD],
            Value::String("success".into())
        );
    }

    /// #1680 disc-4, at the writer: a write can only ever name the member it
    /// was handed, even when the row it was given claims another identity.
    #[test]
    fn a_write_names_exactly_the_member_it_was_handed() {
        let sibling = new_key_health("DEEPSEEK_API_KEY", "DEEPSEEK_API_KEY_1", at(0));

        let write = record_key_outcome(
            Some(&sibling),
            "DEEPSEEK_API_KEY",
            "DEEPSEEK_API_KEY_2",
            TypedOutcome::AuthFailed,
            EvidenceKind::Probed,
            None,
            at(1),
        );

        assert_eq!(write.health.logical_name, "DEEPSEEK_API_KEY");
        assert_eq!(write.health.key_id, "DEEPSEEK_API_KEY_2");
    }
}
