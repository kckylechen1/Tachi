//! Memory lifecycle proposal v2: typed immutable apply payload with a
//! deterministic SHA-256 identity, `hard_state` CAS review, and a single
//! `BEGIN IMMEDIATE` apply transaction that revalidates hash/revision/path/
//! protection, mutates memory, and stamps the proposal — atomically.
//!
//! Legacy v1 proposals (no `schema_version=2` / no `identity`) remain
//! listable but are loudly refused at review and apply: callers must
//! re-propose to upgrade them.
//!
//! ## Identity
//!
//! `compute_lifecycle_identity` hashes a canonical serialization of the
//! typed [`LifecycleApplyPayload`] (source/target endpoint snapshots,
//! nonvolatile human-review display, action, and schema/policy version).
//! Volatile fields (status, review notes, `applied_at`, `expires_at`) live
//! OUTSIDE the payload in the stored proposal JSON, so they can never perturb
//! the identity. At apply, the endpoint portion is rebuilt from LIVE rows and
//! combined with the validated immutable display before identity recomputation;
//! a mismatch means the world drifted between approve and apply → refuse.
//!
//! The membership rule for [`LifecycleEndpointSnapshot`] is **every input the
//! proposal's eligibility/action selection and apply step read**, not every
//! volatile field on a memory row. Anything either boundary consumes but the
//! identity omits is a field a writer can change after approval without
//! tripping the drift check, which makes the reviewed decision and the live
//! decision inputs two different things. Set-valued inputs go in through
//! [`canonical_tags`] so storage order is not mistaken for drift.
//!
//! ## Review
//!
//! `pending → approved | rejected` only, via a `hard_state` version CAS
//! ([`MemoryStore::set_state_if_version`]). Any non-pending status is
//! terminal/already-reviewed and is refused. `rejected` represents keep:
//! no memory mutation, just a terminal status stamp.
//!
//! ## Apply
//!
//! `approved → applied` only, inside one `BEGIN IMMEDIATE` transaction
//! that covers BOTH the memory mutation AND the proposal-status CAS, with
//! recall-cache invalidation deferred to AFTER commit. The full
//! hash/revision/path/protection revalidation runs inside the locked
//! transaction so an intervening writer cannot race the checks.

use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db;
use crate::error::MemoryError;
use crate::types::MemoryEntry;
use crate::MemoryStore;

/// `hard_state` namespace for lifecycle proposals (shared with
/// `tachi-server::facade_memory_ops::consolidate_ops`).
pub const LIFECYCLE_PROPOSAL_NS: &str = "memory_lifecycle_proposals";
/// Schema version for v2 proposals. Anything else is legacy and refused at
/// review/apply (still listable).
pub const LIFECYCLE_SCHEMA_VERSION: u32 = 2;
/// Policy stamp included in every v2 identity so a policy change invalidates
/// all prior proposals.
pub const LIFECYCLE_POLICY_VERSION: &str = "memory-lifecycle-v2";

/// Terminal-state TTL (days) stamped on `rejected` and `applied` proposals.
const LIFECYCLE_TERMINAL_TTL_DAYS: i64 = 30;

pub const ACTION_MERGE_INTO: &str = "merge_into";
pub const ACTION_SUPERSEDE: &str = "supersede";
pub const ACTION_NEAR_DUP_MERGE: &str = "near_dup_merge";
pub const ACTION_ARCHIVE: &str = "archive";
pub const ACTION_PROMOTE_DISTILLED: &str = "promote_distilled";

// ─── Identity ─────────────────────────────────────────────────────────────

/// Canonical form of a tag list bound into the identity: sorted byte-wise and
/// deduplicated.
///
/// Both capture sites — [`snapshot_endpoint`] (propose time, from a live
/// [`MemoryEntry`]) and [`read_endpoint_snapshot`] (apply time, straight off
/// the row) — run every list through this, so the identity is a function of
/// the *set*, not of the column's storage order. Without that, a writer that
/// rewrites the same keywords in a different order would read as drift and
/// refuse every proposal touching the row.
///
/// This is also exactly the shape the merge fold in `apply` produces (it folds
/// through a `BTreeSet`), which is what lets the "did the survivor actually
/// change" guard there compare canonical against canonical instead of
/// canonical against raw storage order.
pub fn canonical_tags(values: &[String]) -> Vec<String> {
    values
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<String>>()
        .into_iter()
        .collect()
}

/// Immutable snapshot of one endpoint (source or target) captured at propose
/// time. Every field participates in the identity hash, so any drift is
/// detected at apply.
///
/// The rule for what belongs here is **every input proposal eligibility/action
/// selection or the apply step reads**, not just the fields a human reviewer
/// looks at. `keywords`/`entities`/
/// `importance` are execution inputs for the merge actions (the fold + the
/// `MAX(importance)` in `apply_lifecycle_proposal_once`), and two live writers
/// mutate exactly those three columns *without* bumping `revision`:
/// `db::memory_crud::update::update_enrichment_fields` (background enrichment,
/// `WHERE id=? AND revision=?`, deliberately no bump) and the write-time
/// Jaccard near-dup merge in `db::memory_crud` (`SET keywords=?, entities=?,
/// importance=MAX(importance,?)`, no bump at all). If those columns were not
/// bound, an enrichment pass landing between approve and apply would leave the
/// snapshot byte-identical while changing what actually gets folded — the
/// reviewer would have approved one keyword set and a different one would
/// execute.
///
/// Generator-only inputs that are not execution inputs are bound
/// action-selectively. Search access recording mutates `access_count`,
/// `recall_count`, `query_diversity`, and sometimes `tier` without a revision
/// bump; stale archive and promote-distilled eligibility consume those fields.
/// Pair generators also consume summary/timestamp when choosing an action or
/// survivor. The optional fields below are populated only for actions that
/// actually use them, so ordinary supersession does not become sensitive to
/// unrelated recall bookkeeping.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LifecycleEndpointSnapshot {
    pub id: String,
    pub revision: i64,
    pub path: String,
    /// SHA-256 hex of `entry.text`.
    pub text_digest: String,
    /// Keyword set in [`canonical_tags`] form (sorted, deduplicated).
    pub keywords: Vec<String>,
    /// Entity set in [`canonical_tags`] form (sorted, deduplicated). This is
    /// the stored `entities` column, which already has `persons` folded in by
    /// `types::fold_person_names_into_entities` at write time, so propose and
    /// apply read the same list.
    pub entities: Vec<String>,
    /// `importance` as raw IEEE-754 bits rather than `f64`.
    ///
    /// Three reasons, all load-bearing: (1) it keeps `Eq` derivable on this
    /// struct, (2) `serde_json` renders non-finite floats as `null`, which
    /// would collapse distinct values into one identity, and (3) bits are an
    /// exact round-trip through SQLite's `REAL`, so a byte-for-byte unchanged
    /// row can never hash differently.
    pub importance_bits: u64,
    /// SHA-256 of `summary`, only for same-path merge/supersede eligibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eligibility_summary_digest: Option<String>,
    /// Source/survivor ordering or staleness timestamp, only for actions whose
    /// generator reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eligibility_timestamp: Option<String>,
    /// Stale-archive's "unused" gate, action-scoped so other actions do not
    /// refuse merely because normal recall bookkeeping advanced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eligibility_access_count: Option<i64>,
    /// Used by stale archive and promote-distilled eligibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eligibility_recall_count: Option<i64>,
    /// Used only by promote-distilled eligibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eligibility_query_diversity: Option<i64>,
    pub archived: bool,
    /// The supersession edge is written by both revisioned lifecycle applies
    /// and `mark_superseded_closing_validity` (without a revision bump). A
    /// stale approved proposal must never overwrite a later canonical edge.
    pub superseded_by: Option<String>,
    /// The validity boundary coupled to `superseded_by`. This is semantic
    /// lifecycle state, not a volatile bookkeeping timestamp.
    pub valid_until: Option<String>,
    pub retention_policy: Option<String>,
    pub tier: String,
    pub is_wiki: bool,
    /// `None` when the endpoint is eligible (not protected); a static reason
    /// tag when protected. Protected rows can never be lifecycle sources.
    pub protected_reason: Option<String>,
}

/// Immutable human-review copy displayed at the proposal top level.
///
/// `status`, review notes, and proposal timestamps are deliberately absent:
/// they are mutable lifecycle bookkeeping. Every nonvolatile field a human
/// reviews before approving is instead hash-bound here and cross-checked
/// against its top-level copy by [`validate_lifecycle_proposal`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LifecycleReviewDisplay {
    pub kind: String,
    pub requires_human_approval: bool,
    pub path: String,
    pub rationale: String,
    pub evidence: serde_json::Value,
}

/// Typed immutable apply payload — the canonical shape hashed for identity.
/// Volatile state (status, review notes, timestamps) is excluded by
/// construction: it lives outside this struct in the stored proposal JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LifecycleApplyPayload {
    pub schema_version: u32,
    pub policy_version: String,
    pub lifecycle_action: String,
    pub source: LifecycleEndpointSnapshot,
    pub target: Option<LifecycleEndpointSnapshot>,
    pub review_display: LifecycleReviewDisplay,
}

/// Deterministic SHA-256 identity over the canonical serialization of the
/// apply payload. Returns lowercase hex.
///
/// `serde_json::to_vec` serializes named-field structs in definition order
/// with no map randomness, so the output is canonical without needing sorted
/// keys.
pub fn compute_lifecycle_identity(payload: &LifecycleApplyPayload) -> String {
    let bytes = serde_json::to_vec(payload).expect("lifecycle payload serializable");
    format!("{:x}", Sha256::digest(&bytes))
}

/// Deterministic, bounded storage key for a v2 lifecycle proposal. The action
/// is deliberately whitelisted so proposal keys remain a finite protocol
/// surface rather than accepting arbitrary caller-controlled prefixes.
pub fn lifecycle_proposal_id(payload: &LifecycleApplyPayload) -> Result<String, MemoryError> {
    let action = payload.lifecycle_action.as_str();
    if !matches!(
        action,
        ACTION_MERGE_INTO
            | ACTION_SUPERSEDE
            | ACTION_NEAR_DUP_MERGE
            | ACTION_ARCHIVE
            | ACTION_PROMOTE_DISTILLED
    ) {
        return Err(MemoryError::InvalidArg(format!(
            "unsupported lifecycle_action '{action}' for proposal id"
        )));
    }
    Ok(format!(
        "lifecycle:{action}:{}",
        compute_lifecycle_identity(payload)
    ))
}

/// SHA-256 hex digest of a text blob (lowercase).
pub fn lifecycle_text_digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// The ONE rendering every timestamp this module writes must use: RFC3339,
/// UTC, millisecond precision, `Z` suffix (`...123Z`). Byte-compatible with
/// `db::common::now_utc_iso`, which is what the rest of the DB layer stamps
/// into `updated_at`/`valid_until` — see `db::memory_crud::supersede_memory`
/// and `repair::memory_hygiene`'s `strftime('%Y-%m-%dT%H:%M:%fZ','now')`.
///
/// Not optional dress code. `valid_until` is compared **lexically** by the
/// as-of search predicates — `db/memory_crud/search.rs:147` (and :268/:434/
/// :524) read `m.valid_until > ?6` with no `datetime()` wrapper — so the
/// column only behaves like a timestamp as long as every writer renders it
/// identically, and `chrono`'s bare `to_rfc3339()` does not: it emits a
/// numeric offset and auto-precision sub-seconds (`...123456789+00:00`),
/// where a `Z`-rendered as-of clock puts `'Z'` (0x5A) against a fraction
/// digit. Every other route into this column already normalizes —
/// `db::memory_crud::upsert` runs `valid_until` through `normalize_utc_iso`
/// (Millis + `Z`), and the raw-UPDATE writers `supersede_memory` /
/// `repair::memory_hygiene` stamp `now_utc_iso()` /
/// `strftime('%Y-%m-%dT%H:%M:%fZ','now')` — so a bare `to_rfc3339()` here
/// would leave a rogue rendering sitting in the column until something
/// happened to upsert the row again.
///
/// `db::state`'s reaper already ate this exact bug on `expires_at` and fixed
/// it at the reader with `datetime(...)` (`db/state.rs:123-134`).
/// `valid_until` has no such wrapper, so here the fix has to be at the
/// writer.
///
/// Declared in this module rather than imported because `db::common` is a
/// private module and `db`'s `now_utc_iso` re-export is `#[cfg(test)]`-gated
/// (`db/mod.rs:163-164`), so `store::` cannot reach it. Same reason
/// `store::enrichment.rs:25` carries its own copy.
fn lifecycle_iso(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// [`lifecycle_iso`] of now.
fn lifecycle_now_iso() -> String {
    lifecycle_iso(chrono::Utc::now())
}

/// Single source of truth for lifecycle-source protection. The facade maps
/// these tags for scope accounting, while apply uses them directly.
fn protection_reason_from_fields(
    path: &str,
    archived: bool,
    tier: &str,
    is_wiki: bool,
    retention_policy: Option<&str>,
) -> Option<&'static str> {
    if archived {
        return Some("archived");
    }
    if tier.eq_ignore_ascii_case("pattern") {
        return Some("pattern_tier");
    }
    if is_wiki {
        return Some("wiki_category");
    }
    if path.starts_with("/wiki") {
        return Some("wiki_path");
    }
    let normalized = retention_policy.map(|r| r.to_ascii_lowercase());
    if matches!(
        normalized.as_deref(),
        Some("permanent" | "pinned" | "durable")
    ) {
        return Some("retention_policy");
    }
    None
}

/// True when `entry` is protected from being a lifecycle source.
pub fn lifecycle_protection_reason(entry: &MemoryEntry) -> Option<&'static str> {
    protection_reason_from_fields(
        &entry.path,
        entry.archived,
        &entry.tier,
        entry.is_wiki(),
        entry.retention_policy.as_deref(),
    )
}

fn entry_is_wiki(category: &str, domain: Option<&str>, metadata: &serde_json::Value) -> bool {
    category.eq_ignore_ascii_case("wiki")
        || domain == Some("wiki")
        || metadata
            .get("wiki")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
}

/// Build an endpoint snapshot from a live [`MemoryEntry`] (used at propose
/// time by the generators).
pub fn snapshot_endpoint(entry: &MemoryEntry) -> LifecycleEndpointSnapshot {
    LifecycleEndpointSnapshot {
        id: entry.id.clone(),
        revision: entry.revision,
        path: entry.path.clone(),
        text_digest: lifecycle_text_digest(&entry.text),
        keywords: canonical_tags(&entry.keywords),
        entities: canonical_tags(&entry.entities),
        importance_bits: entry.importance.to_bits(),
        eligibility_summary_digest: None,
        eligibility_timestamp: None,
        eligibility_access_count: None,
        eligibility_recall_count: None,
        eligibility_query_diversity: None,
        archived: entry.archived,
        // Proposal generators only consider active rows (supersession edges
        // are hidden from their input queries), so `None` is the state a
        // reviewer approved. Apply re-reads the actual column in the locked
        // transaction and refuses if a no-revision writer set it meanwhile.
        superseded_by: None,
        valid_until: entry.valid_until.clone(),
        retention_policy: entry.retention_policy.clone(),
        tier: entry.tier.clone(),
        is_wiki: entry.is_wiki(),
        protected_reason: lifecycle_protection_reason(entry).map(str::to_string),
    }
}

fn bind_action_eligibility(
    snapshot: &mut LifecycleEndpointSnapshot,
    action: &str,
    summary: &str,
    timestamp: &str,
    access_count: i64,
    recall_count: i64,
    query_diversity: i64,
) {
    match action {
        ACTION_MERGE_INTO | ACTION_SUPERSEDE => {
            snapshot.eligibility_summary_digest = Some(lifecycle_text_digest(summary));
            snapshot.eligibility_timestamp = Some(timestamp.to_string());
        }
        ACTION_NEAR_DUP_MERGE => {
            snapshot.eligibility_timestamp = Some(timestamp.to_string());
        }
        ACTION_ARCHIVE => {
            snapshot.eligibility_timestamp = Some(timestamp.to_string());
            snapshot.eligibility_access_count = Some(access_count);
            snapshot.eligibility_recall_count = Some(recall_count);
        }
        ACTION_PROMOTE_DISTILLED => {
            snapshot.eligibility_recall_count = Some(recall_count);
            snapshot.eligibility_query_diversity = Some(query_diversity);
        }
        _ => {}
    }
}

fn snapshot_endpoint_for_action(action: &str, entry: &MemoryEntry) -> LifecycleEndpointSnapshot {
    let mut snapshot = snapshot_endpoint(entry);
    bind_action_eligibility(
        &mut snapshot,
        action,
        &entry.summary,
        &entry.timestamp,
        entry.access_count,
        entry.recall_count,
        entry.query_diversity,
    );
    snapshot
}

fn validate_action_target_cardinality(
    proposal_id: &str,
    action: &str,
    target: Option<&LifecycleEndpointSnapshot>,
) -> Result<(), MemoryError> {
    match action {
        ACTION_MERGE_INTO | ACTION_SUPERSEDE | ACTION_NEAR_DUP_MERGE => {
            if target.is_none() {
                return Err(MemoryError::InvalidArg(format!(
                    "v2 lifecycle proposal {proposal_id} action '{action}' requires exactly one target"
                )));
            }
        }
        ACTION_ARCHIVE | ACTION_PROMOTE_DISTILLED => {
            if target.is_some() {
                return Err(MemoryError::InvalidArg(format!(
                    "v2 lifecycle proposal {proposal_id} action '{action}' requires no target"
                )));
            }
        }
        other => {
            return Err(MemoryError::InvalidArg(format!(
                "v2 lifecycle proposal {proposal_id} has unsupported lifecycle_action '{other}'"
            )));
        }
    }
    Ok(())
}

fn validate_action_eligibility_shape(
    proposal_id: &str,
    action: &str,
    endpoint_label: &str,
    endpoint: &LifecycleEndpointSnapshot,
) -> Result<(), MemoryError> {
    let expected = match action {
        ACTION_MERGE_INTO | ACTION_SUPERSEDE => (true, true, false, false, false),
        ACTION_NEAR_DUP_MERGE => (false, true, false, false, false),
        ACTION_ARCHIVE => (false, true, true, true, false),
        ACTION_PROMOTE_DISTILLED => (false, false, false, true, true),
        other => {
            return Err(MemoryError::InvalidArg(format!(
                "v2 lifecycle proposal {proposal_id} has unsupported lifecycle_action '{other}'"
            )));
        }
    };
    let actual = (
        endpoint.eligibility_summary_digest.is_some(),
        endpoint.eligibility_timestamp.is_some(),
        endpoint.eligibility_access_count.is_some(),
        endpoint.eligibility_recall_count.is_some(),
        endpoint.eligibility_query_diversity.is_some(),
    );
    if actual != expected {
        return Err(MemoryError::InvalidArg(format!(
            "v2 lifecycle proposal {proposal_id} {endpoint_label} eligibility snapshot does not match action '{action}'"
        )));
    }
    Ok(())
}

/// Build a v2 apply payload from the source/target entries and the action
/// name. Used by the proposal generators.
pub fn build_apply_payload(
    action: &str,
    source: &MemoryEntry,
    target: Option<&MemoryEntry>,
) -> LifecycleApplyPayload {
    build_apply_payload_with_review_display(
        action,
        source,
        target,
        LifecycleReviewDisplay {
            kind: "memory_lifecycle".to_string(),
            requires_human_approval: true,
            path: source.path.clone(),
            rationale: "direct lifecycle payload".to_string(),
            evidence: serde_json::json!({}),
        },
    )
}

/// Build a v2 apply payload with the exact nonvolatile copy shown to the
/// human reviewer. Proposal generators must use this form so the display is
/// bound to the same identity as the execution inputs.
pub fn build_apply_payload_with_review_display(
    action: &str,
    source: &MemoryEntry,
    target: Option<&MemoryEntry>,
    review_display: LifecycleReviewDisplay,
) -> LifecycleApplyPayload {
    LifecycleApplyPayload {
        schema_version: LIFECYCLE_SCHEMA_VERSION,
        policy_version: LIFECYCLE_POLICY_VERSION.to_string(),
        lifecycle_action: action.to_string(),
        source: snapshot_endpoint_for_action(action, source),
        target: target.map(|entry| snapshot_endpoint_for_action(action, entry)),
        review_display,
    }
}

/// True when a stored proposal JSON is a legacy v1 proposal (missing
/// `schema_version=2`). Legacy proposals remain listable but are loudly
/// refused at review/apply.
pub fn is_legacy_v1_proposal(value: &serde_json::Value) -> bool {
    value.get("schema_version").and_then(|v| v.as_u64()) != Some(LIFECYCLE_SCHEMA_VERSION as u64)
}

/// Validate immutable execution and human-review fields duplicated at the
/// proposal top level against its typed apply payload, then verify that the
/// stored identity hashes that payload. Review and apply both call this guard
/// so neither execution nor display fields can be tampered after proposal.
pub fn validate_lifecycle_proposal(
    proposal_id: &str,
    proposal: &serde_json::Value,
) -> Result<LifecycleApplyPayload, MemoryError> {
    let payload_value = proposal.get("apply_payload").ok_or_else(|| {
        MemoryError::InvalidArg(format!(
            "v2 lifecycle proposal {proposal_id} missing apply_payload"
        ))
    })?;
    let payload: LifecycleApplyPayload =
        serde_json::from_value(payload_value.clone()).map_err(|e| {
            MemoryError::InvalidArg(format!(
                "lifecycle proposal {proposal_id} malformed apply_payload: {e}"
            ))
        })?;
    if payload.schema_version != LIFECYCLE_SCHEMA_VERSION
        || payload.policy_version != LIFECYCLE_POLICY_VERSION
    {
        return Err(MemoryError::InvalidArg(format!(
            "v2 lifecycle proposal {proposal_id} uses unsupported schema/policy version"
        )));
    }
    validate_action_target_cardinality(
        proposal_id,
        &payload.lifecycle_action,
        payload.target.as_ref(),
    )?;
    validate_action_eligibility_shape(
        proposal_id,
        &payload.lifecycle_action,
        "source",
        &payload.source,
    )?;
    if let Some(target) = payload.target.as_ref() {
        validate_action_eligibility_shape(
            proposal_id,
            &payload.lifecycle_action,
            "target",
            target,
        )?;
    }

    let expected_target_id =
        serde_json::to_value(payload.target.as_ref().map(|target| target.id.clone()))
            .expect("lifecycle target id serializable");
    for (field, expected) in [
        (
            "kind",
            serde_json::json!(payload.review_display.kind.clone()),
        ),
        ("schema_version", serde_json::json!(payload.schema_version)),
        (
            "policy_version",
            serde_json::json!(payload.policy_version.clone()),
        ),
        (
            "lifecycle_action",
            serde_json::json!(payload.lifecycle_action.clone()),
        ),
        ("source_id", serde_json::json!(payload.source.id.clone())),
        ("target_id", expected_target_id),
        (
            "requires_human_approval",
            serde_json::json!(payload.review_display.requires_human_approval),
        ),
        (
            "path",
            serde_json::json!(payload.review_display.path.clone()),
        ),
        (
            "rationale",
            serde_json::json!(payload.review_display.rationale.clone()),
        ),
        ("evidence", payload.review_display.evidence.clone()),
    ] {
        if proposal.get(field) != Some(&expected) {
            return Err(MemoryError::InvalidArg(format!(
                "v2 lifecycle proposal {proposal_id} top-level {field} differs from apply_payload"
            )));
        }
    }

    let stored_identity = proposal
        .get("identity")
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            MemoryError::InvalidArg(format!(
                "v2 lifecycle proposal {proposal_id} missing identity"
            ))
        })?;
    let payload_identity = compute_lifecycle_identity(&payload);
    if stored_identity != payload_identity {
        return Err(MemoryError::InvalidArg(format!(
            "v2 lifecycle proposal {proposal_id} identity differs from apply_payload hash"
        )));
    }
    let expected_proposal_id = lifecycle_proposal_id(&payload)?;
    if proposal.get("proposal_id").and_then(|value| value.as_str()) != Some(proposal_id)
        || proposal_id != expected_proposal_id
    {
        return Err(MemoryError::InvalidArg(format!(
            "v2 lifecycle proposal {proposal_id} key differs from immutable payload identity"
        )));
    }

    Ok(payload)
}

// ─── Review ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleReviewDecision {
    Approved,
    Rejected,
}

impl LifecycleReviewDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "approved" | "approve" => Ok(Self::Approved),
            "rejected" | "reject" => Ok(Self::Rejected),
            other => Err(MemoryError::InvalidArg(format!(
                "Invalid review_status '{other}'. Expected approved|rejected"
            ))),
        }
    }
}

/// Review a v2 lifecycle proposal: `pending → approved | rejected` via a
/// `hard_state` version CAS. Terminal/already-reviewed states are immutable.
/// Legacy v1 proposals are refused loudly.
pub fn review_lifecycle_proposal(
    store: &MemoryStore,
    proposal_id: &str,
    decision: LifecycleReviewDecision,
    note: Option<&str>,
) -> Result<serde_json::Value, MemoryError> {
    let (raw, version) = store
        .get_state_kv(LIFECYCLE_PROPOSAL_NS, proposal_id)?
        .ok_or_else(|| {
            MemoryError::InvalidArg(format!("lifecycle proposal not found: {proposal_id}"))
        })?;
    let mut value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        MemoryError::InvalidArg(format!("parse lifecycle proposal {proposal_id}: {e}"))
    })?;

    if is_legacy_v1_proposal(&value) {
        return Err(MemoryError::InvalidArg(format!(
            "legacy v1 lifecycle proposal {proposal_id} cannot be reviewed; re-propose to upgrade to schema_version={LIFECYCLE_SCHEMA_VERSION}"
        )));
    }
    validate_lifecycle_proposal(proposal_id, &value)?;

    let status = value
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("pending");
    if status != "pending" {
        return Err(MemoryError::InvalidArg(format!(
            "lifecycle proposal {proposal_id} is terminal/already-reviewed (status={status}); only pending proposals can be reviewed"
        )));
    }

    let now = lifecycle_now_iso();
    let decision_str = decision.as_str();
    value["status"] = serde_json::json!(decision_str);
    value["review"] = serde_json::json!({
        "status": decision_str,
        "note": note,
        "reviewed_at": now,
    });
    // `rejected` is terminal (will never be applied) → 30-day TTL now.
    // `approved` is NOT terminal (awaits apply) → stays TTL-less; the
    // terminal TTL is stamped by `apply_lifecycle_proposal` instead.
    if matches!(decision, LifecycleReviewDecision::Rejected) {
        let expires = chrono::Utc::now() + chrono::Duration::days(LIFECYCLE_TERMINAL_TTL_DAYS);
        value["expires_at"] = serde_json::json!(lifecycle_iso(expires));
    }

    let updated = serde_json::to_string(&value)
        .map_err(|e| MemoryError::Internal(format!("serialize lifecycle review: {e}")))?;
    let swapped =
        store.set_state_if_version(LIFECYCLE_PROPOSAL_NS, proposal_id, &updated, version)?;
    if !swapped {
        return Err(MemoryError::InvalidArg(format!(
            "lifecycle proposal {proposal_id} review CAS failed (concurrent modification); reload and retry"
        )));
    }
    Ok(value)
}

// ─── Apply ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LifecycleApplyResult {
    pub proposal: serde_json::Value,
    pub apply_result: serde_json::Value,
}

/// Read a full [`MemoryEntry`] by id inside a transaction, with no
/// archived/superseded filter (revalidation must see drifted rows).
fn read_memory_in_tx(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
) -> Result<Option<MemoryEntry>, MemoryError> {
    use crate::db::{row_to_entry, MEMORY_SELECT_COLUMNS};
    let sql = format!("SELECT {MEMORY_SELECT_COLUMNS} FROM memories WHERE id = ?1");
    let mut stmt = tx.prepare(&sql)?;
    let entry = stmt.query_row(params![id], row_to_entry).optional()?;
    Ok(entry)
}

/// Read just the endpoint snapshot fields inside a transaction (no full
/// MemoryEntry materialization — the revalidation hot path).
fn read_endpoint_snapshot(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    action: &str,
) -> Result<Option<LifecycleEndpointSnapshot>, MemoryError> {
    let snapshot = tx
        .query_row(
            "SELECT id, path, summary, text, timestamp, archived, revision, retention_policy, tier,
                    category, domain, metadata, keywords, entities, importance, superseded_by,
                    valid_until, access_count, recall_count, query_diversity
             FROM memories WHERE id = ?1",
            params![id],
            |r| {
                let id: String = r.get(0)?;
                let path: String = r.get(1)?;
                let summary: String = r.get(2)?;
                let text: String = r.get(3)?;
                let timestamp: String = r.get(4)?;
                let archived: bool = r.get(5)?;
                let revision: i64 = r.get(6)?;
                let retention_policy: Option<String> = r.get(7)?;
                let tier: String = r.get(8)?;
                let category: String = r.get(9)?;
                let domain: Option<String> = r.get(10)?;
                let metadata_str: String = r.get(11)?;
                let keywords_json: String = r.get(12)?;
                let entities_json: String = r.get(13)?;
                let importance: f64 = r.get(14)?;
                let superseded_by: Option<String> = r.get(15)?;
                let valid_until: Option<String> = r.get(16)?;
                let access_count: i64 = r.get(17)?;
                let recall_count: i64 = r.get(18)?;
                let query_diversity: i64 = r.get(19)?;

                let metadata: serde_json::Value =
                    serde_json::from_str(&metadata_str).unwrap_or(serde_json::json!({}));
                let is_wiki = entry_is_wiki(&category, domain.as_deref(), &metadata);
                let protected_reason = protection_reason_from_fields(
                    &path,
                    archived,
                    &tier,
                    is_wiki,
                    retention_policy.as_deref(),
                )
                .map(str::to_string);
                // Same lenient parse as `db::row_to_entry`'s
                // `json_string_array_column`: a malformed column degrades to an
                // empty list on BOTH capture sites, so propose and apply still
                // agree instead of the drift check firing on a parse quirk.
                let keywords: Vec<String> =
                    serde_json::from_str(&keywords_json).unwrap_or_default();
                let entities: Vec<String> =
                    serde_json::from_str(&entities_json).unwrap_or_default();

                let mut snapshot = LifecycleEndpointSnapshot {
                    id,
                    revision,
                    path,
                    text_digest: lifecycle_text_digest(&text),
                    keywords: canonical_tags(&keywords),
                    entities: canonical_tags(&entities),
                    importance_bits: importance.to_bits(),
                    eligibility_summary_digest: None,
                    eligibility_timestamp: None,
                    eligibility_access_count: None,
                    eligibility_recall_count: None,
                    eligibility_query_diversity: None,
                    archived,
                    superseded_by,
                    valid_until,
                    retention_policy,
                    tier,
                    is_wiki,
                    protected_reason,
                };
                bind_action_eligibility(
                    &mut snapshot,
                    action,
                    &summary,
                    &timestamp,
                    access_count,
                    recall_count,
                    query_diversity,
                );
                Ok(snapshot)
            },
        )
        .optional()?;
    Ok(snapshot)
}

/// Build an error message prefix for drift failures.
fn drift_err(proposal_id: &str, detail: impl std::fmt::Display) -> MemoryError {
    MemoryError::InvalidArg(format!(
        "lifecycle proposal {proposal_id} revalidation failed: {detail}"
    ))
}

/// Apply an approved v2 lifecycle proposal inside ONE `BEGIN IMMEDIATE`
/// transaction: revalidate hash/revision/path/protection from the live rows,
/// mutate memory, stamp the proposal `applied`, commit. Recall-cache
/// invalidation is the caller's responsibility (must run AFTER this returns,
/// i.e. after commit).
///
/// Legacy v1 proposals are refused loudly.
pub fn apply_lifecycle_proposal(
    store: &mut MemoryStore,
    proposal_id: &str,
) -> Result<LifecycleApplyResult, MemoryError> {
    let db_label = store.db_label.clone();
    db::retry_memory_locked("lifecycle_apply", &db_label, || {
        apply_lifecycle_proposal_once(store, proposal_id)
    })
}

fn apply_lifecycle_proposal_once(
    store: &mut MemoryStore,
    proposal_id: &str,
) -> Result<LifecycleApplyResult, MemoryError> {
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;

    // ── Load + gate proposal INSIDE the locked tx ─────────────────────────
    let (raw, proposal_version) = tx
        .query_row(
            "SELECT value_json, version FROM hard_state WHERE namespace = ?1 AND key = ?2",
            params![LIFECYCLE_PROPOSAL_NS, proposal_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)),
        )
        .optional()?
        .ok_or_else(|| {
            MemoryError::InvalidArg(format!("lifecycle proposal not found: {proposal_id}"))
        })?;

    let mut proposal: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        MemoryError::InvalidArg(format!("parse lifecycle proposal {proposal_id}: {e}"))
    })?;

    if is_legacy_v1_proposal(&proposal) {
        return Err(MemoryError::InvalidArg(format!(
            "legacy v1 lifecycle proposal {proposal_id} cannot be applied; re-propose to upgrade to schema_version={LIFECYCLE_SCHEMA_VERSION}"
        )));
    }

    let status = proposal
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("pending");
    if status != "approved" {
        return Err(MemoryError::InvalidArg(format!(
            "lifecycle proposal {proposal_id} must be approved before apply; current status={status}"
        )));
    }

    let stored_payload = validate_lifecycle_proposal(proposal_id, &proposal)?;
    let stored_identity = compute_lifecycle_identity(&stored_payload);

    let action = stored_payload.lifecycle_action.as_str();
    let source_id = stored_payload.source.id.clone();
    let target_id = stored_payload.target.as_ref().map(|t| t.id.clone());

    // ── Revalidate: rebuild payload from LIVE rows, recompute identity ────
    let live_source = read_endpoint_snapshot(&tx, &source_id, action)?
        .ok_or_else(|| drift_err(proposal_id, format!("source row missing: {source_id}")))?;
    if live_source.protected_reason.is_some() {
        return Err(drift_err(
            proposal_id,
            format!(
                "source {source_id} is now protected ({})",
                live_source.protected_reason.as_deref().unwrap_or("?")
            ),
        ));
    }
    let live_target = if let Some(ref tid) = target_id {
        Some(
            read_endpoint_snapshot(&tx, tid, action)?
                .ok_or_else(|| drift_err(proposal_id, format!("target row missing: {tid}")))?,
        )
    } else {
        None
    };

    let live_payload = LifecycleApplyPayload {
        schema_version: LIFECYCLE_SCHEMA_VERSION,
        policy_version: LIFECYCLE_POLICY_VERSION.to_string(),
        lifecycle_action: action.to_string(),
        source: live_source,
        target: live_target,
        // The display copy was validated against the stored top-level fields
        // above. It is immutable proposal content, not a live memory row, so
        // preserve it while rebuilding the endpoint portion of the identity.
        review_display: stored_payload.review_display.clone(),
    };
    let live_identity = compute_lifecycle_identity(&live_payload);
    if live_identity != stored_identity {
        return Err(drift_err(
            proposal_id,
            format!(
                "identity mismatch (stored={stored_identity:.12}… live={live_identity:.12}…); source/target drifted since approve"
            ),
        ));
    }

    let source_revision = stored_payload.source.revision;

    // ── Mutate memory INSIDE the same tx ──────────────────────────────────
    let apply_result = match action {
        ACTION_ARCHIVE => {
            let now = lifecycle_now_iso();
            let changed = tx.execute(
                "UPDATE memories SET archived = 1, updated_at = ?1, revision = revision + 1
                 WHERE id = ?2 AND revision = ?3 AND archived = 0",
                params![now, source_id, source_revision],
            )?;
            if changed == 0 {
                return Err(drift_err(
                    proposal_id,
                    format!("archive CAS failed for {source_id} (revision {source_revision})"),
                ));
            }
            serde_json::json!({
                "lifecycle_action": ACTION_ARCHIVE,
                "source_id": source_id,
                "archived": true,
            })
        }
        ACTION_SUPERSEDE => {
            let target = target_id.as_deref().ok_or_else(|| {
                MemoryError::InvalidArg(format!(
                    "supersede proposal {proposal_id} requires target_id"
                ))
            })?;
            let now = lifecycle_now_iso();
            let changed = tx.execute(
                "UPDATE memories SET archived = 1, superseded_by = ?1,
                 valid_until = COALESCE(valid_until, ?2), updated_at = ?2, revision = revision + 1
                 WHERE id = ?3 AND revision = ?4 AND archived = 0
                 AND (superseded_by IS NULL OR superseded_by != ?1)",
                params![target, now, source_id, source_revision],
            )?;
            if changed == 0 {
                return Err(drift_err(
                    proposal_id,
                    format!("supersede CAS failed for {source_id} (revision {source_revision})"),
                ));
            }
            serde_json::json!({
                "lifecycle_action": ACTION_SUPERSEDE,
                "source_id": source_id,
                "target_id": target,
                "superseded": true,
                "archived": true,
            })
        }
        ACTION_MERGE_INTO | ACTION_NEAR_DUP_MERGE => {
            let target = target_id.as_deref().ok_or_else(|| {
                MemoryError::InvalidArg(format!(
                    "{action} proposal {proposal_id} requires target_id"
                ))
            })?;
            let source = read_memory_in_tx(&tx, &source_id)?
                .ok_or_else(|| drift_err(proposal_id, format!("source missing: {source_id}")))?;
            let mut survivor = read_memory_in_tx(&tx, target)?
                .ok_or_else(|| drift_err(proposal_id, format!("target missing: {target}")))?;
            // BOTH sides of the no-op comparison below must be canonical. The
            // fold produces sorted+deduplicated lists, while the stored column
            // is in whatever order the last writer serialized
            // (`db::memory_crud` writes `serde_json::to_string(&entry.keywords)`
            // verbatim), so comparing the fold against the raw storage order
            // reports "changed" for every target whose array is not already
            // sorted and unique — which is most of them. That turns the guard
            // into a no-op and re-introduces exactly the sibling-proposal
            // invalidation it exists to prevent.
            let target_keywords = canonical_tags(&survivor.keywords);
            let target_entities = canonical_tags(&survivor.entities);
            let target_importance = survivor.importance;
            // Fold unique keywords/entities; keep survivor text canonical.
            let mut merged_kw = survivor.keywords.clone();
            merged_kw.extend(source.keywords.iter().cloned());
            survivor.keywords = canonical_tags(&merged_kw);
            let mut merged_ents = survivor.entities.clone();
            merged_ents.extend(source.entities.iter().cloned());
            survivor.entities = canonical_tags(&merged_ents);
            if survivor.importance < source.importance {
                survivor.importance = source.importance;
            }
            let merged_keywords = survivor.keywords.len();
            let merged_entities = survivor.entities.len();
            // Do not rewrite an unchanged survivor: an unconditional upsert
            // bumps its revision, invalidating already-approved sibling star
            // proposals that share this target snapshot. When merged data did
            // change, preserve the full transaction-aware upsert path.
            if survivor.keywords != target_keywords
                || survivor.entities != target_entities
                || survivor.importance != target_importance
            {
                db::upsert_within_tx(&tx, &survivor, store.vec_available, None)?;
            }
            // Combined supersede + archive on the source with revision CAS.
            let now = lifecycle_now_iso();
            let changed = tx.execute(
                "UPDATE memories SET archived = 1, superseded_by = ?1,
                 valid_until = COALESCE(valid_until, ?2), updated_at = ?2, revision = revision + 1
                 WHERE id = ?3 AND revision = ?4 AND archived = 0
                 AND (superseded_by IS NULL OR superseded_by != ?1)",
                params![target, now, source_id, source_revision],
            )?;
            if changed == 0 {
                return Err(drift_err(
                    proposal_id,
                    format!(
                        "{action} supersede/archive CAS failed for {source_id} (revision {source_revision})"
                    ),
                ));
            }
            serde_json::json!({
                "lifecycle_action": action,
                "source_id": source_id,
                "target_id": target,
                "merged_keywords": merged_keywords,
                "merged_entities": merged_entities,
                "superseded": true,
                "archived": true,
            })
        }
        ACTION_PROMOTE_DISTILLED => {
            let mut entry = read_memory_in_tx(&tx, &source_id)?
                .ok_or_else(|| drift_err(proposal_id, format!("source missing: {source_id}")))?;
            let tier_before = entry.tier.clone();
            entry.tier = "consolidated".to_string();
            db::upsert_within_tx(&tx, &entry, store.vec_available, None)?;
            serde_json::json!({
                "lifecycle_action": ACTION_PROMOTE_DISTILLED,
                "source_id": source_id,
                "tier_before": tier_before,
                "tier_after": "consolidated",
                "changed": true,
            })
        }
        other => {
            return Err(MemoryError::InvalidArg(format!(
                "unsupported lifecycle_action '{other}' in proposal {proposal_id}"
            )));
        }
    };

    // ── Stamp proposal applied (CAS inside the same tx) ───────────────────
    let now = lifecycle_now_iso();
    let expires =
        lifecycle_iso(chrono::Utc::now() + chrono::Duration::days(LIFECYCLE_TERMINAL_TTL_DAYS));
    proposal["status"] = serde_json::json!("applied");
    proposal["applied_at"] = serde_json::json!(now);
    proposal["expires_at"] = serde_json::json!(expires);
    proposal["apply_result"] = apply_result.clone();
    let updated_json = serde_json::to_string(&proposal)
        .map_err(|e| MemoryError::Internal(format!("serialize applied lifecycle proposal: {e}")))?;
    let changed = tx.execute(
        "UPDATE hard_state SET value_json = ?1, version = version + 1, updated_at = ?2
         WHERE namespace = ?3 AND key = ?4 AND version = ?5",
        params![
            updated_json,
            now,
            LIFECYCLE_PROPOSAL_NS,
            proposal_id,
            proposal_version
        ],
    )?;
    if changed == 0 {
        return Err(MemoryError::InvalidArg(format!(
            "lifecycle proposal {proposal_id} apply CAS failed (concurrent modification)"
        )));
    }

    tx.commit()?;

    Ok(LifecycleApplyResult {
        proposal,
        apply_result,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal_from_payload(payload: &LifecycleApplyPayload) -> (String, serde_json::Value) {
        let proposal_id = lifecycle_proposal_id(payload).expect("known action");
        let proposal = serde_json::json!({
            "proposal_id": proposal_id.clone(),
            "kind": payload.review_display.kind.clone(),
            "schema_version": payload.schema_version,
            "policy_version": payload.policy_version.clone(),
            "lifecycle_action": payload.lifecycle_action.clone(),
            "status": "pending",
            "source_id": payload.source.id.clone(),
            "target_id": payload
                .target
                .as_ref()
                .map(|target| serde_json::json!(target.id.clone()))
                .unwrap_or(serde_json::Value::Null),
            "requires_human_approval": payload.review_display.requires_human_approval,
            "path": payload.review_display.path.clone(),
            "rationale": payload.review_display.rationale.clone(),
            "evidence": payload.review_display.evidence.clone(),
            "identity": compute_lifecycle_identity(payload),
            "apply_payload": payload,
        });
        (proposal_id, proposal)
    }

    #[test]
    fn identity_is_deterministic_and_stable_under_rebuild() {
        let entry = MemoryEntry {
            id: "src-1".into(),
            path: "/scratch/x".into(),
            text: "hello world".into(),
            revision: 3,
            archived: false,
            retention_policy: None,
            tier: "raw".into(),
            ..test_entry()
        };
        let target = MemoryEntry {
            id: "tgt-1".into(),
            path: "/scratch/x".into(),
            text: "hello world".into(),
            revision: 5,
            ..test_entry()
        };
        let review_display = LifecycleReviewDisplay {
            kind: "memory_lifecycle".to_string(),
            requires_human_approval: true,
            path: entry.path.clone(),
            rationale: "test rationale".to_string(),
            evidence: serde_json::json!({"test": true}),
        };
        let payload = build_apply_payload_with_review_display(
            ACTION_MERGE_INTO,
            &entry,
            Some(&target),
            review_display.clone(),
        );
        let id_a = compute_lifecycle_identity(&payload);
        let id_b = compute_lifecycle_identity(&payload);
        assert_eq!(id_a, id_b, "identity must be deterministic");
        assert_eq!(id_a.len(), 64, "SHA-256 hex is 64 chars");
        // Rebuilding from an equivalent snapshot reproduces the same identity.
        let rebuilt = LifecycleApplyPayload {
            schema_version: LIFECYCLE_SCHEMA_VERSION,
            policy_version: LIFECYCLE_POLICY_VERSION.to_string(),
            lifecycle_action: ACTION_MERGE_INTO.to_string(),
            source: snapshot_endpoint_for_action(ACTION_MERGE_INTO, &entry),
            target: Some(snapshot_endpoint_for_action(ACTION_MERGE_INTO, &target)),
            review_display,
        };
        assert_eq!(
            compute_lifecycle_identity(&rebuilt),
            id_a,
            "identity must be stable when rebuilt from matching fields"
        );
    }

    /// Each of `keywords`/`entities`/`importance` is an execution input for
    /// the merge actions, so each must move the identity on its own. Delete
    /// any one of the three from `LifecycleEndpointSnapshot` and the matching
    /// assertion below fails.
    #[test]
    fn identity_binds_every_merge_execution_input() {
        let base = MemoryEntry {
            id: "exec-input-src".into(),
            path: "/scratch/exec".into(),
            text: "body".into(),
            keywords: vec!["alpha".into()],
            entities: vec!["ent-a".into()],
            importance: 0.5,
            ..test_entry()
        };
        let target = MemoryEntry {
            id: "exec-input-tgt".into(),
            path: "/scratch/exec".into(),
            text: "other body".into(),
            ..test_entry()
        };
        let identity_of = |entry: &MemoryEntry| {
            compute_lifecycle_identity(&build_apply_payload(
                ACTION_MERGE_INTO,
                entry,
                Some(&target),
            ))
        };
        let baseline = identity_of(&base);

        let mut keywords_drifted = base.clone();
        keywords_drifted.keywords = vec!["alpha".into(), "injected".into()];
        assert_ne!(
            baseline,
            identity_of(&keywords_drifted),
            "keyword drift is folded into the survivor at apply, so it must change the identity"
        );

        let mut entities_drifted = base.clone();
        entities_drifted.entities = vec!["ent-a".into(), "ent-injected".into()];
        assert_ne!(
            baseline,
            identity_of(&entities_drifted),
            "entity drift is folded into the survivor at apply, so it must change the identity"
        );

        let mut importance_drifted = base.clone();
        importance_drifted.importance = 0.9;
        assert_ne!(
            baseline,
            identity_of(&importance_drifted),
            "importance drift wins the MAX at apply, so it must change the identity"
        );
    }

    #[test]
    fn identity_binds_only_each_actions_eligibility_inputs() {
        let base = MemoryEntry {
            id: "eligibility-source".into(),
            path: "/scratch/eligibility".into(),
            summary: "same-path summary".into(),
            timestamp: "2025-01-01T00:00:00Z".into(),
            access_count: 0,
            recall_count: 0,
            query_diversity: 0,
            ..test_entry()
        };
        let identity_of = |action: &str, entry: &MemoryEntry| {
            compute_lifecycle_identity(&build_apply_payload(action, entry, None))
        };

        let archive = identity_of(ACTION_ARCHIVE, &base);
        for (field, drifted) in [
            (
                "access_count",
                MemoryEntry {
                    access_count: 1,
                    ..base.clone()
                },
            ),
            (
                "recall_count",
                MemoryEntry {
                    recall_count: 1,
                    ..base.clone()
                },
            ),
            (
                "timestamp",
                MemoryEntry {
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    ..base.clone()
                },
            ),
        ] {
            assert_ne!(
                archive,
                identity_of(ACTION_ARCHIVE, &drifted),
                "archive eligibility field {field} must move the identity"
            );
        }
        assert_eq!(
            archive,
            identity_of(
                ACTION_ARCHIVE,
                &MemoryEntry {
                    query_diversity: 9,
                    ..base.clone()
                }
            ),
            "archive eligibility does not consume query_diversity"
        );

        let promote = identity_of(ACTION_PROMOTE_DISTILLED, &base);
        for (field, drifted) in [
            (
                "recall_count",
                MemoryEntry {
                    recall_count: 1,
                    ..base.clone()
                },
            ),
            (
                "query_diversity",
                MemoryEntry {
                    query_diversity: 1,
                    ..base.clone()
                },
            ),
        ] {
            assert_ne!(
                promote,
                identity_of(ACTION_PROMOTE_DISTILLED, &drifted),
                "promotion eligibility field {field} must move the identity"
            );
        }
        assert_eq!(
            promote,
            identity_of(
                ACTION_PROMOTE_DISTILLED,
                &MemoryEntry {
                    access_count: 9,
                    ..base.clone()
                }
            ),
            "promotion eligibility does not consume access_count"
        );

        let supersede = identity_of(ACTION_SUPERSEDE, &base);
        assert_ne!(
            supersede,
            identity_of(
                ACTION_SUPERSEDE,
                &MemoryEntry {
                    summary: "changed same-path summary".into(),
                    ..base.clone()
                }
            ),
            "same-path action selection consumes summary"
        );
        assert_ne!(
            supersede,
            identity_of(
                ACTION_SUPERSEDE,
                &MemoryEntry {
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    ..base.clone()
                }
            ),
            "same-path survivor selection consumes timestamp"
        );
        let near_dup = identity_of(ACTION_NEAR_DUP_MERGE, &base);
        assert_ne!(
            near_dup,
            identity_of(
                ACTION_NEAR_DUP_MERGE,
                &MemoryEntry {
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    ..base.clone()
                }
            ),
            "near-duplicate survivor selection consumes timestamp"
        );
        assert_eq!(
            supersede,
            identity_of(
                ACTION_SUPERSEDE,
                &MemoryEntry {
                    access_count: 9,
                    recall_count: 9,
                    query_diversity: 9,
                    ..base
                }
            ),
            "same-path supersession must ignore unrelated recall bookkeeping"
        );
    }

    /// The identity must be a function of the tag SET, not of the column's
    /// storage order — otherwise every proposal against a row whose array
    /// happens to be unsorted would refuse at apply as false drift. Remove the
    /// `canonical_tags` call from either capture site and this goes red.
    #[test]
    fn identity_ignores_tag_storage_order_and_duplicates() {
        let ordered = MemoryEntry {
            id: "tag-order".into(),
            path: "/scratch/tags".into(),
            keywords: vec!["alpha".into(), "beta".into(), "gamma".into()],
            entities: vec!["x".into(), "y".into()],
            ..test_entry()
        };
        let shuffled = MemoryEntry {
            keywords: vec![
                "gamma".into(),
                "alpha".into(),
                "beta".into(),
                "alpha".into(),
            ],
            entities: vec!["y".into(), "x".into(), "y".into()],
            ..ordered.clone()
        };
        assert_eq!(
            compute_lifecycle_identity(&build_apply_payload(ACTION_ARCHIVE, &ordered, None)),
            compute_lifecycle_identity(&build_apply_payload(ACTION_ARCHIVE, &shuffled, None)),
            "reordering/duplicating the same tag set must not read as drift"
        );
        // ...but a genuinely different set still must.
        let extra = MemoryEntry {
            keywords: vec![
                "alpha".into(),
                "beta".into(),
                "gamma".into(),
                "delta".into(),
            ],
            ..ordered.clone()
        };
        assert_ne!(
            compute_lifecycle_identity(&build_apply_payload(ACTION_ARCHIVE, &ordered, None)),
            compute_lifecycle_identity(&build_apply_payload(ACTION_ARCHIVE, &extra, None)),
            "a different tag set must still change the identity"
        );
    }

    #[test]
    fn identity_changes_when_revision_drifts() {
        let mut entry = MemoryEntry {
            id: "src-2".into(),
            path: "/scratch/y".into(),
            text: "body".into(),
            revision: 1,
            ..test_entry()
        };
        let payload_before = build_apply_payload(ACTION_ARCHIVE, &entry, None);
        let id_before = compute_lifecycle_identity(&payload_before);
        entry.revision = 2;
        let payload_after = build_apply_payload(ACTION_ARCHIVE, &entry, None);
        let id_after = compute_lifecycle_identity(&payload_after);
        assert_ne!(
            id_before, id_after,
            "revision drift must change the identity"
        );
        entry.text.push_str(" changed");
        let content_identity =
            compute_lifecycle_identity(&build_apply_payload(ACTION_ARCHIVE, &entry, None));
        assert_ne!(
            id_after, content_identity,
            "content drift must change the identity"
        );
        let mut policy_changed = build_apply_payload(ACTION_ARCHIVE, &entry, None);
        policy_changed.policy_version = "memory-lifecycle-v3".to_string();
        assert_ne!(
            content_identity,
            compute_lifecycle_identity(&policy_changed),
            "policy drift must change the identity"
        );
    }

    // NOTE: a former `identity_excludes_volatile_fields` test hashed the SAME
    // payload twice and asserted equality. Nothing in production could break
    // it, and the property it named ("volatile state cannot perturb the
    // identity") is enforced by `LifecycleApplyPayload` simply having no slot
    // for status/review/applied_at — a type-level fact, not a runtime one.
    // `identity_is_deterministic_and_stable_under_rebuild` already covers the
    // determinism half. Deleted rather than kept as coverage theatre.

    #[test]
    fn legacy_v1_detection() {
        let v1 = serde_json::json!({
            "proposal_id": "lifecycle:archive:abc",
            "status": "pending",
            "source_id": "abc",
        });
        assert!(is_legacy_v1_proposal(&v1));
        let v2 = serde_json::json!({
            "schema_version": 2,
            "identity": "deadbeef",
            "status": "pending",
        });
        assert!(!is_legacy_v1_proposal(&v2));
        let v_wrong = serde_json::json!({"schema_version": 1});
        assert!(is_legacy_v1_proposal(&v_wrong));
    }

    #[test]
    fn proposal_id_is_bounded_and_derived_from_typed_payload() {
        let source = MemoryEntry {
            id: "source-with-a-long-and-otherwise-unbounded-identifier".into(),
            ..test_entry()
        };
        let payload = build_apply_payload(ACTION_ARCHIVE, &source, None);
        let proposal_id = lifecycle_proposal_id(&payload).expect("known action");
        let prefix = "lifecycle:archive:";
        assert_eq!(
            proposal_id,
            lifecycle_proposal_id(&payload).expect("known action"),
            "identical payloads must produce stable proposal IDs"
        );
        assert!(proposal_id.starts_with(prefix));
        assert_eq!(proposal_id.len(), prefix.len() + 64);
        assert!(proposal_id[prefix.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')));
        assert!(!proposal_id.contains(&source.id));
        let mut changed = payload.clone();
        changed.source.revision += 1;
        assert_ne!(
            proposal_id,
            lifecycle_proposal_id(&changed).expect("known action"),
            "different immutable payloads must have distinct proposal IDs"
        );
    }

    #[test]
    fn validation_rejects_stale_policy_version() {
        let source = MemoryEntry {
            id: "stale-policy-source".into(),
            ..test_entry()
        };
        let mut payload = build_apply_payload(ACTION_ARCHIVE, &source, None);
        payload.policy_version = "memory-lifecycle-v1".into();
        let proposal_id = lifecycle_proposal_id(&payload).expect("known action");
        let proposal = serde_json::json!({
            "proposal_id": proposal_id,
            "kind": payload.review_display.kind.clone(),
            "schema_version": payload.schema_version,
            "policy_version": payload.policy_version.clone(),
            "lifecycle_action": payload.lifecycle_action.clone(),
            "source_id": payload.source.id.clone(),
            "target_id": serde_json::Value::Null,
            "requires_human_approval": payload.review_display.requires_human_approval,
            "path": payload.review_display.path.clone(),
            "rationale": payload.review_display.rationale.clone(),
            "evidence": payload.review_display.evidence.clone(),
            "identity": compute_lifecycle_identity(&payload),
            "apply_payload": payload,
        });

        let err = validate_lifecycle_proposal(
            proposal["proposal_id"].as_str().expect("proposal id"),
            &proposal,
        )
        .expect_err("stale policy must be rejected before review");
        assert!(err.to_string().contains("unsupported schema/policy"));
    }

    #[test]
    fn validation_rejects_action_payload_missing_required_eligibility_input() {
        let source = MemoryEntry {
            id: "missing-archive-access-input".into(),
            ..test_entry()
        };
        let mut payload = build_apply_payload(ACTION_ARCHIVE, &source, None);
        payload.source.eligibility_access_count = None;
        let proposal_id = lifecycle_proposal_id(&payload).expect("known action");
        let proposal = serde_json::json!({
            "proposal_id": proposal_id,
            "kind": payload.review_display.kind.clone(),
            "schema_version": payload.schema_version,
            "policy_version": payload.policy_version.clone(),
            "lifecycle_action": payload.lifecycle_action.clone(),
            "source_id": payload.source.id.clone(),
            "target_id": serde_json::Value::Null,
            "requires_human_approval": payload.review_display.requires_human_approval,
            "path": payload.review_display.path.clone(),
            "rationale": payload.review_display.rationale.clone(),
            "evidence": payload.review_display.evidence.clone(),
            "identity": compute_lifecycle_identity(&payload),
            "apply_payload": payload,
        });

        let err = validate_lifecycle_proposal(
            proposal["proposal_id"].as_str().expect("proposal id"),
            &proposal,
        )
        .expect_err("archive payload without access_count must refuse review/apply validation");
        assert!(
            err.to_string()
                .contains("source eligibility snapshot does not match action 'archive'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validation_and_review_reject_wrong_target_cardinality_for_every_action() {
        for (action, target_is_present, expected_requirement) in [
            (ACTION_MERGE_INTO, false, "requires exactly one target"),
            (ACTION_SUPERSEDE, false, "requires exactly one target"),
            (ACTION_NEAR_DUP_MERGE, false, "requires exactly one target"),
            (ACTION_ARCHIVE, true, "requires no target"),
            (ACTION_PROMOTE_DISTILLED, true, "requires no target"),
        ] {
            let mut store = MemoryStore::open_in_memory().expect("open test store");
            let source = MemoryEntry {
                id: format!("wrong-cardinality-source-{action}"),
                path: format!("/scratch/wrong-cardinality/{action}"),
                ..test_entry()
            };
            let target = MemoryEntry {
                id: format!("wrong-cardinality-target-{action}"),
                path: format!("/scratch/wrong-cardinality/{action}"),
                ..test_entry()
            };
            store.upsert(&source).expect("seed source");
            store.upsert(&target).expect("seed target");
            let source = store
                .get(&source.id)
                .expect("load source")
                .expect("source exists");
            let target = store
                .get(&target.id)
                .expect("load target")
                .expect("target exists");
            let payload =
                build_apply_payload(action, &source, target_is_present.then_some(&target));
            let (proposal_id, proposal) = proposal_from_payload(&payload);
            let expected_error = format!(
                "Invalid argument: v2 lifecycle proposal {proposal_id} action '{action}' {expected_requirement}"
            );

            let validation_error = validate_lifecycle_proposal(&proposal_id, &proposal)
                .expect_err("self-consistent wrong-cardinality payload must fail validation");
            assert_eq!(validation_error.to_string(), expected_error);

            let proposal_raw = serde_json::to_string(&proposal).expect("serialize proposal");
            store
                .set_state(LIFECYCLE_PROPOSAL_NS, &proposal_id, &proposal_raw)
                .expect("persist pending malformed proposal");
            let proposal_before = store
                .get_state_kv(LIFECYCLE_PROPOSAL_NS, &proposal_id)
                .expect("load pending proposal")
                .expect("pending proposal exists");
            let review_error = review_lifecycle_proposal(
                &store,
                &proposal_id,
                LifecycleReviewDecision::Approved,
                None,
            )
            .expect_err("wrong-cardinality proposal must not become approved");
            assert_eq!(review_error.to_string(), expected_error);
            assert_eq!(
                store
                    .get_state_kv(LIFECYCLE_PROPOSAL_NS, &proposal_id)
                    .expect("reload refused proposal")
                    .expect("refused proposal remains"),
                proposal_before,
                "review refusal must preserve the exact pending row and version for {action}"
            );

            let source_after = store
                .get(&source.id)
                .expect("reload source")
                .expect("source remains");
            let target_after = store
                .get(&target.id)
                .expect("reload target")
                .expect("target remains");
            assert_eq!(source_after.archived, source.archived);
            assert_eq!(source_after.revision, source.revision);
            assert_eq!(source_after.tier, source.tier);
            assert_eq!(target_after.archived, target.archived);
            assert_eq!(target_after.revision, target.revision);
            assert_eq!(target_after.tier, target.tier);
        }
    }

    #[test]
    fn apply_rolls_back_memory_when_proposal_stamp_aborts() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let source = MemoryEntry {
            id: "rollback-source".into(),
            path: "/scratch/rollback".into(),
            ..test_entry()
        };
        store.upsert(&source).expect("seed source");
        let source_before = store
            .get("rollback-source")
            .expect("load source")
            .expect("source exists");
        let payload = build_apply_payload(ACTION_ARCHIVE, &source_before, None);
        let proposal_id = lifecycle_proposal_id(&payload).expect("known action");
        let proposal = serde_json::json!({
            "proposal_id": proposal_id.clone(),
            "kind": payload.review_display.kind.clone(),
            "schema_version": payload.schema_version,
            "policy_version": payload.policy_version.clone(),
            "lifecycle_action": payload.lifecycle_action.clone(),
            "status": "pending",
            "source_id": payload.source.id.clone(),
            "target_id": serde_json::Value::Null,
            "requires_human_approval": payload.review_display.requires_human_approval,
            "path": payload.review_display.path.clone(),
            "rationale": payload.review_display.rationale.clone(),
            "evidence": payload.review_display.evidence.clone(),
            "identity": compute_lifecycle_identity(&payload),
            "apply_payload": payload.clone(),
        });
        store
            .set_state(
                LIFECYCLE_PROPOSAL_NS,
                &proposal_id,
                &serde_json::to_string(&proposal).expect("serialize proposal"),
            )
            .expect("persist proposal");
        review_lifecycle_proposal(
            &store,
            &proposal_id,
            LifecycleReviewDecision::Approved,
            None,
        )
        .expect("approve proposal");
        let (proposal_before, _) = store
            .get_state_kv(LIFECYCLE_PROPOSAL_NS, &proposal_id)
            .expect("load approved proposal")
            .expect("approved proposal exists");

        store
            .conn
            .execute_batch(
                "CREATE TRIGGER abort_lifecycle_proposal_stamp
                 BEFORE UPDATE ON hard_state
                 WHEN NEW.namespace = 'memory_lifecycle_proposals'
                 BEGIN SELECT RAISE(ABORT, 'abort lifecycle proposal stamp'); END;",
            )
            .expect("install abort trigger");
        let err = apply_lifecycle_proposal(&mut store, &proposal_id)
            .expect_err("proposal stamp trigger must abort the transaction");
        assert!(err.to_string().contains("abort lifecycle proposal stamp"));

        let source_after = store
            .get_with_options("rollback-source", true)
            .expect("load source after rollback")
            .expect("source remains");
        let (proposal_after, _) = store
            .get_state_kv(LIFECYCLE_PROPOSAL_NS, &proposal_id)
            .expect("load proposal after rollback")
            .expect("proposal remains");
        assert_eq!(source_after.archived, source_before.archived);
        assert_eq!(source_after.revision, source_before.revision);
        assert_eq!(proposal_after, proposal_before);
    }

    /// Seed a v2 proposal for `payload` into `hard_state` and drive it to
    /// `approved`, the state `apply_lifecycle_proposal` requires.
    fn persist_and_approve(store: &MemoryStore, payload: &LifecycleApplyPayload) -> String {
        let (proposal_id, proposal) = proposal_from_payload(payload);
        store
            .set_state(
                LIFECYCLE_PROPOSAL_NS,
                &proposal_id,
                &serde_json::to_string(&proposal).expect("serialize proposal"),
            )
            .expect("persist proposal");
        review_lifecycle_proposal(store, &proposal_id, LifecycleReviewDecision::Approved, None)
            .expect("approve proposal");
        proposal_id
    }

    fn seed(store: &mut MemoryStore, id: &str, text: &str, keywords: &[&str]) -> MemoryEntry {
        let entry = MemoryEntry {
            id: id.into(),
            path: format!("/scratch/lifecycle/{id}"),
            text: text.into(),
            keywords: keywords.iter().map(|k| (*k).to_string()).collect(),
            ..test_entry()
        };
        store.upsert(&entry).expect("seed row");
        store
            .get(id)
            .expect("load seeded row")
            .expect("seeded row exists")
    }

    /// THE discriminating test for "the identity must bind every execution
    /// input". A background enrichment pass rewrites the source row's
    /// `keywords` between approve and apply — no `revision` bump, no text
    /// change, no path change, which is exactly what
    /// `db::memory_crud::update::update_enrichment_fields` does in production.
    /// If `keywords` is not in the snapshot the rebuilt payload is byte-identical
    /// to the approved one, apply sails through, and a keyword set no human ever
    /// saw gets folded into the survivor.
    #[test]
    fn apply_refuses_source_keyword_drift_that_leaves_revision_untouched() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let source = seed(
            &mut store,
            "kwdrift-source",
            "quantum widget calibration notes for the alpha bench",
            &["alpha"],
        );
        let target = seed(
            &mut store,
            "kwdrift-target",
            "maritime logistics rota covering harbour pilots",
            &["zulu"],
        );
        let payload = build_apply_payload(ACTION_MERGE_INTO, &source, Some(&target));
        let proposal_id = persist_and_approve(&store, &payload);

        // Background enrichment: keywords only. Mirrors
        // `update_enrichment_fields`' deliberate no-revision-bump UPDATE.
        let touched = store
            .conn
            .execute(
                "UPDATE memories SET keywords = ?1, updated_at = ?2 WHERE id = ?3",
                params![
                    r#"["alpha","injected-after-approval"]"#,
                    lifecycle_now_iso(),
                    source.id
                ],
            )
            .expect("simulate enrichment write");
        assert_eq!(touched, 1, "enrichment write must have landed");
        let source_after_enrichment = store
            .get(&source.id)
            .expect("reload source")
            .expect("source exists");
        assert_eq!(
            source_after_enrichment.revision, source.revision,
            "the attack depends on revision NOT moving; if this fails the test no longer probes the hole"
        );

        let err = apply_lifecycle_proposal(&mut store, &proposal_id)
            .expect_err("keyword drift on the source must refuse the apply");
        assert!(
            err.to_string().contains("revalidation failed"),
            "expected a drift refusal, got: {err}"
        );

        let survivor = store
            .get(&target.id)
            .expect("reload target")
            .expect("target exists");
        assert_eq!(
            survivor.keywords, target.keywords,
            "a refused apply must not fold anything into the survivor"
        );
        assert_eq!(
            survivor.revision, target.revision,
            "a refused apply must not bump the survivor"
        );
        let source_row = store
            .get_with_options(&source.id, true)
            .expect("reload source")
            .expect("source exists");
        assert!(
            !source_row.archived,
            "a refused apply must not archive the source"
        );
    }

    /// THE discriminating test for the no-op guard. The target's stored
    /// keyword/entity arrays are unsorted (the normal case — `db::memory_crud`
    /// serializes them in whatever order the writer supplied) and the source
    /// contributes nothing new, so the merge is a genuine no-op on the
    /// survivor. Comparing the sorted fold against the raw stored order would
    /// report "changed", upsert, and bump `revision` — invalidating every other
    /// approved proposal that shares this target.
    #[test]
    fn merge_leaves_the_target_alone_when_the_fold_only_reorders_tags() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let target = seed(
            &mut store,
            "noop-target",
            "maritime logistics rota covering harbour pilots",
            &["zulu", "alpha", "mike"],
        );
        assert_eq!(
            target.keywords,
            vec!["zulu".to_string(), "alpha".to_string(), "mike".to_string()],
            "the target's stored order must stay unsorted or this test proves nothing"
        );
        let source = seed(
            &mut store,
            "noop-source",
            "quantum widget calibration notes for the alpha bench",
            &["alpha", "mike"],
        );
        let payload = build_apply_payload(ACTION_MERGE_INTO, &source, Some(&target));
        let proposal_id = persist_and_approve(&store, &payload);

        apply_lifecycle_proposal(&mut store, &proposal_id).expect("apply must succeed");

        let survivor = store
            .get(&target.id)
            .expect("reload target")
            .expect("target exists");
        assert_eq!(
            survivor.revision, target.revision,
            "the fold added no new tag, so the survivor must not be rewritten"
        );
        assert_eq!(
            survivor.keywords, target.keywords,
            "an untouched survivor keeps its stored array verbatim"
        );
        let source_row = store
            .get_with_options(&source.id, true)
            .expect("reload source")
            .expect("source exists");
        assert!(
            source_row.archived,
            "the source side of the merge still executes"
        );
    }

    fn test_entry() -> MemoryEntry {
        MemoryEntry {
            id: String::new(),
            path: "/test".into(),
            summary: "s".into(),
            text: "t".into(),
            importance: 0.5,
            timestamp: "2026-01-01T00:00:00Z".into(),
            valid_from: "2026-01-01T00:00:00Z".into(),
            valid_until: None,
            category: "fact".into(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "mcp".into(),
            scope: "general".into(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: serde_json::json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".into(),
        }
    }
}
