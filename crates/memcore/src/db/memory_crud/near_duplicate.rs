//! Near-duplicate candidate selection and transactional merge writes.

use super::*;

/// Heuristic merging must not invent a lifetime for a same-instant or future
/// row. Keep that row independent. Preserve an explicit nonempty window.
pub(crate) fn jaccard_supersession_valid_until(
    entry: &MemoryEntry,
    write_time: &str,
) -> Result<Option<String>, MemoryError> {
    let from = normalize_utc_iso(if entry.valid_from.trim().is_empty() {
        &entry.timestamp
    } else {
        &entry.valid_from
    })?;
    let now = normalize_utc_iso(write_time)?;
    let until = entry
        .valid_until
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(normalize_utc_iso)
        .transpose()?;
    if until.as_ref().is_some_and(|end| end <= &from) {
        return Err(MemoryError::InvalidArg(
            "near-duplicate merge requires a positive half-open validity window".into(),
        ));
    }
    if from >= now {
        return Ok(None);
    }
    Ok(Some(until.unwrap_or(now)))
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
        // Id-less callers have already inserted the source. Exclude it so a
        // perfect self-match cannot become a supersession candidate.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jaccard_boundary_never_invents_a_nonempty_lifetime() {
        let mut row: MemoryEntry = serde_json::from_value(serde_json::json!({
            "id":"near", "text":"near duplicate", "timestamp":"2026-10-30T00:00:00.000Z"
        }))
        .unwrap();
        assert!(
            jaccard_supersession_valid_until(&row, "2026-10-30T00:00:00Z")
                .unwrap()
                .is_none()
        );
        assert!(
            jaccard_supersession_valid_until(&row, "2026-10-29T00:00:00Z")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            jaccard_supersession_valid_until(&row, "2026-10-31T00:00:00Z")
                .unwrap()
                .as_deref(),
            Some("2026-10-31T00:00:00.000Z")
        );
        row.valid_until = Some("2026-10-30T12:00:00Z".into());
        assert_eq!(
            jaccard_supersession_valid_until(&row, "2026-10-31T00:00:00Z")
                .unwrap()
                .as_deref(),
            Some("2026-10-30T12:00:00.000Z")
        );
        row.valid_until = Some("2026-10-29T00:00:00Z".into());
        assert!(jaccard_supersession_valid_until(&row, "2026-10-31T00:00:00Z").is_err());
    }
}
