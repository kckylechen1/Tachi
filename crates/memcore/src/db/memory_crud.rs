use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{Map, Value};
use std::sync::{Mutex, MutexGuard};

use crate::error::MemoryError;
use crate::types::{default_retention_for, MemoryCategory, MemoryEntry, MemoryScope, MemorySource};

use super::common::{normalize_utc_iso, now_utc_iso, row_to_entry};
use super::sqlite_vec::serialize_f32;

mod access;
mod read;
mod search;
mod update;

pub use access::get_access_times;
pub(crate) use access::record_access_with_updates;
#[cfg(test)]
pub(crate) use access::{record_access, AccessUpdate};
pub use read::{
    fetch_by_ids, find_active_wiki_entry_by_path_or_topic, find_exact_path_text_id, get_all,
    list_by_path, list_by_path_recent, list_wiki_duplicate_candidates,
};
pub(crate) use search::search_fts_raw_match;
pub(crate) use search::search_symbolic_candidates_with_relevance;
pub use search::{
    search_fts, search_symbolic_candidates, search_vec, symbolic_trigram_select_sql,
    SYMBOLIC_TRIGRAM_SELECT_SQL_TEMPLATE,
};
pub use update::{
    record_enrichment_failure, release_event_claim, set_keyword_enrichment_pending_if_unset,
    set_keyword_enrichment_status, try_claim_event, update_enrichment_fields, update_with_revision,
};

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Token-based Jaccard similarity between two texts.
/// Uses the same tokeniser as [`crate::scorer::tokenize`].
fn jaccard_similarity(a: &str, b: &str) -> f64 {
    use std::collections::HashSet;
    let ta: HashSet<String> = crate::scorer::tokenize(a).into_iter().collect();
    let tb: HashSet<String> = crate::scorer::tokenize(b).into_iter().collect();
    if ta.is_empty() && tb.is_empty() {
        return 0.0;
    }
    let intersection = ta.intersection(&tb).count() as f64;
    let union = ta.union(&tb).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

fn simple_query_input(query: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;

    for ch in query.chars() {
        if ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(ch);
        } else if ch.is_whitespace() || !out.is_empty() {
            pending_space = true;
        }
    }

    out
}

pub(crate) const MEMORY_SELECT_COLUMNS: &str = "id,path,summary,text,importance,timestamp,valid_from,valid_until,category,topic,keywords,'[]' AS persons,entities,'' AS location,source,scope,archived,access_count,last_access,revision,metadata,retention_policy,domain,recall_count,query_diversity,tier";
pub(crate) const MEMORY_SELECT_COLUMNS_QUALIFIED: &str = "m.id,m.path,m.summary,m.text,m.importance,m.timestamp,m.valid_from,m.valid_until,m.category,m.topic,m.keywords,'[]' AS persons,m.entities,'' AS location,m.source,m.scope,m.archived,m.access_count,m.last_access,m.revision,m.metadata,m.retention_policy,m.domain,m.recall_count,m.query_diversity,m.tier";

static FTS_SYNC_LOCK: Mutex<()> = Mutex::new(());

fn acquire_fts_sync_lock() -> MutexGuard<'static, ()> {
    FTS_SYNC_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn sync_memories_fts(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    path: &str,
    summary: &str,
    text: &str,
    keywords_joined: &str,
    entities_joined: &str,
) -> Result<(), MemoryError> {
    // The wangfenjin/simple tokenizer has process-global Pinyin state. Keep
    // in-process FTS syncs single-file so parallel test/server writes do not
    // stampede that tokenizer while preserving the same SQL effects.
    let _fts_guard = acquire_fts_sync_lock();
    tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![id])?;
    tx.execute(
        "INSERT INTO memories_fts(id, path, summary, text, keywords, entities)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![id, path, summary, text, keywords_joined, entities_joined],
    )?;
    // Symbolic trigram index stores raw memories column bytes (JSON tags
    // included) so LIKE eligibility matches a table scan (#1331).
    sync_memories_symbolic_fts(tx, id)?;
    Ok(())
}

/// Refresh one row in `memories_symbolic_fts` from the live `memories` row.
/// No-op when the virtual table is absent (legacy fixtures / mid-migration).
///
/// Public so repair writers (`tachi repair` quarantine restore/purge/junk)
/// share the same tx-scoped projection as normal CRUD (#1335 oracle).
pub fn sync_memories_symbolic_fts(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<(), MemoryError> {
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memories_symbolic_fts'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !exists {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM memories_symbolic_fts WHERE id = ?1",
        params![id],
    )?;
    conn.execute(
        r#"INSERT INTO memories_symbolic_fts(id, path, summary, text, keywords, entities, topic)
           SELECT id, path, summary, text, keywords, entities, topic
           FROM memories WHERE id = ?1"#,
        params![id],
    )?;
    Ok(())
}

/// Drop one row from `memories_symbolic_fts`. No-op when the table is absent.
pub fn delete_memories_symbolic_fts(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<(), MemoryError> {
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memories_symbolic_fts'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !exists {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM memories_symbolic_fts WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// Write-time near-duplicate dedup (kckylechen1/tachi#1115, restored by
/// #1167 review round 2 — see `upsert_with_idless_identity` call sites for
/// why this must run *after* the exact-identity unique index has already
/// decided the atomic-duplicate question for id-less saves).
///
/// Searches FTS for candidates sharing the entry's leading tokens, and for
/// the first one whose token-Jaccard similarity to `entry.text` exceeds
/// 0.9, folds `entry`'s keywords/entities into the candidate (bumping its
/// importance to the max of the two) and resyncs the candidate's FTS row.
/// Returns the candidate's id when a merge happened, `None` when `entry` is
/// genuinely novel (or the FTS query was empty, e.g. all-punctuation text).
///
/// Callers are responsible for turning `entry` itself into the losing,
/// `superseded_by`-pointing side of the merge — this only decides *whether*
/// to merge and updates the winner.
fn merge_into_jaccard_candidate(
    tx: &rusqlite::Transaction<'_>,
    entry: &MemoryEntry,
    importance: f64,
    write_time_utc: &str,
) -> Result<Option<String>, MemoryError> {
    let safe_query = simple_query_input(
        &entry
            .text
            .split_whitespace()
            .take(12)
            .collect::<Vec<_>>()
            .join(" "),
    );
    if safe_query.is_empty() {
        return Ok(None);
    }
    let fts_candidates: Vec<(String, String)> = {
        // `AND m.id != ?2` is a necessary adaptation of this recovery path,
        // not a deviation from origin/main's inline dedup: this function now
        // runs *after* the atomic identity insert has already committed
        // `entry` as a row, so the candidate scan would otherwise find
        // `entry` matching itself (Jaccard 1.0) and "merge" it into itself.
        // origin/main's inline version runs the search *before* insertion,
        // when `entry` has no row yet, so it has no self-match to exclude.
        let mut stmt = tx.prepare(
            "SELECT m.id, m.text FROM memories_fts
             JOIN memories m ON m.id = memories_fts.id
             WHERE memories_fts MATCH simple_query(?1)
               AND m.archived = 0 AND m.superseded_by IS NULL
               AND m.id != ?2
             LIMIT 5",
        )?;
        let rows = stmt.query_map(params![safe_query, entry.id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<Result<_, _>>()?
    };
    for (cand_id, cand_text) in fts_candidates {
        if jaccard_similarity(&entry.text, &cand_text) > 0.9 {
            // Merge: update candidate with max importance and merged tags
            let merge_kws = {
                let cand_kws_json: String = tx
                    .query_row(
                        "SELECT keywords FROM memories WHERE id = ?1",
                        params![cand_id],
                        |r| r.get(0),
                    )
                    .unwrap_or_else(|_| "[]".to_string());
                let mut kws: Vec<String> = serde_json::from_str(&cand_kws_json).unwrap_or_default();
                for k in &entry.keywords {
                    if !kws.contains(k) {
                        kws.push(k.clone());
                    }
                }
                serde_json::to_string(&kws).unwrap_or_else(|_| "[]".to_string())
            };
            let merge_ents = {
                let cand_ents_json: String = tx
                    .query_row(
                        "SELECT entities FROM memories WHERE id = ?1",
                        params![cand_id],
                        |r| r.get(0),
                    )
                    .unwrap_or_else(|_| "[]".to_string());
                let mut ents: Vec<String> =
                    serde_json::from_str(&cand_ents_json).unwrap_or_default();
                for e in &entry.entities {
                    if !ents.contains(e) {
                        ents.push(e.clone());
                    }
                }
                crate::types::fold_person_names_into_entities(&mut ents, entry.persons.clone());
                serde_json::to_string(&ents).unwrap_or_else(|_| "[]".to_string())
            };
            tx.execute(
                "UPDATE memories SET keywords = ?1, entities = ?2,
                 importance = MAX(importance, ?3), updated_at = ?4
                 WHERE id = ?5",
                params![merge_kws, merge_ents, importance, write_time_utc, cand_id],
            )?;
            let (cand_path, cand_summary, cand_text) = tx.query_row(
                "SELECT path, summary, text FROM memories WHERE id = ?1",
                params![cand_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )?;
            let kws_joined: String = serde_json::from_str::<Vec<String>>(&merge_kws)
                .unwrap_or_default()
                .join(" ");
            let ents_joined: String = serde_json::from_str::<Vec<String>>(&merge_ents)
                .unwrap_or_default()
                .join(" ");
            sync_memories_fts(
                tx,
                &cand_id,
                &cand_path,
                &cand_summary,
                &cand_text,
                &kws_joined,
                &ents_joined,
            )?;
            return Ok(Some(cand_id));
        }
    }
    Ok(None)
}

// ─── Normalization ────────────────────────────────────────────────────────────

/// Coerce caller-provided enum-like fields to the canonical vocabulary
/// enforced by the CHECK constraints on `memories`. Idempotent.
///
/// - `source`: routed through [`MemorySource::parse_or_external`]; user-controlled
///   non-canonical values become `external:<sanitized>`.
/// - `category`: clamped to one of fact/decision/experience/preference/entity/other.
/// - `scope`: clamped to user/project/general (rejects `self`, `other_agent:*`).
/// - `retention_policy`: defaulted via [`default_retention_for`] if the caller
///   left it `None`.
pub fn normalize_for_write(entry: &mut MemoryEntry) {
    entry.path = crate::path_router::normalize_path(&entry.path);
    entry.source = MemorySource::parse_or_external(&entry.source);
    entry.category = MemoryCategory::normalize(&entry.category).to_string();
    entry.scope = MemoryScope::normalize(&entry.scope).to_string();
    entry.importance = entry.importance.clamp(0.0, 1.0);
    if entry.retention_policy.is_none() {
        if let Some(d) = default_retention_for(&entry.path, &entry.source) {
            entry.retention_policy = Some(d.to_string());
        }
    }
    entry.fold_persons_into_entities();
    entry.fold_location_into_metadata();
}

/// Serialize `entities` with legacy `persons` folded in.
fn canonical_entities_json(entry: &MemoryEntry) -> Result<String, MemoryError> {
    let mut entities = entry.entities.clone();
    crate::types::fold_person_names_into_entities(&mut entities, entry.persons.clone());
    Ok(serde_json::to_string(&entities)?)
}

// ─── UPSERT ───────────────────────────────────────────────────────────────────

/// Outcome of an id-less write protected by the modern identity constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdlessUpsertResult {
    Saved,
    Duplicate { id: String },
}

/// Shape-validated reserved-reference mutation accepted by the atomic merge.
///
/// Construction proves only that the value has a normalized supported wire
/// shape. It does not confer authority; callers must authorize the source at
/// their own boundary before selecting this mutation API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedReferenceMutation {
    operation: ReservedReferenceOperation,
}

pub const MAX_REFERENCE_BYTES: usize = 4_096;
pub const MAX_REFERENCE_ID_BYTES: usize = 1_024;
pub const MAX_REFERENCE_KIND_BYTES: usize = 64;
pub const MAX_REFERENCE_TIMESTAMP_BYTES: usize = 64;
pub const MAX_REFERENCE_HASH_BYTES: usize = 1_024;
pub const MAX_REFERENCE_SECTION_BYTES: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReservedReferenceOperation {
    Append {
        target: ReservedReferenceTarget,
        value: Value,
    },
    EnsureEmptyEvidenceRefsV1,
    TombstoneLegacySourceRefs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReservedReferenceTarget {
    EvidenceRefsV1,
    SourceRefs,
}

impl ReservedReferenceTarget {
    fn metadata_key(self) -> &'static str {
        match self {
            Self::EvidenceRefsV1 => "evidence_refs_v1",
            Self::SourceRefs => "source_refs",
        }
    }
}

impl ValidatedReferenceMutation {
    pub fn evidence(
        reference: String,
        captured_at: String,
        target_kind: Option<String>,
    ) -> Result<Self, MemoryError> {
        let reference = normalize_required_bounded(reference, "evidence ref", MAX_REFERENCE_BYTES)?;
        let captured_at = normalize_timestamp(captured_at, "evidence captured_at")?;
        let target_kind = target_kind
            .map(|kind| {
                normalize_required_lower_bounded(
                    kind,
                    "evidence target_kind",
                    MAX_REFERENCE_KIND_BYTES,
                )
            })
            .transpose()?;
        if let Some(kind) = target_kind.as_deref() {
            if !matches!(
                kind,
                "issue"
                    | "comment"
                    | "pr"
                    | "commit"
                    | "canonical_doc"
                    | "episodic_memory"
                    | "wiki"
                    | "guide"
                    | "precedent"
                    | "eval"
                    | "runtime"
            ) {
                return Err(MemoryError::InvalidArg(format!(
                    "unsupported evidence target_kind: {kind}"
                )));
            }
        }
        let mut object = Map::new();
        object.insert("ref".to_string(), Value::String(reference));
        object.insert("captured_at".to_string(), Value::String(captured_at));
        if let Some(kind) = target_kind {
            object.insert("target_kind".to_string(), Value::String(kind));
        }
        Ok(Self {
            operation: ReservedReferenceOperation::Append {
                target: ReservedReferenceTarget::EvidenceRefsV1,
                value: Value::Object(object),
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn precedent_source(
        relation: String,
        target_kind: String,
        target_ref: String,
        comment_id: Option<String>,
        updated_at: Option<String>,
        body_hash: Option<String>,
        commit_sha: Option<String>,
        section_or_span: Option<String>,
    ) -> Result<Self, MemoryError> {
        let relation = normalize_required_lower_bounded(
            relation,
            "source ref relation",
            MAX_REFERENCE_KIND_BYTES,
        )?;
        if !matches!(
            relation.as_str(),
            "derived_from" | "supports" | "contradicts" | "supersedes" | "applies_to"
        ) {
            return Err(MemoryError::InvalidArg(format!(
                "unsupported source ref relation: {relation}"
            )));
        }
        let target_kind = normalize_required_lower_bounded(
            target_kind,
            "source ref target_kind",
            MAX_REFERENCE_KIND_BYTES,
        )?;
        if !matches!(
            target_kind.as_str(),
            "issue" | "comment" | "pr" | "commit" | "canonical_doc" | "verification"
        ) {
            return Err(MemoryError::InvalidArg(format!(
                "unsupported source ref target_kind: {target_kind}"
            )));
        }
        let target_ref =
            normalize_required_bounded(target_ref, "source ref target_ref", MAX_REFERENCE_BYTES)?;
        let comment_id = normalize_optional_bounded(
            comment_id,
            "source ref comment_id",
            MAX_REFERENCE_ID_BYTES,
        )?;
        let updated_at = normalize_optional_timestamp(updated_at, "source ref updated_at")?;
        let body_hash = normalize_optional_bounded(
            body_hash,
            "source ref body_hash",
            MAX_REFERENCE_HASH_BYTES,
        )?;
        let commit_sha = normalize_optional_bounded(
            commit_sha,
            "source ref commit_sha",
            MAX_REFERENCE_HASH_BYTES,
        )?;
        let section_or_span = normalize_optional_bounded(
            section_or_span,
            "source ref section_or_span",
            MAX_REFERENCE_SECTION_BYTES,
        )?;
        if target_kind == "comment" && comment_id.is_none() {
            return Err(MemoryError::InvalidArg(
                "comment source ref requires comment_id".to_string(),
            ));
        }
        match target_kind.as_str() {
            "commit" if commit_sha.is_none() => {
                return Err(MemoryError::InvalidArg(
                    "commit source ref requires commit_sha".to_string(),
                ));
            }
            "comment" | "issue" | "pr" | "canonical_doc" | "verification"
                if body_hash.is_none() =>
            {
                return Err(MemoryError::InvalidArg(format!(
                    "{target_kind} source ref requires body_hash"
                )));
            }
            _ => {}
        }
        let mut object = Map::new();
        object.insert("relation".to_string(), Value::String(relation));
        object.insert("target_kind".to_string(), Value::String(target_kind));
        object.insert("target_ref".to_string(), Value::String(target_ref));
        for (key, value) in [
            ("comment_id", comment_id),
            ("updated_at", updated_at),
            ("body_hash", body_hash),
            ("commit_sha", commit_sha),
            ("section_or_span", section_or_span),
        ] {
            if let Some(value) = value {
                object.insert(key.to_string(), Value::String(value));
            }
        }
        Ok(Self {
            operation: ReservedReferenceOperation::Append {
                target: ReservedReferenceTarget::SourceRefs,
                value: Value::Object(object),
            },
        })
    }

    pub fn capture_source(
        ref_type: String,
        ref_id: String,
        revision: Option<String>,
    ) -> Result<Self, MemoryError> {
        let ref_type = normalize_required_lower_bounded(
            ref_type,
            "capture source ref_type",
            MAX_REFERENCE_KIND_BYTES,
        )?;
        if !matches!(ref_type.as_str(), "turn" | "compact_window") {
            return Err(MemoryError::InvalidArg(format!(
                "unsupported capture source ref_type: {ref_type}"
            )));
        }
        let ref_id =
            normalize_required_bounded(ref_id, "capture source ref_id", MAX_REFERENCE_ID_BYTES)?;
        let revision = normalize_optional_bounded(
            revision,
            "capture source revision",
            MAX_REFERENCE_ID_BYTES,
        )?;
        let mut object = Map::new();
        object.insert("ref_type".to_string(), Value::String(ref_type));
        object.insert("ref_id".to_string(), Value::String(ref_id));
        if let Some(revision) = revision {
            object.insert("revision".to_string(), Value::String(revision));
        }
        Ok(Self {
            operation: ReservedReferenceOperation::Append {
                target: ReservedReferenceTarget::SourceRefs,
                value: Value::Object(object),
            },
        })
    }

    /// Trusted wiki migration marker: establish the canonical typed field
    /// when no evidence values exist yet.
    pub fn ensure_empty_evidence_refs_v1() -> Self {
        Self {
            operation: ReservedReferenceOperation::EnsureEmptyEvidenceRefsV1,
        }
    }

    /// Trusted wiki migration marker: retain the historical explicit null
    /// tombstone so readers cannot fall back to stale legacy provenance.
    pub fn tombstone_legacy_source_refs() -> Self {
        Self {
            operation: ReservedReferenceOperation::TombstoneLegacySourceRefs,
        }
    }
}

fn normalize_required_bounded(
    value: String,
    field: &str,
    max_bytes: usize,
) -> Result<String, MemoryError> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(MemoryError::InvalidArg(format!(
            "{field} must be non-empty"
        )));
    }
    if value.len() > max_bytes {
        return Err(MemoryError::InvalidArg(format!(
            "{field} exceeds {max_bytes} bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(MemoryError::InvalidArg(format!(
            "{field} contains control characters"
        )));
    }
    Ok(value)
}

fn normalize_required_lower_bounded(
    value: String,
    field: &str,
    max_bytes: usize,
) -> Result<String, MemoryError> {
    normalize_required_bounded(value, field, max_bytes).map(|value| value.to_ascii_lowercase())
}

fn normalize_optional_bounded(
    value: Option<String>,
    field: &str,
    max_bytes: usize,
) -> Result<Option<String>, MemoryError> {
    value
        .map(|value| normalize_required_bounded(value, field, max_bytes))
        .transpose()
}

fn normalize_timestamp(value: String, field: &str) -> Result<String, MemoryError> {
    let value = normalize_required_bounded(value, field, MAX_REFERENCE_TIMESTAMP_BYTES)?;
    normalize_utc_iso(&value)
}

fn normalize_optional_timestamp(
    value: Option<String>,
    field: &str,
) -> Result<Option<String>, MemoryError> {
    value
        .map(|value| normalize_timestamp(value, field))
        .transpose()
}

/// Result of an atomic insert-only memory write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertMemoryResult {
    Inserted,
    Existing,
}

/// Insert or update a memory entry (and its embedding vector if provided).
pub(crate) fn upsert(
    conn: &mut Connection,
    entry: &MemoryEntry,
    vec_available: bool,
) -> Result<(), MemoryError> {
    upsert_with_idless_identity(conn, entry, vec_available, None).map(|_| ())
}

const RESERVED_REFERENCE_KEYS: [&str; 2] = ["evidence_refs_v1", "source_refs"];

fn strip_untrusted_reserved_metadata(metadata: &Value) -> Value {
    let Some(mut object) = metadata.as_object().cloned() else {
        return metadata.clone();
    };
    for key in RESERVED_REFERENCE_KEYS {
        object.remove(key);
    }
    Value::Object(object)
}

fn read_existing_metadata(
    tx: &rusqlite::Transaction<'_>,
    entry_id: &str,
) -> Result<Option<Value>, MemoryError> {
    tx.query_row(
        "SELECT metadata FROM memories WHERE id = ?1",
        params![entry_id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|raw| serde_json::from_str::<Value>(&raw))
    .transpose()
    .map_err(Into::into)
}

fn merge_ordinary_reserved_metadata(
    tx: &rusqlite::Transaction<'_>,
    entry_id: &str,
    incoming: &Value,
) -> Result<Value, MemoryError> {
    let mut sanitized = strip_untrusted_reserved_metadata(incoming);
    let Some(existing) = read_existing_metadata(tx, entry_id)? else {
        return Ok(sanitized);
    };
    let Some(existing_object) = existing.as_object() else {
        return Ok(sanitized);
    };
    let reserved = RESERVED_REFERENCE_KEYS
        .into_iter()
        .filter_map(|key| existing_object.get(key).cloned().map(|value| (key, value)))
        .collect::<Vec<_>>();
    if reserved.is_empty() {
        return Ok(sanitized);
    }
    let mut object = sanitized.as_object().cloned().unwrap_or_default();
    for (key, value) in reserved {
        object.insert(key.to_string(), value);
    }
    sanitized = Value::Object(object);
    Ok(sanitized)
}

fn atomic_evidence_path_validation_disabled() -> bool {
    matches!(
        std::env::var("TACHI_DISABLE_PATH_VALIDATION")
            .ok()
            .as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes")
    )
}

impl crate::MemoryStore {
    /// Save through the normal upsert body while atomically preserving and
    /// mutating reserved reference metadata. The validated mutations and
    /// metadata patch are separate arguments so caller-controlled metadata
    /// cannot impersonate an authorized server reference write.
    pub fn upsert_with_validated_reference_mutations(
        &mut self,
        entry: &MemoryEntry,
        idless_identity: Option<&str>,
        metadata_patch: &Map<String, Value>,
        mutations: &[ValidatedReferenceMutation],
    ) -> Result<(IdlessUpsertResult, Value), MemoryError> {
        self.upsert_with_validated_reference_mutations_and_metadata_removals(
            entry,
            idless_identity,
            metadata_patch,
            &[],
            mutations,
        )
    }

    /// Trusted metadata-removal counterpart used when a server-side policy
    /// must atomically delete caller-forged authority while preserving typed
    /// reference metadata. Reserved reference keys cannot be removed here.
    pub fn upsert_with_validated_reference_mutations_and_metadata_removals(
        &mut self,
        entry: &MemoryEntry,
        idless_identity: Option<&str>,
        metadata_patch: &Map<String, Value>,
        metadata_removals: &[&str],
        mutations: &[ValidatedReferenceMutation],
    ) -> Result<(IdlessUpsertResult, Value), MemoryError> {
        if self.path_validation && !atomic_evidence_path_validation_disabled() {
            let allow_cross = entry
                .metadata
                .get("allow_cross_project")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Err(error) =
                crate::path_router::validate_path_for_db(&entry.path, &self.db_label, allow_cross)
            {
                eprintln!(
                    "warning: path-routing validation rejected write db_label={} path={} error={}",
                    self.db_label, entry.path, error
                );
                return Err(MemoryError::InvalidArg(error.to_string()));
            }
        }

        let db_label = self.db_label.clone();
        let vec_available = self.vec_available;
        let authorization = self.reserved_reference_write.clone();
        crate::db::retry_memory_locked("upsert_validated_reference_mutations", &db_label, || {
            let _authorization = crate::db::authorize_reserved_reference_write(&authorization)?;
            upsert_with_validated_reference_mutations(
                &mut self.conn,
                entry,
                vec_available,
                idless_identity,
                metadata_patch,
                metadata_removals,
                mutations,
            )
        })
    }

    /// Insert-only counterpart to [`Self::upsert_with_validated_reference_mutations`].
    /// Construction validates shape only; the server remains responsible for
    /// deciding whether the source is authorized before calling this method.
    pub fn insert_if_absent_with_validated_reference_mutations(
        &mut self,
        entry: &MemoryEntry,
        metadata_patch: &Map<String, Value>,
        mutations: &[ValidatedReferenceMutation],
    ) -> Result<InsertMemoryResult, MemoryError> {
        if self.path_validation && !atomic_evidence_path_validation_disabled() {
            let allow_cross = entry
                .metadata
                .get("allow_cross_project")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            crate::path_router::validate_path_for_db(&entry.path, &self.db_label, allow_cross)
                .map_err(|error| MemoryError::InvalidArg(error.to_string()))?;
        }
        let db_label = self.db_label.clone();
        let vec_available = self.vec_available;
        let authorization = self.reserved_reference_write.clone();
        crate::db::retry_memory_locked(
            "insert_if_absent_validated_reference_mutations",
            &db_label,
            || {
                let _authorization = crate::db::authorize_reserved_reference_write(&authorization)?;
                insert_if_absent_with_reference_mutations(
                    &mut self.conn,
                    entry,
                    vec_available,
                    Some(metadata_patch),
                    mutations,
                )
            },
        )
    }
}

fn normalized_append_key(target: ReservedReferenceTarget, value: &Value) -> Option<String> {
    match target {
        ReservedReferenceTarget::EvidenceRefsV1 => {
            let object = value.as_object()?;
            let reference = normalize_required_bounded(
                object.get("ref")?.as_str()?.to_string(),
                "evidence ref",
                MAX_REFERENCE_BYTES,
            )
            .ok()?;
            normalize_timestamp(
                object.get("captured_at")?.as_str()?.to_string(),
                "evidence captured_at",
            )
            .ok()?;
            let target_kind = match object.get("target_kind") {
                Some(Value::String(kind)) => Some(
                    normalize_required_lower_bounded(
                        kind.to_string(),
                        "evidence target_kind",
                        MAX_REFERENCE_KIND_BYTES,
                    )
                    .ok()?,
                ),
                Some(_) => return None,
                None => None,
            };
            serde_json::to_string(&(reference, target_kind)).ok()
        }
        ReservedReferenceTarget::SourceRefs => serde_json::to_string(value).ok(),
    }
}

fn merge_validated_reference_metadata(
    tx: &rusqlite::Transaction<'_>,
    entry_id: &str,
    metadata_patch: &Map<String, Value>,
    metadata_removals: &[&str],
    mutations: &[ValidatedReferenceMutation],
) -> Result<Value, MemoryError> {
    let existing_metadata = read_existing_metadata(tx, entry_id)?;
    let mut merged = existing_metadata
        .as_ref()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (key, value) in metadata_patch {
        if key != "evidence_refs_v1" && key != "source_refs" {
            merged.insert(key.clone(), value.clone());
        }
    }
    for key in metadata_removals {
        if RESERVED_REFERENCE_KEYS.contains(key) {
            return Err(MemoryError::InvalidArg(format!(
                "trusted metadata removal cannot delete reserved reference key '{key}'"
            )));
        }
        merged.remove(*key);
    }
    for mutation in mutations {
        match &mutation.operation {
            ReservedReferenceOperation::Append { target, value } => {
                let key = target.metadata_key();
                let refs = match merged.get(key) {
                    Some(Value::Array(values)) => values.clone(),
                    Some(_) => {
                        return Err(MemoryError::InvalidArg(format!(
                            "cannot append validated reference to malformed existing metadata.{key}"
                        )))
                    }
                    None => Vec::new(),
                };
                let append_key = normalized_append_key(*target, value).ok_or_else(|| {
                    MemoryError::InvalidArg("invalid normalized reference append".into())
                })?;
                if !refs.iter().any(|existing| {
                    normalized_append_key(*target, existing).as_deref() == Some(append_key.as_str())
                }) {
                    let mut refs = refs;
                    refs.push(value.clone());
                    merged.insert(key.to_string(), Value::Array(refs));
                }
            }
            ReservedReferenceOperation::EnsureEmptyEvidenceRefsV1 => {
                merged
                    .entry("evidence_refs_v1".to_string())
                    .or_insert_with(|| Value::Array(Vec::new()));
            }
            ReservedReferenceOperation::TombstoneLegacySourceRefs => {
                merged.insert("source_refs".to_string(), Value::Null);
            }
        }
    }
    Ok(Value::Object(merged))
}

fn upsert_with_validated_reference_mutations(
    conn: &mut Connection,
    entry: &MemoryEntry,
    vec_available: bool,
    idless_identity: Option<&str>,
    metadata_patch: &Map<String, Value>,
    metadata_removals: &[&str],
    mutations: &[ValidatedReferenceMutation],
) -> Result<(IdlessUpsertResult, Value), MemoryError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let result = upsert_with_validated_reference_mutations_within_tx_and_metadata_removals(
        &tx,
        entry,
        vec_available,
        idless_identity,
        metadata_patch,
        metadata_removals,
        mutations,
    )?;
    tx.commit()?;
    Ok(result)
}

pub(crate) fn upsert_with_validated_reference_mutations_within_tx(
    tx: &rusqlite::Transaction<'_>,
    entry: &MemoryEntry,
    vec_available: bool,
    idless_identity: Option<&str>,
    metadata_patch: &Map<String, Value>,
    mutations: &[ValidatedReferenceMutation],
) -> Result<(IdlessUpsertResult, Value), MemoryError> {
    upsert_with_validated_reference_mutations_within_tx_and_metadata_removals(
        tx,
        entry,
        vec_available,
        idless_identity,
        metadata_patch,
        &[],
        mutations,
    )
}

fn upsert_with_validated_reference_mutations_within_tx_and_metadata_removals(
    tx: &rusqlite::Transaction<'_>,
    entry: &MemoryEntry,
    vec_available: bool,
    idless_identity: Option<&str>,
    metadata_patch: &Map<String, Value>,
    metadata_removals: &[&str],
    mutations: &[ValidatedReferenceMutation],
) -> Result<(IdlessUpsertResult, Value), MemoryError> {
    let mut merged_entry = entry.clone();
    merged_entry.metadata = merge_validated_reference_metadata(
        tx,
        &entry.id,
        metadata_patch,
        metadata_removals,
        mutations,
    )?;
    let result = upsert_prepared_within_tx(tx, &merged_entry, vec_available, idless_identity)?;
    Ok((result, merged_entry.metadata))
}

#[cfg(test)]
mod reserved_reference_tests {
    use super::*;
    use serde_json::json;

    fn entry(id: &str, metadata: Value) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: format!("/audit/reserved-reference/{id}"),
            summary: "reserved reference boundary".to_string(),
            text: format!("reserved reference boundary fixture {id}"),
            importance: 0.7,
            timestamp: "2026-07-25T00:00:00Z".to_string(),
            valid_from: "2026-07-25T00:00:00Z".to_string(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata,
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    fn open_store() -> (tempfile::TempDir, crate::MemoryStore) {
        let dir = tempfile::tempdir().expect("temp db dir");
        let path = dir.path().join("memory.db");
        let store = crate::MemoryStore::open(&path.to_string_lossy()).expect("open memory store");
        (dir, store)
    }

    fn append(reference: &str, captured_at: &str) -> ValidatedReferenceMutation {
        ValidatedReferenceMutation::evidence(reference.to_string(), captured_at.to_string(), None)
            .expect("typed append")
    }

    fn refs(entry: &MemoryEntry) -> Vec<&str> {
        entry.metadata["evidence_refs_v1"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|value| value["ref"].as_str().expect("typed ref"))
            .collect()
    }

    #[test]
    fn ordinary_store_upsert_strips_hostile_reserved_metadata_on_create() {
        let (_dir, mut store) = open_store();
        let hostile = entry(
            "hostile-create",
            json!({
                "kept": true,
                "evidence_refs_v1": [{
                    "ref": "#999",
                    "captured_at": "2026-07-25T00:00:00Z"
                }],
                "source_refs": [{ "target_ref": "#998" }]
            }),
        );
        store.upsert(&hostile).expect("ordinary create");

        let stored = store.get(&hostile.id).unwrap().unwrap();
        assert_eq!(stored.metadata["kept"], json!(true));
        assert!(stored.metadata.get("evidence_refs_v1").is_none());
        assert!(stored.metadata.get("source_refs").is_none());
    }

    #[test]
    fn ordinary_store_upsert_cannot_replace_trusted_evidence_on_update() {
        let (_dir, mut store) = open_store();
        let clean = entry("hostile-update", json!({}));
        store
            .upsert_with_validated_reference_mutations(
                &clean,
                None,
                &Map::new(),
                &[append("#100", "2026-07-25T00:00:00Z")],
            )
            .expect("trusted seed");
        let hostile = entry(
            "hostile-update",
            json!({
                "kept": true,
                "evidence_refs_v1": [{
                    "ref": "#999",
                    "captured_at": "2026-07-25T00:00:00Z"
                }],
                "source_refs": ["#998"]
            }),
        );
        store.upsert(&hostile).expect("ordinary hostile update");

        let stored = store.get(&hostile.id).unwrap().unwrap();
        assert_eq!(refs(&stored), vec!["#100"]);
        assert_eq!(stored.metadata["kept"], json!(true));
        assert!(stored.metadata.get("source_refs").is_none());
    }

    #[test]
    fn stale_ordinary_store_upsert_cannot_erase_later_trusted_append() {
        let dir = tempfile::tempdir().expect("temp db dir");
        let path = dir.path().join("memory.db");
        let mut stale_store = crate::MemoryStore::open(&path.to_string_lossy()).unwrap();
        let mut trusted_store = crate::MemoryStore::open(&path.to_string_lossy()).unwrap();
        let clean = entry("stale-writer", json!({ "owner": "ordinary" }));
        trusted_store
            .upsert_with_validated_reference_mutations(
                &clean,
                None,
                &Map::new(),
                &[append("#100", "2026-07-25T00:00:00Z")],
            )
            .expect("trusted seed");
        let mut stale = stale_store.get(&clean.id).unwrap().unwrap();
        stale.metadata["stale_patch"] = json!(true);
        trusted_store
            .upsert_with_validated_reference_mutations(
                &clean,
                None,
                &Map::new(),
                &[append("#101", "2026-07-25T00:01:00Z")],
            )
            .expect("trusted concurrent append");

        stale_store.upsert(&stale).expect("stale ordinary update");
        let stored = stale_store.get(&clean.id).unwrap().unwrap();
        assert_eq!(refs(&stored), vec!["#100", "#101"]);
        assert_eq!(stored.metadata["stale_patch"], json!(true));
    }

    #[test]
    fn ordinary_store_upsert_preserves_existing_legacy_source_refs() {
        let (_dir, mut store) = open_store();
        let clean = entry("legacy-preserve", json!({ "before": true }));
        store.upsert(&clean).unwrap();
        let _authorization =
            crate::db::authorize_reserved_reference_write(&store.reserved_reference_write).unwrap();
        store
            .connection()
            .execute(
                "UPDATE memories SET metadata = ?1 WHERE id = ?2",
                params![
                    json!({
                        "before": true,
                        "source_refs": [{ "target_ref": "#100" }]
                    })
                    .to_string(),
                    clean.id
                ],
            )
            .unwrap();
        drop(_authorization);
        let update = entry("legacy-preserve", json!({ "after": true }));
        store.upsert(&update).unwrap();

        let stored = store.get(&update.id).unwrap().unwrap();
        assert_eq!(stored.metadata["after"], json!(true));
        assert_eq!(
            stored.metadata["source_refs"][0]["target_ref"],
            json!("#100")
        );
    }

    #[test]
    fn trusted_append_normalizes_before_dedupe_and_validates_timestamp() {
        let (_dir, mut store) = open_store();
        let clean = entry("normalized-append", json!({}));
        store
            .upsert_with_validated_reference_mutations(
                &clean,
                None,
                &Map::new(),
                &[
                    append("#100", "2026-07-25T00:00:00Z"),
                    append(" #100 ", "2026-07-25T08:00:00+08:00"),
                ],
            )
            .expect("trusted normalized append");
        let stored = store.get(&clean.id).unwrap().unwrap();
        assert_eq!(refs(&stored), vec!["#100"]);
        assert!(ValidatedReferenceMutation::evidence(
            "#101".to_string(),
            "not-a-timestamp".to_string(),
            None
        )
        .is_err());
    }

    #[test]
    fn evidence_dedupe_preserves_target_kind_first_timestamp_and_order() {
        let (_dir, mut store) = open_store();
        let clean = entry("typed-evidence-identity", json!({}));
        store
            .upsert_with_validated_reference_mutations(
                &clean,
                None,
                &Map::new(),
                &[ValidatedReferenceMutation::evidence(
                    " #100 ".to_string(),
                    "2026-07-25T08:00:00+08:00".to_string(),
                    Some(" ISSUE ".to_string()),
                )
                .unwrap()],
            )
            .expect("seed typed evidence");

        store
            .upsert_with_validated_reference_mutations(
                &clean,
                None,
                &Map::new(),
                &[
                    ValidatedReferenceMutation::evidence(
                        "#100".to_string(),
                        "2026-07-25T00:00:00Z".to_string(),
                        Some("issue".to_string()),
                    )
                    .unwrap(),
                    ValidatedReferenceMutation::evidence(
                        "#100".to_string(),
                        "2026-07-25T00:00:00Z".to_string(),
                        Some("pr".to_string()),
                    )
                    .unwrap(),
                    ValidatedReferenceMutation::evidence(
                        "#100".to_string(),
                        "2026-07-26T00:00:00Z".to_string(),
                        Some("issue".to_string()),
                    )
                    .unwrap(),
                ],
            )
            .expect("append semantically distinct evidence");

        let stored = store.get(&clean.id).unwrap().unwrap();
        assert_eq!(
            stored.metadata["evidence_refs_v1"],
            json!([
                {
                    "ref": "#100",
                    "captured_at": "2026-07-25T00:00:00.000Z",
                    "target_kind": "issue"
                },
                {
                    "ref": "#100",
                    "captured_at": "2026-07-25T00:00:00.000Z",
                    "target_kind": "pr"
                }
            ]),
            "ref+kind duplicates keep the first timestamp while kind distinctions survive"
        );
    }

    #[test]
    fn trusted_metadata_removals_cannot_delete_reserved_references() {
        let (_dir, mut store) = open_store();
        let clean = entry("metadata-removal-reference-boundary", json!({}));
        store
            .upsert_with_validated_reference_mutations(
                &clean,
                None,
                &Map::new(),
                &[append("#100", "2026-07-25T00:00:00Z")],
            )
            .expect("seed trusted evidence");

        for key in RESERVED_REFERENCE_KEYS {
            let error = store
                .upsert_with_validated_reference_mutations_and_metadata_removals(
                    &clean,
                    None,
                    &Map::new(),
                    &[key],
                    &[],
                )
                .expect_err("metadata removal must not erase reserved references");
            assert!(
                error
                    .to_string()
                    .contains("cannot delete reserved reference key"),
                "unexpected {key} removal refusal: {error}"
            );
        }

        let stored = store.get(&clean.id).unwrap().unwrap();
        assert_eq!(refs(&stored), vec!["#100"]);
    }

    #[test]
    fn precedent_source_append_normalizes_before_dedupe() {
        let (_dir, mut store) = open_store();
        let clean = entry("precedent-source-normalized", json!({}));
        let source = |target_ref: &str, updated_at: &str| {
            ValidatedReferenceMutation::precedent_source(
                " SUPPORTS ".to_string(),
                " COMMENT ".to_string(),
                target_ref.to_string(),
                Some(" 42 ".to_string()),
                Some(updated_at.to_string()),
                Some(" hash-42 ".to_string()),
                None,
                Some(" lines 1-2 ".to_string()),
            )
            .expect("precedent source mutation")
        };
        store
            .upsert_with_validated_reference_mutations(
                &clean,
                None,
                &Map::new(),
                &[
                    source(" #100 ", "2026-07-25T08:00:00+08:00"),
                    source("#100", "2026-07-25T00:00:00Z"),
                ],
            )
            .expect("normalized precedent source append");

        let stored = store.get(&clean.id).unwrap().unwrap();
        let refs = stored.metadata["source_refs"].as_array().unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0]["relation"], json!("supports"));
        assert_eq!(refs[0]["target_kind"], json!("comment"));
        assert_eq!(refs[0]["target_ref"], json!("#100"));
        assert_eq!(refs[0]["updated_at"], json!("2026-07-25T00:00:00.000Z"));
    }

    #[test]
    fn validated_reference_mutations_reject_oversized_and_unknown_fields() {
        assert!(ValidatedReferenceMutation::evidence(
            "r".repeat(MAX_REFERENCE_BYTES + 1),
            "2026-07-25T00:00:00Z".to_string(),
            Some("issue".to_string()),
        )
        .is_err());
        assert!(ValidatedReferenceMutation::evidence(
            "#100".to_string(),
            "2026-07-25T00:00:00Z".to_string(),
            Some("invented_kind".to_string()),
        )
        .is_err());
        assert!(ValidatedReferenceMutation::evidence(
            "#100".to_string(),
            "t".repeat(MAX_REFERENCE_TIMESTAMP_BYTES + 1),
            None,
        )
        .is_err());

        let precedent = |relation: &str,
                         target_kind: &str,
                         target_ref: String,
                         comment_id: Option<String>,
                         updated_at: Option<String>,
                         body_hash: Option<String>,
                         commit_sha: Option<String>,
                         section_or_span: Option<String>| {
            ValidatedReferenceMutation::precedent_source(
                relation.to_string(),
                target_kind.to_string(),
                target_ref,
                comment_id,
                updated_at,
                body_hash,
                commit_sha,
                section_or_span,
            )
        };
        assert!(precedent(
            "invented_relation",
            "comment",
            "#100".to_string(),
            Some("42".to_string()),
            None,
            Some("hash".to_string()),
            None,
            None,
        )
        .is_err());
        assert!(precedent(
            "supports",
            "invented_kind",
            "#100".to_string(),
            None,
            None,
            None,
            None,
            None,
        )
        .is_err());
        assert!(precedent(
            "supports",
            "comment",
            "r".repeat(MAX_REFERENCE_BYTES + 1),
            Some("42".to_string()),
            None,
            Some("hash".to_string()),
            None,
            None,
        )
        .is_err());
        assert!(precedent(
            "supports",
            "comment",
            "#100".to_string(),
            Some("c".repeat(MAX_REFERENCE_ID_BYTES + 1)),
            None,
            Some("hash".to_string()),
            None,
            None,
        )
        .is_err());
        assert!(precedent(
            "supports",
            "comment",
            "#100".to_string(),
            Some("42".to_string()),
            Some("not-a-timestamp".to_string()),
            Some("hash".to_string()),
            None,
            None,
        )
        .is_err());
        assert!(precedent(
            "supports",
            "comment",
            "#100".to_string(),
            Some("42".to_string()),
            None,
            Some("h".repeat(MAX_REFERENCE_HASH_BYTES + 1)),
            None,
            None,
        )
        .is_err());
        assert!(precedent(
            "supports",
            "comment",
            "#100".to_string(),
            Some("42".to_string()),
            None,
            Some("hash".to_string()),
            None,
            Some("s".repeat(MAX_REFERENCE_SECTION_BYTES + 1)),
        )
        .is_err());
        assert!(precedent(
            "supports",
            "comment",
            "#100".to_string(),
            None,
            None,
            Some("hash".to_string()),
            None,
            None,
        )
        .is_err());
        assert!(precedent(
            "supports",
            "comment",
            "#100".to_string(),
            Some("42".to_string()),
            None,
            None,
            None,
            None,
        )
        .is_err());
        assert!(precedent(
            "supports",
            "commit",
            "deadbeef".to_string(),
            None,
            None,
            None,
            None,
            None,
        )
        .is_err());

        assert!(ValidatedReferenceMutation::capture_source(
            "invented_capture_kind".to_string(),
            "capture-id".to_string(),
            None,
        )
        .is_err());
        assert!(ValidatedReferenceMutation::capture_source(
            "k".repeat(MAX_REFERENCE_KIND_BYTES + 1),
            "capture-id".to_string(),
            None,
        )
        .is_err());
        assert!(ValidatedReferenceMutation::capture_source(
            "turn".to_string(),
            "i".repeat(MAX_REFERENCE_ID_BYTES + 1),
            None,
        )
        .is_err());
        assert!(ValidatedReferenceMutation::capture_source(
            "turn".to_string(),
            "capture-id".to_string(),
            Some("r".repeat(MAX_REFERENCE_ID_BYTES + 1)),
        )
        .is_err());
    }

    #[test]
    fn insert_if_absent_strips_hostile_reserved_metadata() {
        let (_dir, mut store) = open_store();
        let hostile = entry(
            "insert-hostile",
            json!({
                "kept": true,
                "evidence_refs_v1": [{
                    "ref": "#999",
                    "captured_at": "2026-07-25T00:00:00Z"
                }],
                "source_refs": ["#998"]
            }),
        );
        assert_eq!(
            store.insert_if_absent(&hostile).unwrap(),
            InsertMemoryResult::Inserted
        );
        let stored = store.get(&hostile.id).unwrap().unwrap();
        assert_eq!(stored.metadata["kept"], json!(true));
        assert!(stored.metadata.get("evidence_refs_v1").is_none());
        assert!(stored.metadata.get("source_refs").is_none());
    }

    #[test]
    fn idless_upsert_strips_hostile_reserved_metadata() {
        let (_dir, mut store) = open_store();
        let hostile = entry(
            "idless-hostile",
            json!({
                "kept": true,
                "evidence_refs_v1": [{
                    "ref": "#999",
                    "captured_at": "2026-07-25T00:00:00Z"
                }],
                "source_refs": ["#998"]
            }),
        );
        assert_eq!(
            store.upsert_idless(&hostile, "hostile-identity").unwrap(),
            IdlessUpsertResult::Saved
        );
        let stored = store.get(&hostile.id).unwrap().unwrap();
        assert_eq!(stored.metadata["kept"], json!(true));
        assert!(stored.metadata.get("evidence_refs_v1").is_none());
        assert!(stored.metadata.get("source_refs").is_none());
    }

    #[test]
    fn raw_db_upsert_is_not_an_authorized_channel() {
        let (_dir, mut store) = open_store();
        let hostile = entry(
            "raw-hostile",
            json!({
                "evidence_refs_v1": [{
                    "ref": "#999",
                    "captured_at": "2026-07-25T00:00:00Z"
                }],
                "source_refs": ["#998"]
            }),
        );
        let vec_available = store.vec_available;
        let result = super::upsert(store.connection_mut(), &hostile, vec_available);
        assert!(
            result.is_err(),
            "raw DB upsert bypassed the scoped MemoryStore write channel"
        );
        assert!(store.get(&hostile.id).unwrap().is_none());
    }

    #[test]
    fn raw_connections_cannot_erase_or_forge_reserved_reference_metadata() {
        let dir = tempfile::tempdir().expect("temp db dir");
        let path = dir.path().join("memory.db");
        let mut store = crate::MemoryStore::open(&path.to_string_lossy()).unwrap();
        let clean = entry("raw-guard", json!({ "kept": true }));
        store
            .upsert_with_validated_reference_mutations(
                &clean,
                None,
                &Map::new(),
                &[append("#100", "2026-07-25T00:00:00Z")],
            )
            .unwrap();

        let erase = store.connection().execute(
            "UPDATE memories SET metadata = '{}' WHERE id = ?1",
            params![clean.id],
        );
        assert!(erase.is_err(), "raw store connection erased reserved refs");
        let ordinary_patch = store.connection().execute(
            "UPDATE memories
             SET metadata = json_set(metadata, '$.ordinary_patch', 1)
             WHERE id = ?1",
            params![clean.id],
        );
        assert!(
            ordinary_patch.is_err(),
            "raw store connection mutated the protected metadata column"
        );

        let raw = crate::db::open_raw(&path).unwrap();
        let raw_ordinary_patch = raw.execute(
            "UPDATE memories
             SET metadata = json_set(metadata, '$.raw_ordinary_patch', 1)
             WHERE id = ?1",
            params![clean.id],
        );
        assert!(
            raw_ordinary_patch.is_err(),
            "open_raw mutated the protected metadata column"
        );
        let forge = raw.execute(
            "UPDATE memories
             SET metadata = json_set(metadata, '$.evidence_refs_v1', json('[{\"ref\":\"#999\"}]'))
             WHERE id = ?1",
            params![clean.id],
        );
        assert!(forge.is_err(), "open_raw forged reserved refs");

        let stored = store.get(&clean.id).unwrap().unwrap();
        assert_eq!(refs(&stored), vec!["#100"]);
        assert!(stored.metadata.get("ordinary_patch").is_none());
        assert!(stored.metadata.get("raw_ordinary_patch").is_none());
    }

    #[test]
    fn raw_connection_cannot_disable_reserved_reference_guards() {
        let dir = tempfile::tempdir().expect("temp db dir");
        let path = dir.path().join("memory.db");
        let mut store = crate::MemoryStore::open(&path.to_string_lossy()).unwrap();
        let clean = entry("raw-guard-ddl", json!({ "kept": true }));
        store
            .upsert_with_validated_reference_mutations(
                &clean,
                None,
                &Map::new(),
                &[append("#100", "2026-07-25T00:00:00Z")],
            )
            .unwrap();

        let drop_guard = store
            .connection_mut()
            .execute_batch("DROP TRIGGER memories_reserved_refs_update_guard");
        let overwrite_after_drop = store.connection().execute(
            "UPDATE memories SET metadata = '{}' WHERE id = ?1",
            params![clean.id],
        );
        let after_drop = store.get(&clean.id).unwrap().unwrap();
        assert!(
            drop_guard.is_err(),
            "raw handle dropped the guard and bypassed it: overwrite={overwrite_after_drop:?}, refs={:?}",
            refs(&after_drop)
        );
        assert!(
            store
                .connection()
                .execute_batch("DROP TRIGGER memory_search_generation_after_update")
                .is_err(),
            "raw store handle dropped the canonical search-generation trigger"
        );
        assert!(
            overwrite_after_drop.is_err(),
            "raw overwrite succeeded after rejected DROP"
        );
        assert_eq!(refs(&after_drop), vec!["#100"]);

        let replacement = store.connection().execute_batch(
            "CREATE TEMP TRIGGER memories_reserved_refs_update_guard
             BEFORE UPDATE OF metadata ON main.memories
             BEGIN SELECT 1; END;",
        );
        assert!(
            replacement.is_err(),
            "raw handle created a replacement guard trigger"
        );

        store
            .connection()
            .create_scalar_function(
                "tachi_reserved_reference_write_enabled",
                0,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                |_| Ok(1_i64),
            )
            .expect("hostile function replacement demonstrates independent authorizer guard");
        let spoofed_overwrite = store.connection().execute(
            "UPDATE memories SET metadata = '{}' WHERE id = ?1",
            params![clean.id],
        );
        assert!(
            spoofed_overwrite.is_err(),
            "spoofed authorization function enabled raw metadata overwrite"
        );

        assert!(
            store
                .connection()
                .execute_batch("ATTACH DATABASE ':memory:' AS bypass")
                .is_err(),
            "raw handle attached an unguarded schema"
        );
        assert!(
            store
                .connection()
                .execute_batch("PRAGMA writable_schema = ON")
                .is_err(),
            "raw handle enabled writable_schema"
        );

        let migration = crate::db::authorize_schema_migration(&store.reserved_reference_write)
            .expect("authorize private schema fixture");
        store
            .connection()
            .execute_batch(
                "CREATE TABLE private_schema_probe(value INTEGER NOT NULL);
                 CREATE INDEX private_schema_probe_index ON private_schema_probe(value);
                 CREATE VIEW private_schema_probe_view AS
                     SELECT value FROM private_schema_probe;",
            )
            .expect("private migration scope may install schema objects");
        drop(migration);

        for (label, sql) in [
            (
                "create table",
                "CREATE TABLE raw_guard_probe(value INTEGER NOT NULL)",
            ),
            (
                "create index",
                "CREATE INDEX raw_guard_probe_index ON private_schema_probe(value)",
            ),
            (
                "create view",
                "CREATE VIEW raw_guard_probe_view AS SELECT value FROM private_schema_probe",
            ),
            (
                "alter table",
                "ALTER TABLE private_schema_probe ADD COLUMN injected INTEGER",
            ),
            ("drop table", "DROP TABLE private_schema_probe"),
            ("drop index", "DROP INDEX private_schema_probe_index"),
            ("drop view", "DROP VIEW private_schema_probe_view"),
        ] {
            assert!(
                store.connection().execute_batch(sql).is_err(),
                "public store handle allowed {label}"
            );
        }
        store
            .connection()
            .execute(
                "UPDATE memories SET text = 'ordinary update' WHERE id = ?1",
                params![clean.id],
            )
            .expect("ordinary non-protected memory column remains writable");

        let raw = crate::db::open_raw(&path).unwrap();
        for (label, sql) in [
            (
                "create table",
                "CREATE TABLE open_raw_probe(value INTEGER NOT NULL)",
            ),
            (
                "create index",
                "CREATE INDEX open_raw_probe_index ON private_schema_probe(value)",
            ),
            (
                "create view",
                "CREATE VIEW open_raw_probe_view AS SELECT value FROM private_schema_probe",
            ),
            (
                "alter table",
                "ALTER TABLE private_schema_probe ADD COLUMN open_raw_injected INTEGER",
            ),
            ("drop table", "DROP TABLE private_schema_probe"),
            ("drop index", "DROP INDEX private_schema_probe_index"),
            ("drop view", "DROP VIEW private_schema_probe_view"),
        ] {
            assert!(raw.execute_batch(sql).is_err(), "open_raw allowed {label}");
        }
        raw.execute(
            "UPDATE memories SET text = 'open_raw ordinary update' WHERE id = ?1",
            params![clean.id],
        )
        .expect("open_raw ordinary DML remains available");
        assert!(
            raw.execute_batch("ANALYZE").is_err(),
            "open_raw gained planner-maintenance schema authority"
        );
        assert!(
            raw.execute_batch("DROP TRIGGER memories_reserved_refs_update_guard")
                .is_err(),
            "open_raw dropped the canonical guard"
        );
        assert!(
            raw.execute_batch("DROP TRIGGER memory_search_generation_after_update")
                .is_err(),
            "open_raw dropped the canonical search-generation trigger"
        );
        assert!(
            raw.execute(
                "UPDATE memories SET metadata = '{}' WHERE id = ?1",
                params![clean.id],
            )
            .is_err(),
            "open_raw overwrote protected metadata"
        );

        let stored = store.get(&clean.id).unwrap().unwrap();
        assert_eq!(stored.text, "open_raw ordinary update");
        assert_eq!(refs(&stored), vec!["#100"]);
    }

    #[test]
    fn raw_triggers_cannot_launder_typed_write_authority() {
        for (label, create_trigger) in [
            (
                "main",
                "CREATE TRIGGER malicious_memory_update
                 AFTER UPDATE ON memories
                 BEGIN
                   UPDATE memories
                   SET path = '/wiki/trigger-forged',
                       metadata = json_set(metadata, '$.source_refs', json('[\"#trigger\"]'))
                   WHERE id = NEW.id;
                 END;",
            ),
            (
                "temp-case-variant",
                "CREATE TEMP TRIGGER MaLiCiOuS_MeMoRy_UpDaTe
                 AFTER UPDATE ON main.memories
                 BEGIN
                   UPDATE memories
                   SET source = 'wiki',
                       metadata = json_set(metadata, '$.source_refs', json('[\"#temp-trigger\"]'))
                   WHERE id = NEW.id;
                 END;",
            ),
        ] {
            let (_dir, mut store) = open_store();
            let seed = entry(&format!("trigger-{label}"), json!({ "kept": true }));
            store.upsert(&seed).expect("seed ordinary row");
            let baseline = store.get(&seed.id).unwrap().unwrap();

            let create = store.connection().execute_batch(create_trigger);
            let mut typed_update = baseline.clone();
            typed_update.text.push_str(" typed update");
            store
                .upsert(&typed_update)
                .expect("legitimate typed update");
            let stored = store.get(&seed.id).unwrap().unwrap();

            assert!(
                create.is_err(),
                "raw {label} trigger inherited typed authority: path={}, source={}, metadata={}",
                stored.path,
                stored.source,
                stored.metadata
            );
            assert_eq!(stored.path, baseline.path);
            assert_eq!(stored.source, baseline.source);
            assert!(stored.metadata.get("source_refs").is_none());
        }
    }

    #[test]
    fn raw_trigger_drop_is_denied_for_arbitrary_main_and_temp_triggers() {
        let (dir, store) = open_store();
        let path = dir.path().join("memory.db");
        let offline = rusqlite::Connection::open(&path).expect("open offline trigger fixture");
        offline
            .execute_batch(
                "CREATE TRIGGER MixedCaseAuxTrigger
                 AFTER INSERT ON access_history BEGIN SELECT 1; END;",
            )
            .expect("plant persistent trigger after protected open");
        drop(offline);

        assert!(
            store
                .connection()
                .execute_batch("DROP TRIGGER mixedcaseauxtrigger")
                .is_err(),
            "raw handle dropped an arbitrary persistent trigger"
        );
        assert!(
            store
                .connection()
                .execute_batch("CREATE TABLE raw_drop_probe(value INTEGER NOT NULL)")
                .is_err(),
            "raw handle created an auxiliary table"
        );

        let temp = rusqlite::Connection::open_in_memory().expect("open temp trigger fixture");
        temp.execute_batch(
            "CREATE TABLE auxiliary(value INTEGER);
             CREATE TEMP TRIGGER TempAuxTrigger
             AFTER INSERT ON auxiliary BEGIN SELECT 1; END;",
        )
        .expect("seed temp trigger before protection");
        crate::db::install_reserved_reference_authorizer(&temp, None)
            .expect("install connection authorizer");
        assert!(
            temp.execute_batch("DROP TRIGGER temp.TempAuxTrigger")
                .is_err(),
            "protected raw handle dropped an arbitrary temp trigger"
        );
    }

    #[test]
    fn raw_auxiliary_trigger_cannot_chain_into_typed_search_write() {
        let (_dir, mut store) = open_store();
        let seed = entry("trigger-access-history", json!({ "kept": true }));
        store.upsert(&seed).expect("seed searchable row");

        let create = store.connection().execute_batch(
            "CREATE TRIGGER malicious_access_history_insert
             AFTER INSERT ON access_history
             BEGIN
               UPDATE memories
               SET category = 'wiki', path = '/wiki/aux-trigger-forged'
               WHERE id = NEW.memory_id;
             END;",
        );
        let results = store
            .search(
                "reserved reference boundary fixture trigger access history",
                None,
            )
            .expect("legitimate typed search");
        assert!(
            results.iter().any(|result| result.entry.id == seed.id),
            "fixture must exercise access recording"
        );
        let stored = store.get(&seed.id).unwrap().unwrap();

        assert!(
            create.is_err(),
            "raw auxiliary trigger inherited typed search authority: path={}, category={}",
            stored.path,
            stored.category
        );
        assert_eq!(stored.path, seed.path);
        assert_eq!(stored.category, seed.category);
    }

    #[test]
    fn schema_migration_scope_allows_only_canonical_trigger_ddl() {
        let (_dir, store) = open_store();
        let typed = crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
            .expect("authorize typed DML");
        assert!(
            store
                .connection()
                .execute_batch(
                    "CREATE TRIGGER typed_dml_ddl_bypass
                     AFTER UPDATE ON memories BEGIN SELECT 1; END;"
                )
                .is_err(),
            "typed DML scope authorized trigger DDL"
        );
        drop(typed);

        let migration = crate::db::authorize_schema_migration(&store.reserved_reference_write)
            .expect("authorize schema migration");
        assert!(
            store
                .connection()
                .execute_batch(
                    "CREATE TRIGGER migration_ddl_bypass
                     AFTER UPDATE ON memories BEGIN SELECT 1; END;"
                )
                .is_err(),
            "schema migration scope authorized an unknown trigger"
        );
        store
            .connection()
            .execute_batch("DROP TRIGGER memory_search_generation_after_update")
            .expect("schema migration may remove a canonical search-generation trigger");
        crate::db::search_generation::ensure_search_generation_schema(store.connection())
            .expect("schema migration may restore canonical search-generation triggers");
        crate::db::install_reserved_reference_guard(store.connection())
            .expect("schema migration may reinstall exact canonical guards");
        drop(migration);
        crate::db::validate_persistent_trigger_inventory(store.connection(), true)
            .expect("canonical guard inventory remains exact");
        crate::db::search_generation(store.connection())
            .expect("canonical search-generation inventory remains usable");
    }

    #[test]
    fn private_authorization_scopes_reset_after_database_errors() {
        let (_dir, store) = open_store();

        let typed_error = (|| -> Result<(), crate::error::MemoryError> {
            let _authorization =
                crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)?;
            store.connection().execute_batch(
                "CREATE TRIGGER typed_scope_error
                     AFTER UPDATE ON memories BEGIN SELECT 1; END;",
            )?;
            Ok(())
        })();
        assert!(
            typed_error.is_err(),
            "typed DML scope must not permit trigger DDL"
        );
        let typed_retry =
            crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                .expect("typed DML scope must release after an error");
        drop(typed_retry);

        let migration_error = (|| -> Result<(), crate::error::MemoryError> {
            let _authorization =
                crate::db::authorize_schema_migration(&store.reserved_reference_write)?;
            store
                .connection()
                .execute_batch("CREATE TABLE memories(id TEXT PRIMARY KEY);")?;
            Ok(())
        })();
        assert!(
            migration_error.is_err(),
            "fixture must make schema migration fail"
        );
        let migration_retry =
            crate::db::authorize_schema_migration(&store.reserved_reference_write)
                .expect("schema migration scope must release after an error");
        drop(migration_retry);

        let planner_error = (|| -> Result<(), crate::error::MemoryError> {
            let _authorization =
                crate::db::authorize_planner_maintenance(&store.reserved_reference_write)?;
            store
                .connection()
                .execute_batch("CREATE TABLE planner_scope_error(value INTEGER);")?;
            Ok(())
        })();
        assert!(
            planner_error.is_err(),
            "planner maintenance scope must not permit arbitrary table DDL"
        );
        let planner_retry =
            crate::db::authorize_planner_maintenance(&store.reserved_reference_write)
                .expect("planner maintenance scope must release after an error");
        drop(planner_retry);

        assert!(
            store
                .connection()
                .execute_batch(
                    "CREATE TRIGGER authorization_leak
                     AFTER UPDATE ON memories BEGIN SELECT 1; END;"
                )
                .is_err(),
            "a failed private scope leaked trigger DDL authority"
        );
    }

    #[test]
    fn revision_checked_full_metadata_update_preserves_reserved_references() {
        let (_dir, mut store) = open_store();
        let clean = entry("revision-metadata-guard", json!({ "before": true }));
        store
            .upsert_with_validated_reference_mutations(
                &clean,
                None,
                &Map::new(),
                &[append("#100", "2026-07-25T00:00:00Z")],
            )
            .unwrap();
        let stored = store.get(&clean.id).unwrap().unwrap();
        assert!(store
            .update_with_revision(
                &stored.id,
                "updated content",
                "updated summary",
                &stored.source,
                &json!({ "after": true }),
                None,
                stored.revision,
            )
            .unwrap());

        let updated = store.get(&clean.id).unwrap().unwrap();
        assert_eq!(refs(&updated), vec!["#100"]);
        assert_eq!(updated.metadata["after"], json!(true));
        assert!(updated.metadata.get("before").is_none());
    }
}

/// Insert `entry` only when its id is absent. The existence decision and all
/// main/FTS/vector writes share the same transaction, so an `Existing` result
/// never mutates any representation of the winning row.
pub(crate) fn insert_if_absent(
    conn: &mut Connection,
    entry: &MemoryEntry,
    vec_available: bool,
) -> Result<InsertMemoryResult, MemoryError> {
    insert_if_absent_with_reference_mutations(conn, entry, vec_available, None, &[])
}

fn insert_if_absent_with_reference_mutations(
    conn: &mut Connection,
    entry: &MemoryEntry,
    vec_available: bool,
    metadata_patch: Option<&Map<String, Value>>,
    mutations: &[ValidatedReferenceMutation],
) -> Result<InsertMemoryResult, MemoryError> {
    if entry.id.trim().is_empty() || entry.id.starts_with("anchor:") {
        return Err(MemoryError::InvalidArg(
            "entry.id must be non-empty and outside the reserved 'anchor:' namespace".to_string(),
        ));
    }
    let path = crate::path_router::normalize_path(&entry.path);
    let source = MemorySource::parse_or_external(&entry.source);
    let category = MemoryCategory::normalize(&entry.category);
    let scope = MemoryScope::normalize(&entry.scope);
    let retention_policy = entry
        .retention_policy
        .clone()
        .or_else(|| default_retention_for(&path, &source).map(str::to_string));
    let clean_text = crate::noise::scrub_think_tags(&entry.text);
    let clean_summary = crate::noise::scrub_think_tags(&entry.summary);
    let force = entry
        .metadata
        .get("force")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let importance = crate::types::normalize_importance(entry.importance, category, force);
    let timestamp_utc = normalize_utc_iso(&entry.timestamp)?;
    let valid_from_utc = if entry.valid_from.trim().is_empty() {
        timestamp_utc.clone()
    } else {
        normalize_utc_iso(&entry.valid_from)?
    };
    let valid_until_utc = entry
        .valid_until
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(normalize_utc_iso)
        .transpose()?;
    let last_access_utc = entry
        .last_access
        .as_deref()
        .map(normalize_utc_iso)
        .transpose()?;
    let write_time_utc = now_utc_iso();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut metadata = match metadata_patch {
        Some(metadata_patch) => {
            merge_validated_reference_metadata(&tx, &entry.id, metadata_patch, &[], mutations)?
        }
        None => strip_untrusted_reserved_metadata(&entry.metadata),
    };
    let path = crate::types::apply_location_relocation(&path, &entry.location, &mut metadata);
    let metadata_json = serde_json::to_string(&metadata)?;
    let kws_json = serde_json::to_string(&entry.keywords)?;
    let e_json = canonical_entities_json(entry)?;

    let rows_changed = tx.execute(
        r#"INSERT INTO memories
              (id, path, summary, text, importance, timestamp, valid_from, valid_until,
               category, topic, keywords, entities, source, scope, archived, created_at,
               updated_at, access_count, last_access, revision, metadata, retention_policy,
               domain, recall_count, query_diversity, tier)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,
                   ?19,?20,?21,?22,?23,?24,?25,?26)
           ON CONFLICT(id) DO NOTHING"#,
        params![
            entry.id,
            &path,
            &clean_summary,
            &clean_text,
            importance,
            timestamp_utc,
            valid_from_utc,
            valid_until_utc,
            category,
            entry.topic,
            kws_json,
            e_json,
            &source,
            scope,
            entry.archived,
            &write_time_utc,
            &write_time_utc,
            entry.access_count,
            last_access_utc,
            entry.revision.max(1),
            metadata_json,
            &retention_policy,
            entry.domain,
            entry.recall_count,
            entry.query_diversity,
            &entry.tier
        ],
    )?;
    if rows_changed == 0 {
        tx.commit()?;
        return Ok(InsertMemoryResult::Existing);
    }
    let kws = entry.keywords.join(" ");
    let mut entities = entry.entities.clone();
    crate::types::fold_person_names_into_entities(&mut entities, entry.persons.clone());
    sync_memories_fts(
        &tx,
        &entry.id,
        &path,
        &clean_summary,
        &clean_text,
        &kws,
        &entities.join(" "),
    )?;
    if vec_available {
        if let Some(vector) = &entry.vector {
            tx.execute(
                "INSERT INTO memories_vec(id, embedding) VALUES (?1, ?2)",
                params![entry.id, serialize_f32(vector)],
            )?;
        }
    }
    tx.commit()?;
    Ok(InsertMemoryResult::Inserted)
}

/// Insert an id-less entry once. A unique modern identity chooses one winner
/// without rewriting legacy rows that predate the constraint.
pub(crate) fn upsert_idless(
    conn: &mut Connection,
    entry: &MemoryEntry,
    vec_available: bool,
    identity: &str,
) -> Result<IdlessUpsertResult, MemoryError> {
    let identity = identity.trim();
    if identity.is_empty() {
        return Err(MemoryError::InvalidArg(
            "id-less identity must be non-empty".to_string(),
        ));
    }
    upsert_with_idless_identity(conn, entry, vec_available, Some(identity))
}

fn upsert_with_idless_identity(
    conn: &mut Connection,
    entry: &MemoryEntry,
    vec_available: bool,
    idless_identity: Option<&str>,
) -> Result<IdlessUpsertResult, MemoryError> {
    if entry.id.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "entry.id must be provided by caller".to_string(),
        ));
    }
    // tachi#773 item 4 guard (c): the `anchor:` id namespace is reserved for
    // `ensure_anchor` (memcore::db::anchor). Ordinary upserts must never
    // create or silently overwrite an anchor row — `ensure_anchor` uses its
    // own dedicated INSERT OR IGNORE, not this function, so any caller
    // reaching here with an `anchor:`-prefixed id is a bug (or a hostile
    // write), not a legitimate anchor creation.
    if entry.id.starts_with("anchor:") {
        return Err(MemoryError::InvalidArg(format!(
            "id '{}' is in the reserved 'anchor:' namespace; use ensure_anchor, not upsert",
            entry.id
        )));
    }
    // All writes for one upsert must be atomic across main table + FTS + vec.
    // The transaction body lives in [`upsert_within_tx`] so lifecycle-apply
    // can run the full upsert (main row + FTS + vectors + idless semantics)
    // inside a caller-owned `BEGIN IMMEDIATE` transaction alongside
    // archive/supersede and the proposal-state CAS.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let result = upsert_within_tx(&tx, entry, vec_available, idless_identity)?;
    tx.commit()?;
    Ok(result)
}

/// Caller-transaction upsert body. Performs the same normalization, main-row
/// INSERT/UPDATE, FTS sync, vector sync, and write-time Jaccard dedup as
/// [`upsert_with_idless_identity`], but does NOT commit — the caller owns
/// the transaction. This is the seam that lets lifecycle-apply run the full
/// upsert inside a single `BEGIN IMMEDIATE` transaction without duplicating a
/// reduced upsert: ordinary `upsert`/`upsert_idless` behavior (main row, FTS,
/// vectors, idless semantics) is byte-for-byte identical because both paths
/// execute this same body; only the commit site differs.
pub(crate) fn upsert_within_tx(
    tx: &rusqlite::Transaction<'_>,
    entry: &MemoryEntry,
    vec_available: bool,
    idless_identity: Option<&str>,
) -> Result<IdlessUpsertResult, MemoryError> {
    let mut sanitized = entry.clone();
    sanitized.metadata = merge_ordinary_reserved_metadata(tx, &entry.id, &entry.metadata)?;
    upsert_prepared_within_tx(tx, &sanitized, vec_available, idless_identity)
}

fn upsert_prepared_within_tx(
    tx: &rusqlite::Transaction<'_>,
    entry: &MemoryEntry,
    vec_available: bool,
    idless_identity: Option<&str>,
) -> Result<IdlessUpsertResult, MemoryError> {
    // Normalize only the fields enforced by CHECK constraints; avoid cloning
    // the full entry/vector on the hot write path.
    let path = crate::path_router::normalize_path(&entry.path);
    let source = MemorySource::parse_or_external(&entry.source);
    let category = MemoryCategory::normalize(&entry.category);
    let scope = MemoryScope::normalize(&entry.scope);
    let retention_policy = entry
        .retention_policy
        .clone()
        .or_else(|| default_retention_for(&path, &source).map(str::to_string));

    let clean_text = crate::noise::scrub_think_tags(&entry.text);
    let clean_summary = crate::noise::scrub_think_tags(&entry.summary);
    let force = entry
        .metadata
        .get("force")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let importance = crate::types::normalize_importance(entry.importance, category, force);

    let timestamp_utc = normalize_utc_iso(&entry.timestamp)?;
    let valid_from_utc = if entry.valid_from.trim().is_empty() {
        timestamp_utc.clone()
    } else {
        normalize_utc_iso(&entry.valid_from)?
    };
    let valid_until_utc = entry
        .valid_until
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(normalize_utc_iso)
        .transpose()?;
    let last_access_utc = entry
        .last_access
        .as_deref()
        .map(normalize_utc_iso)
        .transpose()?;
    let write_time_utc = now_utc_iso();

    let mut metadata = entry.metadata.clone();
    let path = crate::types::apply_location_relocation(&path, &entry.location, &mut metadata);
    let metadata_json = serde_json::to_string(&metadata)?;
    let kws_json = serde_json::to_string(&entry.keywords)?;
    let e_json = canonical_entities_json(entry)?;

    // ── Write-time Jaccard deduplication (new entries only) ──────────────────
    // Only for net-new IDs; ON CONFLICT path below handles updates.
    let is_new: bool = tx.query_row(
        "SELECT COUNT(*) FROM memories WHERE id = ?1",
        params![entry.id],
        |r| r.get::<_, i64>(0),
    )? == 0;

    if is_new && idless_identity.is_none() {
        if let Some(cand_id) = merge_into_jaccard_candidate(tx, entry, importance, &write_time_utc)?
        {
            // Write this entry as superseded by the candidate
            tx.execute(
                r#"INSERT INTO memories
                      (id, path, summary, text, importance,
                       timestamp, valid_from, valid_until, category, topic, keywords, entities,
                       source, scope, archived, created_at, updated_at,
                       access_count, last_access, revision, metadata,
                       retention_policy, domain, recall_count, query_diversity, tier,
                       superseded_by)
                   VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27)
                   ON CONFLICT(id) DO NOTHING"#,
                params![
                    entry.id, &path, &clean_summary, &clean_text, importance,
                    timestamp_utc, valid_from_utc, valid_until_utc, category, entry.topic,
                    kws_json, e_json, &source, scope,
                    entry.archived, &write_time_utc, &write_time_utc,
                    entry.access_count, last_access_utc, entry.revision.max(1),
                    metadata_json, &retention_policy, entry.domain,
                    0i64, 0i64, "raw",
                    cand_id,
                ],
            )?;
            // Winner was synced inside `merge_into_jaccard_candidate`; the
            // superseded loser must land in the symbolic projection too so
            // `include_superseded` recall can see it before any repair (#1331).
            sync_memories_symbolic_fts(tx, &entry.id)?;
            return Ok(IdlessUpsertResult::Saved);
        }
    }

    // Write to main table
    let rows_written = tx.execute(
        r#"INSERT INTO memories
              (id, path, summary, text, importance,
               timestamp, valid_from, valid_until, category, topic, keywords, entities,
               source, scope, archived, created_at, updated_at,
               access_count, last_access, revision, metadata,
               retention_policy, domain, idless_identity, recall_count, query_diversity, tier)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27)
           ON CONFLICT(id) DO UPDATE SET
               path         = excluded.path,
               summary      = excluded.summary,
               text         = excluded.text,
               importance   = excluded.importance,
               timestamp    = excluded.timestamp,
               valid_from   = excluded.valid_from,
               valid_until  = excluded.valid_until,
               category     = excluded.category,
               topic        = excluded.topic,
               keywords     = excluded.keywords,
               entities     = excluded.entities,
               source       = excluded.source,
               scope        = excluded.scope,
               archived     = excluded.archived,
               created_at   = memories.created_at,
               updated_at   = excluded.updated_at,
               access_count = excluded.access_count,
               last_access  = excluded.last_access,
               revision     = memories.revision + 1,
               metadata     = excluded.metadata,
               retention_policy = excluded.retention_policy,
               domain       = excluded.domain,
               idless_identity = excluded.idless_identity,
               tier         = CASE WHEN memories.tier IN ('consolidated','pattern') THEN memories.tier ELSE excluded.tier END
           ON CONFLICT(idless_identity)
             WHERE idless_identity IS NOT NULL AND archived = 0 AND superseded_by IS NULL
             DO NOTHING"#,
        params![
            entry.id,
            &path,
            &clean_summary,
            &clean_text,
            importance,
            timestamp_utc,
            valid_from_utc,
            valid_until_utc,
            category,
            entry.topic,
            kws_json,
            e_json,
            &source,
            scope,
            entry.archived,
            &write_time_utc,
            &write_time_utc,
            entry.access_count,
            last_access_utc,
            entry.revision.max(1),
            metadata_json,
            &retention_policy,
            entry.domain,
            idless_identity,
            entry.recall_count,
            entry.query_diversity,
            &entry.tier,
        ],
    )?;

    if rows_written == 0 {
        let identity = idless_identity.ok_or_else(|| {
            MemoryError::Internal("ordinary upsert unexpectedly wrote zero rows".to_string())
        })?;
        let winner_id = tx
            .query_row(
                "SELECT id FROM memories
                 WHERE idless_identity = ?1 AND archived = 0 AND superseded_by IS NULL",
                params![identity],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| {
                MemoryError::Internal(format!(
                    "id-less identity conflict without an active winner: {identity}"
                ))
            })?;
        return Ok(IdlessUpsertResult::Duplicate { id: winner_id });
    }

    // ── Restore write-time Jaccard near-dup dedup for id-less saves ──────────
    // (kckylechen1/tachi#1167 review round 2, restoring #1115's pre-existing
    // behavior that #1167 silently dropped.) The exact-identity unique index
    // above has already decided the *exact*-duplicate question atomically —
    // `rows_written == 0` returned `Duplicate` before we ever got here, so
    // reaching this point means `entry` just landed as a genuinely new,
    // active row (this INSERT is the only writer of `entry.id`'s row, and it
    // is not yet present in `memories_fts`, so it cannot self-match below).
    // What the unique index does NOT catch is a *near* duplicate — same
    // path, 0.9 < Jaccard < 1.0 similar text but a different identity hash —
    // which origin/main's `upsert()` always deduped via this same
    // FTS+Jaccard search. Run it now, after the atomic decision, so the two
    // mechanisms never compete over the same row.
    if idless_identity.is_some() {
        if let Some(cand_id) = merge_into_jaccard_candidate(tx, entry, importance, &write_time_utc)?
        {
            // `entry`'s row just won the identity race and is currently the
            // active holder of `idless_identity` — but it is about to become
            // the *losing* side of a near-duplicate merge. Demote it in the
            // same transaction: mark it superseded by the merge winner and
            // release its identity claim, so `idx_memories_idless_identity_active`
            // (scoped to `superseded_by IS NULL`) never has to reason about a
            // dead row holding a live-looking identity. `cand_id` keeps
            // whatever identity (if any) it already had — it is addressed by
            // its own path+text, not by the entry that just merged into it.
            tx.execute(
                "UPDATE memories SET superseded_by = ?1, idless_identity = NULL WHERE id = ?2",
                params![cand_id, entry.id],
            )?;
            // Same early-return hole as the explicit-id Jaccard path: the
            // loser never reaches the post-block `sync_memories_fts` call.
            sync_memories_symbolic_fts(tx, &entry.id)?;
            return Ok(IdlessUpsertResult::Saved);
        }
    }

    let kws = entry.keywords.join(" ");
    let mut ents_vec = entry.entities.clone();
    crate::types::fold_person_names_into_entities(&mut ents_vec, entry.persons.clone());
    let ents = ents_vec.join(" ");
    sync_memories_fts(
        tx,
        &entry.id,
        &path,
        &clean_summary,
        &clean_text,
        &kws,
        &ents,
    )?;

    if let Some(vec) = &entry.vector {
        if vec_available {
            let blob = serialize_f32(vec);
            // vec0 virtual tables do NOT support ON CONFLICT / UPSERT.
            // Use DELETE + INSERT (same pattern as FTS sync above).
            tx.execute("DELETE FROM memories_vec WHERE id = ?1", params![entry.id])?;
            tx.execute(
                "INSERT INTO memories_vec(id, embedding) VALUES (?1, ?2)",
                params![entry.id, blob],
            )?;
        }
    }

    Ok(IdlessUpsertResult::Saved)
}

#[cfg(test)]
mod idless_upsert_tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/notes/atomic-idless".to_string(),
            summary: "atomic id-less save".to_string(),
            text: "identical concurrent id-less save payload".to_string(),
            importance: 0.7,
            timestamp: "2026-07-16T00:00:00Z".to_string(),
            valid_from: "2026-07-16T00:00:00Z".to_string(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "mcp".to_string(),
            scope: "general".to_string(),
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
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn two_concurrent_same_identity_idless_saves_choose_one_winner() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("memory.db")
            .to_string_lossy()
            .to_string();
        {
            let store = crate::MemoryStore::open(&path).unwrap();
            let has_identity_column: bool = store
                .connection()
                .query_row(
                    "SELECT EXISTS (\
                         SELECT 1 FROM pragma_table_info('memories') \
                         WHERE name = 'idless_identity'\
                     )",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let has_identity_index: bool = store
                .connection()
                .query_row(
                    "SELECT EXISTS (\
                         SELECT 1 FROM sqlite_master \
                         WHERE type = 'index' \
                           AND name = 'idx_memories_idless_identity_active'\
                     )",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(has_identity_column);
            assert!(has_identity_index);
        }
        let barrier = Arc::new(Barrier::new(2));
        let identity = "idless:concurrent-fixture".to_string();

        let workers = ["candidate-a", "candidate-b"].map(|id| {
            let path = path.clone();
            let identity = identity.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut store = crate::MemoryStore::open(&path).unwrap();
                barrier.wait();
                match store.upsert_idless(&entry(id), &identity).unwrap() {
                    IdlessUpsertResult::Saved => id.to_string(),
                    IdlessUpsertResult::Duplicate { id } => id,
                }
            })
        });
        let winner_ids = workers.map(|worker| worker.join().unwrap());

        assert_eq!(
            winner_ids[0], winner_ids[1],
            "both concurrent writers must resolve the same persisted id"
        );
        let store = crate::MemoryStore::open(&path).unwrap();
        let active_rows: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM memories
                 WHERE idless_identity = ?1 AND archived = 0 AND superseded_by IS NULL",
                params![identity],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            active_rows, 1,
            "the identity constraint must leave one active row"
        );
    }

    #[test]
    fn concurrent_insert_if_absent_preserves_exactly_one_payload_and_fts_projection() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("insert-only.db")
            .to_string_lossy()
            .to_string();
        crate::MemoryStore::open(&path).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let workers = ["payload alpha", "payload beta"].map(|payload| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut item = entry("shared-insert-id");
                item.text = payload.to_string();
                item.summary = format!("summary {payload}");
                let mut store = crate::MemoryStore::open(&path).unwrap();
                barrier.wait();
                (payload, store.insert_if_absent(&item).unwrap())
            })
        });
        let outcomes = workers.map(|worker| worker.join().unwrap());
        assert_eq!(
            outcomes
                .iter()
                .filter(|(_, result)| *result == InsertMemoryResult::Inserted)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|(_, result)| *result == InsertMemoryResult::Existing)
                .count(),
            1
        );
        let winner = outcomes
            .iter()
            .find(|(_, result)| *result == InsertMemoryResult::Inserted)
            .unwrap()
            .0;
        let store = crate::MemoryStore::open(&path).unwrap();
        let (text, revision): (String, i64) = store
            .connection()
            .query_row(
                "SELECT text, revision FROM memories WHERE id = 'shared-insert-id'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let fts_text: String = store
            .connection()
            .query_row(
                "SELECT text FROM memories_fts WHERE id = 'shared-insert-id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revision, 1);
        assert_eq!(text, winner);
        assert_eq!(fts_text, winner);
    }

    #[test]
    fn insert_if_absent_makes_new_row_available_to_symbolic_trigram_lookup() {
        let mut store = crate::MemoryStore::open_in_memory().unwrap();
        let symbolic_fts_exists: bool = store
            .connection()
            .query_row(
                "SELECT EXISTS (\
                     SELECT 1 FROM sqlite_master \
                     WHERE type = 'table' AND name = 'memories_symbolic_fts'\
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            symbolic_fts_exists,
            "this regression must exercise the symbolic trigram index, not its table-scan fallback"
        );
        let mut item = entry("insert-only-symbolic");
        item.text = "insertonlysymbolicneedle".to_string();

        assert_eq!(
            store.insert_if_absent(&item).unwrap(),
            InsertMemoryResult::Inserted
        );

        let hits = search_symbolic_candidates(
            store.connection(),
            "insertonlysymbolicneedle",
            10,
            false,
            false,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(
            hits.iter().any(|entry| entry.id == item.id),
            "an inserted row must be discoverable through the symbolic trigram path; got {:?}",
            hits.iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn explicit_id_update_clears_stale_idless_identity() {
        let mut store = crate::MemoryStore::open_in_memory().unwrap();
        assert_eq!(
            store
                .upsert_idless(&entry("shared-id"), "old-identity")
                .unwrap(),
            IdlessUpsertResult::Saved
        );

        let mut replacement = entry("shared-id");
        replacement.path = "/notes/replaced".to_string();
        replacement.text = "replacement explicit-id payload".to_string();
        store.upsert(&replacement).unwrap();

        assert_eq!(
            store
                .upsert_idless(&entry("new-id"), "old-identity")
                .unwrap(),
            IdlessUpsertResult::Saved,
            "an explicit-ID rewrite must release the obsolete id-less identity"
        );
        let old_identity: Option<String> = store
            .connection()
            .query_row(
                "SELECT idless_identity FROM memories WHERE id = 'shared-id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_identity, None);
    }

    /// kckylechen1/tachi#1167 review round 2: #1167 narrowed the write-time
    /// Jaccard near-duplicate guard from `if is_new` (origin/main) to
    /// `if is_new && idless_identity.is_none()`, which silently stopped
    /// merging 0.9<Jaccard<1.0 near-duplicates on the id-less save path (the
    /// MCP `save_memory` primary path). This is a discriminating test for
    /// that regression: it MUST fail against PR #1167's tip (`16755394`)
    /// before this round's fix and pass after.
    #[test]
    fn idless_save_near_duplicate_merges_into_existing_active_row() {
        let mut store = crate::MemoryStore::open_in_memory().unwrap();

        // 19 shared tokens + one differing tail token each => Jaccard is
        // set-based (intersection / union), not count-based: intersection =
        // 19, union = 19 shared + 2 unique tail tokens ("tango", "uniform")
        // = 21, so Jaccard = 19/21 ≈ 0.905, inside the (0.9, 1.0) exclusive
        // band — similar enough to merge, but NOT identical (so the
        // exact-identity unique index never fires; only the Jaccard path can
        // catch this).
        let shared = "alpha bravo charlie delta echo foxtrot golf hotel india juliet \
                       kilo lima mike november oscar papa quebec romeo sierra";
        let base_text = format!("{shared} tango");
        let near_dup_text = format!("{shared} uniform");
        assert!(jaccard_similarity(&base_text, &near_dup_text) > 0.9);
        assert!(jaccard_similarity(&base_text, &near_dup_text) < 1.0);

        let mut original = entry("near-dup-original");
        original.path = "/notes/near-dup".to_string();
        original.text = base_text;
        assert_eq!(
            store.upsert_idless(&original, "identity-original").unwrap(),
            IdlessUpsertResult::Saved
        );

        let mut near_dup = entry("near-dup-second");
        near_dup.path = "/notes/near-dup".to_string();
        near_dup.text = near_dup_text;
        // A distinct identity: this is not an exact path+text duplicate (the
        // unique index would not fire on it), only a Jaccard-similar one.
        assert_eq!(
            store.upsert_idless(&near_dup, "identity-near-dup").unwrap(),
            IdlessUpsertResult::Saved,
            "a Jaccard near-duplicate id-less save must still report Saved \
             — it is silently merged into the existing row, matching \
             origin/main's upsert() behavior for explicit-id near-duplicates"
        );

        let conn = store.connection();
        let active_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories
                 WHERE path = '/notes/near-dup' AND archived = 0 AND superseded_by IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            active_rows, 1,
            "the Jaccard near-duplicate must be merged/superseded into the \
             original row, not accumulate as a second active row"
        );

        let superseded_by: Option<String> = conn
            .query_row(
                "SELECT superseded_by FROM memories WHERE id = 'near-dup-second'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            superseded_by.as_deref(),
            Some("near-dup-original"),
            "the second save must be recorded as superseded by the original"
        );

        let superseded_identity: Option<String> = conn
            .query_row(
                "SELECT idless_identity FROM memories WHERE id = 'near-dup-second'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            superseded_identity, None,
            "a superseded row must release its identity claim so \
             idx_memories_idless_identity_active never has to reason about \
             a dead row holding a live-looking identity"
        );

        // The winner keeps its own identity untouched — it is addressed by
        // its own path+text, not by the entry that merged into it.
        let winner_identity: Option<String> = conn
            .query_row(
                "SELECT idless_identity FROM memories WHERE id = 'near-dup-original'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(winner_identity.as_deref(), Some("identity-original"));
    }

    /// #1331 BUG 2: id-less Jaccard early-return must sync the superseded
    /// loser into `memories_symbolic_fts` before commit so
    /// `include_superseded` symbolic recall sees it immediately.
    #[test]
    fn idless_jaccard_loser_is_symbolic_searchable_when_include_superseded() {
        let mut store = crate::MemoryStore::open_in_memory().unwrap();

        let shared = "alpha bravo charlie delta echo foxtrot golf hotel india juliet \
                       kilo lima mike november oscar papa quebec romeo sierra";
        let base_text = format!("{shared} tango");
        let near_dup_text = format!("{shared} uniformuniquesymbol");
        assert!(jaccard_similarity(&base_text, &near_dup_text) > 0.9);
        assert!(jaccard_similarity(&base_text, &near_dup_text) < 1.0);

        let mut original = entry("sym-sync-original");
        original.path = "/notes/sym-sync".to_string();
        original.text = base_text;
        assert_eq!(
            store
                .upsert_idless(&original, "identity-sym-original")
                .unwrap(),
            IdlessUpsertResult::Saved
        );

        let mut near_dup = entry("sym-sync-loser");
        near_dup.path = "/notes/sym-sync".to_string();
        near_dup.text = near_dup_text;
        assert_eq!(
            store
                .upsert_idless(&near_dup, "identity-sym-loser")
                .unwrap(),
            IdlessUpsertResult::Saved
        );

        let conn = store.connection();
        let superseded_by: Option<String> = conn
            .query_row(
                "SELECT superseded_by FROM memories WHERE id = 'sym-sync-loser'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(superseded_by.as_deref(), Some("sym-sync-original"));

        let in_symbolic: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories_symbolic_fts WHERE id = 'sym-sync-loser'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            in_symbolic, 1,
            "superseded Jaccard loser must be projected into memories_symbolic_fts \
             before the early-return commit"
        );

        let hits = search_symbolic_candidates(
            conn,
            "uniformuniquesymbol",
            10,
            false,
            true,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(
            hits.iter().any(|e| e.id == "sym-sync-loser"),
            "include_superseded symbolic recall must find the early-return loser; got {:?}",
            hits.iter().map(|e| e.id.as_str()).collect::<Vec<_>>()
        );
    }
}

/// Maximum IDs per batch for IN clause queries (SQLite has a 999 parameter limit).
const IN_BATCH_SIZE: usize = 900;

// ─── DELETE ───────────────────────────────────────────────────────────────────

/// Delete a memory entry by ID from main table, FTS index, and vector table.
/// Returns true if an entry was found and deleted.
pub fn delete(conn: &mut Connection, id: &str, vec_available: bool) -> Result<bool, MemoryError> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Err(MemoryError::InvalidArg("empty ID".to_string()));
    }

    let tx = conn.transaction()?;

    // Delete from main table and check if anything was actually removed
    tx.execute("DELETE FROM memories WHERE id = ?1", params![trimmed])?;
    let deleted = tx.changes() > 0;

    if deleted {
        // Clean up FTS index
        tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![trimmed])?;
        delete_memories_symbolic_fts(&tx, trimmed)?;

        if vec_available {
            tx.execute("DELETE FROM memories_vec WHERE id = ?1", params![trimmed])?;
        }

        // Clean up graph edges
        tx.execute(
            "DELETE FROM memory_edges WHERE source_id = ?1 OR target_id = ?1",
            params![trimmed],
        )?;

        // Clean up access history (CASCADE)
        tx.execute(
            "DELETE FROM access_history WHERE memory_id = ?1",
            params![trimmed],
        )?;

        // Clean up agent known state (CASCADE)
        tx.execute(
            "DELETE FROM agent_known_state WHERE memory_id = ?1",
            params![trimmed],
        )?;
    }

    tx.commit()?;
    Ok(deleted)
}

pub fn archive_memory(conn: &Connection, id: &str) -> Result<bool, MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        "UPDATE memories SET archived = 1, updated_at = ?1, revision = revision + 1 WHERE id = ?2 AND archived = 0",
        params![now, id],
    )?;
    Ok(conn.changes() > 0)
}

pub fn archive_memory_if_revision(
    conn: &Connection,
    id: &str,
    expected_revision: i64,
) -> Result<bool, MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        "UPDATE memories SET archived = 1, updated_at = ?1, revision = revision + 1 WHERE id = ?2 AND archived = 0 AND revision = ?3",
        params![now, id, expected_revision],
    )?;
    Ok(conn.changes() > 0)
}

pub fn restore_archived_if_revision(
    conn: &Connection,
    id: &str,
    expected_revision: i64,
) -> Result<bool, MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        "UPDATE memories SET archived = 0, updated_at = ?1, revision = revision + 1 WHERE id = ?2 AND archived = 1 AND revision = ?3",
        params![now, id, expected_revision],
    )?;
    Ok(conn.changes() > 0)
}

/// Mark a memory as superseded by a newer/canonical memory. Superseded rows are
/// hidden from default search but remain available for audit/history.
pub fn supersede_memory(
    conn: &Connection,
    id: &str,
    superseded_by: &str,
) -> Result<bool, MemoryError> {
    if id == superseded_by {
        return Ok(false);
    }
    let now = now_utc_iso();
    // Closing valid_until at supersession time turns the superseded row into a
    // point-in-time-recoverable version: `as_of` before `now` still returns it,
    // `as_of` after `now` correctly prefers the superseding row. COALESCE keeps
    // an explicitly-set validity window intact. NOTE: this invalidation is
    // specific to supersession; archive_memory (stale/dedup eviction) must NOT
    // close valid_until, since a GC'd fact may still have been true.
    conn.execute(
        "UPDATE memories
         SET superseded_by = ?1, updated_at = ?2, revision = revision + 1,
             valid_until = COALESCE(valid_until, ?2)
         WHERE id = ?3 AND superseded_by IS NULL",
        params![superseded_by, now, id],
    )?;
    Ok(conn.changes() > 0)
}

#[cfg(test)]
mod tests {
    use super::simple_query_input;

    #[test]
    fn simple_query_input_treats_fts_punctuation_as_separators() {
        let cases = [
            (")))", ""),
            ("!!!", ""),
            ("  foo  ", "foo"),
            ("foo_bar-baz.qux", "foo_bar-baz.qux"),
            ("don't \"panic\"", "don t panic"),
            ("foo)))bar", "foo bar"),
            ("spaced\twords\nok", "spaced words ok"),
            ("模型（搜索）", "模型 搜索"),
        ];

        for (input, expected) in cases {
            assert_eq!(simple_query_input(input), expected, "input={input:?}");
        }
    }
}
