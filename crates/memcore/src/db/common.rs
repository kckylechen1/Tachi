use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, Result as SqlResult};

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

pub(crate) fn normalize_sqlite_as_of(conn: &Connection, ts: &str) -> Result<String, MemoryError> {
    let canonical = normalize_utc_iso(ts)?;
    let anchored: Option<f64> = conn.query_row("SELECT julianday(?1)", [&canonical], |row| {
        row.get::<_, Option<f64>>(0)
    })?;
    if anchored.is_none() {
        return Err(MemoryError::InvalidArg(format!(
            "as_of instant {canonical} is outside the SQLite julianday range"
        )));
    }
    Ok(canonical)
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

// ─── tachi#1432: canonical timestamp shape contracts ───────────────────────
//
// The doc comment on `now_utc_iso` above states the invariant every writer in
// this crate must uphold: millisecond precision, `Z` suffix, because the
// as-of validity predicates in `db/memory_crud/search.rs` compare
// `valid_until` lexically, not with a SQL `datetime()` wrapper. These tests
// pin the renderer contract itself; the real-search-path demonstration of
// *why* it matters (a legacy bare-shape row surviving alongside canonical
// `as_of` parameters) lives in `db/tests/search_ops.rs`, next to the other
// as-of validity-window tests.
#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    fn canonical_shape() -> Regex {
        Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$").unwrap()
    }

    fn assert_canonical(ts: &str) {
        assert!(
            canonical_shape().is_match(ts),
            "not canonical Millis+Z shape: {ts:?}"
        );
    }

    // ── (a) shape lock ──────────────────────────────────────────────────

    #[test]
    fn now_utc_iso_matches_canonical_shape() {
        assert_canonical(&now_utc_iso());
    }

    // ── (c) writer conformance ──────────────────────────────────────────

    #[test]
    fn normalize_utc_iso_canonicalizes_z_suffix() {
        let out = normalize_utc_iso("2026-01-01T00:00:00Z").unwrap();
        assert_canonical(&out);
        assert_eq!(out, "2026-01-01T00:00:00.000Z");
    }

    #[test]
    fn normalize_utc_iso_canonicalizes_numeric_utc_offset() {
        let out = normalize_utc_iso("2026-01-01T00:00:00+00:00").unwrap();
        assert_canonical(&out);
        assert_eq!(out, "2026-01-01T00:00:00.000Z");
    }

    #[test]
    fn normalize_utc_iso_converts_non_utc_offset_to_utc() {
        let out = normalize_utc_iso("2026-01-01T05:00:00+05:00").unwrap();
        assert_canonical(&out);
        assert_eq!(out, "2026-01-01T00:00:00.000Z");
    }

    #[test]
    fn normalize_utc_iso_pads_zero_digit_fraction_to_millis() {
        // No fractional seconds at all in the input (`Z` right after `:00`).
        let out = normalize_utc_iso("2026-01-01T00:00:00Z").unwrap();
        assert_canonical(&out);
        assert_eq!(out, "2026-01-01T00:00:00.000Z");
    }

    #[test]
    fn normalize_utc_iso_preserves_three_digit_fraction() {
        let out = normalize_utc_iso("2026-01-01T00:00:00.123Z").unwrap();
        assert_canonical(&out);
        assert_eq!(out, "2026-01-01T00:00:00.123Z");
    }

    #[test]
    fn normalize_utc_iso_truncates_six_digit_fraction_to_millis() {
        // chrono's `SecondsFormat::Millis` truncates (integer-divides), it
        // does not round: 123_456us -> 123ms, not 124ms.
        let out = normalize_utc_iso("2026-01-01T00:00:00.123456Z").unwrap();
        assert_canonical(&out);
        assert_eq!(out, "2026-01-01T00:00:00.123Z");
    }

    #[test]
    fn normalize_utc_iso_truncates_nine_digit_fraction_to_millis() {
        let out = normalize_utc_iso("2026-01-01T00:00:00.123456789Z").unwrap();
        assert_canonical(&out);
        assert_eq!(out, "2026-01-01T00:00:00.123Z");
    }

    #[test]
    fn normalize_utc_iso_equality_boundary_same_instant_identical_bytes() {
        // Same instant, four legal RFC3339 renderings; every one must
        // normalize to byte-identical output. If this regresses, the lexical
        // `valid_until > ?` predicates in db/memory_crud/search.rs start
        // comparing apples to oranges again.
        let z = normalize_utc_iso("2026-06-15T12:30:00Z").unwrap();
        let offset = normalize_utc_iso("2026-06-15T12:30:00+00:00").unwrap();
        let zero_fraction = normalize_utc_iso("2026-06-15T12:30:00.000Z").unwrap();
        let non_utc_offset = normalize_utc_iso("2026-06-15T20:30:00+08:00").unwrap();
        assert_canonical(&z);
        assert_eq!(z, "2026-06-15T12:30:00.000Z");
        assert_eq!(z, offset);
        assert_eq!(z, zero_fraction);
        assert_eq!(z, non_utc_offset);
    }

    #[test]
    fn normalize_utc_iso_or_now_preserves_canonical_shape_on_valid_input() {
        let out = normalize_utc_iso_or_now("2026-01-01T00:00:00+00:00");
        assert_canonical(&out);
        assert_eq!(out, "2026-01-01T00:00:00.000Z");
    }

    #[test]
    fn normalize_utc_iso_or_now_falls_back_to_canonical_shape_on_invalid_input() {
        // The fallback path (`now_utc_iso()`) must land in the same shape as
        // the primary path -- a caller cannot tell from the string alone
        // whether the input was garbage or a real timestamp.
        let out = normalize_utc_iso_or_now("not-a-timestamp");
        assert_canonical(&out);
    }

    // ── supports (b): why the shape is load-bearing, not cosmetic ──────
    //
    // Two renderings of the identical instant sort in the OPPOSITE order
    // lexically ('.' is 0x2E, '+' is 0x2B, so '.' > '+') from how they sort
    // temporally (equal). A legacy bare `+00:00` row sitting next to a
    // canonical `as_of` search parameter at a near-identical instant is
    // exactly the boundary case tachi#1432's writer migration does not
    // reach (it rewrites self-consistent bare families at write time, it
    // does not touch the comparison). The real-search-path demonstration of
    // the residual risk (a well-separated legacy bare row still resolving
    // correctly, and where the boundary actually lives) is in
    // db/tests/search_ops.rs.
    #[test]
    fn same_instant_bare_offset_and_canonical_forms_are_lexically_unordered() {
        let canonical = normalize_utc_iso("2026-06-15T12:30:00Z").unwrap();
        let bare = "2026-06-15T12:30:00+00:00";
        assert_eq!(
            normalize_utc_iso(bare).unwrap(),
            canonical,
            "both renderings must resolve to the identical instant"
        );
        assert!(
            canonical.as_str() > bare,
            "canonical Millis+Z form must lexically sort AFTER the bare \
             +00:00 form of the identical instant ('.' > '+'), proving a raw \
             string compare cannot treat them as equal: canonical={canonical:?} bare={bare:?}"
        );
    }
}
