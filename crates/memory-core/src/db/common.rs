use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Result as SqlResult;

use crate::error::MemoryError;
use crate::types::MemoryEntry;

// ─── Row mapping ──────────────────────────────────────────────────────────────

#[inline]
pub(crate) fn now_utc_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[inline]
pub(crate) fn normalize_utc_iso(ts: &str) -> Result<String, MemoryError> {
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

pub fn row_to_entry(row: &rusqlite::Row<'_>) -> SqlResult<MemoryEntry> {
    let metadata_str: String = row.get("metadata")?;
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_str).unwrap_or(serde_json::json!({}));

    let last_access = row.get("last_access").unwrap_or(None);

    Ok(MemoryEntry {
        id: row.get("id")?,
        path: row.get("path")?,
        summary: row.get("summary")?,
        text: row.get("text")?,
        importance: row.get("importance")?,
        timestamp: row.get("timestamp")?,
        valid_from: row
            .get("valid_from")
            .unwrap_or_else(|_| row.get("timestamp").unwrap_or_default()),
        valid_until: row.get("valid_until").unwrap_or(None),
        category: row.get("category")?,
        topic: row.get("topic")?,
        keywords: serde_json::from_str(&row.get::<_, String>("keywords")?).unwrap_or_default(),
        persons: serde_json::from_str(&row.get::<_, String>("persons")?).unwrap_or_default(),
        entities: serde_json::from_str(&row.get::<_, String>("entities")?).unwrap_or_default(),
        location: row.get("location")?,
        source: row.get("source")?,
        scope: row.get("scope")?,
        archived: row.get("archived")?,
        access_count: row.get("access_count")?,
        last_access,
        revision: row.get("revision").unwrap_or(1),
        retention_policy: row.get("retention_policy").unwrap_or(None),
        domain: row.get("domain").unwrap_or(None),
        metadata,
        vector: None,
    })
}
