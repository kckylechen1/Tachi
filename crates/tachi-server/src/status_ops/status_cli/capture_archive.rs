//! Explicit, reversible archive sweep for #1301 auto-capture artifacts.

use chrono::{DateTime, Utc};
use memcore::{MemoryEntry, MemoryStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::Path;

use crate::foundry_runtime_ops::{CAPTURE_EPHEMERAL_TTL_DAYS, CAPTURE_RETENTION_POLICY_VERSION};

const PLAN_SCHEMA: &str = "tachi.capture-archive-plan.v1";
const APPLY_SCHEMA: &str = "tachi.capture-archive-apply.v1";
const RESTORE_SCHEMA: &str = "tachi.capture-archive-restore.v1";
const STORED_CAPTURE_SOURCE: &str = "external:capture_session";
/// Provisional bound: an incomplete scan is an error, never a partial plan.
const CAPTURE_ARCHIVE_PLAN_SCAN_LIMIT: usize = 100_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct DbIdentity {
    canonical_path: String,
    file_identity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct PlanRow {
    status: String,
    reason: String,
    memory_id: String,
    path: String,
    source_revision: Option<String>,
    source_key: Option<String>,
    memory_revision: i64,
    policy_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct CaptureArchivePlan {
    schema: String,
    db_identity: DbIdentity,
    as_of: String,
    policy_version: String,
    source_state_hash: String,
    counts: Counts,
    rows: Vec<PlanRow>,
    plan_hash: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Counts {
    retained: usize,
    refused: usize,
    eligible: usize,
    error: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ApplyReceipt {
    schema: String,
    db_identity: DbIdentity,
    policy_version: String,
    plan_hash: String,
    rows: Vec<ApplyRow>,
    survivor_count: usize,
    archive_count: usize,
    retained_count: usize,
    refusal_count: usize,
    error_count: usize,
    receipt_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApplyRow {
    memory_id: String,
    path: String,
    source_revision: Option<String>,
    source_key: Option<String>,
    policy_version: String,
    status: String,
    reason: String,
    before_revision: i64,
    archived_revision: Option<i64>,
}

impl ApplyRow {
    fn plan_lineage_missing(&self) -> bool {
        self.source_revision.as_deref().is_none_or(str::is_empty)
            || self.source_key.as_deref().is_none_or(str::is_empty)
            || self.policy_version.is_empty()
    }
}

#[derive(Debug, Serialize)]
pub(super) struct RestoreReceipt {
    schema: &'static str,
    db_identity: DbIdentity,
    source_receipt_hash: String,
    rows: Vec<RestoreRow>,
    restored_count: usize,
    refusal_count: usize,
    error_count: usize,
    receipt_hash: String,
}
#[derive(Debug, Serialize)]
struct RestoreRow {
    memory_id: String,
    status: String,
    reason: String,
    archived_revision: i64,
    restored_revision: Option<i64>,
}

fn hash<T: Serialize>(value: &T) -> Result<String, String> {
    crate::tool_params::canonical_json_sha256(
        &serde_json::to_value(value).map_err(|e| e.to_string())?,
    )
}

fn identity(path: &Path) -> Result<DbIdentity, String> {
    let canonical =
        fs::canonicalize(path).map_err(|e| format!("canonicalize {}: {e}", path.display()))?;
    let metadata = fs::metadata(&canonical).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    let file_identity = {
        use std::os::unix::fs::MetadataExt;
        format!("unix:{}:{}", metadata.dev(), metadata.ino())
    };
    #[cfg(not(unix))]
    let file_identity = format!("portable:length:{}", metadata.len()); // Path + length is the documented safe fallback.
    Ok(DbIdentity {
        canonical_path: canonical.to_string_lossy().into_owned(),
        file_identity,
    })
}

fn capture_signaled(e: &MemoryEntry) -> bool {
    e.source == STORED_CAPTURE_SOURCE
        || e.metadata
            .pointer("/provenance/source_kind")
            .and_then(Value::as_str)
            == Some("session_capture")
        || e.metadata.get("capture_retention").is_some()
        || e.metadata.get("capture_replay_key").is_some()
        || e.metadata.get("capture_replay_keys").is_some()
        || e.metadata.get("artifact_kind").and_then(Value::as_str) == Some("session_capture")
}

fn field<'a>(e: &'a MemoryEntry, key: &str) -> Option<&'a str> {
    e.metadata
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// Validate the immutable identity, lineage, and expiry shape emitted by #1301.
/// Archive state and the operator's `as_of` are deliberately checked by the
/// caller so restore can reuse this for an archived row.
fn capture_shape(e: &MemoryEntry) -> Result<DateTime<Utc>, &'static str> {
    if e.source != STORED_CAPTURE_SOURCE {
        return Err("source_not_capture_session");
    }
    if e.metadata
        .pointer("/provenance/source_kind")
        .and_then(Value::as_str)
        != Some("session_capture")
    {
        return Err("provenance_not_session_capture");
    }
    if e.metadata
        .pointer("/provenance/tool_name")
        .and_then(Value::as_str)
        != Some("capture_session")
    {
        return Err("provenance_tool_not_capture_session");
    }
    if field(e, "artifact_kind") != Some("session_capture") {
        return Err("artifact_kind_not_session_capture");
    }
    let replay_key = field(e, "capture_replay_key").ok_or("missing_deterministic_lineage")?;
    let source_revision = field(e, "source_revision").ok_or("missing_deterministic_lineage")?;
    let source_event_id = field(e, "source_event_id").ok_or("missing_deterministic_lineage")?;
    if e.id != replay_key.replacen("capture-replay:", "capture-session:", 1)
        || !replay_key.starts_with("capture-replay:")
        || e.metadata
            .get("capture_replay_keys")
            .and_then(Value::as_array)
            != Some(&vec![Value::String(replay_key.to_string())])
        || e.metadata.get("source_revisions").and_then(Value::as_array)
            != Some(&vec![Value::String(source_revision.to_string())])
    {
        return Err("missing_deterministic_lineage");
    }
    let source_ref_matches = e
        .metadata
        .get("source_refs")
        .and_then(Value::as_array)
        .is_some_and(|refs| {
            refs.len() == 1
                && refs[0].get("ref_type").and_then(Value::as_str) == Some("turn")
                && refs[0].get("ref_id").and_then(Value::as_str) == Some(source_event_id)
                && refs[0].get("revision").and_then(Value::as_str) == Some(source_revision)
        });
    if !source_ref_matches {
        return Err("capture_lineage_mismatch");
    }
    if e.retention_policy.as_deref() != Some("ephemeral") {
        return Err("retention_not_ephemeral");
    }
    let r = e
        .metadata
        .get("capture_retention")
        .and_then(Value::as_object)
        .ok_or("malformed_capture_retention")?;
    if r.get("class").and_then(Value::as_str) != Some("ephemeral") {
        return Err("capture_class_not_ephemeral");
    }
    if r.get("policy_version").and_then(Value::as_str) != Some(CAPTURE_RETENTION_POLICY_VERSION) {
        return Err("capture_policy_version_mismatch");
    }
    if r.get("ttl_days").and_then(Value::as_i64) != Some(CAPTURE_EPHEMERAL_TTL_DAYS) {
        return Err("capture_ttl_mismatch");
    }
    let timestamp = e
        .timestamp
        .parse::<DateTime<Utc>>()
        .map_err(|_| "malformed_capture_timestamp")?;
    let valid_from = e
        .valid_from
        .parse::<DateTime<Utc>>()
        .map_err(|_| "malformed_valid_from")?;
    let until = e
        .valid_until
        .as_deref()
        .ok_or("missing_valid_until")?
        .parse::<DateTime<Utc>>()
        .map_err(|_| "malformed_valid_until")?;
    if valid_from != timestamp
        || until != timestamp + chrono::Duration::days(CAPTURE_EPHEMERAL_TTL_DAYS)
    {
        return Err("capture_expiry_interval_mismatch");
    }
    Ok(until)
}

/// The single pure eligibility predicate used both while planning and immediately before CAS.
fn eligibility(e: &MemoryEntry, as_of: DateTime<Utc>) -> Result<(), &'static str> {
    if e.archived {
        return Err("already_archived");
    }
    if capture_shape(e)? > as_of {
        return Err("not_expired");
    }
    Ok(())
}

fn approved_active(e: &MemoryEntry) -> bool {
    e.metadata.get("lifecycle").and_then(Value::as_str) == Some("active")
        && e.metadata
            .pointer("/review_receipt/decision")
            .and_then(Value::as_str)
            == Some("approved")
        && e.metadata
            .pointer("/review_receipt/approver")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
        && e.metadata
            .pointer("/review_receipt/decided_at")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<DateTime<Utc>>().ok())
            .is_some()
}

fn protected_shape(e: &MemoryEntry) -> bool {
    let hay = format!(
        "{} {} {} {} {} {} {} {}",
        e.path,
        e.category,
        e.topic,
        e.tier,
        e.domain.as_deref().unwrap_or(""),
        e.metadata
            .get("authority")
            .and_then(Value::as_str)
            .unwrap_or(""),
        e.metadata.get("kind").and_then(Value::as_str).unwrap_or(""),
        e.metadata
            .get("lifecycle")
            .and_then(Value::as_str)
            .unwrap_or("")
    )
    .to_ascii_lowercase();
    [
        "soul",
        "current-truth",
        "current_truth",
        "precedent",
        "wiki",
        "guide",
        "decision",
        "pattern",
        "pinned",
        "permanent",
        "active",
    ]
    .iter()
    .any(|x| hay.contains(x))
}

fn evidence_index(all: &[MemoryEntry]) -> (BTreeSet<String>, bool) {
    let mut refs = BTreeSet::new();
    let mut incomplete = false;
    for e in all.iter().filter(|e| !e.archived && approved_active(e)) {
        if let Some(typed) = e.metadata.get("evidence_refs_v1") {
            match typed.as_array() {
                Some(items) if !items.is_empty() => {
                    for item in items {
                        match item
                            .get("ref")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            Some(v) => {
                                refs.insert(v.to_string());
                            }
                            None => incomplete = true,
                        }
                    }
                }
                _ => incomplete = true,
            }
        } else if let Some(legacy) = e.metadata.get("source_refs") {
            match legacy.as_array() {
                Some(items) if !items.is_empty() => {
                    for item in items {
                        match item.as_str().filter(|s| !s.is_empty()) {
                            Some(v) => {
                                refs.insert(v.to_string());
                            }
                            None => incomplete = true,
                        }
                    }
                }
                _ => incomplete = true,
            }
        } else {
            incomplete = true;
        }
    }
    (refs, incomplete)
}

fn evidence_protected(e: &MemoryEntry, refs: &BTreeSet<String>) -> bool {
    [&e.id, &e.path].into_iter().any(|v| refs.contains(v))
        || [
            field(e, "source_event_id"),
            field(e, "source_revision"),
            field(e, "capture_replay_key"),
        ]
        .into_iter()
        .flatten()
        .any(|v| refs.contains(v))
}

fn observed_policy_version(e: &MemoryEntry) -> String {
    match e.metadata.pointer("/capture_retention/policy_version") {
        Some(Value::String(value)) if !value.is_empty() => value.clone(),
        Some(_) => "malformed".into(),
        None => "missing".into(),
    }
}

pub(super) fn build_plan(path: &Path, as_of: DateTime<Utc>) -> Result<CaptureArchivePlan, String> {
    let id = identity(path)?;
    // This SQLite read-only open cannot initialize, migrate, checkpoint, or
    // mutate memory rows. SQLite may still use its normal read-side WAL
    // coordination files when the source database is live.
    let store = MemoryStore::open_read_only(&id.canonical_path).map_err(|e| e.to_string())?;
    let all = store
        .get_all_with_options(CAPTURE_ARCHIVE_PLAN_SCAN_LIMIT + 1, true)
        .map_err(|e| e.to_string())?;
    if all.len() > CAPTURE_ARCHIVE_PLAN_SCAN_LIMIT {
        return Err("capture archive scan limit exceeded; no partial plan emitted".into());
    }
    let (refs, incomplete) = evidence_index(&all);
    let mut examined: Vec<_> = all.iter().filter(|e| capture_signaled(e)).collect();
    examined.sort_by(|a, b| a.id.cmp(&b.id).then(a.path.cmp(&b.path)));
    let mut rows = Vec::new();
    let mut counts = Counts::default();
    for e in examined {
        let (status, reason) = match eligibility(e, as_of) {
            Ok(()) if protected_shape(e) => ("refused", "protected_active_truth_shape"),
            Ok(()) if evidence_protected(e, &refs) => ("refused", "required_active_evidence"),
            Ok(()) if incomplete => ("refused", "incomplete_active_evidence_index"),
            Ok(()) => ("eligible", "expired_exact_ephemeral"),
            Err("already_archived" | "not_expired") => {
                ("retained", eligibility(e, as_of).unwrap_err())
            }
            Err(reason) => ("refused", reason),
        };
        match status {
            "eligible" => counts.eligible += 1,
            "retained" => counts.retained += 1,
            "refused" => counts.refused += 1,
            _ => counts.error += 1,
        }
        rows.push(PlanRow {
            status: status.into(),
            reason: reason.into(),
            memory_id: e.id.clone(),
            path: e.path.clone(),
            source_revision: field(e, "source_revision").map(str::to_owned),
            source_key: field(e, "capture_replay_key").map(str::to_owned),
            memory_revision: e.revision,
            policy_version: observed_policy_version(e),
        });
    }
    // Deliberately conservative: any row or access-state drift anywhere in the
    // database changes this hash and invalidates the plan, not only candidate drift.
    let state_input = (&all, &refs, incomplete);
    let source_state_hash = hash(&state_input)?;
    let mut plan = CaptureArchivePlan {
        schema: PLAN_SCHEMA.into(),
        db_identity: id,
        as_of: as_of.to_rfc3339(),
        policy_version: CAPTURE_RETENTION_POLICY_VERSION.into(),
        source_state_hash,
        counts,
        rows,
        plan_hash: String::new(),
    };
    plan.plan_hash = hash(&plan)?;
    Ok(plan)
}

fn verify_plan(plan: &CaptureArchivePlan) -> Result<(), String> {
    let mut unsigned = plan.clone();
    let expected = unsigned.plan_hash.clone();
    unsigned.plan_hash.clear();
    if plan.schema != PLAN_SCHEMA
        || plan.policy_version != CAPTURE_RETENTION_POLICY_VERSION
        || hash(&unsigned)? != expected
    {
        return Err("invalid plan schema or hash".into());
    }
    let mut ids = HashSet::new();
    if plan.rows.iter().any(|row| !ids.insert(&row.memory_id)) {
        return Err("invalid plan: duplicate memory id".into());
    }
    let counts = Counts {
        retained: plan.rows.iter().filter(|r| r.status == "retained").count(),
        refused: plan.rows.iter().filter(|r| r.status == "refused").count(),
        eligible: plan.rows.iter().filter(|r| r.status == "eligible").count(),
        error: plan.rows.iter().filter(|r| r.status == "error").count(),
    };
    if counts != plan.counts
        || counts.retained + counts.refused + counts.eligible + counts.error != plan.rows.len()
    {
        return Err("invalid plan: row accounting mismatch".into());
    }
    Ok(())
}

/// Rechecks all policy and evidence under the same RESERVED lock as the CAS.
/// Cooperating SQLite writers cannot insert protection evidence between this
/// snapshot and the archive update.
fn archive_one_transactionally(
    store: &MemoryStore,
    id: &str,
    expected_revision: i64,
    as_of: DateTime<Utc>,
) -> Result<Result<i64, &'static str>, String> {
    let conn = store.connection();
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| e.to_string())?;
    let operation = (|| {
        let all = store
            .get_all_with_options(CAPTURE_ARCHIVE_PLAN_SCAN_LIMIT + 1, true)
            .map_err(|e| e.to_string())?;
        if all.len() > CAPTURE_ARCHIVE_PLAN_SCAN_LIMIT {
            return Err("capture archive scan limit exceeded inside mutation transaction".into());
        }
        let candidate = all
            .iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| "missing_at_apply".to_string())?;
        if candidate.revision != expected_revision {
            return Ok(Err("planned_revision_changed"));
        }
        let (refs, incomplete) = evidence_index(&all);
        if eligibility(candidate, as_of).is_err()
            || protected_shape(candidate)
            || evidence_protected(candidate, &refs)
            || incomplete
        {
            return Ok(Err("apply_time_protection_or_policy_changed"));
        }
        if store
            .archive_memory_if_revision(id, expected_revision)
            .map_err(|e| e.to_string())?
        {
            Ok(Ok(expected_revision + 1))
        } else {
            Ok(Err("revision_cas_conflict"))
        }
    })();
    match operation {
        Ok(result) => {
            if result.is_ok() {
                conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
            } else {
                conn.execute_batch("ROLLBACK").map_err(|e| e.to_string())?;
            }
            Ok(result)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// Bind rollback to the exact archived #1301 row under the same RESERVED
/// lock as the restore CAS. A receipt hash is an integrity checksum, not an
/// authorization signature, so the database row must independently agree.
fn restore_one_transactionally(
    store: &MemoryStore,
    row: &ApplyRow,
) -> Result<Result<i64, &'static str>, String> {
    let archived_revision = row
        .archived_revision
        .ok_or_else(|| "archived receipt row lacks revision".to_string())?;
    let conn = store.connection();
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| e.to_string())?;
    let operation = (|| {
        let candidate = store
            .get_with_options(&row.memory_id, true)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "missing_at_restore".to_string())?;
        if !candidate.archived
            || candidate.revision != archived_revision
            || candidate.path != row.path
            || field(&candidate, "source_revision") != row.source_revision.as_deref()
            || field(&candidate, "capture_replay_key") != row.source_key.as_deref()
            || observed_policy_version(&candidate) != row.policy_version
            || capture_shape(&candidate).is_err()
        {
            return Ok(Err("restore_target_or_lineage_mismatch"));
        }
        if store
            .restore_archived_if_revision(&row.memory_id, archived_revision)
            .map_err(|e| e.to_string())?
        {
            Ok(Ok(archived_revision + 1))
        } else {
            Ok(Err("stale_or_missing_archive"))
        }
    })();
    match operation {
        Ok(result) => {
            if result.is_ok() {
                conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
            } else {
                conn.execute_batch("ROLLBACK").map_err(|e| e.to_string())?;
            }
            Ok(result)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

pub(super) fn apply(
    path: &Path,
    plan: CaptureArchivePlan,
    confirm: bool,
) -> Result<ApplyReceipt, String> {
    if !confirm {
        return Err("confirmation required; zero writes performed".into());
    }
    verify_plan(&plan)?;
    if identity(path)? != plan.db_identity {
        return Err("plan DB identity mismatch; zero writes performed".into());
    }
    let rebuilt = build_plan(path, plan.as_of.parse().map_err(|_| "invalid plan as_of")?)?;
    if rebuilt.source_state_hash != plan.source_state_hash || rebuilt.rows != plan.rows {
        return Err("stale plan; zero writes performed".into());
    }
    let store =
        MemoryStore::open_with_label(&plan.db_identity.canonical_path, "capture-archive-apply")
            .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (index, row) in plan.rows.iter().enumerate() {
        let mut status = row.status.clone();
        let mut reason = row.reason.clone();
        let mut archived = None;
        if row.status == "eligible" {
            match archive_one_transactionally(
                &store,
                &row.memory_id,
                row.memory_revision,
                plan.as_of.parse().map_err(|_| "invalid plan as_of")?,
            ) {
                Err(error) => {
                    status = "error".into();
                    reason = format!("mutation_error:{error}");
                    out.push(ApplyRow {
                        memory_id: row.memory_id.clone(),
                        path: row.path.clone(),
                        source_revision: row.source_revision.clone(),
                        source_key: row.source_key.clone(),
                        policy_version: row.policy_version.clone(),
                        status,
                        reason,
                        before_revision: row.memory_revision,
                        archived_revision: None,
                    });
                    for remaining in &plan.rows[index + 1..] {
                        out.push(ApplyRow {
                            memory_id: remaining.memory_id.clone(),
                            path: remaining.path.clone(),
                            source_revision: remaining.source_revision.clone(),
                            source_key: remaining.source_key.clone(),
                            policy_version: remaining.policy_version.clone(),
                            status: "error".into(),
                            reason: "not_attempted_after_fatal_error".into(),
                            before_revision: remaining.memory_revision,
                            archived_revision: None,
                        });
                    }
                    break;
                }
                Ok(result) => match result {
                    Ok(revision) => {
                        status = "archived".into();
                        reason = "archived_by_revision_cas".into();
                        archived = Some(revision)
                    }
                    Err(why) => {
                        status = "refused".into();
                        reason = why.into()
                    }
                },
            }
        }
        out.push(ApplyRow {
            memory_id: row.memory_id.clone(),
            path: row.path.clone(),
            source_revision: row.source_revision.clone(),
            source_key: row.source_key.clone(),
            policy_version: row.policy_version.clone(),
            status,
            reason,
            before_revision: row.memory_revision,
            archived_revision: archived,
        });
    }
    let mut receipt = ApplyReceipt {
        schema: APPLY_SCHEMA.into(),
        db_identity: plan.db_identity,
        policy_version: plan.policy_version,
        plan_hash: plan.plan_hash,
        archive_count: out.iter().filter(|r| r.status == "archived").count(),
        retained_count: out.iter().filter(|r| r.status == "retained").count(),
        refusal_count: out.iter().filter(|r| r.status == "refused").count(),
        error_count: out.iter().filter(|r| r.status == "error").count(),
        survivor_count: out.iter().filter(|r| r.status != "archived").count(),
        rows: out,
        receipt_hash: String::new(),
    };
    receipt.receipt_hash = hash(&receipt)?;
    Ok(receipt)
}

pub(super) fn restore(
    path: &Path,
    receipt: ApplyReceipt,
    confirm: bool,
) -> Result<RestoreReceipt, String> {
    if !confirm {
        return Err("confirmation required; zero writes performed".into());
    }
    let mut unsigned = receipt.clone();
    let expected = unsigned.receipt_hash.clone();
    unsigned.receipt_hash.clear();
    if receipt.schema != APPLY_SCHEMA
        || receipt.policy_version != CAPTURE_RETENTION_POLICY_VERSION
        || hash(&unsigned)? != expected
    {
        return Err("invalid apply receipt schema or hash".into());
    }
    let mut ids = HashSet::new();
    if receipt.rows.iter().any(|row| !ids.insert(&row.memory_id)) {
        return Err("invalid apply receipt: duplicate memory id".into());
    }
    let archives = receipt
        .rows
        .iter()
        .filter(|r| r.status == "archived")
        .count();
    let refusals = receipt
        .rows
        .iter()
        .filter(|r| r.status == "refused")
        .count();
    let retained = receipt
        .rows
        .iter()
        .filter(|r| r.status == "retained")
        .count();
    let errors = receipt.rows.iter().filter(|r| r.status == "error").count();
    if archives != receipt.archive_count
        || refusals != receipt.refusal_count
        || retained != receipt.retained_count
        || errors != receipt.error_count
        || receipt.survivor_count != receipt.rows.len() - archives
        || receipt.rows.iter().any(|r| {
            !matches!(
                r.status.as_str(),
                "archived" | "retained" | "refused" | "error"
            ) || (r.status == "archived") != r.archived_revision.is_some()
                || r.before_revision < 1
                || r.archived_revision
                    .is_some_and(|revision| r.before_revision.checked_add(1) != Some(revision))
                || r.memory_id.is_empty()
                || (r.status == "archived"
                    && (r.plan_lineage_missing()
                        || r.policy_version != CAPTURE_RETENTION_POLICY_VERSION
                        || r.source_key.as_deref().is_none_or(|key| {
                            !key.starts_with("capture-replay:")
                                || r.memory_id
                                    != key.replacen("capture-replay:", "capture-session:", 1)
                        })))
        })
    {
        return Err("invalid apply receipt: row accounting or archive revision mismatch".into());
    }
    if identity(path)? != receipt.db_identity {
        return Err("receipt DB identity mismatch".into());
    }
    let store = MemoryStore::open_with_label(
        &receipt.db_identity.canonical_path,
        "capture-archive-restore",
    )
    .map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    for r in receipt.rows.iter().filter(|r| r.status == "archived") {
        let rev = r
            .archived_revision
            .ok_or("invalid apply receipt: archived row lacks revision")?;
        let result = restore_one_transactionally(&store, r);
        rows.push(RestoreRow {
            memory_id: r.memory_id.clone(),
            status: match &result {
                Ok(Ok(_)) => "restored",
                Ok(Err(_)) => "refused",
                Err(_) => "error",
            }
            .into(),
            reason: match &result {
                Ok(Ok(_)) => "restored_by_revision_cas".into(),
                Ok(Err(reason)) => (*reason).into(),
                Err(error) => format!("restore_error:{error}"),
            },
            archived_revision: rev,
            restored_revision: result.ok().and_then(Result::ok),
        })
    }
    let mut out = RestoreReceipt {
        schema: RESTORE_SCHEMA,
        db_identity: receipt.db_identity,
        source_receipt_hash: receipt.receipt_hash,
        restored_count: rows.iter().filter(|r| r.status == "restored").count(),
        refusal_count: rows.iter().filter(|r| r.status == "refused").count(),
        error_count: rows.iter().filter(|r| r.status == "error").count(),
        rows,
        receipt_hash: String::new(),
    };
    out.receipt_hash = hash(&out)?;
    Ok(out)
}

pub(super) fn read_plan(path: &Path) -> Result<CaptureArchivePlan, String> {
    serde_json::from_slice(&fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?)
        .map_err(|e| e.to_string())
}
pub(super) fn read_receipt(path: &Path) -> Result<ApplyReceipt, String> {
    serde_json::from_slice(&fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?)
        .map_err(|e| e.to_string())
}
pub(super) fn write_owner_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    crate::utils::write_owner_only_file_atomic(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;
    use tempfile::TempDir;

    const AS_OF_TEXT: &str = "2026-07-20T00:00:00Z";

    fn as_of() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 20, 0, 0, 0)
            .single()
            .expect("valid fixture time")
    }

    fn capture(id: &str) -> MemoryEntry {
        let memory_id = format!("capture-session:{id}");
        let replay_key = format!("capture-replay:{id}");
        let source_revision = format!("revision-{id}");
        let source_event_id = format!("event-{id}");
        MemoryEntry {
            id: memory_id,
            path: format!("/capture/{id}"),
            summary: id.into(),
            text: format!("captured {id}"),
            importance: 0.5,
            timestamp: "2026-05-01T00:00:00Z".into(),
            valid_from: "2026-05-01T00:00:00Z".into(),
            valid_until: Some("2026-05-31T00:00:00Z".into()),
            category: "experience".into(),
            topic: "capture".into(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            source: "capture_session".into(),
            scope: "project".into(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            vector: None,
            retention_policy: Some("ephemeral".into()),
            domain: None,
            metadata: json!({
                "artifact_kind": "session_capture",
                "capture_replay_key": replay_key,
                "capture_replay_keys": [replay_key],
                "source_revision": source_revision,
                "source_revisions": [source_revision],
                "source_event_id": source_event_id,
                "source_refs": [{
                    "ref_type": "turn",
                    "ref_id": source_event_id,
                    "revision": source_revision
                }],
                "provenance": {
                    "source_kind": "session_capture",
                    "tool_name": "capture_session"
                },
                "capture_retention": {
                    "class": "ephemeral",
                    "policy_version": CAPTURE_RETENTION_POLICY_VERSION,
                    "ttl_days": CAPTURE_EPHEMERAL_TTL_DAYS
                }
            }),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".into(),
        }
    }

    fn approved_evidence(id: &str, referenced_id: &str) -> MemoryEntry {
        let mut entry = capture(id);
        entry.source = "manual".into();
        entry.retention_policy = Some("permanent".into());
        entry.valid_until = None;
        entry.metadata = json!({
            "lifecycle": "active",
            "review_receipt": {
                "decision": "approved",
                "approver": "reviewer",
                "decided_at": AS_OF_TEXT
            },
            "evidence_refs_v1": [{ "ref": referenced_id }]
        });
        entry
    }

    fn database(entries: &[MemoryEntry]) -> (TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp database directory");
        let path = dir.path().join("memory.db");
        let mut store = MemoryStore::open(path.to_str().expect("UTF-8 path")).expect("open store");
        for entry in entries {
            store.upsert(entry).expect("seed entry");
        }
        drop(store);
        (dir, path)
    }

    #[test]
    fn archive_implementation_has_no_delete_sql() {
        let source = include_str!("capture_archive.rs").to_ascii_uppercase();
        assert!(!source.contains(&["DELETE", " FROM MEMORIES"].concat()));
        let crud = include_str!("../../../../memcore/src/db/memory_crud.rs").to_ascii_uppercase();
        let archive = crud
            .split("PUB FN ARCHIVE_MEMORY_IF_REVISION")
            .nth(1)
            .expect("archive CAS function")
            .split("PUB FN RESTORE_ARCHIVED_IF_REVISION")
            .next()
            .expect("archive CAS body");
        let restore = crud
            .split("PUB FN RESTORE_ARCHIVED_IF_REVISION")
            .nth(1)
            .expect("restore CAS function")
            .split("/// MARK A MEMORY AS SUPERSEDED")
            .next()
            .expect("restore CAS body");
        assert!(archive.contains("UPDATE MEMORIES SET ARCHIVED = 1"));
        assert!(restore.contains("UPDATE MEMORIES SET ARCHIVED = 0"));
        assert!(!archive.contains("DELETE FROM"));
        assert!(!restore.contains("DELETE FROM"));
    }

    #[test]
    fn plan_is_read_only_and_accounts_for_exact_policy_and_protected_rows() {
        let eligible = capture("eligible");
        let mut fresh = capture("fresh");
        fresh.timestamp = "2026-07-02T00:00:00Z".into();
        fresh.valid_from = fresh.timestamp.clone();
        fresh.valid_until = Some("2026-08-01T00:00:00Z".into());
        let mut malformed = capture("malformed");
        malformed.metadata["capture_retention"]["policy_version"] = json!(42);
        let mut soul = capture("soul");
        soul.path = "/soul/working-experience".into();
        let mut pattern = capture("pattern");
        pattern.tier = "pattern".into();
        let mut wrong_expiry = capture("wrong-expiry");
        wrong_expiry.valid_until = Some("2026-05-02T00:00:00Z".into());
        let evidence = approved_evidence("review", "capture-session:eligible");
        let (_dir, path) = database(&[
            eligible,
            fresh,
            malformed,
            soul,
            pattern,
            wrong_expiry,
            evidence,
        ]);
        let before = fs::read(&path).expect("read DB before plan");

        let plan = build_plan(&path, as_of()).expect("build plan");

        assert_eq!(fs::read(&path).expect("read DB after plan"), before);
        assert_eq!(plan.counts.eligible, 0, "{plan:#?}");
        assert_eq!(plan.counts.retained, 1, "{plan:#?}");
        assert_eq!(plan.counts.refused, 5);
        assert_eq!(
            plan.rows.len(),
            6,
            "unrelated reviewed evidence is not a capture row"
        );
        let row = |id: &str| {
            let id = format!("capture-session:{id}");
            plan.rows.iter().find(|row| row.memory_id == id).unwrap()
        };
        assert_eq!(row("fresh").reason, "not_expired");
        assert_eq!(row("malformed").reason, "capture_policy_version_mismatch");
        assert_eq!(row("malformed").policy_version, "malformed");
        assert_eq!(row("soul").reason, "protected_active_truth_shape");
        assert_eq!(row("pattern").reason, "protected_active_truth_shape");
        assert_eq!(
            row("wrong-expiry").reason,
            "capture_expiry_interval_mismatch"
        );
        assert_eq!(row("eligible").reason, "required_active_evidence");
    }

    #[test]
    fn apply_and_restore_are_revision_bound_and_reversible() {
        let (_dir, path) = database(&[capture("candidate")]);
        let plan = build_plan(&path, as_of()).expect("build plan");
        assert_eq!(plan.counts.eligible, 1, "{plan:#?}");

        let receipt = apply(&path, plan, true).expect("apply plan");
        assert_eq!(receipt.archive_count, 1);
        assert_eq!(receipt.survivor_count, 0);
        let store = MemoryStore::open_read_only(path.to_str().unwrap()).expect("read archived row");
        let archived = store
            .get_with_options("capture-session:candidate", true)
            .expect("fetch")
            .expect("candidate exists");
        assert!(archived.archived);
        assert_eq!(
            archived.revision,
            receipt.rows[0].archived_revision.unwrap()
        );
        drop(store);

        let restored = restore(&path, receipt.clone(), true).expect("restore archive");
        assert_eq!(restored.restored_count, 1);
        let store = MemoryStore::open_read_only(path.to_str().unwrap()).expect("read restored row");
        assert!(
            !store
                .get("capture-session:candidate")
                .expect("fetch")
                .unwrap()
                .archived
        );
        drop(store);

        let stale = restore(&path, receipt, true).expect("repeat restore is accounted refusal");
        assert_eq!(stale.restored_count, 0);
        assert_eq!(stale.refusal_count, 1);
        assert_eq!(stale.rows[0].reason, "restore_target_or_lineage_mismatch");
    }

    #[test]
    fn apply_refuses_confirmation_tampering_wrong_database_and_stale_state() {
        let (_dir, path) = database(&[capture("candidate")]);
        let plan = build_plan(&path, as_of()).expect("build plan");
        assert!(apply(&path, plan.clone(), false)
            .unwrap_err()
            .contains("confirmation required"));

        let mut tampered = plan.clone();
        tampered.rows[0].memory_revision += 1;
        assert!(apply(&path, tampered, true)
            .unwrap_err()
            .contains("invalid plan schema or hash"));

        let (_other_dir, other) = database(&[capture("candidate")]);
        assert!(apply(&other, plan.clone(), true)
            .unwrap_err()
            .contains("DB identity mismatch"));

        let mut store = MemoryStore::open(path.to_str().unwrap()).expect("open writer");
        let mut changed = store
            .get("capture-session:candidate")
            .expect("fetch")
            .unwrap();
        changed.text.push_str(" changed");
        store.upsert(&changed).expect("change candidate revision");
        drop(store);
        assert!(apply(&path, plan, true).unwrap_err().contains("stale plan"));
        let store = MemoryStore::open_read_only(path.to_str().unwrap()).expect("read candidate");
        assert!(
            !store
                .get("capture-session:candidate")
                .expect("fetch")
                .unwrap()
                .archived
        );
    }

    #[test]
    fn apply_time_transaction_rechecks_new_reviewed_evidence() {
        let (_dir, path) = database(&[capture("candidate")]);
        let mut store = MemoryStore::open(path.to_str().unwrap()).expect("open writer");
        store
            .upsert(&approved_evidence("review", "capture-session:candidate"))
            .expect("insert protection evidence");

        let revision = store
            .get("capture-session:candidate")
            .expect("fetch")
            .unwrap()
            .revision;
        let outcome =
            archive_one_transactionally(&store, "capture-session:candidate", revision, as_of())
                .expect("transaction completes");
        assert_eq!(outcome, Err("apply_time_protection_or_policy_changed"));
        let candidate = store
            .get("capture-session:candidate")
            .expect("fetch")
            .unwrap();
        assert!(!candidate.archived);
    }

    #[test]
    fn apply_transaction_refuses_a_revision_other_than_the_planned_revision() {
        let (_dir, path) = database(&[capture("candidate")]);
        let store = MemoryStore::open(path.to_str().unwrap()).expect("open writer");
        let candidate = store
            .get("capture-session:candidate")
            .expect("fetch")
            .unwrap();

        let outcome =
            archive_one_transactionally(&store, &candidate.id, candidate.revision + 1, as_of())
                .expect("transaction completes");

        assert_eq!(outcome, Err("planned_revision_changed"));
        assert!(!store.get(&candidate.id).expect("fetch").unwrap().archived);
    }

    #[test]
    fn restore_rejects_tampered_and_duplicate_receipts_before_writing() {
        let (_dir, path) = database(&[capture("candidate")]);
        let receipt = apply(&path, build_plan(&path, as_of()).unwrap(), true).unwrap();

        let mut tampered = receipt.clone();
        tampered.rows[0].archived_revision = Some(999);
        assert!(restore(&path, tampered, true)
            .unwrap_err()
            .contains("invalid apply receipt schema or hash"));

        let mut duplicate = receipt.clone();
        duplicate.rows.push(duplicate.rows[0].clone());
        duplicate.archive_count += 1;
        duplicate.receipt_hash.clear();
        duplicate.receipt_hash = hash(&duplicate).unwrap();
        assert!(restore(&path, duplicate, true)
            .unwrap_err()
            .contains("duplicate memory id"));

        let mut unknown = receipt.clone();
        unknown.rows[0].status = "mystery".into();
        unknown.rows[0].archived_revision = None;
        unknown.archive_count = 0;
        unknown.survivor_count = 1;
        unknown.receipt_hash.clear();
        unknown.receipt_hash = hash(&unknown).unwrap();
        assert!(restore(&path, unknown, true)
            .unwrap_err()
            .contains("row accounting or archive revision mismatch"));

        let store = MemoryStore::open_read_only(path.to_str().unwrap()).expect("read candidate");
        assert!(
            store
                .get_with_options("capture-session:candidate", true)
                .expect("fetch")
                .unwrap()
                .archived
        );
    }

    #[test]
    fn rehashed_receipt_cannot_restore_an_unrelated_archived_capture_row() {
        let (_dir, path) = database(&[capture("candidate")]);
        let receipt = apply(&path, build_plan(&path, as_of()).unwrap(), true).unwrap();
        let mut unrelated = capture("durable");
        unrelated.retention_policy = Some("durable".into());
        unrelated.metadata["artifact_kind"] = json!("bracket_self_evolution");
        unrelated.metadata["capture_retention"] = json!({
            "class": "durable",
            "policy_version": CAPTURE_RETENTION_POLICY_VERSION,
            "ttl_days": null
        });
        let mut store = MemoryStore::open(path.to_str().unwrap()).expect("open writer");
        store.upsert(&unrelated).expect("seed unrelated capture");
        assert!(store
            .archive_memory(&unrelated.id)
            .expect("archive unrelated capture"));
        let archived = store
            .get_with_options(&unrelated.id, true)
            .expect("fetch")
            .unwrap();
        drop(store);

        let mut forged = receipt;
        forged.rows[0].memory_id = archived.id.clone();
        forged.rows[0].path = archived.path.clone();
        forged.rows[0].source_revision = field(&archived, "source_revision").map(str::to_owned);
        forged.rows[0].source_key = field(&archived, "capture_replay_key").map(str::to_owned);
        forged.rows[0].before_revision = archived.revision - 1;
        forged.rows[0].archived_revision = Some(archived.revision);
        forged.receipt_hash.clear();
        forged.receipt_hash = hash(&forged).unwrap();

        let rollback = restore(&path, forged, true).expect("forgery is accounted as refusal");
        assert_eq!(rollback.restored_count, 0);
        assert_eq!(rollback.refusal_count, 1);
        assert_eq!(
            rollback.rows[0].reason,
            "restore_target_or_lineage_mismatch"
        );
        let store = MemoryStore::open_read_only(path.to_str().unwrap()).expect("read target");
        let still_archived = store
            .get_with_options(&archived.id, true)
            .expect("fetch")
            .unwrap();
        assert!(still_archived.archived);
        assert_eq!(still_archived.revision, archived.revision);
    }
}
