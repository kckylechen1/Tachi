//! Read-only discovery and shared Kanban maintenance eligibility.
//!
//! A prefix discovers rows; it never confers deletion authority. Automatic
//! maintenance also requires a terminal lifecycle, an elapsed terminal retention
//! interval, no pin and a complete local reference scan.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

use super::memory_crud::refuse_reserved_rem_operation_mutation;
use super::operator_maintenance::encode_value;
use super::StoreProfile;
use crate::error::MemoryError;

pub(crate) const KANBAN_PATH_PREFIX: &str = "/kanban/";
const KANBAN_DISPATCH_PATH_PREFIX: &str = "/kanban/tasks/";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceDisposition {
    Eligible,
    Protected,
    ManualReview,
}

/// Identity of a protecting record. No memory or evidence body is exposed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MaintenanceReference {
    pub table: String,
    pub row_id: String,
    pub field: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceScanIssue {
    pub source: MaintenanceReference,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceCandidate {
    pub id: String,
    pub path: String,
    pub category: String,
    pub retention_policy: Option<String>,
    pub terminal_at: Option<String>,
    /// SHA-256 of every persisted memories column, with typed framing.
    pub row_hash: String,
    pub disposition: MaintenanceDisposition,
    pub reasons: Vec<String>,
    pub protecting_references: Vec<MaintenanceReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceCandidateReport {
    pub prefix: String,
    pub as_of: String,
    pub reference_scan_complete: bool,
    pub scanned_reference_fields: Vec<String>,
    pub reference_scan_issues: Vec<MaintenanceScanIssue>,
    pub reference_scope: String,
    pub candidates: Vec<MaintenanceCandidate>,
}

/// Terminal states are deliberately closed. Unknown/active states never age out.
pub fn is_terminal_kanban_dispatch(state: &str, closure_kind: Option<&str>) -> bool {
    matches!(
        state,
        "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED"
    ) || (state == "TASK_STATE_INPUT_REQUIRED" && closure_kind == Some("partial"))
}

fn lifecycle_reason(
    category: &str,
    path: &str,
    metadata: &Value,
    as_of: &str,
    max_age_days: u64,
) -> Result<Option<&'static str>, MemoryError> {
    let as_of = chrono::DateTime::parse_from_rfc3339(as_of)
        .map_err(|error| MemoryError::InvalidArg(format!("invalid GC as_of: {error}")))?;
    if !metadata.is_object() {
        return Ok(Some("invalid_lifecycle_metadata"));
    }
    let terminal = if category == "fact" && path.starts_with(KANBAN_DISPATCH_PATH_PREFIX) {
        metadata
            .get("a2a_state")
            .and_then(Value::as_str)
            .is_some_and(|state| {
                is_terminal_kanban_dispatch(
                    state,
                    metadata.get("closure_kind").and_then(Value::as_str),
                )
            })
    } else if category == "kanban" && path.starts_with(KANBAN_PATH_PREFIX) {
        metadata
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| {
                matches!(
                    status.trim().to_ascii_lowercase().as_str(),
                    "resolved" | "expired"
                )
            })
    } else {
        return Ok(Some("no_automatic_lifecycle"));
    };
    if !terminal {
        return Ok(Some("active_or_unknown_lifecycle"));
    }
    let Some(terminal_at) = metadata.get("terminal_at").and_then(Value::as_str) else {
        return Ok(Some("unknown_terminal_time"));
    };
    let Ok(terminal_at) = chrono::DateTime::parse_from_rfc3339(terminal_at) else {
        return Ok(Some("invalid_terminal_time"));
    };
    if terminal_at > as_of {
        return Ok(Some("future_terminal_time"));
    }
    let days = i64::try_from(max_age_days)
        .ok()
        .and_then(chrono::Duration::try_days)
        .ok_or_else(|| {
            MemoryError::InvalidArg("Kanban GC retention interval is out of range".to_string())
        })?;
    let cutoff = as_of.checked_sub_signed(days).ok_or_else(|| {
        MemoryError::InvalidArg("Kanban GC retention cutoff is out of range".to_string())
    })?;
    Ok((terminal_at >= cutoff).then_some("terminal_retention_not_elapsed"))
}

/// Lifecycle-only compatibility predicate. Real GC must use the full classifier
/// below so retention and protecting references are checked in the same snapshot.
pub fn is_kanban_gc_candidate(
    category: &str,
    path: &str,
    metadata_json: &str,
    _creation_timestamp: &str,
    as_of: &str,
    max_age_days: u64,
) -> Result<bool, MemoryError> {
    let metadata: Value = serde_json::from_str(metadata_json).map_err(|error| {
        MemoryError::InvalidArg(format!("parse kanban metadata failed: {error}"))
    })?;
    Ok(lifecycle_reason(category, path, &metadata, as_of, max_age_days)?.is_none())
}

#[derive(Default)]
struct ReferenceScan {
    tokens: BTreeMap<String, BTreeSet<String>>,
    references: BTreeMap<String, BTreeSet<MaintenanceReference>>,
    issues: Vec<MaintenanceScanIssue>,
    fields: Vec<String>,
}

impl ReferenceScan {
    fn new(candidates: &[MaintenanceCandidate]) -> Self {
        let mut scan = Self::default();
        for row in candidates {
            for token in [&row.id, &row.path, &format!("memory:{}", row.id)] {
                scan.tokens
                    .entry(token.clone())
                    .or_default()
                    .insert(row.id.clone());
            }
        }
        scan
    }

    fn text(&mut self, text: &str, source: &MaintenanceReference) {
        // Exact values, structured map keys and punctuation-delimited references
        // are recognized. Text bodies, external stores and files are not scanned.
        let values = std::iter::once(text).chain(text.split(|ch: char| {
            !(ch.is_alphanumeric() || matches!(ch, '_' | '-' | '/' | ':' | '.' | '@'))
        }));
        for value in values {
            for token in [value, value.trim_end_matches('.')] {
                if let Some(targets) = self.tokens.get(token) {
                    for id in targets {
                        if source.table == "memories"
                            && source.field == "metadata"
                            && source.row_id == *id
                        {
                            continue;
                        }
                        self.references
                            .entry(id.clone())
                            .or_default()
                            .insert(source.clone());
                    }
                }
            }
        }
    }

    fn json(&mut self, text: &str, source: &MaintenanceReference) {
        match serde_json::from_str::<Value>(text) {
            Ok(value) => {
                let mut pending = vec![&value];
                while let Some(value) = pending.pop() {
                    match value {
                        Value::String(text) => self.text(text, source),
                        Value::Array(values) => pending.extend(values),
                        Value::Object(values) => {
                            for (key, value) in values {
                                self.text(key, source);
                                pending.push(value);
                            }
                        }
                        Value::Number(number) => self.text(&number.to_string(), source),
                        _ => {}
                    }
                }
            }
            Err(_) => self.issues.push(MaintenanceScanIssue {
                source: source.clone(),
                reason: "malformed_or_unsupported_json".to_string(),
            }),
        }
    }

    /// Only closed, source-defined table/column names reach this helper.
    fn table(
        &mut self,
        conn: &Connection,
        table: &'static str,
        identity: &'static str,
        fields: &[(&'static str, bool)],
    ) -> Result<(), MemoryError> {
        let columns = fields
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(",");
        let mut stmt = conn.prepare(&format!(
            "SELECT {identity},{columns} FROM {table} ORDER BY {identity}"
        ))?;
        let mut rows = stmt.query([])?;
        self.fields
            .extend(fields.iter().map(|(field, _)| format!("{table}.{field}")));
        while let Some(row) = rows.next()? {
            let row_id: String = row.get(0)?;
            for (index, (field, json)) in fields.iter().enumerate() {
                let Some(text) = row.get::<_, Option<String>>(index + 1)? else {
                    continue;
                };
                let source = MaintenanceReference {
                    table: table.to_string(),
                    row_id: row_id.clone(),
                    field: (*field).to_string(),
                };
                if *json {
                    self.json(&text, &source);
                } else {
                    self.text(&text, &source);
                }
            }
        }
        Ok(())
    }

    fn collect(&mut self, conn: &Connection, profile: StoreProfile) -> Result<(), MemoryError> {
        self.table(
            conn,
            "memories",
            "id",
            &[("superseded_by", false), ("metadata", true)],
        )?;
        self.table(
            conn,
            "memory_edges",
            "json_array(source_id,target_id,relation)",
            &[
                ("source_id", false),
                ("target_id", false),
                ("metadata", true),
            ],
        )?;
        self.table(
            conn,
            "tachi_events",
            "id",
            &[
                ("payload_json", true),
                ("provenance_json", true),
                ("effects", true),
                ("projection_hints", true),
            ],
        )?;
        self.table(
            conn,
            "hard_state",
            "json_array(namespace,key)",
            &[("value_json", true)],
        )?;
        if profile.includes_product() {
            self.table(
                conn,
                "dispatch_outcomes",
                "outcome_id",
                &[
                    ("eval_memory_id", false),
                    ("evidence_refs", true),
                    ("identity_receipt", true),
                ],
            )?;
            self.table(
                conn,
                "dispatch_adjudications",
                "adjudication_id",
                &[("evidence_ref", false)],
            )?;
            self.table(
                conn,
                "dispatch_adjudication_signatures",
                "json_array(adjudication_id,signature_id)",
                &[("evidence_ref", false)],
            )?;
            self.table(
                conn,
                "mirror_eval_adjudications",
                "adjudication_id",
                &[("evidence_ref", false)],
            )?;
            self.table(
                conn,
                "mirror_eval_runs",
                "eval_run_id",
                &[("frozen_contract_ref", false)],
            )?;
            self.table(
                conn,
                "mirror_eval_observations",
                "observation_id",
                &[("result_ref", false), ("artifacts", true)],
            )?;
            self.table(
                conn,
                "foundry_jobs",
                "id",
                &[("memory_ids", true), ("metadata", true)],
            )?;
        }
        Ok(())
    }
}

/// Inspect every row under a literal prefix in one caller-owned snapshot.
/// Unknown/malformed reference evidence protects every discovered row.
pub(crate) fn maintenance_candidates(
    conn: &Connection,
    profile: StoreProfile,
    prefix: &str,
    as_of: &str,
    max_age_days: u64,
) -> Result<MaintenanceCandidateReport, MemoryError> {
    if !prefix.starts_with('/') {
        return Err(MemoryError::InvalidArg(
            "maintenance discovery requires an absolute literal path prefix".to_string(),
        ));
    }
    chrono::DateTime::parse_from_rfc3339(as_of)
        .map_err(|error| MemoryError::InvalidArg(format!("invalid GC as_of: {error}")))?;
    let mut stmt =
        conn.prepare("SELECT * FROM memories WHERE substr(path,1,length(?1))=?1 ORDER BY id")?;
    let column_count = stmt.column_count();
    let mut rows = stmt.query([prefix])?;
    let mut candidates = Vec::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get("id")?;
        let path: String = row.get("path")?;
        let category: String = row.get("category")?;
        let retention_policy: Option<String> = row.get("retention_policy")?;
        let source: String = row.get("source")?;
        let metadata: String = row.get("metadata")?;
        let mut encoded = Vec::new();
        for index in 0..column_count {
            encode_value(row.get_ref(index)?, &mut encoded);
        }
        let mut reasons = Vec::new();
        match retention_policy
            .as_deref()
            .or_else(|| crate::types::default_retention_for(&path, &source))
        {
            Some("pinned" | "permanent") => reasons.push("retention_protected".to_string()),
            None | Some("ephemeral" | "durable") => {}
            Some(_) => reasons.push("unknown_retention_policy".to_string()),
        }
        let parsed = serde_json::from_str::<Value>(&metadata).ok();
        let terminal_at = parsed
            .as_ref()
            .and_then(|m| m.get("terminal_at"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let lifecycle = match parsed {
            Some(metadata) => lifecycle_reason(&category, &path, &metadata, as_of, max_age_days)?,
            None => Some("invalid_lifecycle_metadata"),
        };
        if let Some(reason) = lifecycle {
            reasons.push(reason.to_string());
        }
        let guard =
            refuse_reserved_rem_operation_mutation(&id, "deleted by Kanban GC").and_then(|()| {
                super::refuse_retired_sticky_row_within_tx(conn, &id, "deleted by Kanban GC")
            });
        match guard {
            Ok(()) => {}
            Err(MemoryError::InvalidArg(_)) => {
                reasons.push("reserved_memory_authority".to_string())
            }
            Err(error) => return Err(error),
        }
        let disposition = if reasons.is_empty() {
            MaintenanceDisposition::Eligible
        } else if reasons
            .iter()
            .all(|reason| reason == "no_automatic_lifecycle")
        {
            MaintenanceDisposition::ManualReview
        } else {
            MaintenanceDisposition::Protected
        };
        candidates.push(MaintenanceCandidate {
            id,
            path,
            category,
            retention_policy,
            terminal_at,
            row_hash: format!("{:x}", Sha256::digest(encoded)),
            disposition,
            reasons,
            protecting_references: Vec::new(),
        });
    }
    let mut scan = ReferenceScan::new(&candidates);
    if !candidates.is_empty() {
        scan.collect(conn, profile)?;
    }
    for row in &mut candidates {
        row.protecting_references = scan
            .references
            .remove(&row.id)
            .unwrap_or_default()
            .into_iter()
            .collect();
        if !row.protecting_references.is_empty() {
            row.reasons.push("referenced".to_string());
            row.disposition = MaintenanceDisposition::Protected;
        }
        if !scan.issues.is_empty() {
            row.reasons.push("reference_scan_incomplete".to_string());
            row.disposition = MaintenanceDisposition::Protected;
        }
        if row.disposition == MaintenanceDisposition::Eligible {
            row.reasons.push("terminal_retention_elapsed".to_string());
        }
    }
    Ok(MaintenanceCandidateReport {
        prefix: prefix.to_string(),
        as_of: as_of.to_string(),
        reference_scan_complete: scan.issues.is_empty(),
        scanned_reference_fields: scan.fields,
        reference_scan_issues: scan.issues,
        reference_scope: "one physical store; complete listed JSON fields and exact/punctuation-delimited ID, memory:ID and path references; no memory text bodies, files or external stores".to_string(),
        candidates,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPrefixMemoryRow {
    pub id: String,
    pub path: String,
    pub category: String,
    pub timestamp: String,
    pub metadata: String,
}

/// List `memories` rows whose `path` matches a SQL `LIKE` pattern (caller
/// supplies the full pattern, e.g. `"{prefix}%"`).
///
/// tachi#1459: this route records nothing. The access counters observe the
/// search path only — `record_access_with_updates`, from `hybrid_search`, is
/// their sole production observation path. `gc_tables` can reconcile
/// `query_diversity` from search-written history but adds no non-search use, so
/// a sweep over this function neither records an observation nor may treat zero
/// values as evidence that the rows it found are unused.
pub fn list_memories_by_path_prefix(
    conn: &Connection,
    path_like_pattern: &str,
) -> Result<Vec<PathPrefixMemoryRow>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id, path, category, timestamp, metadata
         FROM memories
         WHERE path LIKE ?1",
    )?;
    let rows = stmt.query_map([path_like_pattern], |row| {
        Ok(PathPrefixMemoryRow {
            id: row.get(0)?,
            path: row.get(1)?,
            category: row.get(2)?,
            timestamp: row.get(3)?,
            metadata: row.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryPathPrefixMemoryRow {
    pub id: String,
    pub timestamp: String,
    pub metadata: String,
    pub archived: bool,
}

/// List `memories` rows matching an exact `category` and a `path` `LIKE`
/// pattern (caller supplies the full pattern, e.g. `"{prefix}%"`).
pub fn list_memories_by_category_and_path_prefix(
    conn: &Connection,
    category: &str,
    path_like_pattern: &str,
) -> Result<Vec<CategoryPathPrefixMemoryRow>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, metadata, archived
         FROM memories
         WHERE category = ?1 AND path LIKE ?2",
    )?;
    let rows = stmt.query_map((category, path_like_pattern), |row| {
        Ok(CategoryPathPrefixMemoryRow {
            id: row.get(0)?,
            timestamp: row.get(1)?,
            metadata: row.get(2)?,
            archived: row.get(3)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}
