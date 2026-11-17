use rusqlite::{Connection, Result as SqlResult};

use crate::error::MemoryError;
use crate::types::MemoryEntry;

/// The persisted timestamp representations used by memory and health records.
/// Arithmetic and instant identity remain native Jiff operations.
pub trait TimestampFormat {
    fn to_rfc3339_auto(self) -> String;
    fn to_iso_millis(self) -> String;
    fn to_iso_micros(self) -> String;
    fn to_iso_seconds(self) -> String;
}

impl TimestampFormat for jiff::Timestamp {
    fn to_rfc3339_auto(self) -> String {
        let nanos = self.subsec_nanosecond().unsigned_abs();
        let precision = if nanos == 0 {
            0
        } else if nanos.is_multiple_of(1_000_000) {
            3
        } else if nanos.is_multiple_of(1_000) {
            6
        } else {
            9
        };
        retained_year_width(
            jiff::fmt::temporal::DateTimePrinter::new()
                .precision(Some(precision))
                .timestamp_with_offset_to_string(&self, jiff::tz::Offset::UTC),
        )
    }

    fn to_iso_millis(self) -> String {
        retained_year_width(
            jiff::fmt::temporal::DateTimePrinter::new()
                .precision(Some(3))
                .timestamp_to_string(&self),
        )
    }

    fn to_iso_micros(self) -> String {
        retained_year_width(
            jiff::fmt::temporal::DateTimePrinter::new()
                .precision(Some(6))
                .timestamp_to_string(&self),
        )
    }

    fn to_iso_seconds(self) -> String {
        retained_year_width(
            jiff::fmt::temporal::DateTimePrinter::new()
                .precision(Some(0))
                .timestamp_to_string(&self),
        )
    }
}

fn retained_year_width(mut rendered: String) -> String {
    // Temporal prints negative years with six digits; stored RFC3339 used four.
    if rendered.starts_with('-') {
        rendered.drain(1..3);
    }
    rendered
}

/// Unix milliseconds floor before the epoch, preserving stored ordering keys.
pub fn epoch_millisecond(timestamp: jiff::Timestamp) -> i64 {
    timestamp.as_millisecond() - i64::from(timestamp.subsec_nanosecond() % 1_000_000 < 0)
}

fn invalid_timestamp(raw: &str) -> MemoryError {
    MemoryError::InvalidArg(format!("invalid timestamp format: {raw}"))
}

/// Parse the retained RFC3339 grammar without Temporal annotations or leap-second
/// clamping. Additional fractional digits are truncated to nanosecond precision.
pub fn parse_rfc3339_timestamp(raw: &str) -> Result<jiff::Timestamp, MemoryError> {
    let bytes = raw.as_bytes();
    if bytes.len() < 20
        || ![0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18]
            .into_iter()
            .all(|index| bytes[index].is_ascii_digit())
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't' | b' ')
        || bytes[13] != b':'
        || bytes[16] != b':'
        || &bytes[17..19] == b"60"
    {
        return Err(invalid_timestamp(raw));
    }
    let mut offset_start = 19;
    if bytes[offset_start] == b'.' {
        offset_start += 1;
        let fraction_start = offset_start;
        while bytes.get(offset_start).is_some_and(u8::is_ascii_digit) {
            offset_start += 1;
        }
        if offset_start == fraction_start {
            return Err(invalid_timestamp(raw));
        }
    }
    let raw_suffix = &raw[offset_start..];
    // The retained strict grammar permits U+2212 only as the offset minus sign.
    let ascii_suffix = raw_suffix.strip_prefix('−').map(|rest| format!("-{rest}"));
    let suffix = ascii_suffix.as_deref().unwrap_or(raw_suffix);
    let suffix_bytes = suffix.as_bytes();
    if !matches!(suffix_bytes, b"Z" | b"z") {
        if suffix_bytes.len() != 6
            || !matches!(suffix_bytes[0], b'+' | b'-')
            || suffix_bytes[3] != b':'
            || ![1, 2, 4, 5]
                .into_iter()
                .all(|index| suffix_bytes[index].is_ascii_digit())
        {
            return Err(invalid_timestamp(raw));
        }
        let hours = u16::from(suffix_bytes[1] - b'0') * 10 + u16::from(suffix_bytes[2] - b'0');
        let minutes = u16::from(suffix_bytes[4] - b'0') * 10 + u16::from(suffix_bytes[5] - b'0');
        if hours > 23 || minutes > 59 {
            return Err(invalid_timestamp(raw));
        }
    }
    let parsed = if offset_start > 29 || ascii_suffix.is_some() {
        format!("{}{}", &raw[..offset_start.min(29)], suffix).parse()
    } else {
        raw.parse()
    };
    parsed.map_err(|_| invalid_timestamp(raw))
}

/// Parse the existing relaxed UTC timestamp forms used by stored memory rows.
/// Strict API inputs use parse_rfc3339_timestamp instead.
pub fn parse_timestamp(raw: &str) -> Result<jiff::Timestamp, MemoryError> {
    if let Ok(timestamp) = parse_rfc3339_timestamp(raw) {
        return Ok(timestamp);
    }
    static RELAXED: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let grammar = RELAXED.get_or_init(|| {
        regex::Regex::new(concat!(
            r"(?ix)^\s*(?<year>[+-][0-9]+|[0-9]{1,4})\s*-\s*(?<month>[0-9]{1,2})",
            r"\s*-\s*(?<day>[0-9]{1,2})[t\x20]\s*(?<hour>[0-9]{1,2})",
            r"\s*:\s*(?<minute>[0-9]{1,2})\s*:\s*(?<second>[0-9]{1,2})",
            r"(?:\.(?<fraction>[0-9]+))?\s*(?<offset>utc|z|[+−-][0-9]{2}[:\s]*[0-9]{2})\s*$"
        ))
        .expect("retained UTC timestamp grammar")
    });
    let captures = grammar
        .captures(raw)
        .ok_or_else(|| invalid_timestamp(raw))?;
    let field = |name: &str| -> Result<i16, MemoryError> {
        captures[name].parse().map_err(|_| invalid_timestamp(raw))
    };
    let small_field = |name| -> Result<i8, MemoryError> {
        i8::try_from(field(name)?).map_err(|_| invalid_timestamp(raw))
    };
    let fraction = captures.name("fraction").map_or("", |value| value.as_str());
    let nanos = if fraction.is_empty() {
        0
    } else {
        format!("{:0<9}", &fraction[..fraction.len().min(9)])
            .parse::<i32>()
            .map_err(|_| invalid_timestamp(raw))?
    };
    let civil = jiff::civil::DateTime::new(
        field("year")?,
        small_field("month")?,
        small_field("day")?,
        small_field("hour")?,
        small_field("minute")?,
        small_field("second")?,
        nanos,
    )
    .map_err(|_| invalid_timestamp(raw))?;
    let offset = &captures["offset"];
    let offset = if offset.eq_ignore_ascii_case("utc") || offset.eq_ignore_ascii_case("z") {
        jiff::tz::Offset::UTC
    } else {
        let digits: String = offset.chars().filter(char::is_ascii_digit).collect();
        let hours = digits[..2]
            .parse::<i32>()
            .map_err(|_| invalid_timestamp(raw))?;
        let minutes = digits[2..]
            .parse::<i32>()
            .map_err(|_| invalid_timestamp(raw))?;
        if hours > 23 || minutes > 59 {
            return Err(invalid_timestamp(raw));
        }
        let seconds = (hours * 3600 + minutes * 60)
            * if offset.starts_with(['-', '−']) {
                -1
            } else {
                1
            };
        jiff::tz::Offset::from_seconds(seconds).map_err(|_| invalid_timestamp(raw))?
    };
    offset
        .to_timestamp(civil)
        .map_err(|_| invalid_timestamp(raw))
}

// ─── Row mapping ──────────────────────────────────────────────────────────────

/// The one sanctioned renderer for "now" as an RFC3339 UTC timestamp:
/// millisecond precision, `Z` suffix (`...123Z`). Every writer that stamps
/// `created_at`/`updated_at`/`valid_until` must go through this (or
/// [`normalize_utc_iso`]/[`normalize_utc_iso_or_now`]) — several of those
/// columns are compared **lexically** (no `datetime()` wrapper) by the as-of
/// search predicates, so a bare `jiff::Timestamp::now().to_rfc3339_auto()` (numeric
/// offset, auto-precision fraction) sorts wrong against this format.
#[inline]
pub fn now_utc_iso() -> String {
    jiff::Timestamp::now().to_iso_millis()
}

#[inline]
pub fn normalize_utc_iso(ts: &str) -> Result<String, MemoryError> {
    let raw = ts.trim();
    if raw.is_empty() {
        return Err(MemoryError::InvalidArg("empty timestamp".to_string()));
    }

    if let Ok(dt) = crate::parse_rfc3339_timestamp(raw) {
        return Ok(dt.to_iso_millis());
    }

    if let Ok(dt) = parse_timestamp(raw) {
        return Ok(dt.to_iso_millis());
    }

    Err(MemoryError::InvalidArg(format!(
        "invalid timestamp format: {}",
        ts
    )))
}

pub(crate) fn normalize_sqlite_as_of(conn: &Connection, ts: &str) -> Result<String, MemoryError> {
    let canonical = normalize_utc_iso(ts).map_err(|error| {
        MemoryError::InvalidArg(format!(
            "as_of instant is malformed or outside the SQLite julianday range: {error}"
        ))
    })?;
    let anchored: Option<f64> = conn
        .prepare_cached("SELECT julianday(?1)")?
        .query_row([&canonical], |row| row.get::<_, Option<f64>>(0))?;
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

/// Run one write atomically, composing with a transaction the caller already
/// owns. A top-level call takes the SQLite write lock before its first read;
/// a nested call uses a savepoint and leaves the caller's transaction open.
#[cfg(feature = "admin")]
pub(crate) fn with_composable_write<T>(
    conn: &Connection,
    write: impl FnOnce(&Connection) -> Result<T, MemoryError>,
) -> Result<T, MemoryError> {
    const SAVEPOINT: &str = "memcore_composable_write";
    let owns_transaction = conn.is_autocommit();
    if owns_transaction {
        conn.execute_batch("BEGIN IMMEDIATE")?;
    }
    if let Err(error) = conn.execute_batch(&format!("SAVEPOINT {SAVEPOINT}")) {
        if owns_transaction {
            let _ = conn.execute_batch("ROLLBACK");
        }
        return Err(error.into());
    }

    let result = write(conn);
    match result {
        Ok(value) => {
            if let Err(error) = conn.execute_batch(&format!("RELEASE {SAVEPOINT}")) {
                let _ = conn.execute_batch(&format!("ROLLBACK TO {SAVEPOINT}"));
                let _ = conn.execute_batch(&format!("RELEASE {SAVEPOINT}"));
                if owns_transaction {
                    let _ = conn.execute_batch("ROLLBACK");
                }
                return Err(error.into());
            }
            if owns_transaction {
                if let Err(error) = conn.execute_batch("COMMIT") {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(error.into());
                }
            }
            Ok(value)
        }
        Err(error) => {
            let _ = conn.execute_batch(&format!("ROLLBACK TO {SAVEPOINT}"));
            let _ = conn.execute_batch(&format!("RELEASE {SAVEPOINT}"));
            if owns_transaction {
                let _ = conn.execute_batch("ROLLBACK");
            }
            Err(error)
        }
    }
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

    #[test]
    fn grouped_precision_and_negative_epoch_ordering_survive_time_migration() {
        for (raw, expected) in [
            ("2026-01-02T03:04:05Z", "2026-01-02T03:04:05+00:00"),
            ("2026-01-02T03:04:05.1Z", "2026-01-02T03:04:05.100+00:00"),
            (
                "2026-01-02T03:04:05.1234Z",
                "2026-01-02T03:04:05.123400+00:00",
            ),
            ("1969-12-31T23:59:59.1Z", "1969-12-31T23:59:59.100+00:00"),
            ("-0001-01-02T03:04:05.1Z", "-0001-01-02T03:04:05.100+00:00"),
        ] {
            assert_eq!(parse_timestamp(raw).unwrap().to_rfc3339_auto(), expected);
        }
        for nanos in [-1_000_001_i128, -1_000_000, -1, 0, 1, 1_000_001] {
            let instant = jiff::Timestamp::from_nanosecond(nanos).unwrap();
            assert_eq!(
                i128::from(epoch_millisecond(instant)),
                nanos.div_euclid(1_000_000)
            );
        }
    }

    #[test]
    fn stored_relaxed_forms_do_not_expand_strict_timestamp_inputs() {
        let expected = parse_rfc3339_timestamp("2012-12-12T12:12:12Z").unwrap();
        for raw in [
            "2012-12-12 12:12:12+0000",
            "2012-12-12 12:12:12 UTC",
            " 2012 - 12 - 12 12 : 12 : 12+00:: 00 ",
            "+000002012-12-12T12:12:12Z",
        ] {
            assert_eq!(parse_timestamp(raw).unwrap(), expected);
            assert!(parse_rfc3339_timestamp(raw).is_err());
        }
        for (raw, expected) in [
            ("2012-12-12T12:12:12−00:00", "2012-12-12T12:12:12Z"),
            ("2012-12-12T12:12:12−05:30", "2012-12-12T17:42:12Z"),
            (
                "2012-12-12T12:12:12.123456789999−05:30",
                "2012-12-12T17:42:12.123456789Z",
            ),
        ] {
            assert_eq!(
                parse_rfc3339_timestamp(raw).unwrap(),
                parse_rfc3339_timestamp(expected).unwrap()
            );
        }
        assert_eq!(
            normalize_utc_iso("2026-01-02t03:04:05.123456789999z").unwrap(),
            "2026-01-02T03:04:05.123Z"
        );
        for raw in [
            "2026-01-02T03:04Z",
            "2026-01-02T03:04:05Z[UTC]",
            "2026-01-02T03:04:60Z",
            "+10000-01-02T03:04:05Z",
            "2026-01-02T03:04:05+24:00",
            "02012-12-12T12:12:12Z",
        ] {
            assert!(parse_timestamp(raw).is_err(), "accepted {raw}");
            assert!(parse_rfc3339_timestamp(raw).is_err(), "accepted {raw}");
        }
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
