use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Result as SqlResult;

use crate::error::MemoryError;
use crate::types::MemoryEntry;

// ─── Row mapping ──────────────────────────────────────────────────────────────

/// The one sanctioned renderer for "now" as an RFC3339 UTC timestamp:
/// millisecond precision, `Z` suffix (`...123Z`). Every writer that stamps
/// `created_at`/`updated_at`/`valid_until` must go through this (or
/// [`normalize_utc_iso`]/[`normalize_utc_iso_or_now`]) — several of those
/// columns are compared **lexically** (no `datetime()` wrapper) by the as-of
/// search predicates, so a bare `chrono::Utc::now().to_rfc3339()` (numeric
/// offset, auto-precision fraction) sorts wrong against this format.
#[inline]
pub fn now_utc_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[inline]
pub fn normalize_utc_iso(ts: &str) -> Result<String, MemoryError> {
    let raw = ts.trim();
    if raw.is_empty() {
        return Err(MemoryError::InvalidArg("empty timestamp".to_string()));
    }

    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Ok(dt
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true));
    }

    if let Ok(dt) = raw.parse::<DateTime<Utc>>() {
        return Ok(dt.to_rfc3339_opts(SecondsFormat::Millis, true));
    }

    Err(MemoryError::InvalidArg(format!(
        "invalid timestamp format: {}",
        ts
    )))
}

#[inline]
pub fn normalize_utc_iso_or_now(ts: &str) -> String {
    normalize_utc_iso(ts).unwrap_or_else(|_| now_utc_iso())
}

fn json_string_array_column(row: &rusqlite::Row<'_>, entry_id: &str, column: &str) -> Vec<String> {
    let raw = match row.get::<_, String>(column) {
        Ok(raw) => raw,
        Err(err) => {
            tracing::warn!(entry_id, column, error = %err, "memory row text column fallback");
            return Vec::new();
        }
    };
    match serde_json::from_str(&raw) {
        Ok(values) => values,
        Err(err) => {
            tracing::warn!(entry_id, column, error = %err, "memory row JSON array fallback");
            Vec::new()
        }
    }
}

fn optional_text_column(row: &rusqlite::Row<'_>, entry_id: &str, column: &str) -> String {
    row.get::<_, String>(column).unwrap_or_else(|err| {
        tracing::warn!(entry_id, column, error = %err, "memory row text column fallback");
        String::new()
    })
}

pub fn row_to_entry(row: &rusqlite::Row<'_>) -> SqlResult<MemoryEntry> {
    let id: String = row.get("id")?;
    let metadata_str: String = row.get("metadata")?;
    let metadata: serde_json::Value = serde_json::from_str(&metadata_str).unwrap_or_else(|err| {
        tracing::warn!(entry_id = %id, column = "metadata", error = %err, "memory row JSON object fallback");
        serde_json::json!({})
    });

    let last_access = row.get("last_access").unwrap_or(None);

    Ok(MemoryEntry {
        id: id.clone(),
        path: row.get("path")?,
        summary: row.get("summary")?,
        text: row.get("text")?,
        importance: row.get("importance")?,
        timestamp: row.get("timestamp")?,
        valid_from: {
            let vf: Option<String> = row.get("valid_from").unwrap_or(None);
            match vf {
                Some(s) if !s.trim().is_empty() => s,
                _ => row.get("timestamp").unwrap_or_default(),
            }
        },
        valid_until: row.get("valid_until").unwrap_or(None),
        category: row.get("category")?,
        topic: row.get("topic")?,
        keywords: json_string_array_column(row, &id, "keywords"),
        persons: json_string_array_column(row, &id, "persons"),
        entities: json_string_array_column(row, &id, "entities"),
        location: optional_text_column(row, &id, "location"),
        source: row.get("source")?,
        scope: row.get("scope")?,
        archived: row.get("archived")?,
        access_count: row.get("access_count")?,
        scored_count: row.get("scored_count").unwrap_or(0),
        last_access,
        // tachi#1446. Same tolerant read as `last_access` above: a SELECT that
        // does not project the column (or a DB opened before `ensure_column`
        // ran) yields None rather than failing the whole row.
        last_use_at: row.get("last_use_at").unwrap_or(None),
        revision: row.get("revision").unwrap_or(1),
        retention_policy: row.get("retention_policy").unwrap_or(None),
        domain: row.get("domain").unwrap_or(None),
        metadata,
        vector: None,
        recall_count: row.get("recall_count").unwrap_or(0),
        query_diversity: row.get("query_diversity").unwrap_or(0),
        tier: row
            .get::<_, String>("tier")
            .unwrap_or_else(|_| "raw".to_string()),
    })
}
