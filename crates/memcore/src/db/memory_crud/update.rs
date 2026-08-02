use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{Map, Value};

use crate::error::MemoryError;
use crate::types::{ExpectedMemoryState, MemorySource};

use super::now_utc_iso;

const ENRICHMENT_AUTH_RETRY_MAX_ATTEMPTS: i64 = 3;

fn auth_class_enrichment_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("auth_failed")
        || lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("invalid api key")
        || lower.contains("unusable")
}

/// Atomically try to claim an event for processing.
/// Uses INSERT OR IGNORE: if the row didn't exist, it's inserted and we return true (claimed).
/// If the row already existed, nothing happens and we return false (already processed).
/// This fixes the TOCTOU race in the old check-then-mark pattern.
pub fn try_claim_event(
    conn: &Connection,
    event_hash: &str,
    event_id: &str,
    worker: &str,
) -> Result<bool, MemoryError> {
    let now = now_utc_iso();
    let rows_changed = conn.execute(
        "INSERT OR IGNORE INTO processed_events (event_hash, event_id, worker, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![event_hash, event_id, worker, now],
    )?;
    Ok(rows_changed > 0)
}

/// Release a previously claimed event so it can be retried.
/// Called when background processing fails to ensure at-least-once delivery.
pub fn release_event_claim(
    conn: &Connection,
    event_hash: &str,
    worker: &str,
) -> Result<(), MemoryError> {
    conn.execute(
        "DELETE FROM processed_events WHERE event_hash = ?1 AND worker = ?2",
        params![event_hash, worker],
    )?;
    Ok(())
}

/// Update a memory row only when its revision matches `expected_revision`.
/// Returns `Ok(true)` when updated, `Ok(false)` on revision mismatch.
#[allow(clippy::too_many_arguments)]
pub fn update_with_revision(
    conn: &mut Connection,
    id: &str,
    new_text: &str,
    new_summary: &str,
    new_source: &str,
    new_metadata: &str,
    new_vec: Option<&[u8]>,
    expected_revision: i64,
) -> Result<bool, MemoryError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let updated = update_with_revision_within_tx(
        &tx,
        id,
        new_text,
        new_summary,
        new_source,
        new_metadata,
        new_vec,
        expected_revision,
    )?;
    tx.commit()?;
    Ok(updated)
}

/// Revision update whose typed complete-state snapshot is evaluated after
/// `BEGIN IMMEDIATE` and against the same writer snapshot as the update.
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_with_revision_if_expected_state(
    conn: &mut Connection,
    id: &str,
    new_text: &str,
    new_summary: &str,
    new_source: &str,
    new_metadata: &str,
    new_vec: Option<&[u8]>,
    expected: &ExpectedMemoryState,
) -> Result<bool, MemoryError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let ids = vec![id.to_string()];
    let mut current = super::fetch_by_ids(&tx, &ids, true)?;
    let Some(current) = current.remove(id) else {
        tx.commit()?;
        return Ok(false);
    };
    let superseded_by = tx
        .query_row(
            "SELECT superseded_by FROM memories WHERE id = ?1",
            [id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    if !expected.matches(&current, superseded_by.as_deref()) {
        tx.commit()?;
        return Ok(false);
    }
    let updated = update_with_revision_within_tx(
        &tx,
        id,
        new_text,
        new_summary,
        new_source,
        new_metadata,
        new_vec,
        expected.revision(),
    )?;
    tx.commit()?;
    Ok(updated)
}

fn expected_state_matches_within_tx(
    tx: &Transaction<'_>,
    id: &str,
    expected: &ExpectedMemoryState,
) -> Result<bool, MemoryError> {
    let ids = vec![id.to_string()];
    let mut current = super::fetch_by_ids(tx, &ids, true)?;
    let Some(current) = current.remove(id) else {
        return Ok(false);
    };
    let superseded_by = tx
        .query_row(
            "SELECT superseded_by FROM memories WHERE id = ?1",
            [id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(expected.matches(&current, superseded_by.as_deref()))
}

/// Atomically bind a complete expected source state to its final migration
/// receipt and supersession lifecycle transition.
pub(crate) fn supersede_with_metadata_if_expected_state(
    conn: &mut Connection,
    id: &str,
    superseded_by: &str,
    new_metadata: &str,
    expected: &ExpectedMemoryState,
) -> Result<bool, MemoryError> {
    if id == superseded_by {
        return Ok(false);
    }
    super::refuse_reserved_rem_operation_mutation(id, "superseded")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !expected_state_matches_within_tx(&tx, id, expected)? {
        tx.commit()?;
        return Ok(false);
    }
    let incoming_metadata = serde_json::from_str(new_metadata)?;
    let metadata = super::merge_ordinary_reserved_metadata(&tx, id, &incoming_metadata)?;
    let metadata_json = serde_json::to_string(&metadata)?;
    let now = now_utc_iso();
    tx.execute(
        "UPDATE memories
         SET metadata = ?1, superseded_by = ?2,
             valid_until = COALESCE(valid_until, ?3), updated_at = ?3,
             revision = revision + 1
         WHERE id = ?4 AND revision = ?5 AND superseded_by IS NULL",
        params![metadata_json, superseded_by, now, id, expected.revision()],
    )?;
    let updated = tx.changes() == 1;
    tx.commit()?;
    Ok(updated)
}

/// Atomically mark an exact deterministic occupant non-canonical while
/// replacing its migration metadata. The row remains durable for audit.
pub(crate) fn archive_with_metadata_if_expected_state(
    conn: &mut Connection,
    id: &str,
    new_metadata: &str,
    expected: &ExpectedMemoryState,
) -> Result<bool, MemoryError> {
    super::refuse_reserved_rem_operation_mutation(id, "archived")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !expected_state_matches_within_tx(&tx, id, expected)? {
        tx.commit()?;
        return Ok(false);
    }
    let incoming_metadata = serde_json::from_str(new_metadata)?;
    let metadata = super::merge_ordinary_reserved_metadata(&tx, id, &incoming_metadata)?;
    let metadata_json = serde_json::to_string(&metadata)?;
    let now = now_utc_iso();
    tx.execute(
        "UPDATE memories
         SET metadata = ?1, archived = 1, updated_at = ?2,
             revision = revision + 1
         WHERE id = ?3 AND revision = ?4",
        params![metadata_json, now, id, expected.revision()],
    )?;
    let updated = tx.changes() == 1;
    tx.commit()?;
    Ok(updated)
}

#[allow(clippy::too_many_arguments)]
fn update_with_revision_within_tx(
    tx: &Transaction<'_>,
    id: &str,
    new_text: &str,
    new_summary: &str,
    new_source: &str,
    new_metadata: &str,
    new_vec: Option<&[u8]>,
    expected_revision: i64,
) -> Result<bool, MemoryError> {
    let now = now_utc_iso();
    let new_revision = expected_revision + 1;
    // Normalize source to satisfy CHECK constraint.
    let new_source = MemorySource::parse_or_external(new_source);
    let incoming_metadata = serde_json::from_str(new_metadata)?;
    let metadata = super::merge_ordinary_reserved_metadata(tx, id, &incoming_metadata)?;
    let metadata_json = serde_json::to_string(&metadata)?;

    let clean_text = crate::noise::scrub_think_tags(new_text);
    let clean_summary = crate::noise::scrub_think_tags(new_summary);

    tx.execute(
        "UPDATE memories
         SET text = ?1, summary = ?2, source = ?3, metadata = ?4, updated_at = ?5, revision = ?6
         WHERE id = ?7 AND revision = ?8",
        params![
            &clean_text,
            &clean_summary,
            new_source,
            metadata_json,
            &now,
            new_revision,
            id,
            expected_revision
        ],
    )?;
    let updated = tx.changes() > 0;

    if updated {
        tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![id])?;
        tx.execute(
            r#"INSERT INTO memories_fts(id, path, summary, text, keywords, entities)
               SELECT
                 id,
                 path,
                 summary,
                 text,
                 trim(replace(replace(replace(keywords, '[', ' '), ']', ' '), '"', ' ')),
                 trim(replace(replace(replace(entities, '[', ' '), ']', ' '), '"', ' '))
               FROM memories WHERE id = ?1"#,
            params![id],
        )?;
        super::sync_memories_symbolic_fts(tx, id)?;

        if let Some(vec_blob) = new_vec {
            tx.execute("DELETE FROM memories_vec WHERE id = ?1", params![id])?;
            tx.execute(
                "INSERT INTO memories_vec(id, embedding) VALUES (?1, ?2)",
                params![id, vec_blob],
            )?;
        }
    }

    Ok(updated)
}

/// Update only the enrichment fields (summary, embedding, keywords, entities) if
/// the revision hasn't changed since the enrichment was queued. This prevents
/// stale background enrichment from overwriting concurrent updates.
#[allow(clippy::too_many_arguments)]
pub fn update_enrichment_fields(
    conn: &mut Connection,
    id: &str,
    raw_summary: Option<&str>,
    new_vec: Option<&[u8]>,
    new_keywords: Option<&[String]>,
    new_entities: Option<&[String]>,
    expected_revision: i64,
    summary_receipt: Option<&str>,
    metadata_receipt: Option<&str>,
    keywords_receipt: Option<&str>,
) -> Result<bool, MemoryError> {
    // Seam layer: scrub think-tags first, then normalize the scrubbed
    // summary (and the empty keyword/entity slices) to None before
    // classification. Order matters: the SQL below binds `clean_summary`,
    // the post-scrub value, so the write-classification invariant this
    // seam exists to uphold is that write classification, receipt pairing,
    // and the SQL COALESCE effect all agree on that same post-scrub value.
    // COALESCE only guards against NULL, so a summary that is non-empty
    // pre-scrub but scrubs down to "" (e.g. `Some("<think>...</think>")`)
    // would otherwise be classified as a real write, pass receipt pairing,
    // and then bind an empty string that erases the stored summary while
    // stamping a successful summarized status with no receipt.
    // `raw_summary` (the as-received parameter) must not be consumed
    // anywhere below this point — every downstream decision uses
    // `clean_summary`.
    let raw_summary_was_nonblank = raw_summary.is_some_and(|value| !value.trim().is_empty());
    let clean_summary = raw_summary
        .map(crate::noise::scrub_think_tags)
        .filter(|value| !value.trim().is_empty());
    let new_keywords = new_keywords.filter(|value| !value.is_empty());
    let new_entities = new_entities.filter(|value| !value.is_empty());

    // Seam-nulled summary: `raw_summary` was substantive pre-scrub (e.g. a
    // think-tag-only model reply) but the seam normalized it down to
    // nothing. The model ran and the seam judged its output empty, so this
    // is not the caller-visible-blank case (that stays a hard InvalidArg
    // below) -- a summary receipt for this stage is dropped together with
    // the field instead of rejected, so co-batched vector/keyword writes
    // still land and no sticky failed_stage is minted.
    let summary_receipt = if raw_summary_was_nonblank
        && clean_summary.is_none()
        && summary_receipt.is_some()
    {
        eprintln!(
                "[enrichment] discarded stale enrichment receipt for id={id}: summary seam-normalized to empty (think-tag-only input)"
            );
        None
    } else {
        summary_receipt
    };

    let receipts = parse_enrichment_receipts(
        summary_receipt,
        metadata_receipt,
        keywords_receipt,
        clean_summary.as_deref(),
        new_keywords,
        new_entities,
    )?;

    if clean_summary.is_none()
        && new_vec.is_none()
        && new_keywords.is_none()
        && new_entities.is_none()
    {
        // The pairing validation above makes receipt-only success impossible.
        return Ok(true); // legacy no-op
    }

    let now = now_utc_iso();
    let tx = conn.transaction()?;
    let keywords_json = new_keywords.map(serde_json::to_string).transpose()?;
    let entities_json = new_entities.map(serde_json::to_string).transpose()?;

    // Read the exact revision whose fields and metadata may be changed. The
    // final UPDATE repeats the revision predicate, so a concurrent writer can
    // only turn this into a clean `false`, never split fields from receipts.
    let existing_metadata: Option<String> = tx
        .query_row(
            "SELECT metadata FROM memories WHERE id = ?1 AND revision = ?2",
            params![id, expected_revision],
            |row| row.get(0),
        )
        .optional()?;
    let Some(existing_metadata) = existing_metadata else {
        tx.commit()?;
        eprintln!(
            "[enrichment] discarded stale enrichment for id={id}: revision {expected_revision} no longer current"
        );
        return Ok(false);
    };

    let mut metadata = parse_enrichment_metadata(&existing_metadata);

    let status = match (
        new_vec.is_some(),
        clean_summary.is_some(),
        new_keywords.is_some() || new_entities.is_some(),
    ) {
        (true, true, true) => "embedded+summarized+metadata",
        (true, true, false) => "embedded+summarized",
        (true, false, true) => "embedded+metadata",
        (true, false, false) => "embedded",
        (false, true, true) => "summarized+metadata",
        (false, true, false) => "summarized",
        (false, false, true) => "metadata",
        (false, false, false) => "touched",
    };

    // Mixed-stage merge (#943): if another enrichment stage already recorded a
    // durable failure that this write does NOT resolve, preserve aggregate
    // failure metadata (failed_stage / last_error / last_failure_at / retry)
    // and only stamp last_success_at for the stages that succeeded. Clearing
    // then re-applying keywords_status alone used to drop operator-visible
    // failure context on partial success.
    let existing_failed_stage = metadata
        .get("enrichment")
        .and_then(Value::as_object)
        .and_then(|enrichment| enrichment.get("failed_stage"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let failed_stage_resolved = match existing_failed_stage.as_deref() {
        None => true,
        Some("embedding") => new_vec.is_some(),
        Some("summary") => clean_summary.is_some(),
        // Both metadata extraction and write-side keyword enrichment land in
        // the keywords/entities columns.
        Some("metadata") | Some("keywords") => new_keywords.is_some() || new_entities.is_some(),
        // Unknown / db_update: do not claim resolution from a field write.
        Some(_) => false,
    };

    let enrichment = enrichment_metadata_object(&mut metadata);
    if failed_stage_resolved {
        enrichment.insert("status".to_string(), Value::String(status.to_string()));
        enrichment.insert("last_success_at".to_string(), Value::String(now.clone()));
        enrichment.insert("last_error".to_string(), Value::Null);
        enrichment.remove("failed_stage");
        enrichment.remove("last_failure_at");
        enrichment.remove("retry");
    } else {
        // Preserve failure aggregate; still record that some stages succeeded.
        enrichment.insert("last_success_at".to_string(), Value::String(now.clone()));
        enrichment.insert(
            "partial_success_status".to_string(),
            Value::String(status.to_string()),
        );
    }
    merge_enrichment_receipts(enrichment, receipts);
    let metadata_json = serde_json::to_string(&metadata)?;

    // One revision-checked update owns generated fields and their receipt
    // metadata. Never append receipt metadata after this CAS has committed.
    let rows_affected = tx.execute(
        r#"UPDATE memories
           SET summary = COALESCE(?1, summary),
               keywords = COALESCE(?2, keywords),
               entities = COALESCE(?3, entities),
               metadata = ?4,
               updated_at = ?5
           WHERE id = ?6 AND revision = ?7"#,
        params![
            clean_summary.as_deref(),
            keywords_json.as_deref(),
            entities_json.as_deref(),
            &metadata_json,
            &now,
            id,
            expected_revision,
        ],
    )?;
    if rows_affected == 0 {
        tx.commit()?;
        eprintln!(
            "[enrichment] discarded stale enrichment for id={id}: revision {expected_revision} no longer current"
        );
        return Ok(false);
    }

    // Refresh FTS only after the field-and-receipt CAS has landed. It remains
    // in the same transaction, so an FTS failure rolls the field and receipt
    // back together rather than leaving a partial enrichment success.
    //
    // FTS safety (#943): keywords/entities land in `memories` via parameterized
    // UPDATE binds above; this INSERT copies column content with `WHERE id = ?1`.
    // Keyword text becomes document content (not a MATCH expression), so FTS
    // operators inside keywords are not injection vectors. User queries still
    // go through `simple_query` / `simple_query_input` on the search path.
    if clean_summary.is_some() || new_keywords.is_some() || new_entities.is_some() {
        tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![id])?;
        tx.execute(
            r#"INSERT INTO memories_fts(id, path, summary, text, keywords, entities)
               SELECT
                 id,
                 path,
                 summary,
                 text,
                 trim(replace(replace(replace(keywords, '[', ' '), ']', ' '), '"', ' ')),
                 trim(replace(replace(replace(entities, '[', ' '), ']', ' '), '"', ' '))
               FROM memories WHERE id = ?1"#,
            params![id],
        )?;
        super::sync_memories_symbolic_fts(&tx, id)?;
    }

    if let Some(vec_blob) = new_vec {
        tx.execute("DELETE FROM memories_vec WHERE id = ?1", params![id])?;
        tx.execute(
            "INSERT INTO memories_vec(id, embedding) VALUES (?1, ?2)",
            params![id, vec_blob],
        )?;
    }

    tx.commit()?;
    Ok(true)
}

#[derive(Default)]
struct ParsedEnrichmentReceipts {
    summary: Option<Value>,
    metadata: Option<Value>,
    keywords: Option<Value>,
}

/// Contract: `clean_summary` must be the post-scrub, normalized value --
/// never the raw as-received summary. Passing the raw value here revives
/// the pre-scrub/post-scrub split-decision bug (r2 root cause): receipt
/// pairing would then be judged against different text than the SQL bind
/// actually writes.
fn parse_enrichment_receipts(
    summary: Option<&str>,
    metadata: Option<&str>,
    keywords: Option<&str>,
    clean_summary: Option<&str>,
    new_keywords: Option<&[String]>,
    new_entities: Option<&[String]>,
) -> Result<ParsedEnrichmentReceipts, MemoryError> {
    let summary_writes = clean_summary.is_some_and(|value| !value.trim().is_empty());
    let metadata_writes = new_keywords.is_some_and(|value| !value.is_empty())
        || new_entities.is_some_and(|value| !value.is_empty());
    let keywords_writes = new_keywords.is_some_and(|value| !value.is_empty());

    receipt_for_generated_field("summary", summary, summary_writes)?;
    receipt_for_generated_field("metadata", metadata, metadata_writes)?;
    receipt_for_generated_field("keywords", keywords, keywords_writes)?;

    Ok(ParsedEnrichmentReceipts {
        summary: parse_receipt_json(summary)?,
        metadata: parse_receipt_json(metadata)?,
        keywords: parse_receipt_json(keywords)?,
    })
}

fn receipt_for_generated_field(
    stage: &str,
    receipt: Option<&str>,
    generated_field_written: bool,
) -> Result<(), MemoryError> {
    if receipt.is_some() && !generated_field_written {
        return Err(MemoryError::InvalidArg(format!(
            "enrichment receipt invariant: stage={stage} receipt requires an accepted generated field"
        )));
    }
    Ok(())
}

fn parse_receipt_json(receipt: Option<&str>) -> Result<Option<Value>, MemoryError> {
    receipt
        .map(|raw| {
            let value: Value = serde_json::from_str(raw)?;
            if !value.is_object() {
                return Err(MemoryError::InvalidArg(
                    "enrichment receipt invariant: receipt must be a JSON object".to_string(),
                ));
            }
            Ok(value)
        })
        .transpose()
}

fn parse_enrichment_metadata(raw: &str) -> Value {
    match serde_json::from_str::<Value>(raw) {
        Ok(value) if value.is_object() => value,
        _ => Value::Object(Map::new()),
    }
}

fn enrichment_metadata_object(metadata: &mut Value) -> &mut Map<String, Value> {
    let root = metadata
        .as_object_mut()
        .expect("parse_enrichment_metadata always returns an object");
    let enrichment = root
        .entry("enrichment".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !enrichment.is_object() {
        *enrichment = Value::Object(Map::new());
    }
    enrichment
        .as_object_mut()
        .expect("enrichment was normalized to an object")
}

fn merge_enrichment_receipts(
    enrichment: &mut Map<String, Value>,
    receipts: ParsedEnrichmentReceipts,
) {
    if receipts.summary.is_none() && receipts.metadata.is_none() && receipts.keywords.is_none() {
        return;
    }

    let invocations = enrichment
        .entry("invocations".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !invocations.is_object() {
        *invocations = Value::Object(Map::new());
    }
    let invocations = invocations
        .as_object_mut()
        .expect("invocations was normalized to an object");
    if let Some(receipt) = receipts.summary {
        invocations.insert("summary".to_string(), receipt);
    }
    if let Some(receipt) = receipts.metadata {
        invocations.insert("metadata".to_string(), receipt);
    }
    if let Some(receipt) = receipts.keywords {
        invocations.insert("keywords".to_string(), receipt);
    }
}

pub fn record_enrichment_failure(
    conn: &Connection,
    id: &str,
    stage: &str,
    error: &str,
) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    // Write-side keyword enrichment (#921) keeps a dedicated keywords_status so
    // operators can distinguish enriched/pending/skipped/failed without
    // collapsing it into the multi-stage overall enrichment.status string.
    let keywords_status = if stage == "keywords" {
        Some("failed")
    } else {
        None
    };
    if auth_class_enrichment_error(error) {
        if let Some(kw_status) = keywords_status {
            conn.execute(
                r#"UPDATE memories
                   SET metadata = json_set(
                         CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                         '$.enrichment.status', 'failed',
                         '$.enrichment.failed_stage', ?1,
                         '$.enrichment.last_error', ?2,
                         '$.enrichment.last_failure_at', ?3,
                         '$.enrichment.keywords_status', ?4,
                         '$.enrichment.retry.kind', 'auth',
                         '$.enrichment.retry.attempts',
                            COALESCE(CAST(json_extract(metadata, '$.enrichment.retry.attempts') AS INTEGER), 0),
                         '$.enrichment.retry.max_attempts', ?5,
                         '$.enrichment.retry.next_retry_at', ?3
                       ),
                       updated_at = ?3
                   WHERE id = ?6"#,
                params![
                    stage,
                    error,
                    &now,
                    kw_status,
                    ENRICHMENT_AUTH_RETRY_MAX_ATTEMPTS,
                    id
                ],
            )?;
        } else {
            conn.execute(
                r#"UPDATE memories
                   SET metadata = json_set(
                         CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                         '$.enrichment.status', 'failed',
                         '$.enrichment.failed_stage', ?1,
                         '$.enrichment.last_error', ?2,
                         '$.enrichment.last_failure_at', ?3,
                         '$.enrichment.retry.kind', 'auth',
                         '$.enrichment.retry.attempts',
                            COALESCE(CAST(json_extract(metadata, '$.enrichment.retry.attempts') AS INTEGER), 0),
                         '$.enrichment.retry.max_attempts', ?4,
                         '$.enrichment.retry.next_retry_at', ?3
                       ),
                       updated_at = ?3
                   WHERE id = ?5"#,
                params![stage, error, &now, ENRICHMENT_AUTH_RETRY_MAX_ATTEMPTS, id],
            )?;
        }
    } else if let Some(kw_status) = keywords_status {
        conn.execute(
            r#"UPDATE memories
               SET metadata = json_remove(
                     json_set(
                       CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                       '$.enrichment.status', 'failed',
                       '$.enrichment.failed_stage', ?1,
                       '$.enrichment.last_error', ?2,
                       '$.enrichment.last_failure_at', ?3,
                       '$.enrichment.keywords_status', ?4
                     ),
                     '$.enrichment.retry'
                   ),
                   updated_at = ?3
               WHERE id = ?5"#,
            params![stage, error, &now, kw_status, id],
        )?;
    } else {
        conn.execute(
            r#"UPDATE memories
               SET metadata = json_remove(
                     json_set(
                       CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                       '$.enrichment.status', 'failed',
                       '$.enrichment.failed_stage', ?1,
                       '$.enrichment.last_error', ?2,
                       '$.enrichment.last_failure_at', ?3
                     ),
                     '$.enrichment.retry'
                   ),
                   updated_at = ?3
               WHERE id = ?4"#,
            params![stage, error, &now, id],
        )?;
    }
    Ok(())
}

/// Set operator-visible write-side keyword enrichment status (#921).
///
/// Values: `enriched` | `pending` | `skipped` | `failed`.
pub fn set_keyword_enrichment_status(
    conn: &Connection,
    id: &str,
    status: &str,
) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        r#"UPDATE memories
           SET metadata = json_set(
                 CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                 '$.enrichment.keywords_status', ?1
               ),
               updated_at = ?2
           WHERE id = ?3"#,
        params![status, &now, id],
    )?;
    Ok(())
}

/// Stamp `keywords_status=pending` only when absent or already pending.
///
/// Returns `true` when a row was updated. Terminal statuses
/// (`enriched` / `skipped` / `failed`) are never overwritten — this closes the
/// race where a late pending write lands after a fast worker already finished
/// (#943).
pub fn set_keyword_enrichment_pending_if_unset(
    conn: &Connection,
    id: &str,
) -> Result<bool, MemoryError> {
    let now = now_utc_iso();
    let rows = conn.execute(
        r#"UPDATE memories
           SET metadata = json_set(
                 CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                 '$.enrichment.keywords_status', 'pending'
               ),
               updated_at = ?1
           WHERE id = ?2
             AND (
               json_extract(
                 CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                 '$.enrichment.keywords_status'
               ) IS NULL
               OR json_extract(
                 CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                 '$.enrichment.keywords_status'
               ) = 'pending'
             )"#,
        params![&now, id],
    )?;
    Ok(rows > 0)
}
