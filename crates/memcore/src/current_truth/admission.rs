//! Validated append and the admission policy.
//!
//! Two halves that must be read together:
//!
//! - **Append** ([`build_truth_assertion_event`]) turns a caller's observation
//!   into a [`TachiEventRecord`]. It is pure: no connection, no clock, no
//!   randomness. The event it returns is byte-identical for byte-identical
//!   input, which is what makes `insert_tachi_event_if_absent` a real
//!   append-if-absent rather than a churn generator.
//! - **Admission** ([`classify_assertion`]) decides whether a decoded
//!   assertion may enter reduction at all. `None` means admitted; `Some(reason)`
//!   means candidate. There is no third answer and no score.
//!
//! Both halves enforce redline 3 (agent-authored assertions never decide
//! anything), on purpose, in two independent places:
//!
//! 1. `build_truth_assertion_event` **overwrites** the caller's requested
//!    `authority` with [`AGENT_AUTHORITY_CAP`] whenever the issuer is
//!    [`TruthIssuerV1::Agent`], so an agent-authored row cannot even be written
//!    to the ledger carrying a decision-eligible authority column.
//! 2. `classify_assertion` checks the issuer **first and unconditionally**, so
//!    a row that reached the ledger by some other path — hand-written SQL, an
//!    older writer, a future ingestion bug — still cannot decide anything.
//!
//! Belt and braces is deliberate: (1) alone is a writer-side convention, and
//! the reducer must hold even when its input was not produced by this file.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::types::{AuthorityLevel, EffectScope, TachiEventRecord};

use super::types::{
    normalize_instant, parse_instant, AssertionRelationV1, CandidateReasonV1, CurrentTruthError,
    TruthAssertionV1, TruthIssuerV1, TruthPredicate, TruthValue, TRUTH_ASSERTION_DOMAIN,
    TRUTH_ASSERTION_EVENT_TYPE, TRUTH_ASSERTION_PAYLOAD_VERSION,
};

/// The authority column stamped on any agent-authored assertion, regardless of
/// what the caller asked for.
///
/// `review_signal_only` is deliberately outside
/// [`AuthorityLevel::is_decision_eligible`]'s set
/// (`types/continuity.rs:53-58`), so the existing hook — not a parallel
/// vocabulary invented here — is what refuses it.
pub const AGENT_AUTHORITY_CAP: AuthorityLevel = AuthorityLevel::ReviewSignalOnly;

/// The typed `tachi_events.payload_json` of a truth assertion.
///
/// Note what is **absent**: `assertion_id` and `authority`. Those come from the
/// event *row* (`tachi_events.id` and `tachi_events.authority`) and are joined
/// on at decode. A payload that could name its own identity or its own
/// authority would be a self-certifying claim, which is the entire failure mode
/// this module exists to refuse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TruthAssertionPayloadV1 {
    pub payload_version: String,
    pub subject_ref: String,
    pub predicate: TruthPredicate,
    /// Raw string rather than [`TruthValue`]: `TruthValue` is
    /// `#[serde(transparent)]`, so deserialising straight into it would skip
    /// [`TruthValue::new`]'s non-empty check. Decode runs the constructor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub issuer: TruthIssuerV1,
    pub source_revision: String,
    pub observed_at: String,
    pub evidence_ref: String,
    #[serde(default)]
    pub relation: AssertionRelationV1,
}

/// Everything a caller must supply to append one observation.
///
/// There is deliberately no `projection_hints` field and no `effects` field:
/// redline 1 is enforced by making the offending values *unrepresentable* at
/// the input boundary, not by validating them away afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruthAssertionInputV1 {
    /// Ledger routing columns, mirroring the governed-precedent append.
    pub project: String,
    pub source_repo: String,
    pub adapter: String,
    pub session_id: String,
    pub actor: String,

    pub subject_ref: String,
    pub predicate: TruthPredicate,
    /// Required for every relation except [`AssertionRelationV1::Retracts`],
    /// which withdraws without asserting anything.
    pub value: Option<String>,
    pub issuer: TruthIssuerV1,
    pub source_revision: String,
    pub observed_at: String,
    pub evidence_ref: String,
    pub relation: AssertionRelationV1,
    /// Requested `tachi_events.authority`. Silently capped for agent issuers
    /// (see [`AGENT_AUTHORITY_CAP`]); honoured otherwise.
    pub authority: AuthorityLevel,
}

impl TruthAssertionInputV1 {
    /// Convenience constructor for the common shape: a source snapshot with no
    /// relation to any earlier assertion.
    #[allow(clippy::too_many_arguments)]
    pub fn source_snapshot(
        project: &str,
        subject_ref: &str,
        predicate: TruthPredicate,
        value: &str,
        system: &str,
        snapshot_digest: &str,
        source_revision: &str,
        observed_at: &str,
        evidence_ref: &str,
    ) -> Self {
        Self {
            project: project.to_string(),
            source_repo: project.to_string(),
            adapter: TRUTH_ASSERTION_ADAPTER.to_string(),
            session_id: String::new(),
            actor: system.to_string(),
            subject_ref: subject_ref.to_string(),
            predicate,
            value: Some(value.to_string()),
            issuer: TruthIssuerV1::SourceSnapshot {
                system: system.to_string(),
                snapshot_digest: snapshot_digest.to_string(),
            },
            source_revision: source_revision.to_string(),
            observed_at: observed_at.to_string(),
            evidence_ref: evidence_ref.to_string(),
            relation: AssertionRelationV1::Standalone,
            authority: AuthorityLevel::RawFact,
        }
    }
}

/// `tachi_events.adapter` default for assertions built by this module.
pub const TRUTH_ASSERTION_ADAPTER: &str = "current_truth_fold_v1";

/// A built event together with the assertion it decodes back to.
///
/// Returning both is not redundancy: it lets a caller assert
/// `decode(build(x)) == x`'s semantic core without a database round trip, and
/// it gives the live ingestion wrapper the decoded form it needs for logging
/// without re-parsing its own JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct TruthEventEnvelopeV1 {
    pub event: TachiEventRecord,
    pub assertion: TruthAssertionV1,
}

/// Stable, content-derived `tachi_events.id`.
///
/// # Why the key is what it is
///
/// The adjudicated design says "appended if-absent keyed by `(subject_ref,
/// predicate, source_revision)`". That tuple is the core of this key, and two
/// components are added to it:
///
/// - **`relation`** — a standalone observation and a supersession of an earlier
///   assertion are two different facts about the same revision. Collapsing them
///   would make a correction unappendable.
/// - **`issuer`** — two issuers observing the same predicate at the same
///   revision are two independent observations. Collapsing them would make the
///   *second* one vanish at insert time, and a swallowed observation is exactly
///   how a conflict stops being visible. The adjudicated rule wants conflict to
///   be an output state, which requires both members to reach the ledger.
///
/// **`value` is deliberately not in the key.** Two different values from the
/// same issuer at the same `source_revision` is a source-integrity failure, not
/// a conflict of opinion: `source_revision` is a content hash of the semantic
/// basis (`facade_memory_ops/current_work_anchor.rs:149-156`), so identical
/// revision implies identical observed content. Keying without `value` makes
/// that case surface as `insert_tachi_event_if_absent`'s id-collision refusal
/// (`db/event_ledger.rs:147-153`) instead of being appended as if it were a
/// legitimate disagreement.
pub fn truth_assertion_event_id(
    subject_ref: &str,
    predicate: TruthPredicate,
    source_revision: &str,
    relation: &AssertionRelationV1,
    issuer: &TruthIssuerV1,
) -> String {
    // BTreeMap + serde_json (built without `preserve_order`) gives one
    // spelling per basis, so the hash cannot drift with field order.
    let mut basis: BTreeMap<&str, String> = BTreeMap::new();
    basis.insert("subject_ref", subject_ref.trim().to_string());
    basis.insert("predicate", predicate.as_str().to_string());
    basis.insert("source_revision", source_revision.trim().to_string());
    basis.insert("relation", relation.key_token());
    basis.insert("issuer", issuer_identity_token(issuer));
    let serialized = serde_json::to_string(&basis).unwrap_or_default();
    format!("truth-assertion-{:x}", Sha256::digest(serialized.as_bytes()))
}

/// Stable identity of an issuer, for the append-if-absent key only.
///
/// Not a security boundary: leaf 1 does not claim issuer identity is
/// unforgeable, only that two *distinct declared* issuers get distinct events.
fn issuer_identity_token(issuer: &TruthIssuerV1) -> String {
    match issuer {
        TruthIssuerV1::SourceSnapshot {
            system,
            snapshot_digest,
        } => format!("source_snapshot:{}:{}", system.trim(), snapshot_digest.trim()),
        TruthIssuerV1::OwnerDecision { owner_login } => {
            format!("owner_decision:{}", owner_login.trim())
        }
        TruthIssuerV1::DeploymentReceipt {
            host,
            service,
            receipt_hash,
        } => format!(
            "deployment_receipt:{}:{}:{}",
            host.trim(),
            service.trim(),
            receipt_hash.trim()
        ),
        TruthIssuerV1::Agent { agent } => format!("agent:{}", agent.trim()),
    }
}

/// Validate an observation and build the ledger event carrying it.
///
/// Refuses rather than repairs. The one thing it silently rewrites is an agent
/// issuer's `authority` (see [`AGENT_AUTHORITY_CAP`]) — a downgrade can only
/// remove power, so it needs no caller consent.
pub fn build_truth_assertion_event(
    input: &TruthAssertionInputV1,
) -> Result<TruthEventEnvelopeV1, CurrentTruthError> {
    let invalid = CurrentTruthError::InvalidAssertion;

    let subject_ref = input.subject_ref.trim().to_string();
    if subject_ref.is_empty() {
        return Err(invalid("subject_ref is empty".to_string()));
    }
    let source_revision = input.source_revision.trim().to_string();
    if source_revision.is_empty() {
        return Err(invalid("source_revision is empty".to_string()));
    }
    let evidence_ref = input.evidence_ref.trim().to_string();
    if evidence_ref.is_empty() {
        return Err(invalid(
            "evidence_ref is empty; an assertion that cannot name its evidence is refused"
                .to_string(),
        ));
    }
    input.issuer.validate().map_err(invalid)?;

    if let Some(target) = input.relation.target_assertion_id() {
        if target.trim().is_empty() {
            return Err(invalid("relation target_assertion_id is empty".to_string()));
        }
    }

    let observed_at_ts = parse_instant(&input.observed_at)
        .ok_or_else(|| invalid(format!("observed_at is not RFC3339: {}", input.observed_at)))?;
    let observed_at = normalize_instant(observed_at_ts);

    let value = normalize_value(input.predicate, &input.relation, input.value.as_deref())
        .map_err(invalid)?;

    let authority = if matches!(input.issuer, TruthIssuerV1::Agent { .. }) {
        AGENT_AUTHORITY_CAP
    } else {
        input.authority
    };

    let event_id = truth_assertion_event_id(
        &subject_ref,
        input.predicate,
        &source_revision,
        &input.relation,
        &input.issuer,
    );

    let payload = TruthAssertionPayloadV1 {
        payload_version: TRUTH_ASSERTION_PAYLOAD_VERSION.to_string(),
        subject_ref: subject_ref.clone(),
        predicate: input.predicate,
        value: value.as_ref().map(|v| v.as_str().to_string()),
        issuer: input.issuer.clone(),
        source_revision: source_revision.clone(),
        observed_at: observed_at.clone(),
        evidence_ref: evidence_ref.clone(),
        relation: input.relation.clone(),
    };
    let payload_json = serde_json::to_value(&payload)
        .map_err(|e| CurrentTruthError::Serialization(e.to_string()))?;

    let event = TachiEventRecord {
        id: event_id.clone(),
        source_repo: input.source_repo.trim().to_string(),
        adapter: if input.adapter.trim().is_empty() {
            TRUTH_ASSERTION_ADAPTER.to_string()
        } else {
            input.adapter.trim().to_string()
        },
        project: input.project.trim().to_string(),
        domain: TRUTH_ASSERTION_DOMAIN.to_string(),
        session_id: input.session_id.trim().to_string(),
        actor: input.actor.trim().to_string(),
        event_type: TRUTH_ASSERTION_EVENT_TYPE.to_string(),
        authority,
        // Redline 1, append side: `none` says this event may not move recall,
        // prompts, routing, execution, or domain state.
        effects: vec![EffectScope::None],
        // Redline 1, the load-bearing half. `continuity_ops::projection`'s
        // background sweep projects an event into `memories` only when
        // `event_projections(event)` is non-empty
        // (`continuity_ops/projection/entry.rs:65-74`), and that falls back to
        // a prefix table (same file, 35-63) that `truth.assertion.v1` matches
        // no entry of. Empty hints + a non-matching event type is what keeps
        // reducer *input* out of `memories`.
        projection_hints: Vec::new(),
        payload: payload_json,
        // Not a wall clock. Using the normalised source instant makes the
        // ledger's `(created_at, id)` order *be* the observation order the
        // adjudicated rule reduces over, and keeps a re-fetch of an unchanged
        // object byte-identical. `insert_tachi_event_if_absent` excludes
        // `created_at` from its identity comparison anyway
        // (`db/event_ledger.rs:143-146`), so this choice cannot turn a retry
        // into a false collision either way.
        created_at: observed_at.clone(),
        provenance: json!({
            "issuer_class": input.issuer.class().as_str(),
            "source_revision": source_revision.clone(),
            "evidence_ref": evidence_ref.clone(),
        }),
    };

    let assertion = TruthAssertionV1 {
        assertion_id: event_id,
        subject_ref,
        predicate: input.predicate,
        value,
        issuer: input.issuer.clone(),
        source_revision,
        observed_at,
        evidence_ref,
        relation: input.relation.clone(),
        authority,
    };

    Ok(TruthEventEnvelopeV1 { event, assertion })
}

/// Shared value rules for both build and decode, so an event this crate wrote
/// and an event it merely read are held to the same vocabulary.
fn normalize_value(
    predicate: TruthPredicate,
    relation: &AssertionRelationV1,
    raw: Option<&str>,
) -> Result<Option<TruthValue>, String> {
    if matches!(relation, AssertionRelationV1::Retracts { .. }) {
        // A retraction withdraws; it does not assert. Carrying a value would
        // make "what did it retract to?" a question with a wrong answer.
        return match raw.map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(_) => Err("a retraction must not carry a value".to_string()),
        };
    }
    let raw = raw.ok_or_else(|| format!("predicate {predicate} requires a value"))?;
    let value = TruthValue::new(raw)?;
    if let Some(allowed) = predicate.allowed_values() {
        if !allowed.contains(&value.as_str()) {
            return Err(format!(
                "predicate {predicate} rejects value {value:?}; allowed: {allowed:?}"
            ));
        }
    }
    Ok(Some(value))
}

/// Decode one ledger event into an assertion, or say why not.
///
/// `assertion_id` is taken from `tachi_events.id` and `authority` from
/// `tachi_events.authority`; neither is read from the payload, even if the
/// payload happens to contain a field of that name.
pub fn decode_truth_assertion(event: &TachiEventRecord) -> Result<TruthAssertionV1, String> {
    if event.event_type.trim() != TRUTH_ASSERTION_EVENT_TYPE {
        return Err(format!(
            "event {} has type {}, not {TRUTH_ASSERTION_EVENT_TYPE}",
            event.id, event.event_type
        ));
    }
    let payload: TruthAssertionPayloadV1 = serde_json::from_value(event.payload.clone())
        .map_err(|e| format!("event {} has an invalid typed payload: {e}", event.id))?;

    if payload.payload_version != TRUTH_ASSERTION_PAYLOAD_VERSION {
        return Err(format!(
            "event {} has payload_version {}, not {TRUTH_ASSERTION_PAYLOAD_VERSION}",
            event.id, payload.payload_version
        ));
    }

    let subject_ref = payload.subject_ref.trim().to_string();
    if subject_ref.is_empty() {
        return Err(format!("event {} has an empty subject_ref", event.id));
    }
    let source_revision = payload.source_revision.trim().to_string();
    if source_revision.is_empty() {
        return Err(format!("event {} has an empty source_revision", event.id));
    }
    let evidence_ref = payload.evidence_ref.trim().to_string();
    if evidence_ref.is_empty() {
        return Err(format!("event {} has an empty evidence_ref", event.id));
    }
    payload
        .issuer
        .validate()
        .map_err(|e| format!("event {}: {e}", event.id))?;
    if let Some(target) = payload.relation.target_assertion_id() {
        if target.trim().is_empty() {
            return Err(format!(
                "event {} has a relation with an empty target_assertion_id",
                event.id
            ));
        }
    }

    let observed_at_ts = parse_instant(&payload.observed_at).ok_or_else(|| {
        format!(
            "event {} has observed_at {} which is not RFC3339",
            event.id, payload.observed_at
        )
    })?;

    let value = normalize_value(
        payload.predicate,
        &payload.relation,
        payload.value.as_deref(),
    )
    .map_err(|e| format!("event {}: {e}", event.id))?;

    Ok(TruthAssertionV1 {
        assertion_id: event.id.clone(),
        subject_ref,
        predicate: payload.predicate,
        value,
        issuer: payload.issuer,
        source_revision,
        observed_at: normalize_instant(observed_at_ts),
        evidence_ref,
        relation: payload.relation,
        // From the row. Always.
        authority: event.authority,
    })
}

/// The admission policy. `None` ⇒ admitted into reduction; `Some(reason)` ⇒
/// candidate, recorded in diagnostics and unable to move any truth value.
///
/// `as_of` is the only time input in this module; there is no fallback to
/// "now", and an unparseable `as_of` refuses rather than widening the window.
pub fn classify_assertion(
    assertion: &TruthAssertionV1,
    as_of: &str,
) -> Result<Option<CandidateReasonV1>, CurrentTruthError> {
    let as_of_ts =
        parse_instant(as_of).ok_or_else(|| CurrentTruthError::InvalidAsOf(as_of.to_string()))?;
    Ok(classify_assertion_at(assertion, as_of_ts))
}

/// Internal form used by the fold, which parses `as_of` once for the whole
/// projection instead of once per assertion.
pub(super) fn classify_assertion_at(
    assertion: &TruthAssertionV1,
    as_of: DateTime<Utc>,
) -> Option<CandidateReasonV1> {
    // Redline 3. First, and before anything else is even looked at: no
    // authority column, citation, predicate, or relation can lift it.
    if matches!(assertion.issuer, TruthIssuerV1::Agent { .. }) {
        return Some(CandidateReasonV1::AgentAuthored);
    }
    if assertion.issuer.class() != assertion.predicate.required_issuer() {
        return Some(CandidateReasonV1::IssuerNotAuthoritativeForPredicate);
    }
    // The existing hook (`types/continuity.rs:53-58`), not a parallel ladder.
    if !assertion.authority.is_decision_eligible() {
        return Some(CandidateReasonV1::AuthorityNotDecisionEligible);
    }
    let Some(observed_at) = parse_instant(&assertion.observed_at) else {
        return Some(CandidateReasonV1::UnparsableObservedAt);
    };
    if observed_at > as_of {
        // Real, just not yet true at the requested instant.
        return Some(CandidateReasonV1::ObservedAfterAsOf);
    }
    None
}
