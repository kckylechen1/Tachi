use super::*;

pub(super) fn load_supersession_receipt(
    tx: &Connection,
    receipt_id: &str,
) -> Result<Option<SupersessionReceipt>, MemoryError> {
    let value_json = tx
        .query_row(
            "SELECT value_json FROM hard_state WHERE namespace = ?1 AND key = ?2",
            params![SUPERSESSION_RECEIPT_NAMESPACE, receipt_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    value_json
        .map(|value| serde_json::from_str(&value).map_err(MemoryError::from))
        .transpose()
}

pub(super) fn supersession_event_semantic_fields(
    receipt: &SupersessionReceipt,
) -> Result<Vec<String>, MemoryError> {
    Ok(vec![
        String::new(),
        "memcore".to_string(),
        String::new(),
        "memory".to_string(),
        String::new(),
        "memcore".to_string(),
        SUPERSESSION_RECEIPT_EVENT_TYPE.to_string(),
        "structural".to_string(),
        serde_json::to_string(&if receipt.route == "typed_record_revision_v1" {
            vec!["typed_records.append", "typed_record_supersessions.append", "dependent.transaction"]
        } else {
            vec!["memories.superseded_by", "memories.valid_until", "memories.revision", "dependent.transaction"]
        })?,
        serde_json::to_string(&vec!["supersession_receipt"])?,
        serde_json::to_string(receipt)?,
        serde_json::to_string(&serde_json::json!({
            "producer": "memcore::immutable_supersession",
            "route": receipt.route,
            "policy_version": receipt.policy_version,
        }))?,
    ])
}

pub(super) fn load_supersession_event_semantic_fields(
    tx: &Connection,
    receipt_id: &str,
) -> Result<Option<Vec<String>>, MemoryError> {
    tx.query_row(
        "SELECT source_repo, adapter, project, domain, session_id, actor,
                event_type, authority, effects, projection_hints,
                payload_json, provenance_json
         FROM tachi_events WHERE id = ?1",
        [receipt_id],
        |row| {
            let mut fields = Vec::with_capacity(12);
            for index in 0..12 {
                fields.push(row.get::<_, String>(index)?);
            }
            Ok(fields)
        },
    )
    .optional()
    .map_err(MemoryError::from)
}

pub(crate) fn replace_supersession_event_receipt_after_seal(
    tx: &Connection,
    before: &SupersessionReceipt,
    after: &SupersessionReceipt,
) -> Result<(), MemoryError> {
    let has_events = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'tachi_events')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if !has_events {
        return Ok(());
    }
    let expected_before = supersession_event_semantic_fields(before)?;
    if load_supersession_event_semantic_fields(tx, &before.receipt_id)?.as_ref()
        != Some(&expected_before)
    {
        return Err(MemoryError::Internal(format!(
            "supersession event identity conflict before seal: {}",
            before.receipt_id
        )));
    }
    let changed = tx.execute(
        "UPDATE tachi_events SET payload_json = ?1 WHERE id = ?2 AND payload_json = ?3",
        params![
            serde_json::to_string(after)?,
            before.receipt_id,
            serde_json::to_string(before)?,
        ],
    )?;
    if changed != 1
        || load_supersession_event_semantic_fields(tx, &after.receipt_id)?.as_ref()
            != Some(&supersession_event_semantic_fields(after)?)
    {
        return Err(MemoryError::Internal(format!(
            "supersession event durability transition failed: {}",
            after.receipt_id
        )));
    }
    Ok(())
}

pub(super) fn persist_supersession_receipt(
    tx: &Connection,
    receipt: &mut SupersessionReceipt,
) -> Result<(), MemoryError> {
    // A generic database commit is itself the durable authority. A private
    // partition is still only an in-memory working image here; its snapshot
    // copy flips this bit only when constructing the successfully sealed
    // envelope.
    receipt.durable = receipt.partition_id.is_none();
    let value_json = serde_json::to_string(receipt)?;
    let created_at = db::now_utc_iso();
    let changed = tx.execute(
        "INSERT INTO hard_state (namespace, key, value_json, version, created_at, updated_at)
         VALUES (?1, ?2, ?3, 1, ?4, ?4)
         ON CONFLICT(namespace, key) DO NOTHING",
        params![
            SUPERSESSION_RECEIPT_NAMESPACE,
            receipt.receipt_id,
            value_json,
            created_at,
        ],
    )?;
    if changed == 0 {
        let existing = load_supersession_receipt(tx, &receipt.receipt_id)?.ok_or_else(|| {
            MemoryError::Internal(format!(
                "supersession receipt conflict without stored row: {}",
                receipt.receipt_id
            ))
        })?;
        if existing != *receipt {
            return Err(MemoryError::Internal(format!(
                "supersession receipt identity conflict: {}",
                receipt.receipt_id
            )));
        }
    }

    let has_events = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'tachi_events')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if !has_events {
        return Ok(());
    }
    let event_fields = supersession_event_semantic_fields(receipt)?;
    let event_changed = tx.execute(
        "INSERT INTO tachi_events (
            id, source_repo, adapter, project, domain, session_id, actor,
            event_type, authority, effects, projection_hints,
            payload_json, provenance_json, created_at
         )
         VALUES (?1, '', 'memcore', '', 'memory', '', 'memcore',
                 ?2, 'structural', ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO NOTHING",
        params![
            receipt.receipt_id,
            SUPERSESSION_RECEIPT_EVENT_TYPE,
            event_fields[8],
            event_fields[9],
            event_fields[10],
            event_fields[11],
            created_at,
        ],
    )?;
    if event_changed == 0
        && load_supersession_event_semantic_fields(tx, &receipt.receipt_id)?.as_ref()
            != Some(&event_fields)
    {
        return Err(MemoryError::Internal(format!(
            "supersession event identity conflict: {}",
            receipt.receipt_id
        )));
    }
    Ok(())
}

pub(crate) fn finalize_supersession_receipt_within_tx(
    tx: &Connection,
    receipt: &mut SupersessionReceipt,
    dependent_write_disposition: &str,
) -> Result<(), MemoryError> {
    let Some((source_after, source_superseded_by_after)) =
        read_entry_and_supersession(tx, &receipt.source_id)?
    else {
        return Err(MemoryError::Internal(format!(
            "supersession receipt source disappeared before commit: {}",
            receipt.source_id
        )));
    };
    let Some((target_after, target_superseded_by_after)) =
        read_entry_and_supersession(tx, &receipt.target_id)?
    else {
        return Err(MemoryError::InvalidArg(format!(
            "supersession receipt target was not materialized before commit: {}",
            receipt.target_id
        )));
    };
    if target_is_retired(&target_after, target_superseded_by_after.as_deref()) {
        return Err(MemoryError::InvalidArg(format!(
            "supersession receipt target became ineligible before commit: {}",
            receipt.target_id
        )));
    }
    if source_superseded_by_after.as_deref() != Some(receipt.target_id.as_str())
        || (receipt.source_archived_after && !source_after.archived)
        || source_after.path != receipt.source_path_before
        || text_digest(&source_after.text) != receipt.source_text_digest_before
    {
        return Err(MemoryError::InvalidArg(format!(
            "supersession receipt source changed after claim: {}",
            receipt.source_id
        )));
    }
    receipt.source_revision_after = source_after.revision;
    receipt.source_archived_after = source_after.archived;
    receipt.source_superseded_by_after = source_superseded_by_after;
    receipt.source_valid_until_after = source_after.valid_until;
    receipt.target_revision_after = Some(target_after.revision);
    receipt.dependent_write_disposition = dependent_write_disposition.to_string();
    persist_supersession_receipt(tx, receipt)
}

