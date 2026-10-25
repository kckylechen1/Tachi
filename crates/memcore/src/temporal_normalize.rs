//! Cheap, deterministic temporal cue normalization for memory ingest (tachi#2125).
//!
//! When a caller omits `valid_from` / `valid_until`, parse a small fixed set of
//! cues from text (explicit ISO / `YYYY-MM-DD`, relative phrases anchored at
//! ingest "now"). Caller-explicit params always win. No LLM date guessing.
//!
//! Shared helper (not buried only in `remember.rs`) so save / remember / tests
//! share one path and collide less with parallel remember hygiene work.

use chrono::{DateTime, Duration, NaiveDate, SecondsFormat, Timelike, Utc};
use regex::Regex;
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
/// - Otherwise, parse cheap cues from `text` anchored at `now`.
pub fn resolve_ingest_validity(
    explicit_from: Option<&str>,
    explicit_until: Option<&str>,
    text: &str,
    now: DateTime<Utc>,
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

    if let Some(cued) = parse_temporal_cues(text, now) {
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

fn parse_temporal_cues(text: &str, now: DateTime<Utc>) -> Option<CueWindow> {
    let lower = text.to_ascii_lowercase();

    // Relative phrases first (word-boundary-ish via whitespace/punct).
    if let Some(window) = parse_relative_cue(&lower, now) {
        return Some(window);
    }

    // Explicit ISO-8601 timestamps (prefer earliest as valid_from; optional second as until).
    let iso_hits = collect_iso_datetimes(text);
    if !iso_hits.is_empty() {
        return Some(window_from_ordered_instants(&iso_hits, text));
    }

    // Bare YYYY-MM-DD dates.
    let date_hits = collect_ymd_dates(text);
    if !date_hits.is_empty() {
        let instants: Vec<DateTime<Utc>> =
            date_hits.into_iter().map(date_at_utc_midnight).collect();
        return Some(window_from_ordered_instants(&instants, text));
    }

    None
}

fn parse_relative_cue(lower: &str, now: DateTime<Utc>) -> Option<CueWindow> {
    // Order matters: longer phrases before shorter ones.
    let phrases: &[(&str, i64, Option<i64>)] = &[
        ("day before yesterday", -2, None),
        ("the day before yesterday", -2, None),
        ("last week", -7, None),
        ("a week ago", -7, None),
        ("last month", -30, None),
        ("yesterday", -1, None),
        ("today", 0, None),
        ("tomorrow", 1, None),
    ];

    for &(phrase, from_offset_days, until_offset_days) in phrases {
        if contains_phrase(lower, phrase) {
            let from = start_of_utc_day(now + Duration::days(from_offset_days));
            let until = until_offset_days.map(|d| start_of_utc_day(now + Duration::days(d)));
            return Some(CueWindow {
                valid_from: Some(format_utc(from)),
                valid_until: until.map(format_utc),
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

fn collect_iso_datetimes(text: &str) -> Vec<DateTime<Utc>> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?x)
            \b
            \d{4}-\d{2}-\d{2}
            [T ]
            \d{2}:\d{2}:\d{2}
            (?:\.\d+)?
            (?:Z|[+-]\d{2}:?\d{2})?
            \b
            ",
        )
        .expect("iso datetime regex")
    });
    let mut out = Vec::new();
    for m in re.find_iter(text) {
        let raw = m.as_str().replace(' ', "T");
        if let Some(dt) = parse_instant(&raw) {
            out.push(dt);
        }
    }
    out
}

fn collect_ymd_dates(text: &str) -> Vec<NaiveDate> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\b(\d{4})-(\d{2})-(\d{2})\b").expect("ymd regex"));
    let mut out = Vec::new();
    for caps in re.captures_iter(text) {
        let y: i32 = caps[1].parse().unwrap_or(0);
        let m: u32 = caps[2].parse().unwrap_or(0);
        let d: u32 = caps[3].parse().unwrap_or(0);
        if let Some(date) = NaiveDate::from_ymd_opt(y, m, d) {
            out.push(date);
        }
    }
    out
}

fn window_from_ordered_instants(instants: &[DateTime<Utc>], text: &str) -> CueWindow {
    let mut sorted = instants.to_vec();
    sorted.sort();
    let from = sorted.first().copied();
    // Only treat a second instant as valid_until when the text suggests a range.
    let looks_like_range = {
        let lower = text.to_ascii_lowercase();
        lower.contains(" until ")
            || lower.contains(" through ")
            || lower.contains(" thru ")
            || lower.contains(" to ")
            || lower.contains(" between ")
            || lower.contains(" - ")
            || lower.contains("–")
            || lower.contains("—")
    };
    let until = if looks_like_range && sorted.len() >= 2 {
        sorted.get(1).copied()
    } else {
        None
    };
    CueWindow {
        valid_from: from.map(format_utc),
        valid_until: until.map(format_utc),
    }
}

fn parse_instant(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|_| raw.parse::<DateTime<Utc>>())
        .ok()
        .or_else(|| {
            // Date-only already handled elsewhere; try naive local midnight as UTC.
            NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                .ok()
                .map(date_at_utc_midnight)
        })
}

fn date_at_utc_midnight(date: NaiveDate) -> DateTime<Utc> {
    date.and_hms_opt(0, 0, 0)
        .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        .unwrap_or_else(|| DateTime::<Utc>::UNIX_EPOCH)
}

fn start_of_utc_day(dt: DateTime<Utc>) -> DateTime<Utc> {
    dt.date_naive()
        .and_hms_opt(0, 0, 0)
        .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        .unwrap_or(dt)
}

fn format_utc(dt: DateTime<Utc>) -> String {
    // Match memcore's lexical as_of / validity contract (millis + Z).
    dt.with_nanosecond(0)
        .unwrap_or(dt)
        .to_rfc3339_opts(SecondsFormat::Millis, true)
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

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-25T15:00:00.000Z")
            .unwrap()
            .with_timezone(&Utc)
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
    fn ymd_date_becomes_valid_from_midnight_z() {
        let got = resolve_ingest_validity(
            None,
            None,
            "On 2024-03-15 Alice moved to Berlin",
            fixed_now(),
        );
        assert_eq!(got.source, IngestValiditySource::Cue);
        assert_eq!(got.valid_from.as_deref(), Some("2024-03-15T00:00:00.000Z"));
        assert!(got.valid_until.is_none());
    }

    #[test]
    fn iso_timestamp_in_text() {
        let got = resolve_ingest_validity(
            None,
            None,
            "Incident started 2024-07-04T18:30:00Z in prod",
            fixed_now(),
        );
        assert_eq!(got.source, IngestValiditySource::Cue);
        assert_eq!(got.valid_from.as_deref(), Some("2024-07-04T18:30:00.000Z"));
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
