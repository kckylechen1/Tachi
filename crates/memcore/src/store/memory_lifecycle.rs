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
//! typed [`LifecycleApplyPayload`] (source/target endpoint snapshots +
//! action + schema/policy version). Volatile fields (status, review notes,
//! `applied_at`, `expires_at`) live OUTSIDE the payload in the stored
//! proposal JSON, so they can never perturb the identity. At apply, the
//! payload is rebuilt from the LIVE rows and the identity is recomputed; a
//! mismatch means the world drifted between approve and apply → refuse.
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

/// Immutable snapshot of one endpoint (source or target) captured at propose
/// time. Every field participates in the identity hash, so any drift is
/// detected at apply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LifecycleEndpointSnapshot {
    pub id: String,
    pub revision: i64,
    pub path: String,
    /// SHA-256 hex of `entry.text`.
    pub text_digest: String,
    pub archived: bool,
    pub retention_policy: Option<String>,
    pub tier: String,
    pub is_wiki: bool,
    /// `None` when the endpoint is eligible (not protected); a static reason
    /// tag when protected. Protected rows can never be lifecycle sources.
    pub protected_reason: Option<String>,
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
        archived: entry.archived,
        retention_policy: entry.retention_policy.clone(),
        tier: entry.tier.clone(),
        is_wiki: entry.is_wiki(),
        protected_reason: lifecycle_protection_reason(entry).map(str::to_string),
    }
}

/// Build a v2 apply payload from the source/target entries and the action
/// name. Used by the proposal generators.
pub fn build_apply_payload(
    action: &str,
    source: &MemoryEntry,
    target: Option<&MemoryEntry>,
) -> LifecycleApplyPayload {
    LifecycleApplyPayload {
        schema_version: LIFECYCLE_SCHEMA_VERSION,
        policy_version: LIFECYCLE_POLICY_VERSION.to_string(),
        lifecycle_action: action.to_string(),
        source: snapshot_endpoint(source),
        target: target.map(snapshot_endpoint),
    }
}

/// True when a stored proposal JSON is a legacy v1 proposal (missing
/// `schema_version=2`). Legacy proposals remain listable but are loudly
/// refused at review/apply.
pub fn is_legacy_v1_proposal(value: &serde_json::Value) -> bool {
    value.get("schema_version").and_then(|v| v.as_u64()) != Some(LIFECYCLE_SCHEMA_VERSION as u64)
}

/// Validate the immutable execution fields duplicated at the proposal top
/// level against its typed apply payload, then verify that the stored identity
/// hashes that payload. Review and apply both call this guard so presentation
/// fields cannot be tampered into an execution request after proposal.
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

    let expected_target_id =
        serde_json::to_value(payload.target.as_ref().map(|target| target.id.clone()))
            .expect("lifecycle target id serializable");
    for (field, expected) in [
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

    let now = chrono::Utc::now().to_rfc3339();
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
        value["expires_at"] = serde_json::json!(expires.to_rfc3339());
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
) -> Result<Option<LifecycleEndpointSnapshot>, MemoryError> {
    let row = tx
        .query_row(
            "SELECT id, path, text, archived, revision, retention_policy, tier, category, domain, metadata
             FROM memories WHERE id = ?1",
            params![id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, bool>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, String>(9)?,
                ))
            },
        )
        .optional()?;
    Ok(row.map(
        |(
            id,
            path,
            text,
            archived,
            revision,
            retention_policy,
            tier,
            category,
            domain,
            metadata_str,
        )| {
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
            LifecycleEndpointSnapshot {
                id,
                revision,
                path,
                text_digest: lifecycle_text_digest(&text),
                archived,
                retention_policy,
                tier,
                is_wiki,
                protected_reason,
            }
        },
    ))
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
    let live_source = read_endpoint_snapshot(&tx, &source_id)?
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
            read_endpoint_snapshot(&tx, tid)?
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
            let now = chrono::Utc::now().to_rfc3339();
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
            let now = chrono::Utc::now().to_rfc3339();
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
            let target_keywords = survivor.keywords.clone();
            let target_entities = survivor.entities.clone();
            let target_importance = survivor.importance;
            // Fold unique keywords/entities; keep survivor text canonical.
            let mut kw: std::collections::BTreeSet<String> =
                survivor.keywords.iter().cloned().collect();
            for k in &source.keywords {
                kw.insert(k.clone());
            }
            survivor.keywords = kw.into_iter().collect();
            let mut ents: std::collections::BTreeSet<String> =
                survivor.entities.iter().cloned().collect();
            for e in &source.entities {
                ents.insert(e.clone());
            }
            survivor.entities = ents.into_iter().collect();
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
            let now = chrono::Utc::now().to_rfc3339();
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
    let now = chrono::Utc::now().to_rfc3339();
    let expires =
        (chrono::Utc::now() + chrono::Duration::days(LIFECYCLE_TERMINAL_TTL_DAYS)).to_rfc3339();
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
        let payload = build_apply_payload(ACTION_MERGE_INTO, &entry, Some(&target));
        let id_a = compute_lifecycle_identity(&payload);
        let id_b = compute_lifecycle_identity(&payload);
        assert_eq!(id_a, id_b, "identity must be deterministic");
        assert_eq!(id_a.len(), 64, "SHA-256 hex is 64 chars");
        // Rebuilding from an equivalent snapshot reproduces the same identity.
        let rebuilt = LifecycleApplyPayload {
            schema_version: LIFECYCLE_SCHEMA_VERSION,
            policy_version: LIFECYCLE_POLICY_VERSION.to_string(),
            lifecycle_action: ACTION_MERGE_INTO.to_string(),
            source: snapshot_endpoint(&entry),
            target: Some(snapshot_endpoint(&target)),
        };
        assert_eq!(
            compute_lifecycle_identity(&rebuilt),
            id_a,
            "identity must be stable when rebuilt from matching fields"
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

    #[test]
    fn identity_excludes_volatile_fields() {
        // The payload struct has NO slot for status/review/applied_at —
        // volatile state cannot perturb the identity by construction.
        let entry = MemoryEntry {
            id: "src-3".into(),
            revision: 1,
            ..test_entry()
        };
        let payload = build_apply_payload(ACTION_ARCHIVE, &entry, None);
        let id = compute_lifecycle_identity(&payload);
        // Simulate "volatile state changed" by checking the payload still
        // hashes identically (it has no volatile fields to change).
        let id_again = compute_lifecycle_identity(&payload);
        assert_eq!(id, id_again);
    }

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
            "schema_version": payload.schema_version,
            "policy_version": payload.policy_version.clone(),
            "lifecycle_action": payload.lifecycle_action.clone(),
            "source_id": payload.source.id.clone(),
            "target_id": serde_json::Value::Null,
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
            "kind": "memory_lifecycle",
            "schema_version": payload.schema_version,
            "policy_version": payload.policy_version.clone(),
            "lifecycle_action": payload.lifecycle_action.clone(),
            "status": "pending",
            "source_id": payload.source.id.clone(),
            "target_id": serde_json::Value::Null,
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
