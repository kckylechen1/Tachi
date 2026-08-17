use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{Map, Value};
use std::sync::{Mutex, MutexGuard};

use crate::db::StoreProfile;
use crate::error::MemoryError;
use crate::types::{default_retention_for, MemoryCategory, MemoryEntry, MemoryScope, MemorySource};

use super::common::{normalize_utc_iso, now_utc_iso, row_to_entry};
use super::sqlite_vec::serialize_f32;

mod access;
mod read;
mod search;
mod snapshot_import;
mod update;

#[cfg(test)]
pub(crate) use access::query_hash;
pub(crate) use access::record_access_with_updates;
pub use access::{
    access_event_density, get_access_times, get_use_access_times, record_memory_use,
    AccessEventDensity, AccessEventKind,
};
#[cfg(test)]
pub(crate) use access::{record_access, AccessUpdate};
pub use read::{
    fetch_by_ids, fetch_by_ids_excluding_store_internal, find_active_wiki_entry_by_path,
    find_exact_path_text_id, get_all, is_reserved_wiki_internal_path, is_user_facing_wiki_entry,
    list_active_wiki_ingest_predecessors, list_by_path, list_by_path_active_unsuperseded,
    list_by_path_recent, list_user_facing_wiki_entries, list_wiki_duplicate_candidates,
};
pub(crate) use search::search_fts_raw_match;
pub(crate) use search::search_symbolic_candidates_with_relevance;
pub(crate) use search::wiki_corpus_store_sql_splice;
pub use search::{
    search_fts, search_symbolic_candidates, search_vec, symbolic_trigram_select_sql,
    SYMBOLIC_TRIGRAM_SELECT_SQL_TEMPLATE,
};
/// tachi#1607 snapshot-import seam: transaction-scoped, no public signature
/// mentions a `Connection`/`Transaction` — the store-level
/// `MemoryStore::import_snapshot_batch` owns the transaction.
pub(crate) use snapshot_import::{
    import_snapshot_row_within_tx, memory_row_exists_within_tx,
    read_snapshot_lifecycle_row_within_tx, read_snapshot_vector_blob_within_tx,
    SnapshotLifecycleRow, SnapshotVectorRow,
};
#[cfg(any(test, feature = "test-support"))]
pub(crate) use update::supersede_with_metadata_if_expected_state;
pub(crate) use update::{
    archive_with_metadata_if_expected_state, restore_with_metadata_if_expected_state,
    update_with_revision_if_expected_state,
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

/// 28 columns. `row_to_entry` reads them by name, but
/// `memory_crud::read::get_many_with_vectors` appends `v.embedding` after this
/// list and reads it **by ordinal** — see [`MEMORY_EMBEDDING_COLUMN_INDEX`].
pub(crate) const MEMORY_SELECT_COLUMNS: &str = "id,path,summary,text,importance,timestamp,valid_from,valid_until,category,topic,keywords,'[]' AS persons,entities,'' AS location,source,scope,archived,access_count,scored_count,last_access,last_use_at,revision,metadata,retention_policy,domain,recall_count,query_diversity,tier";
pub(crate) const MEMORY_SELECT_COLUMNS_QUALIFIED: &str = "m.id,m.path,m.summary,m.text,m.importance,m.timestamp,m.valid_from,m.valid_until,m.category,m.topic,m.keywords,'[]' AS persons,m.entities,'' AS location,m.source,m.scope,m.archived,m.access_count,m.scored_count,m.last_access,m.last_use_at,m.revision,m.metadata,m.retention_policy,m.domain,m.recall_count,m.query_diversity,m.tier";

/// Ordinal of `v.embedding` when it is selected immediately after
/// [`MEMORY_SELECT_COLUMNS_QUALIFIED`]: the count of columns in that list.
/// Named so the coupling is visible from the constant it depends on — before
/// tachi#1446 this was a bare `26` literal at the read site, three files away
/// from the string it counts.
pub(crate) const MEMORY_EMBEDDING_COLUMN_INDEX: usize = 28;

/// The bare `memories` column names [`MEMORY_SELECT_COLUMNS`] requires a table
/// to actually carry.
///
/// Two entries in that list are synthesized by the SELECT itself rather than
/// read from the table — `'[]' AS persons` and `'' AS location`, both relics of
/// dropped physical columns — so they are filtered out here. Everything else is
/// a hard requirement: name one of them in a query against a table that lacks
/// it and SQLite fails the whole statement with `no such column`.
#[cfg(test)]
pub(crate) fn memory_select_required_columns() -> Vec<&'static str> {
    MEMORY_SELECT_COLUMNS
        .split(',')
        .map(str::trim)
        .filter(|column| !column.contains(" AS "))
        .collect()
}

/// Fail loudly, by name, when a `memories` table has fallen behind
/// [`MEMORY_SELECT_COLUMNS`] — tachi#1446.
///
/// # Why this exists
///
/// Adding `last_use_at` to the select list broke
/// `symbolic_pre_cap_legacy_null_json_columns_fall_back_to_empty_arrays`, a
/// test in an unrelated suite that hand-writes its own `CREATE TABLE memories`
/// and never runs `init_schema`. The symptom was a raw
/// `Sqlite(... "no such column: last_use_at")` several files away from the
/// change that caused it, and — because the suite runs fail-fast — it cancelled
/// 113 later tests, hiding whether there were siblings.
///
/// A hand-built fixture is sometimes the *correct* choice: that test needs NULL
/// JSON columns, which the current DDL forbids and which
/// `rebuild_memories_with_check_constraints` would reject outright
/// (`memories_new.keywords` is `TEXT NOT NULL`). So the fix is not "always run
/// `init_schema`" — it is to make the drift say what it is. Call this
/// immediately after building such a fixture; the panic then names the missing
/// columns and the fixture, instead of surfacing as a query error.
///
/// The paired guard for the other direction — a column named in the select list
/// but missing from the *production* schema — is
/// `every_selected_memory_column_exists_on_every_init_path` in
/// `db/schema/migration_tests.rs`.
///
/// # Residual gap, stated rather than papered over
///
/// This is opt-in: a *newly written* hand-built fixture that forgets to call it
/// is not covered. A source-scanning test could close that, but scanning Rust
/// source text for `CREATE TABLE memories` is brittle against formatting and
/// would not reach sibling crates, so it is deliberately not done. Instead, the
/// predicate for re-finding candidates is written down here: a function that
/// opens a raw `Connection`, executes a `CREATE TABLE memories` naming ~20 or
/// more columns, and calls a reader built on `MEMORY_SELECT_COLUMNS`
/// (`hybrid_search`, `search_symbolic_candidates`, `fetch_by_ids`, anything
/// funnelling into `row_to_entry`) **without** `init_schema` / `MemoryStore::open`
/// / a `setup()`-style helper in between. As of tachi#1446 that predicate
/// matched exactly one function in the whole workspace: the caller below.
#[cfg(test)]
pub(crate) fn assert_memories_fixture_matches_select_columns(conn: &Connection, fixture: &str) {
    let mut stmt = conn
        .prepare("PRAGMA table_info(memories)")
        .expect("PRAGMA table_info(memories) must prepare");
    let present: std::collections::BTreeSet<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .expect("PRAGMA table_info(memories) must run")
        .collect::<Result<_, _>>()
        .expect("PRAGMA table_info(memories) must decode");

    let missing: Vec<&str> = memory_select_required_columns()
        .into_iter()
        .filter(|column| !present.contains(*column))
        .collect();

    assert!(
        missing.is_empty(),
        "hand-built `memories` fixture `{fixture}` has drifted behind MEMORY_SELECT_COLUMNS: \
         missing {missing:?}. Add the column(s) to that fixture's CREATE TABLE — NOT to its \
         INSERT list, and do NOT switch the fixture to init_schema (the rebuild in \
         `rebuild_memories_with_check_constraints` would destroy whatever legacy shape the \
         fixture exists to exercise)."
    );
}

static FTS_SYNC_LOCK: Mutex<()> = Mutex::new(());

fn acquire_fts_sync_lock() -> MutexGuard<'static, ()> {
    FTS_SYNC_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn sync_memories_fts(
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
    let Some(cand_id) = find_jaccard_candidate_within_tx(tx, entry)? else {
        return Ok(None);
    };
    merge_jaccard_candidate_within_tx(tx, entry, importance, write_time_utc, &cand_id)?;
    Ok(Some(cand_id))
}

pub(crate) fn find_jaccard_candidate_within_tx(
    tx: &rusqlite::Transaction<'_>,
    entry: &MemoryEntry,
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
               AND m.id NOT LIKE 'wiki-rem:%'
             LIMIT 5",
        )?;
        let rows = stmt.query_map(params![safe_query, entry.id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<Result<_, _>>()?
    };
    for (cand_id, cand_text) in fts_candidates {
        if jaccard_similarity(&entry.text, &cand_text) > 0.9 {
            return Ok(Some(cand_id));
        }
    }
    Ok(None)
}

pub(crate) fn merge_jaccard_candidate_within_tx(
    tx: &rusqlite::Transaction<'_>,
    entry: &MemoryEntry,
    importance: f64,
    write_time_utc: &str,
    cand_id: &str,
) -> Result<(), MemoryError> {
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
        let mut ents: Vec<String> = serde_json::from_str(&cand_ents_json).unwrap_or_default();
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
        cand_id,
        &cand_path,
        &cand_summary,
        &cand_text,
        &kws_joined,
        &ents_joined,
    )?;
    Ok(())
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
pub(crate) fn canonical_entities_json(entry: &MemoryEntry) -> Result<String, MemoryError> {
    let mut entities = entry.entities.clone();
    crate::types::fold_person_names_into_entities(&mut entities, entry.persons.clone());
    Ok(serde_json::to_string(&entities)?)
}

// ─── UPSERT ───────────────────────────────────────────────────────────────────

/// Whether an upsert may fold write-time near-duplicate content into an
/// existing active row instead of writing the row the caller asked for.
///
/// kckylechen1/tachi#1634 (the #1632 P0 leaf): ordinary upsert used to run
/// [`merge_into_jaccard_candidate`] unconditionally on every new-id write,
/// silently merging any two >0.9 token-Jaccard-similar rows into one
/// regardless of caller intent — an explicit-id write, a fresh `upsert_batch`
/// row, and a tidy-migration copy could all vanish into an unrelated row's
/// `keywords`/`entities` with no signal to the caller beyond a normal
/// `Saved`/`Ok` result. The owner ruling for #1634 (Option A) keeps that
/// merge behavior, but only as an explicit, typed opt-in reserved for
/// id-less `save_memory` (`upsert_idless_save_entry`); every other upsert
/// seam defaults to [`Self::NonSemantic`].
///
/// Known intentional `AllowNearDuplicateMerge` consumers (the #1634 census's
/// "depends on merging" list): production id-less `save_memory`
/// (`upsert_idless_save_entry`), plus two memcore-level tests of the merge
/// machinery itself — `search_ops::jaccard_dedup_refreshes_candidate_fts`
/// and `search_ops::upsert_jaccard_dedup_returns_fts_row_decode_errors`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NearDuplicatePolicy {
    /// Write exactly the row the caller asked for. Never search for or fold
    /// into a near-duplicate. The default for every upsert seam.
    #[default]
    NonSemantic,
    /// Run the write-time Jaccard near-duplicate search
    /// ([`merge_into_jaccard_candidate`]) and, when a >0.9-similar active row
    /// exists, fold into it instead of writing a new row. Reserved for
    /// id-less `save_memory` (kckylechen1/tachi#1634 owner ruling).
    AllowNearDuplicateMerge,
}

impl NearDuplicatePolicy {
    fn allows_merge(self) -> bool {
        matches!(self, Self::AllowNearDuplicateMerge)
    }
}

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
const RESERVED_REM_KEY: &str = "rem";
const RESERVED_WIKI_LOG_KEY: &str = "wiki_log";

fn strip_untrusted_reference_metadata(metadata: &Value) -> Value {
    let Some(mut object) = metadata.as_object().cloned() else {
        return metadata.clone();
    };
    for key in RESERVED_REFERENCE_KEYS {
        object.remove(key);
    }
    Value::Object(object)
}

fn strip_untrusted_reserved_metadata(metadata: &Value) -> Value {
    let mut sanitized = strip_untrusted_reference_metadata(metadata);
    if let Some(object) = sanitized.as_object_mut() {
        object.remove(RESERVED_REM_KEY);
        object.remove(RESERVED_WIKI_LOG_KEY);
    }
    sanitized
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
    // REM state is written only by the dedicated insert-once operation and
    // source-marker seams. Ordinary upsert may preserve an existing value
    // below, but it may never mint or replace one from its input payload.
    let Some(existing) = read_existing_metadata(tx, entry_id)? else {
        return Ok(sanitized);
    };
    let Some(existing_object) = existing.as_object() else {
        return Ok(sanitized);
    };
    let reserved = RESERVED_REFERENCE_KEYS
        .into_iter()
        .chain([RESERVED_REM_KEY, RESERVED_WIKI_LOG_KEY])
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

impl crate::MemoryStore {
    /// Save through the normal upsert body while atomically preserving and
    /// mutating reserved reference metadata. The validated mutations and
    /// metadata patch are separate arguments so caller-controlled metadata
    /// cannot impersonate an authorized server reference write.
    ///
    /// tachi#1634: always writes with [`NearDuplicatePolicy::NonSemantic`] —
    /// this wrapper has many callers across the crate and none of them are
    /// the id-less `save_memory` opt-in, so it never needs the merge
    /// behavior. Callers that must choose the policy explicitly (today, only
    /// `upsert_idless_save_entry`) call
    /// [`Self::upsert_with_validated_reference_mutations_and_metadata_removals`]
    /// directly.
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
            NearDuplicatePolicy::NonSemantic,
        )
    }

    /// Trusted metadata-removal counterpart used when a server-side policy
    /// must atomically delete caller-forged authority while preserving typed
    /// reference metadata. Reserved reference keys cannot be removed here.
    ///
    /// `policy` is a required argument (tachi#1634), not a default, so every
    /// call site states its near-duplicate-merge intent explicitly instead of
    /// inheriting whatever the shared upsert body happened to hardcode.
    pub fn upsert_with_validated_reference_mutations_and_metadata_removals(
        &mut self,
        entry: &MemoryEntry,
        idless_identity: Option<&str>,
        metadata_patch: &Map<String, Value>,
        metadata_removals: &[&str],
        mutations: &[ValidatedReferenceMutation],
        policy: NearDuplicatePolicy,
    ) -> Result<(IdlessUpsertResult, Value), MemoryError> {
        self.validate_write_path(entry)?;

        if matches!(policy, NearDuplicatePolicy::AllowNearDuplicateMerge) {
            let identity = idless_identity.ok_or_else(|| {
                MemoryError::InvalidArg(
                    "near-duplicate merge requires an id-less identity".to_string(),
                )
            })?;
            let identity = identity.to_string();
            return self.with_immutable_supersession_transaction(|replacement| {
                replacement.upsert_idless_with_near_duplicate_merge(
                    entry,
                    &identity,
                    metadata_patch,
                    metadata_removals,
                    mutations,
                )
            });
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
                policy,
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
        self.validate_write_path(entry)?;
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
                    false,
                )
            },
        )
    }

    /// Dedicated internal writer for the reserved Wiki operation-log row.
    /// Ordinary/public upsert cannot mint this identity or visibility marker.
    pub fn upsert_wiki_operation_log(&mut self, entry: &MemoryEntry) -> Result<(), MemoryError> {
        if entry.id != "wiki-operation-log"
            || entry.path != "/wiki/_log"
            || !entry.topic.eq_ignore_ascii_case("wiki_log")
        {
            return Err(MemoryError::InvalidArg(
                "trusted Wiki log write requires the reserved operation-log identity".to_string(),
            ));
        }
        let db_label = self.db_label.clone();
        let vec_available = self.vec_available;
        let authorization = self.reserved_reference_write.clone();
        crate::db::retry_memory_locked("upsert_wiki_operation_log", &db_label, || {
            let _authorization = crate::db::authorize_reserved_reference_write(&authorization)?;
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut trusted = entry.clone();
            trusted.metadata = merge_ordinary_reserved_metadata(&tx, &entry.id, &entry.metadata)?;
            let object = trusted.metadata.as_object_mut().ok_or_else(|| {
                MemoryError::InvalidArg("Wiki operation-log metadata must be an object".to_string())
            })?;
            object.insert(RESERVED_WIKI_LOG_KEY.to_string(), Value::Bool(true));
            upsert_prepared_within_tx(
                &tx,
                &trusted,
                vec_available,
                None,
                NearDuplicatePolicy::NonSemantic,
                true,
                false,
            )?;
            tx.commit()?;
            Ok(())
        })
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
        if key != "evidence_refs_v1"
            && key != "source_refs"
            && key != RESERVED_REM_KEY
            && key != RESERVED_WIKI_LOG_KEY
        {
            merged.insert(key.clone(), value.clone());
        }
    }
    for key in metadata_removals {
        if RESERVED_REFERENCE_KEYS.contains(key) {
            return Err(MemoryError::InvalidArg(format!(
                "trusted metadata removal cannot delete reserved reference key '{key}'"
            )));
        }
        if *key == RESERVED_REM_KEY {
            return Err(MemoryError::InvalidArg(
                "trusted metadata removal cannot delete reserved REM state".to_string(),
            ));
        }
        if *key == RESERVED_WIKI_LOG_KEY {
            return Err(MemoryError::InvalidArg(
                "trusted metadata removal cannot delete reserved Wiki log state".to_string(),
            ));
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

#[allow(clippy::too_many_arguments)] // fixed transaction seam; grouping these trust channels would blur them
fn upsert_with_validated_reference_mutations(
    conn: &mut Connection,
    entry: &MemoryEntry,
    vec_available: bool,
    idless_identity: Option<&str>,
    metadata_patch: &Map<String, Value>,
    metadata_removals: &[&str],
    mutations: &[ValidatedReferenceMutation],
    policy: NearDuplicatePolicy,
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
        policy,
    )?;
    tx.commit()?;
    Ok(result)
}

#[allow(clippy::too_many_arguments)] // fixed transaction seam; grouping these trust channels would blur them
pub(crate) fn upsert_with_validated_reference_mutations_within_tx_and_metadata_removals(
    tx: &rusqlite::Transaction<'_>,
    entry: &MemoryEntry,
    vec_available: bool,
    idless_identity: Option<&str>,
    metadata_patch: &Map<String, Value>,
    metadata_removals: &[&str],
    mutations: &[ValidatedReferenceMutation],
    policy: NearDuplicatePolicy,
) -> Result<(IdlessUpsertResult, Value), MemoryError> {
    let mut merged_entry = entry.clone();
    merged_entry.metadata = merge_validated_reference_metadata(
        tx,
        &entry.id,
        metadata_patch,
        metadata_removals,
        mutations,
    )?;
    let result = upsert_prepared_within_tx(
        tx,
        &merged_entry,
        vec_available,
        idless_identity,
        policy,
        false,
        false,
    )?;
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
            scored_count: 0,
            last_access: None,
            last_use_at: None,
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
                "source_refs": [{ "target_ref": "#998" }],
                "wiki_log": true
            }),
        );
        store.upsert(&hostile).expect("ordinary create");

        let stored = store.get(&hostile.id).unwrap().unwrap();
        assert_eq!(stored.metadata["kept"], json!(true));
        assert!(stored.metadata.get("evidence_refs_v1").is_none());
        assert!(stored.metadata.get("source_refs").is_none());
        assert!(stored.metadata.get("wiki_log").is_none());
    }

    #[test]
    fn only_trusted_wiki_log_seam_can_mint_log_authority() {
        let dir = tempfile::tempdir().expect("temp db dir");
        let path = dir.path().join("wiki.db");
        let mut store = crate::MemoryStore::open_with_label(&path.to_string_lossy(), "wiki")
            .expect("open Wiki store");
        let mut log = entry("wiki-operation-log", json!({"caller": "internal"}));
        log.path = "/wiki/_log".to_string();
        log.topic = "wiki_log".to_string();
        assert!(store.upsert(&log).is_err());
        store
            .upsert_wiki_operation_log(&log)
            .expect("trusted Wiki log write");
        let stored = store.get(&log.id).unwrap().unwrap();
        assert_eq!(stored.metadata["wiki_log"], json!(true));

        let mut hostile_update = log.clone();
        hostile_update.metadata = json!({"wiki_log": false, "caller": "public"});
        assert!(store.upsert(&hostile_update).is_err());
        let preserved = store.get(&log.id).unwrap().unwrap();
        assert_eq!(preserved.metadata["wiki_log"], json!(true));
        assert_eq!(preserved.metadata["caller"], json!("internal"));
    }

    #[test]
    fn ordinary_store_writes_reject_canonical_wiki_log_path_aliases() {
        let aliases = ["/wiki//_log", "/wiki//_log/spoof", "/Wiki/_log"];
        for (index, alias) in aliases.into_iter().enumerate() {
            let (_dir, mut store) = open_store();
            let mut candidate = entry(&format!("ordinary-wiki-log-alias-{index}"), json!({}));
            candidate.path = alias.to_string();

            let upsert_error = store
                .upsert(&candidate)
                .expect_err("ordinary upsert must reject a normalized Wiki log path alias");
            assert!(
                upsert_error
                    .to_string()
                    .contains("Wiki operation-log identity is reserved"),
                "unexpected upsert refusal for {alias}: {upsert_error}"
            );

            let insert_error = store
                .insert_if_absent(&candidate)
                .expect_err("insert-if-absent must reject a normalized Wiki log path alias");
            assert!(
                insert_error
                    .to_string()
                    .contains("Wiki operation-log identity is reserved"),
                "unexpected insert refusal for {alias}: {insert_error}"
            );
            assert!(store.get(&candidate.id).unwrap().is_none());
        }
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
                "source_refs": ["#998"],
                "rem": {
                    "processed": 1,
                    "processed_revision": 1,
                    "processed_by": "wiki-rem:forged"
                }
            }),
        );
        store.upsert(&hostile).expect("ordinary hostile update");

        let stored = store.get(&hostile.id).unwrap().unwrap();
        assert_eq!(refs(&stored), vec!["#100"]);
        assert_eq!(stored.metadata["kept"], json!(true));
        assert!(stored.metadata.get("source_refs").is_none());
        assert!(stored.metadata.get("rem").is_none());
    }

    #[test]
    fn ordinary_store_upsert_cannot_mint_or_replace_reserved_rem_state() {
        let (_dir, mut store) = open_store();
        let forged = entry(
            "reserved-rem-boundary",
            json!({"rem": {"processed": 0, "processed_by": "forged"}}),
        );
        store.upsert(&forged).expect("ordinary insert");
        let inserted = store.get(&forged.id).unwrap().unwrap();
        assert!(inserted.metadata.get("rem").is_none());

        store
            .mark_rem_processed_for_draft_at_revisions(
                &[(inserted.id.clone(), inserted.revision)],
                "2026-07-31T01:00:00Z",
                "wiki-rem:trusted",
            )
            .expect("trusted REM marker");
        let hostile = entry(
            "reserved-rem-boundary",
            json!({
                "ordinary": true,
                "rem": {"processed": 0, "processed_by": "replaced"}
            }),
        );
        store.upsert(&hostile).expect("ordinary hostile update");

        let stored = store.get(&hostile.id).unwrap().unwrap();
        assert_eq!(stored.metadata["ordinary"], true);
        assert_eq!(stored.metadata["rem"]["processed"], 1);
        assert_eq!(stored.metadata["rem"]["processed_by"], "wiki-rem:trusted");
        assert_eq!(
            stored.metadata["rem"]["processed_revision"],
            inserted.revision
        );
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
                    NearDuplicatePolicy::NonSemantic,
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
                "source_refs": ["#998"],
                "rem": {
                    "processed": 1,
                    "processed_revision": 1,
                    "processed_by": "wiki-rem:forged"
                }
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
        assert!(stored.metadata.get("rem").is_none());
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
    insert_if_absent_with_reference_mutations(conn, entry, vec_available, None, &[], false)
}

fn insert_if_absent_with_reference_mutations(
    conn: &mut Connection,
    entry: &MemoryEntry,
    vec_available: bool,
    metadata_patch: Option<&Map<String, Value>>,
    mutations: &[ValidatedReferenceMutation],
    allow_reserved_rem: bool,
) -> Result<InsertMemoryResult, MemoryError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let result = insert_if_absent_with_reference_mutations_within_tx(
        &tx,
        entry,
        vec_available,
        metadata_patch,
        mutations,
        allow_reserved_rem,
    )?;
    tx.commit()?;
    Ok(result)
}

/// Caller-transaction insert-only body. Unlike [`upsert_within_tx`], this
/// never invokes write-time Jaccard merging and never rewrites an existing id.
pub(crate) fn insert_if_absent_within_tx(
    tx: &rusqlite::Transaction<'_>,
    entry: &MemoryEntry,
    vec_available: bool,
) -> Result<InsertMemoryResult, MemoryError> {
    insert_if_absent_with_reference_mutations_within_tx(tx, entry, vec_available, None, &[], false)
}

pub(crate) fn insert_rem_operation_if_absent_within_tx(
    tx: &rusqlite::Transaction<'_>,
    entry: &MemoryEntry,
    vec_available: bool,
) -> Result<InsertMemoryResult, MemoryError> {
    insert_if_absent_with_reference_mutations_within_tx(tx, entry, vec_available, None, &[], true)
}

fn insert_if_absent_with_reference_mutations_within_tx(
    tx: &rusqlite::Transaction<'_>,
    entry: &MemoryEntry,
    vec_available: bool,
    metadata_patch: Option<&Map<String, Value>>,
    mutations: &[ValidatedReferenceMutation],
    allow_reserved_rem: bool,
) -> Result<InsertMemoryResult, MemoryError> {
    if entry.id.trim().is_empty() || entry.id.starts_with("anchor:") {
        return Err(MemoryError::InvalidArg(
            "entry.id must be non-empty and outside the reserved 'anchor:' namespace".to_string(),
        ));
    }
    let path = crate::path_router::normalize_path(&entry.path);
    if entry.id == "wiki-operation-log"
        || path == "/wiki/_log"
        || path.starts_with("/wiki/_log/")
        || entry.topic.eq_ignore_ascii_case("wiki_log")
    {
        return Err(MemoryError::InvalidArg(
            "Wiki operation-log identity is reserved; use the trusted Wiki log seam".to_string(),
        ));
    }
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
    let mut metadata = match metadata_patch {
        Some(metadata_patch) => {
            merge_validated_reference_metadata(tx, &entry.id, metadata_patch, &[], mutations)?
        }
        None if allow_reserved_rem => strip_untrusted_reference_metadata(&entry.metadata),
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
        return Ok(InsertMemoryResult::Existing);
    }
    let kws = entry.keywords.join(" ");
    let mut entities = entry.entities.clone();
    crate::types::fold_person_names_into_entities(&mut entities, entry.persons.clone());
    sync_memories_fts(
        tx,
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
    // Both refusals below are now also enforced at the shared transactional
    // seam (`upsert_prepared_within_tx`, tachi#1602) so batch paths refuse
    // identically; they are kept here as a cheap pre-transaction check that
    // never opens a writer transaction for a doomed entry.
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
///
/// Since tachi#1602 that shared body also carries the blank-id and reserved
/// `anchor:`-namespace refusals, so every caller of this seam — including the
/// batch paths — refuses exactly what single-row `upsert` refuses.
pub(crate) fn upsert_within_tx(
    tx: &rusqlite::Transaction<'_>,
    entry: &MemoryEntry,
    vec_available: bool,
    idless_identity: Option<&str>,
) -> Result<IdlessUpsertResult, MemoryError> {
    // tachi#1634: every caller of this seam (ordinary `upsert`, `upsert_idless`,
    // both batch paths, lifecycle-apply, snapshot import) is NonSemantic —
    // the sole near-duplicate-merge opt-in is id-less `save_memory`, which
    // goes through `upsert_with_validated_reference_mutations_and_metadata_removals`,
    // not this seam.
    upsert_within_tx_inner(
        tx,
        entry,
        vec_available,
        idless_identity,
        false,
        NearDuplicatePolicy::NonSemantic,
    )
}

/// Trusted whole-store-copy variant of [`upsert_within_tx`]: identical body,
/// except rows already living in the reserved `anchor:` id namespace are
/// copied verbatim instead of refused (tachi#1602).
///
/// This is the anchor analogue of `MemoryStore::upsert_wiki_operation_log`'s
/// `allow_wiki_operation_log` channel: the refusal is enforced at the shared
/// seam for every ordinary caller, and exactly one named internal writer opts
/// out. Its only production caller is tachi-server's tidy migration, which
/// copies every row of a source database into the target
/// (`read_source_entries` selects `FROM memories` unfiltered, so an anchor
/// row in the source is a legitimate row to carry, not a hostile write).
/// Ordinary callers — including `MemoryStore::upsert`, `upsert_batch`,
/// `upsert_batch_with_precommit`, and lifecycle-apply — must keep using
/// [`upsert_within_tx`], which refuses.
pub(crate) fn upsert_within_tx_allowing_reserved_anchor_ids(
    tx: &rusqlite::Transaction<'_>,
    entry: &MemoryEntry,
    vec_available: bool,
    idless_identity: Option<&str>,
) -> Result<IdlessUpsertResult, MemoryError> {
    upsert_within_tx_inner(
        tx,
        entry,
        vec_available,
        idless_identity,
        true,
        NearDuplicatePolicy::NonSemantic,
    )
}

fn upsert_within_tx_inner(
    tx: &rusqlite::Transaction<'_>,
    entry: &MemoryEntry,
    vec_available: bool,
    idless_identity: Option<&str>,
    allow_reserved_anchor_id: bool,
    policy: NearDuplicatePolicy,
) -> Result<IdlessUpsertResult, MemoryError> {
    let mut sanitized = entry.clone();
    sanitized.metadata = merge_ordinary_reserved_metadata(tx, &entry.id, &entry.metadata)?;
    upsert_prepared_within_tx(
        tx,
        &sanitized,
        vec_available,
        idless_identity,
        policy,
        false,
        allow_reserved_anchor_id,
    )
}

/// The reserved-identity refusals every main-row writer must run, in one
/// body so no writer can carry a drifted copy.
///
/// tachi#1602: the blank-id and reserved-`anchor:`-namespace refusals live
/// here, at the shared transactional seam, not only at
/// `upsert_with_idless_identity`'s top-level entry point. Every write path
/// that reaches a main row — single-row `upsert`/`upsert_idless`,
/// lifecycle-apply, immutable supersession, both batch paths (`upsert_batch`,
/// admin-gated `upsert_batch_with_precommit`) and, since tachi#1607, the
/// snapshot-import path — funnels through this function, so enforcing here is
/// what makes every entry point refuse identically. Error text is
/// byte-identical to the top-level guard's so callers cannot tell which layer
/// refused.
///
/// `normalized_path` must be the value the caller is about to *write* (i.e.
/// already through [`crate::path_router::normalize_path`]); validating one
/// path and storing another would be a bypass of the Wiki-log guard below.
pub(crate) fn refuse_reserved_write_identity(
    id: &str,
    normalized_path: &str,
    topic: &str,
    allow_reserved_anchor_id: bool,
    allow_wiki_operation_log: bool,
) -> Result<(), MemoryError> {
    if id.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "entry.id must be provided by caller".to_string(),
        ));
    }
    // tachi#773 item 4 guard (c): `ensure_anchor` (memcore::db::anchor) owns
    // the `anchor:` namespace via its own `INSERT OR IGNORE`; an ordinary
    // upsert reaching here with an `anchor:`-prefixed id would create or
    // silently overwrite an anchor row through `ON CONFLICT DO UPDATE`. The
    // sole opt-out is `upsert_within_tx_allowing_reserved_anchor_ids`, the
    // trusted whole-store-copy seam used by tidy migration.
    if !allow_reserved_anchor_id && id.starts_with("anchor:") {
        return Err(MemoryError::InvalidArg(format!(
            "id '{id}' is in the reserved 'anchor:' namespace; use ensure_anchor, not upsert"
        )));
    }
    // `wiki-rem:` rows are deterministic insert-once operation records. They
    // are created only through the REM claim + insert_if_absent transaction;
    // allowing ordinary ON CONFLICT upsert would let any caller rewrite the
    // recovery identity, producer receipt, or active winner in place.
    if crate::namespace::is_reserved_wiki_rem_id(id) {
        return Err(MemoryError::InvalidArg(format!(
            "id '{id}' is in the reserved 'wiki-rem:' namespace; use the REM insert-once operation seam, not upsert"
        )));
    }
    if !allow_wiki_operation_log
        && (id == "wiki-operation-log"
            || normalized_path == "/wiki/_log"
            || normalized_path.starts_with("/wiki/_log/")
            || topic.eq_ignore_ascii_case("wiki_log"))
    {
        return Err(MemoryError::InvalidArg(
            "Wiki operation-log identity is reserved; use the trusted Wiki log seam".to_string(),
        ));
    }
    Ok(())
}

fn upsert_prepared_within_tx(
    tx: &rusqlite::Transaction<'_>,
    entry: &MemoryEntry,
    vec_available: bool,
    idless_identity: Option<&str>,
    policy: NearDuplicatePolicy,
    allow_wiki_operation_log: bool,
    allow_reserved_anchor_id: bool,
) -> Result<IdlessUpsertResult, MemoryError> {
    let path = crate::path_router::normalize_path(&entry.path);
    refuse_reserved_write_identity(
        &entry.id,
        &path,
        &entry.topic,
        allow_reserved_anchor_id,
        allow_wiki_operation_log,
    )?;
    // Normalize only the fields enforced by CHECK constraints; avoid cloning
    // the full entry/vector on the hot write path.
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

    if policy.allows_merge() && is_new && idless_identity.is_none() {
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

    // Write to main table.
    //
    // tachi#1459, for anyone enumerating writers of the access counters: this
    // statement carries the caller's `MemoryEntry` values through, and on
    // conflict `access_count` / `last_access` take the incoming value rather
    // than the stored one. It is a carry-through, not an observation of a read
    // — the only production code that *earns* those counters is
    // `record_access_with_updates`, reached only from `hybrid_search`. A caller
    // that re-saves an existing id from a default-constructed entry therefore
    // resets the counter to that entry's value; `tier` below is deliberately
    // guarded against exactly that kind of downgrade, and these two are not.
    // `recall_count` / `query_diversity` are set on insert and left alone on
    // conflict — they appear in the column list but not in the update set.
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
    if policy.allows_merge() && idless_identity.is_some() {
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
            scored_count: 0,
            last_access: None,
            last_use_at: None,
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

    /// tachi#1634: `MemoryStore::upsert_idless` now writes with
    /// [`NearDuplicatePolicy::NonSemantic`] like every other upsert seam, so
    /// tests that specifically exercise the write-time Jaccard near-duplicate
    /// merge on the id-less path must opt in explicitly, the same way
    /// `upsert_idless_save_entry` (the production id-less `save_memory`
    /// opt-in) does — by going around `MemoryStore::upsert_idless` and
    /// driving the production transaction seam with
    /// [`NearDuplicatePolicy::AllowNearDuplicateMerge`].
    fn upsert_idless_allowing_merge(
        store: &mut crate::MemoryStore,
        entry: &MemoryEntry,
        identity: &str,
    ) -> IdlessUpsertResult {
        store
            .upsert_with_validated_reference_mutations_and_metadata_removals(
                entry,
                Some(identity),
                &Map::new(),
                &[],
                &[],
                NearDuplicatePolicy::AllowNearDuplicateMerge,
            )
            .unwrap()
            .0
    }

    /// A `(base, near_dup)` text pair with 0.9 < Jaccard < 1.0 similarity —
    /// similar enough for [`merge_into_jaccard_candidate`] to consider
    /// merging, but not an exact-identity duplicate. Shared by the
    /// tachi#1634 discriminator tests below so each one only has to state
    /// which seam it is exercising, not re-derive the similarity band.
    fn near_duplicate_text_pair() -> (String, String) {
        let shared = "alpha bravo charlie delta echo foxtrot golf hotel india juliet \
                       kilo lima mike november oscar papa quebec romeo sierra";
        let base_text = format!("{shared} tango");
        let near_dup_text = format!("{shared} uniform");
        assert!(jaccard_similarity(&base_text, &near_dup_text) > 0.9);
        assert!(jaccard_similarity(&base_text, &near_dup_text) < 1.0);
        (base_text, near_dup_text)
    }

    /// Assert that both rows under `path` are active and unsuperseded — the
    /// NonSemantic-default shape every tachi#1634 discriminator below must
    /// land in.
    fn assert_near_duplicates_both_survive(store: &crate::MemoryStore, path: &str, loser_id: &str) {
        let conn = store.connection();
        let active_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories \
                 WHERE path = ?1 AND archived = 0 AND superseded_by IS NULL",
                [path],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            active_rows, 2,
            "both near-duplicate rows under {path} must stay active under NearDuplicatePolicy::NonSemantic"
        );
        let superseded_by: Option<String> = conn
            .query_row(
                "SELECT superseded_by FROM memories WHERE id = ?1",
                [loser_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            superseded_by, None,
            "{loser_id} must not be superseded — near-duplicate detection must not run at all under NonSemantic"
        );
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
            false,
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
    ///
    /// kckylechen1/tachi#1634: near-duplicate merging is now an explicit,
    /// typed opt-in ([`NearDuplicatePolicy::AllowNearDuplicateMerge`])
    /// reserved for id-less `save_memory`, not `MemoryStore::upsert_idless`'s
    /// default. This test drives that opt-in directly via
    /// `upsert_idless_allowing_merge` to keep documenting the merge behavior
    /// itself; the assertions are unchanged.
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
            upsert_idless_allowing_merge(&mut store, &original, "identity-original"),
            IdlessUpsertResult::Saved
        );

        let mut near_dup = entry("near-dup-second");
        near_dup.path = "/notes/near-dup".to_string();
        near_dup.text = near_dup_text;
        // A distinct identity: this is not an exact path+text duplicate (the
        // unique index would not fire on it), only a Jaccard-similar one.
        assert_eq!(
            upsert_idless_allowing_merge(&mut store, &near_dup, "identity-near-dup"),
            IdlessUpsertResult::Saved,
            "a Jaccard near-duplicate id-less save under \
             NearDuplicatePolicy::AllowNearDuplicateMerge must still report \
             Saved — it is silently merged into the existing row"
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

        let canonical_edge: (String, String) = conn
            .query_row(
                "SELECT json_extract(metadata, '$.source'),
                        json_extract(metadata, '$.authority')
                 FROM memory_edges
                 WHERE source_id='near-dup-original'
                   AND target_id='near-dup-second'
                   AND relation='supersedes'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("canonical winner-to-loser supersedes edge");
        assert_eq!(
            canonical_edge,
            (
                "idless_near_duplicate".to_string(),
                "structural_bookkeeping".to_string()
            )
        );
        let receipt_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM hard_state
                 WHERE namespace = ?1
                   AND json_extract(value_json, '$.source_id') = 'near-dup-second'
                   AND json_extract(value_json, '$.target_id') = 'near-dup-original'",
                [crate::SUPERSESSION_RECEIPT_NAMESPACE],
                |row| row.get(0),
            )
            .unwrap();
        let event_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tachi_events
                 WHERE event_type = ?1
                   AND json_extract(payload_json, '$.source_id') = 'near-dup-second'
                   AND json_extract(payload_json, '$.target_id') = 'near-dup-original'",
                [crate::SUPERSESSION_RECEIPT_EVENT_TYPE],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!((receipt_count, event_count), (1, 1));
    }

    /// #1331 BUG 2: id-less Jaccard early-return must sync the superseded
    /// loser into `memories_symbolic_fts` before commit so
    /// `include_superseded` symbolic recall sees it immediately.
    ///
    /// kckylechen1/tachi#1634: exercises the merge path via
    /// `upsert_idless_allowing_merge`'s explicit
    /// [`NearDuplicatePolicy::AllowNearDuplicateMerge`] opt-in — see that
    /// helper's doc comment.
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
            upsert_idless_allowing_merge(&mut store, &original, "identity-sym-original"),
            IdlessUpsertResult::Saved
        );

        let mut near_dup = entry("sym-sync-loser");
        near_dup.path = "/notes/sym-sync".to_string();
        near_dup.text = near_dup_text;
        assert_eq!(
            upsert_idless_allowing_merge(&mut store, &near_dup, "identity-sym-loser"),
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
            false,
        )
        .unwrap();
        assert!(
            hits.iter().any(|e| e.id == "sym-sync-loser"),
            "include_superseded symbolic recall must find the early-return loser; got {:?}",
            hits.iter().map(|e| e.id.as_str()).collect::<Vec<_>>()
        );
    }

    /// tachi#1634 discriminator (i): an explicit-id `upsert` of a
    /// >0.9-Jaccard-similar body must never fold into the existing active
    /// > row — `NearDuplicatePolicy::NonSemantic` is the default for every
    /// > seam except the id-less `save_memory` opt-in. RED pre-#1634, which
    /// > ran `merge_into_jaccard_candidate` unconditionally on every new-id
    /// > write regardless of caller intent.
    #[test]
    fn explicit_id_upsert_near_duplicate_does_not_merge() {
        let mut store = crate::MemoryStore::open_in_memory().unwrap();
        let (base_text, near_dup_text) = near_duplicate_text_pair();

        let mut original = entry("explicit-nonmerge-original");
        original.path = "/notes/explicit-nonmerge".to_string();
        original.text = base_text;
        store.upsert(&original).expect("seed explicit-id original");

        let mut near_dup = entry("explicit-nonmerge-second");
        near_dup.path = "/notes/explicit-nonmerge".to_string();
        near_dup.text = near_dup_text;
        store.upsert(&near_dup).expect("explicit-id near-duplicate");

        assert_near_duplicates_both_survive(
            &store,
            "/notes/explicit-nonmerge",
            "explicit-nonmerge-second",
        );
    }

    /// tachi#1634 discriminator (ii, batch half): `upsert_batch` runs every
    /// row through the same shared `upsert_within_tx` seam as single-row
    /// `upsert` — a near-duplicate pair batched together must not merge
    /// either.
    #[test]
    fn upsert_batch_near_duplicate_does_not_merge() {
        let mut store = crate::MemoryStore::open_in_memory().unwrap();
        let (base_text, near_dup_text) = near_duplicate_text_pair();

        let mut original = entry("batch-nonmerge-original");
        original.path = "/notes/batch-nonmerge".to_string();
        original.text = base_text;

        let mut near_dup = entry("batch-nonmerge-second");
        near_dup.path = "/notes/batch-nonmerge".to_string();
        near_dup.text = near_dup_text;

        store
            .upsert_batch(&[original, near_dup])
            .expect("batch upsert of a near-duplicate pair");

        assert_near_duplicates_both_survive(
            &store,
            "/notes/batch-nonmerge",
            "batch-nonmerge-second",
        );
    }

    /// tachi#1634 discriminator (ii, hub-distill half): hub-distill
    /// (`hub_ops::call::distill`) writes a fresh `Uuid::new_v4` id through
    /// ordinary `upsert`, not an id-less save. A near-duplicate distilled
    /// snapshot must not silently fold into an earlier one.
    #[test]
    fn fresh_uuid_upsert_near_duplicate_does_not_merge() {
        let mut store = crate::MemoryStore::open_in_memory().unwrap();
        let (base_text, near_dup_text) = near_duplicate_text_pair();

        let original_id = uuid::Uuid::new_v4().to_string();
        let mut original = entry(&original_id);
        original.path = "/notes/uuid-nonmerge".to_string();
        original.text = base_text;
        store.upsert(&original).expect("seed fresh-UUID original");

        let near_dup_id = uuid::Uuid::new_v4().to_string();
        let mut near_dup = entry(&near_dup_id);
        near_dup.path = "/notes/uuid-nonmerge".to_string();
        near_dup.text = near_dup_text;
        store.upsert(&near_dup).expect("fresh-UUID near-duplicate");

        assert_near_duplicates_both_survive(&store, "/notes/uuid-nonmerge", &near_dup_id);
    }

    /// tachi#1634 discriminator (iii): tidy migration copies every row of a
    /// source database into the target through
    /// `upsert_batch_with_precommit_preserving_anchor_rows`, which shares the
    /// same `upsert_within_tx_inner` body as every other batch path. Two
    /// similar-body rows batch-copied by a migration must both survive, not
    /// silently collapse into one — the untested landmine the tachi#1634
    /// census flagged.
    #[test]
    fn tidy_migration_batch_copy_near_duplicate_does_not_merge() {
        let mut store = crate::MemoryStore::open_in_memory().unwrap();
        let (base_text, near_dup_text) = near_duplicate_text_pair();

        let mut original = entry("tidy-nonmerge-original");
        original.path = "/notes/tidy-nonmerge".to_string();
        original.text = base_text;

        let mut near_dup = entry("tidy-nonmerge-second");
        near_dup.path = "/notes/tidy-nonmerge".to_string();
        near_dup.text = near_dup_text;

        store
            .upsert_batch_with_precommit_preserving_anchor_rows(&[original, near_dup], |_tx| Ok(()))
            .expect("tidy-migration batch copy of a near-duplicate pair");

        assert_near_duplicates_both_survive(&store, "/notes/tidy-nonmerge", "tidy-nonmerge-second");
    }

    /// tachi#1634 discriminator (iv): `MemoryStore::upsert_idless` without
    /// the explicit `AllowNearDuplicateMerge` opt-in must not merge either —
    /// proving the *policy* gates the merge, not the id-less code path by
    /// itself. Contrast with `idless_save_near_duplicate_merges_into_existing_active_row`
    /// above, which drives the same rows through `upsert_idless_allowing_merge`.
    #[test]
    fn idless_upsert_without_opt_in_near_duplicate_does_not_merge() {
        let mut store = crate::MemoryStore::open_in_memory().unwrap();
        let (base_text, near_dup_text) = near_duplicate_text_pair();

        let mut original = entry("idless-nonmerge-original");
        original.path = "/notes/idless-nonmerge".to_string();
        original.text = base_text;
        assert_eq!(
            store
                .upsert_idless(&original, "identity-idless-nonmerge-original")
                .unwrap(),
            IdlessUpsertResult::Saved
        );

        let mut near_dup = entry("idless-nonmerge-second");
        near_dup.path = "/notes/idless-nonmerge".to_string();
        near_dup.text = near_dup_text;
        assert_eq!(
            store
                .upsert_idless(&near_dup, "identity-idless-nonmerge-second")
                .unwrap(),
            IdlessUpsertResult::Saved
        );

        assert_near_duplicates_both_survive(
            &store,
            "/notes/idless-nonmerge",
            "idless-nonmerge-second",
        );
    }
}

/// Maximum IDs per batch for IN clause queries (SQLite has a 999 parameter limit).
const IN_BATCH_SIZE: usize = 900;

// ─── DELETE ───────────────────────────────────────────────────────────────────

/// Delete a memory entry by ID from main table, FTS index, and vector table.
/// Returns true if an entry was found and deleted.
pub fn delete(
    conn: &mut Connection,
    id: &str,
    vec_available: bool,
    profile: StoreProfile,
) -> Result<bool, MemoryError> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Err(MemoryError::InvalidArg("empty ID".to_string()));
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let deleted = delete_memory_within_tx(&tx, trimmed, vec_available, profile)?;
    tx.commit()?;
    Ok(deleted)
}

/// Canonical exact-id deletion inside a caller-owned writer transaction.
/// Operator maintenance uses this seam so prepared-receipt publication and
/// every associated-index cleanup share one commit boundary.
pub(crate) fn delete_memory_within_tx(
    tx: &Connection,
    id: &str,
    vec_available: bool,
    profile: StoreProfile,
) -> Result<bool, MemoryError> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Err(MemoryError::InvalidArg("empty ID".to_string()));
    }
    refuse_reserved_rem_operation_mutation(trimmed, "deleted")?;
    refuse_retired_sticky_row_within_tx(tx, trimmed, "deleted")?;
    // Delete from main table and check if anything was actually removed
    tx.execute("DELETE FROM memories WHERE id = ?1", params![trimmed])?;
    let deleted = tx.changes() > 0;

    if deleted {
        // Clean up FTS index
        tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![trimmed])?;
        delete_memories_symbolic_fts(tx, trimmed)?;

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

        // Clean up agent known state (CASCADE). PRODUCT table (#1585 D4): a
        // PortableKernel store never created it, so there is nothing to
        // cascade to. Explicit profile check, not a `table_exists` sniff.
        if profile.includes_product() {
            tx.execute(
                "DELETE FROM agent_known_state WHERE memory_id = ?1",
                params![trimmed],
            )?;
        }
    }

    Ok(deleted)
}

pub(crate) fn refuse_reserved_rem_operation_mutation(
    id: &str,
    action: &str,
) -> Result<(), MemoryError> {
    if crate::namespace::is_reserved_wiki_rem_id(id) {
        return Err(MemoryError::InvalidArg(format!(
            "invariant: reserved REM operation {id} cannot be {action} through a generic lifecycle seam"
        )));
    }
    Ok(())
}

/// Refuse every ordinary mutation of a pre-existing retired sticky row.
///
/// The lookup deliberately accepts only a caller-owned transaction so the
/// retirement decision and the mutation share one writer snapshot. A missing
/// row is not retired and preserves each public by-id API's established no-op
/// semantics. The typed sticky-cutover implementation does not call this
/// ordinary-writer seam; its frozen-plan CAS remains the sole exception.
pub fn refuse_retired_sticky_row_within_tx(
    tx: &Connection,
    id: &str,
    operation: &str,
) -> Result<(), MemoryError> {
    let row = tx
        .query_row(
            "SELECT path, category FROM memories WHERE id = ?1",
            [id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((path, category)) = row {
        crate::path_router::validate_retired_sticky_write(&path, &category).map_err(|error| {
            MemoryError::InvalidArg(format!(
                "retired sticky row {id} cannot be {operation} through an ordinary writer: {error}"
            ))
        })?;
    }
    Ok(())
}

pub(crate) fn archive_memory_within_tx(tx: &Connection, id: &str) -> Result<bool, MemoryError> {
    refuse_reserved_rem_operation_mutation(id, "archived")?;
    refuse_retired_sticky_row_within_tx(tx, id, "archived")?;
    let now = now_utc_iso();
    tx.execute(
        "UPDATE memories SET archived = 1, updated_at = ?1, revision = revision + 1 WHERE id = ?2 AND archived = 0",
        params![now, id],
    )?;
    Ok(tx.changes() > 0)
}

pub fn archive_memory(conn: &Connection, id: &str) -> Result<bool, MemoryError> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let changed = archive_memory_within_tx(&tx, id)?;
    tx.commit()?;
    Ok(changed)
}

pub fn archive_memory_if_revision(
    conn: &Connection,
    id: &str,
    expected_revision: i64,
) -> Result<bool, MemoryError> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let changed = archive_memory_revision_within_tx(&tx, id, expected_revision)?;
    tx.commit()?;
    Ok(changed)
}

pub fn archive_memory_revision_within_tx(
    tx: &Connection,
    id: &str,
    expected_revision: i64,
) -> Result<bool, MemoryError> {
    refuse_reserved_rem_operation_mutation(id, "archived")?;
    refuse_retired_sticky_row_within_tx(tx, id, "revision-archived")?;
    let now = now_utc_iso();
    tx.execute(
        "UPDATE memories SET archived = 1, updated_at = ?1, revision = revision + 1 WHERE id = ?2 AND archived = 0 AND revision = ?3",
        params![now, id, expected_revision],
    )?;
    let changed = tx.changes() > 0;
    Ok(changed)
}

pub fn restore_archived_if_revision(
    conn: &Connection,
    id: &str,
    expected_revision: i64,
) -> Result<bool, MemoryError> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let changed = restore_archived_revision_within_tx(&tx, id, expected_revision)?;
    tx.commit()?;
    Ok(changed)
}

pub fn restore_archived_revision_within_tx(
    tx: &Connection,
    id: &str,
    expected_revision: i64,
) -> Result<bool, MemoryError> {
    refuse_reserved_rem_operation_mutation(id, "restored")?;
    refuse_retired_sticky_row_within_tx(tx, id, "restored")?;
    let now = now_utc_iso();
    tx.execute(
        "UPDATE memories SET archived = 0, updated_at = ?1, revision = revision + 1 WHERE id = ?2 AND archived = 1 AND revision = ?3",
        params![now, id, expected_revision],
    )?;
    let changed = tx.changes() > 0;
    Ok(changed)
}

/// Mark a memory as superseded by a newer/canonical memory. Superseded rows are
/// hidden from default search but remain available for audit/history.
#[cfg(any(test, feature = "test-support"))]
pub fn supersede_memory(
    conn: &Connection,
    id: &str,
    superseded_by: &str,
) -> Result<bool, MemoryError> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let changed = supersede_memory_within_tx(&tx, id, superseded_by)?;
    tx.commit()?;
    Ok(changed)
}

#[cfg(any(test, feature = "test-support"))]
fn supersede_memory_within_tx(
    tx: &Connection,
    id: &str,
    superseded_by: &str,
) -> Result<bool, MemoryError> {
    if id == superseded_by {
        return Ok(false);
    }
    refuse_reserved_rem_operation_mutation(id, "superseded")?;
    refuse_retired_sticky_row_within_tx(tx, id, "superseded")?;
    refuse_retired_sticky_row_within_tx(tx, superseded_by, "used as a supersession target")?;
    let now = now_utc_iso();
    // Closing valid_until at supersession time turns the superseded row into a
    // point-in-time-recoverable version: `as_of` before `now` still returns it,
    // `as_of` after `now` correctly prefers the superseding row. COALESCE keeps
    // an explicitly-set validity window intact. NOTE: this invalidation is
    // specific to supersession; archive_memory (stale/dedup eviction) must NOT
    // close valid_until, since a GC'd fact may still have been true.
    tx.execute(
        "UPDATE memories
         SET superseded_by = ?1, updated_at = ?2, revision = revision + 1,
             valid_until = COALESCE(valid_until, ?2)
         WHERE id = ?3 AND superseded_by IS NULL",
        params![superseded_by, now, id],
    )?;
    Ok(tx.changes() > 0)
}

/// Mark a memory as superseded only when its revision is the one the caller
/// inspected. This is the lifecycle counterpart to `update_with_revision` for
/// migration paths that must not race a concurrent content/review write.
#[cfg(any(test, feature = "test-support"))]
pub fn supersede_memory_if_revision(
    conn: &Connection,
    id: &str,
    superseded_by: &str,
    expected_revision: i64,
) -> Result<bool, MemoryError> {
    if id == superseded_by {
        return Ok(false);
    }
    refuse_reserved_rem_operation_mutation(id, "superseded")?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    refuse_retired_sticky_row_within_tx(&tx, id, "revision-superseded")?;
    refuse_retired_sticky_row_within_tx(&tx, superseded_by, "used as a supersession target")?;
    let now = now_utc_iso();
    tx.execute(
        "UPDATE memories
         SET superseded_by = ?1, updated_at = ?2, revision = revision + 1,
             valid_until = COALESCE(valid_until, ?2)
         WHERE id = ?3 AND superseded_by IS NULL AND revision = ?4",
        params![superseded_by, now, id, expected_revision],
    )?;
    let changed = tx.changes() > 0;
    tx.commit()?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::{simple_query_input, supersede_memory, supersede_memory_if_revision};
    use rusqlite::Connection;

    #[test]
    fn supersede_memory_if_revision_is_a_revision_cas() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                category TEXT NOT NULL,
                superseded_by TEXT,
                updated_at TEXT,
                valid_until TEXT,
                revision INTEGER NOT NULL
            );
            INSERT INTO memories (id, path, category, revision)
            VALUES ('source', '/ordinary/source', 'fact', 4);",
        )
        .unwrap();

        assert!(supersede_memory_if_revision(&conn, "source", "target", 4).unwrap());
        assert!(!supersede_memory_if_revision(&conn, "source", "other", 4).unwrap());
        assert_eq!(
            conn.query_row(
                "SELECT superseded_by, revision FROM memories WHERE id = 'source'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap(),
            ("target".to_string(), 5)
        );
    }

    /// tachi#1635 (#1632 conformance, item 1): source == target must refuse,
    /// not silently self-loop. `supersede_memory`'s guard is
    /// `if id == superseded_by { return Ok(false); }` (this file, above) — pin
    /// it as a no-write CAS refusal rather than an error, since the function's
    /// contract is `Result<bool, _>` (changed vs not), not `Result<(), _>`.
    #[test]
    fn supersede_memory_refuses_self_supersession() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                superseded_by TEXT,
                updated_at TEXT,
                valid_until TEXT,
                revision INTEGER NOT NULL
            );
            INSERT INTO memories (id, revision) VALUES ('self-loop', 1);",
        )
        .unwrap();

        assert!(
            !supersede_memory(&conn, "self-loop", "self-loop").unwrap(),
            "id == superseded_by must be refused as a no-op, not applied"
        );
        let (superseded_by, revision): (Option<String>, i64) = conn
            .query_row(
                "SELECT superseded_by, revision FROM memories WHERE id = 'self-loop'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            superseded_by, None,
            "refused self-supersession must not write the edge"
        );
        assert_eq!(
            revision, 1,
            "refused self-supersession must not bump revision"
        );
    }

    /// tachi#1635 (#1632 conformance, item 1): the revisioned CAS variant
    /// shares the same `id == superseded_by` guard as `supersede_memory`
    /// above — pin it separately since it is the seam migration paths use.
    #[test]
    fn supersede_memory_if_revision_refuses_self_supersession() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                superseded_by TEXT,
                updated_at TEXT,
                valid_until TEXT,
                revision INTEGER NOT NULL
            );
            INSERT INTO memories (id, revision) VALUES ('self-loop-rev', 4);",
        )
        .unwrap();

        assert!(
            !supersede_memory_if_revision(&conn, "self-loop-rev", "self-loop-rev", 4).unwrap(),
            "id == superseded_by must be refused even when the revision matches"
        );
        let (superseded_by, revision): (Option<String>, i64) = conn
            .query_row(
                "SELECT superseded_by, revision FROM memories WHERE id = 'self-loop-rev'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(superseded_by, None);
        assert_eq!(revision, 4);
    }

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

#[cfg(test)]
mod select_column_ordinal_tests {
    use super::{
        memory_select_required_columns, MEMORY_EMBEDDING_COLUMN_INDEX, MEMORY_SELECT_COLUMNS,
        MEMORY_SELECT_COLUMNS_QUALIFIED,
    };

    /// Pins the ` AS ` filter in [`memory_select_required_columns`]. Every guard
    /// built on that function is only as good as this extraction: if a future
    /// column were named such that the filter dropped it, the guards would go
    /// quietly blind rather than fail.
    #[test]
    fn required_columns_are_the_select_list_minus_exactly_the_two_synthesized_literals() {
        let required = memory_select_required_columns();
        assert_eq!(
            required.len(),
            MEMORY_SELECT_COLUMNS.split(',').count() - 2,
            "exactly two entries are synthesized by the SELECT (persons, location); \
             got {required:?}"
        );
        for synthesized in ["persons", "location"] {
            assert!(
                !required.contains(&synthesized),
                "{synthesized} is supplied as a literal by the SELECT, not read from the table"
            );
        }
        for real in ["id", "last_access", "last_use_at", "tier"] {
            assert!(
                required.contains(&real),
                "{real} is a real column and must be required of any table this SELECT runs against"
            );
        }
        assert!(
            required.iter().all(|column| !column.contains('\'')
                && !column.contains(' ')
                && !column.is_empty()),
            "a required column name must be a bare identifier; got {required:?}"
        );
    }

    /// tachi#1446 regression guard. `get_many_with_vectors` (`read.rs`) selects
    /// `{MEMORY_SELECT_COLUMNS_QUALIFIED}, v.embedding` and reads the blob by
    /// ordinal, so that ordinal *is* the qualified list's column count and
    /// nothing else. Adding `last_use_at` shifted it; before this test the only
    /// thing pinning the two together was a bare integer literal in another
    /// file. No column in either list contains a comma, so splitting on `,` is
    /// an exact count.
    #[test]
    fn embedding_ordinal_equals_the_selected_column_count() {
        let plain = MEMORY_SELECT_COLUMNS.split(',').count();
        let qualified = MEMORY_SELECT_COLUMNS_QUALIFIED.split(',').count();
        assert_eq!(
            plain, qualified,
            "the qualified and unqualified select lists must stay the same shape"
        );
        assert_eq!(
            qualified, MEMORY_EMBEDDING_COLUMN_INDEX,
            "v.embedding is selected immediately after MEMORY_SELECT_COLUMNS_QUALIFIED, so its \
             ordinal is that list's column count; update MEMORY_EMBEDDING_COLUMN_INDEX when the \
             list changes"
        );
    }
}
