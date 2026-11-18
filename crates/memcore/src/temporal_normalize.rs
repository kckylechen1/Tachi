//! Cheap, deterministic temporal cue normalization for memory ingest (tachi#2125).
//!
//! When a caller omits `valid_from` / `valid_until`, parse a small fixed set of
//! cues from text (declared date ranges and relative phrases anchored at a
//! supplied source timestamp). Mentioned dates alone do not establish validity.
//! Caller-explicit params always win. No LLM date guessing.
//!
//! Shared helper (not buried only in `remember.rs`) so save / remember / tests
//! share one path and collide less with parallel remember hygiene work.

use crate::time_compat::millis_z;
use jiff::civil::Date;
use jiff::Timestamp;
use regex::Regex;
use std::str::FromStr;
use std::sync::OnceLock;

/// Resolved ingest validity window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestValidity {
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    /// How the window was obtained (for tests / receipts).
    pub source: IngestValiditySource,
}

/// Provenance of the resolved window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestValiditySource {
    /// At least one of `valid_from` / `valid_until` was supplied by the caller.
    Explicit,
    /// Filled from a cheap text cue.
    Cue,
    /// No explicit params and no recognized cue.
    None,
}

/// Resolve ingest validity.
///
/// - If the caller supplied a non-empty `valid_from` and/or `valid_until`, those
///   values win and **no** cue parsing runs (missing side stays `None`).
/// - Otherwise, parse cues anchored at the supplied source time.
pub fn resolve_ingest_validity(
    explicit_from: Option<&str>,
    explicit_until: Option<&str>,
    text: &str,
    now: Timestamp,
) -> IngestValidity {
    resolve_ingest_validity_from_source(explicit_from, explicit_until, text, Some(now))
}

/// Relative cues require a source time; import time is not an event anchor.
pub fn resolve_ingest_validity_from_source(
    explicit_from: Option<&str>,
    explicit_until: Option<&str>,
    text: &str,
    source_time: Option<Timestamp>,
) -> IngestValidity {
    let from = non_empty(explicit_from);
    let until = non_empty(explicit_until);
    if from.is_some() || until.is_some() {
        return IngestValidity {
            valid_from: from.map(str::to_string),
            valid_until: until.map(str::to_string),
            source: IngestValiditySource::Explicit,
        };
    }

    if let Some(cued) = parse_temporal_cues(text, source_time) {
        return IngestValidity {
            valid_from: cued.valid_from,
            valid_until: cued.valid_until,
            source: IngestValiditySource::Cue,
        };
    }

    IngestValidity {
        valid_from: None,
        valid_until: None,
        source: IngestValiditySource::None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CueWindow {
    valid_from: Option<String>,
    valid_until: Option<String>,
}

fn parse_temporal_cues(text: &str, source_time: Option<Timestamp>) -> Option<CueWindow> {
    // Explicit recording headers describe the source conversation, not validity.
    static HEADER: OnceLock<Regex> = OnceLock::new();
    let header = HEADER.get_or_init(|| {
        Regex::new(r"(?i)^\s*conversation recorded (\d{4}-\d{2}-\d{2})(?:[T ][0-9:.+Z-]+)?\s*:\s*")
            .expect("recording header regex")
    });
    let captures = header.captures(text);
    let header_time = captures.as_ref().and_then(|c| parse_instant(&c[1]));
    let body = captures
        .as_ref()
        .map(|c| &text[c.get(0).unwrap().end()..])
        .unwrap_or(text);
    let lower = body.to_ascii_lowercase();
    if let Some(anchor) = source_time.or(header_time) {
        if let Some(window) = parse_relative_cue(&lower, anchor) {
            return Some(window);
        }
    }

    // A range must connect its own two dates, not borrow a preposition from
    // another sentence. Multiple independent dates never expire the record.
    static RANGE: OnceLock<Regex> = OnceLock::new();
    let range = RANGE.get_or_init(|| {
        Regex::new(
        r"(?i)^\s*[^.!?\n]*\bfrom (\d{4}-\d{2}-\d{2}) (?:until|to) (\d{4}-\d{2}-\d{2})[.!]?\s*$"
    ).expect("validity range regex")
    });
    if collect_ymd_dates(body).len() == 2 {
        if let Some(c) = range.captures(body) {
            let from = parse_instant(&c[1])?;
            let until = parse_instant(&c[2])?;
            if from < until {
                return Some(CueWindow {
                    valid_from: Some(format_utc(from)),
                    valid_until: Some(format_utc(until)),
                });
            }
        }
    }
    // Dates mentioned in a note are evidence, not its validity boundary.
    // Callers retain their timestamp fallback when no declared cue applies.
    None
}

fn parse_relative_cue(lower: &str, now: Timestamp) -> Option<CueWindow> {
    // Order matters: longer phrases before shorter ones.
    let phrases = [
        ("day before yesterday", -2),
        ("the day before yesterday", -2),
        ("last week", -7),
        ("a week ago", -7),
        ("last month", -30),
        ("yesterday", -1),
        ("today", 0),
        ("tomorrow", 1),
    ];

    for (phrase, days) in phrases {
        if contains_phrase(lower, phrase) {
            let shifted = now
                .checked_add(jiff::SignedDuration::from_secs(days * 86_400))
                .ok()?;
            let from = start_of_utc_day(shifted)?;
            return Some(CueWindow {
                valid_from: Some(format_utc(from)),
                valid_until: None,
            });
        }
    }
    None
}

fn contains_phrase(haystack: &str, phrase: &str) -> bool {
    // Cheap word-ish match: require non-alphanumeric (or edges) around the phrase.
    let mut start = 0;
    while let Some(rel) = haystack[start..].find(phrase) {
        let abs = start + rel;
        let before_ok = abs == 0
            || !haystack
                .as_bytes()
                .get(abs - 1)
                .copied()
                .map(|b| b.is_ascii_alphanumeric())
                .unwrap_or(false);
        let end = abs + phrase.len();
        let after_ok = end >= haystack.len()
            || !haystack
                .as_bytes()
                .get(end)
                .copied()
                .map(|b| b.is_ascii_alphanumeric())
                .unwrap_or(false);
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

fn collect_ymd_dates(text: &str) -> Vec<Date> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\b(\d{4})-(\d{2})-(\d{2})\b").expect("ymd regex"));
    let mut out = Vec::new();
    for caps in re.captures_iter(text) {
        let mut ymd = String::with_capacity(10);
        ymd.push_str(&caps[1]);
        ymd.push('-');
        ymd.push_str(&caps[2]);
        ymd.push('-');
        ymd.push_str(&caps[3]);
        // The regex pinned the shape; the parser rejects impossible dates
        // (chrono `from_ymd_opt` returned None for the same inputs).
        if let Ok(date) = ymd.parse::<Date>() {
            out.push(date);
        }
    }
    out
}

fn parse_instant(raw: &str) -> Option<Timestamp> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    Timestamp::from_str(raw).ok().or_else(|| {
        // Date-only fallback, exactly like chrono's `%Y-%m-%d` arm: a
        // 10-character bare date, read as naive local midnight UTC. jiff's
        // `Date` FromStr also accepts a trailing time-of-day and would
        // silently discard it — the length gate keeps this arm date-only
        // (astra review of #2372, C4).
        if raw.len() != 10 {
            return None;
        }
        raw.parse::<Date>().ok().and_then(date_at_utc_midnight)
    })
}

fn date_at_utc_midnight(date: Date) -> Option<Timestamp> {
    // A valid civil date can have a midnight outside jiff's instant range.
    jiff::tz::TimeZone::UTC
        .to_zoned_datetime(date.to_datetime(jiff::civil::time(0, 0, 0, 0)))
        .ok()
        .map(|zoned| zoned.timestamp())
}

fn start_of_utc_day(ts: Timestamp) -> Option<Timestamp> {
    let date = ts
        .to_zoned_datetime(jiff::tz::TimeZone::UTC)
        .datetime()
        .date();
    date_at_utc_midnight(date)
}

fn format_utc(ts: Timestamp) -> String {
    // Match memcore's lexical as_of / validity contract (millis + Z). The
    // historical chrono form zeroed the subsecond part before rendering, so
    // every cued window carries `.000`; truncate back to the whole second
    // to keep those bytes.
    let whole_second = Timestamp::from_second(ts.as_second())
        .expect("whole-second truncation of a valid instant stays in range");
    millis_z(whole_second)
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

/// Whether a memory row is valid at `as_of` (same math as search filtering).
///
/// A row is valid iff:
/// - `COALESCE(NULLIF(valid_from,''), timestamp) <= as_of`
/// - `valid_until IS NULL OR valid_until > as_of`
pub fn entry_valid_at(
    valid_from: &str,
    valid_until: Option<&str>,
    timestamp: &str,
    as_of: &str,
) -> bool {
    let Some(as_of_dt) = parse_instant(as_of) else {
        return false;
    };
    let from = if valid_from.trim().is_empty() {
        timestamp
    } else {
        valid_from
    };
    let starts_ok = parse_instant(from)
        .map(|from_dt| from_dt <= as_of_dt)
        .unwrap_or_else(|| from <= as_of);
    let ends_ok = valid_until
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|until| {
            parse_instant(until)
                .map(|until_dt| until_dt > as_of_dt)
                .unwrap_or_else(|| until > as_of)
        })
        .unwrap_or(true);
    starts_ok && ends_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_now() -> Timestamp {
        Timestamp::from_str("2026-10-25T15:00:00.000Z").unwrap()
    }

    #[test]
    fn explicit_params_win_over_cues() {
        let text = "Meeting was yesterday and also on 2020-01-01";
        let got = resolve_ingest_validity(
            Some("2026-05-01T00:00:00.000Z"),
            Some("2026-06-01T00:00:00.000Z"),
            text,
            fixed_now(),
        );
        assert_eq!(got.source, IngestValiditySource::Explicit);
        assert_eq!(got.valid_from.as_deref(), Some("2026-05-01T00:00:00.000Z"));
        assert_eq!(got.valid_until.as_deref(), Some("2026-06-01T00:00:00.000Z"));
    }

    #[test]
    fn evidence_repair_relative_cues_require_source_time() {
        let got = resolve_ingest_validity_from_source(None, None, "Alice moved yesterday", None);
        assert_eq!(got.source, IngestValiditySource::None);
        let got = resolve_ingest_validity_from_source(
            None,
            None,
            "Alice moved yesterday",
            parse_instant("2023-05-10T12:00:00Z"),
        );
        assert_eq!(got.valid_from.as_deref(), Some("2023-05-09T00:00:00.000Z"));
        let got = resolve_ingest_validity_from_source(
            None,
            None,
            "Conversation recorded 2023-05-10: Alice moved yesterday.",
            None,
        );
        assert_eq!(got.valid_from.as_deref(), Some("2023-05-09T00:00:00.000Z"));
    }

    #[test]
    fn evidence_repair_independent_events_do_not_create_validity() {
        for text in [
            "Alice moved to Rome on 2023-01-01. Bob joined on 2024-01-01.",
            "On 2023-01-01 Alice joined — Bob joined on 2024-01-01.",
            "Role held from 2023-01-01 until 2023-06-30. Bob joined on 2024-01-01.",
        ] {
            let got = resolve_ingest_validity(None, None, text, fixed_now());
            assert_eq!(got.source, IngestValiditySource::None, "{text}: {got:?}");
            assert!(got.valid_from.is_none(), "{text}: {got:?}");
            assert!(got.valid_until.is_none(), "{text}: {got:?}");
            assert!(!entry_valid_at(
                got.valid_from.as_deref().unwrap_or_default(),
                got.valid_until.as_deref(),
                "2026-10-25T00:00:00Z",
                "2025-01-01T00:00:00Z"
            ));
            assert!(entry_valid_at(
                got.valid_from.as_deref().unwrap_or_default(),
                got.valid_until.as_deref(),
                "2026-10-25T00:00:00Z",
                "2026-10-25T00:00:00Z"
            ));
        }
    }

    #[test]
    fn explicit_from_alone_skips_cue_fill() {
        let got = resolve_ingest_validity(
            Some("2026-05-01T00:00:00.000Z"),
            None,
            "yesterday we shipped",
            fixed_now(),
        );
        assert_eq!(got.source, IngestValiditySource::Explicit);
        assert_eq!(got.valid_from.as_deref(), Some("2026-05-01T00:00:00.000Z"));
        assert!(got.valid_until.is_none());
    }

    #[test]
    fn yesterday_cue_is_deterministic() {
        let got =
            resolve_ingest_validity(None, None, "Shipped the fix yesterday evening", fixed_now());
        assert_eq!(got.source, IngestValiditySource::Cue);
        assert_eq!(got.valid_from.as_deref(), Some("2026-10-24T00:00:00.000Z"));
        assert!(got.valid_until.is_none());
    }

    #[test]
    fn today_and_last_week_cues() {
        let today = resolve_ingest_validity(None, None, "Updated today", fixed_now());
        assert_eq!(
            today.valid_from.as_deref(),
            Some("2026-10-25T00:00:00.000Z")
        );

        let week = resolve_ingest_validity(None, None, "Decision from last week", fixed_now());
        assert_eq!(week.valid_from.as_deref(), Some("2026-10-18T00:00:00.000Z"));
    }

    #[test]
    fn mentioned_ymd_date_does_not_establish_validity() {
        let got = resolve_ingest_validity(
            None,
            None,
            "On 2024-03-15 Alice moved to Berlin",
            fixed_now(),
        );
        assert_eq!(got.source, IngestValiditySource::None);
        assert!(got.valid_from.is_none());
        assert!(got.valid_until.is_none());
    }

    #[test]
    fn mentioned_iso_timestamp_does_not_establish_validity() {
        let got = resolve_ingest_validity(
            None,
            None,
            "Incident started 2024-07-04T18:30:00Z in prod",
            fixed_now(),
        );
        assert_eq!(got.source, IngestValiditySource::None);
        assert!(got.valid_from.is_none());
        assert!(got.valid_until.is_none());
    }

    #[test]
    fn unanchored_handoff_date_does_not_backdate_the_note() {
        let text = "Handoff: rollback landed today. Root cause is the incident from 2026-10-25, see #2137.";
        let got = resolve_ingest_validity_from_source(None, None, text, None);
        assert_eq!(got.source, IngestValiditySource::None);
        assert!(got.valid_from.is_none());
        assert!(got.valid_until.is_none());
    }

    #[test]
    fn mentioned_dates_without_source_time_do_not_establish_validity() {
        for text in [
            "Rollback follows the incident on 2026-10-25.",
            "Compare 2024-07-04T18:30:00Z with 2026-10-30T09:00:00Z.",
            "The incident was 2026-10-25; a follow-up is scheduled for 2030-01-01.",
        ] {
            let got = resolve_ingest_validity_from_source(None, None, text, None);
            assert_eq!(got.source, IngestValiditySource::None, "{text}: {got:?}");
            assert!(got.valid_from.is_none(), "{text}: {got:?}");
            assert!(got.valid_until.is_none(), "{text}: {got:?}");
        }
    }

    #[test]
    fn explicit_until_alone_skips_date_and_relative_cue_fill() {
        let got = resolve_ingest_validity(
            None,
            Some("2026-11-01T00:00:00.000Z"),
            "Yesterday we revisited the incident on 2020-01-01.",
            fixed_now(),
        );
        assert_eq!(got.source, IngestValiditySource::Explicit);
        assert!(got.valid_from.is_none());
        assert_eq!(got.valid_until.as_deref(), Some("2026-11-01T00:00:00.000Z"));
    }

    #[test]
    fn range_with_until_sets_both_bounds() {
        let got = resolve_ingest_validity(
            None,
            None,
            "Role held from 2023-01-01 until 2023-06-30",
            fixed_now(),
        );
        assert_eq!(got.source, IngestValiditySource::Cue);
        assert_eq!(got.valid_from.as_deref(), Some("2023-01-01T00:00:00.000Z"));
        assert_eq!(got.valid_until.as_deref(), Some("2023-06-30T00:00:00.000Z"));
    }

    #[test]
    fn no_cue_returns_none() {
        let got = resolve_ingest_validity(None, None, "No dates in this note", fixed_now());
        assert_eq!(got.source, IngestValiditySource::None);
        assert!(got.valid_from.is_none());
        assert!(got.valid_until.is_none());
    }

    #[test]
    fn out_of_range_cues_leave_both_bounds_unset() {
        let source = Timestamp::from_str("9999-12-30T12:00:00Z").unwrap();
        for text in ["ship tomorrow", "from 9999-12-31 until 2026-01-01"] {
            assert_eq!(
                resolve_ingest_validity(None, None, text, source),
                IngestValidity {
                    valid_from: None,
                    valid_until: None,
                    source: IngestValiditySource::None,
                },
                "{text}",
            );
        }
    }

    #[test]
    fn entry_valid_at_window_math() {
        assert!(entry_valid_at(
            "2026-01-01T00:00:00.000Z",
            Some("2026-02-01T00:00:00.000Z"),
            "2026-01-01T00:00:00.000Z",
            "2026-01-15T00:00:00.000Z",
        ));
        assert!(!entry_valid_at(
            "2026-01-01T00:00:00.000Z",
            Some("2026-02-01T00:00:00.000Z"),
            "2026-01-01T00:00:00.000Z",
            "2026-03-01T00:00:00.000Z",
        ));
        // Empty valid_from falls back to timestamp.
        assert!(entry_valid_at(
            "",
            None,
            "2026-01-01T00:00:00.000Z",
            "2026-06-01T00:00:00.000Z",
        ));
    }

    #[test]
    fn relative_cue_is_stable_across_calls() {
        let a = resolve_ingest_validity(None, None, "met yesterday", fixed_now());
        let b = resolve_ingest_validity(None, None, "met yesterday", fixed_now());
        assert_eq!(a, b);
    }
}
