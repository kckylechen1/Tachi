// time_compat — the chrono-parity timestamp renderers for the chrono→jiff
// migration (#2361 slice 4). Persisted strings keep the byte forms chrono
// produced so the migration is invisible to every reader, and — for the
// memcore canonical format — to SQLite's **lexical** column comparisons:
// jiff's `Timestamp::to_string()` (trimmed fraction, `Z`) sorts `.1Z` after
// `.15Z`, so it must never reach a stored or compared column.
//
// Three forms, all still live in this workspace:
// - `millis_z`: `...SS.mmmZ` — chrono `to_rfc3339_opts(SecondsFormat::Millis,
//   true)`, the one sanctioned memcore store format (`now_utc_iso` et al.).
// - `secs_z`: `...SSZ` — chrono `to_rfc3339_opts(SecondsFormat::Secs, true)`,
//   the frozen-legacy sticky-cutover snapshot form (digest-adjacent; byte
//   drift would rewrite frozen history).
// - `rfc3339_compat`: AutoSi 0/3/6/9-digit fraction by alignment with
//   `+00:00` — chrono `to_rfc3339()`, the persisted receipt/health-stamp form
//   in tachi-llm (same bytes as tachi-params' private copy; converging here
//   first since memcore is the dependency root).

use jiff::Timestamp;
use std::str::FromStr;

/// Format a timestamp exactly as chrono's
/// `DateTime::<Utc>::to_rfc3339_opts(SecondsFormat::Secs, true)` did:
/// `YYYY-MM-DDTHH:MM:SSZ` — fraction dropped (truncated), never rounded.
pub fn secs_z(ts: Timestamp) -> String {
    let dt = ts.to_zoned_datetime(jiff::tz::TimeZone::UTC).datetime();
    format!(
        "{}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year_field(dt.year()),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
    )
}

/// Format a timestamp exactly as chrono's
/// `DateTime::<Utc>::to_rfc3339_opts(SecondsFormat::Micros, true)` did:
/// `YYYY-MM-DDTHH:MM:SS.uuuuuuZ` — always six fraction digits (truncated,
/// never rounded). The delivery-intent cursor form.
pub fn micros_z(ts: Timestamp) -> String {
    let dt = ts.to_zoned_datetime(jiff::tz::TimeZone::UTC).datetime();
    let micros = dt.subsec_nanosecond() / 1_000;
    format!(
        "{}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        year_field(dt.year()),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
        micros,
    )
}

/// Format a timestamp exactly as chrono's
/// `DateTime::<Utc>::to_rfc3339_opts(SecondsFormat::Millis, true)` did:
/// `YYYY-MM-DDTHH:MM:SS.mmmZ` — always three fraction digits (a whole second
/// renders `.000`), fraction **truncated** (integer-divided) like chrono,
/// never rounded, and `Z` for the UTC offset.
pub fn millis_z(ts: Timestamp) -> String {
    let dt = ts.to_zoned_datetime(jiff::tz::TimeZone::UTC).datetime();
    let millis = dt.subsec_nanosecond() / 1_000_000;
    format!(
        "{}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year_field(dt.year()),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
        millis,
    )
}

/// Format a timestamp exactly as chrono's `DateTime::<Utc>::to_rfc3339` did:
/// `YYYY-MM-DDTHH:MM:SS[.fff|.ffffff|.fffffffff]+00:00`, fraction digits by
/// 0/3/6/9 alignment (never trimmed), four year digits with a leading sign
/// for BCE years (chrono prints `-0001`; Rust's `{:04}` counts the sign in
/// its width and would print `-001`).
pub fn rfc3339_compat(ts: Timestamp) -> String {
    let dt = ts.to_zoned_datetime(jiff::tz::TimeZone::UTC).datetime();
    let nanos = dt.subsec_nanosecond();
    let fraction = if nanos == 0 {
        String::new()
    } else if nanos % 1_000_000 == 0 {
        format!(".{:03}", nanos / 1_000_000)
    } else if nanos % 1_000 == 0 {
        format!(".{:06}", nanos / 1_000)
    } else {
        format!(".{:09}", nanos)
    };
    format!(
        "{}-{:02}-{:02}T{:02}:{:02}:{:02}{fraction}+00:00",
        year_field(dt.year()),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
    )
}

/// chrono's `(later - earlier).num_seconds()` parity: whole seconds of the
/// interval, truncated toward zero. Computing this as
/// `later.as_second() - earlier.as_second()` is WRONG at fractional
/// boundaries (12:00:00.9 → 12:30:01.1 is 1800 whole seconds, not 1801) —
/// TTL/backoff/cooldown decisions sit exactly on those boundaries
/// (astra review of #2372, C4).
pub fn whole_seconds_between(later: Timestamp, earlier: Timestamp) -> i64 {
    (later.as_millisecond() - earlier.as_millisecond()) / 1_000
}

/// Parse with the grammar chrono's `parse_from_rfc3339` accepted AND SQLite's
/// `julianday()` can read: plain RFC3339, unsigned four-digit year. jiff alone
/// additionally accepts Temporal extensions (six-digit signed years, `[UTC]`
/// annotations, omitted seconds) that SQLite's date functions refuse, so a
/// guard whose accept/reject must agree with a julianday-based reader routes
/// through this stricter gate instead of bare `Timestamp::from_str`
/// (astra review of #2372, C4: lease write guard vs julianday readers).
pub fn parse_rfc3339_strict(s: &str) -> Option<Timestamp> {
    use regex::Regex;
    use std::sync::OnceLock;
    static SHAPE: OnceLock<Regex> = OnceLock::new();
    let shape = SHAPE.get_or_init(|| {
        Regex::new(r"(?i)^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,9})?(Z|[+-]\d{2}:\d{2})$")
            .expect("strict RFC3339 shape regex")
    });
    let trimmed = s.trim();
    if !shape.is_match(trimmed) {
        return None;
    }
    Timestamp::from_str(trimmed).ok()
}

/// chrono's year field: BCE years carry a sign outside the four digits.
fn year_field(year: i16) -> String {
    if year < 0 {
        format!("-{:04}", -(year as i32))
    } else {
        format!("{year:04}")
    }
}

#[cfg(test)]
mod tests {
    use super::{millis_z, rfc3339_compat};
    use jiff::Timestamp;
    use std::str::FromStr;

    fn ms(s: &str) -> String {
        millis_z(Timestamp::from_str(s).expect("fixture parses"))
    }

    fn compat(s: &str) -> String {
        rfc3339_compat(Timestamp::from_str(s).expect("fixture parses"))
    }

    #[test]
    fn millis_z_matches_chrono_normal_form() {
        assert_eq!(ms("2026-12-01T00:00:00Z"), "2026-12-01T00:00:00.000Z");
        assert_eq!(ms("2026-12-01T00:00:00.100Z"), "2026-12-01T00:00:00.100Z");
        assert_eq!(
            ms("2026-12-01T00:00:00.123456Z"),
            "2026-12-01T00:00:00.123Z"
        );
    }

    #[test]
    fn millis_z_truncates_sub_millisecond_it_does_not_round() {
        // 123.999ms -> 123ms: chrono's SecondsFormat::Millis integer-divides.
        assert_eq!(
            ms("2026-12-01T00:00:00.123999999Z"),
            "2026-12-01T00:00:00.123Z"
        );
    }

    #[test]
    fn millis_z_sorts_lexically_as_chronologically() {
        // The invariant behind the lexical SQL predicates: with a fixed
        // 3-digit fraction and `Z` suffix, byte order is time order. A
        // trimmed-fraction writer (`...1Z`) would sort after `...15Z`.
        let a = ms("2026-12-01T00:00:00.100Z");
        let b = ms("2026-12-01T00:00:00.150Z");
        let c = ms("2026-12-01T00:00:01.000Z");
        assert!(
            a < b && b < c,
            "lexical order must track chronological: {a} < {b} < {c}"
        );
    }

    #[test]
    fn millis_z_signs_bce_years_outside_the_four_digits() {
        assert_eq!(ms("0000-01-01T00:00:00+01:00"), "-0001-12-31T23:00:00.000Z");
    }

    #[test]
    fn millis_z_round_trips_at_millisecond_alignment() {
        // Sub-millisecond input is deliberately truncated (lossy); at ms
        // alignment the rendered form parses back to the same instant.
        for s in [
            "2026-12-01T00:00:00Z",
            "2026-12-01T00:00:00.100Z",
            "1969-12-31T23:59:59.999Z",
        ] {
            let out = ms(s);
            let back = Timestamp::from_str(&out).expect("round trip parses");
            assert_eq!(
                back,
                Timestamp::from_str(s).expect("fixture parses"),
                "round trip changed the instant for {s}"
            );
        }
    }

    #[test]
    fn rfc3339_compat_matches_chrono_normal_form() {
        assert_eq!(compat("2026-12-01T00:00:00Z"), "2026-12-01T00:00:00+00:00");
        assert_eq!(
            compat("2026-12-01T00:00:00.100Z"),
            "2026-12-01T00:00:00.100+00:00"
        );
        assert_eq!(
            compat("2026-12-01T00:00:00.123456Z"),
            "2026-12-01T00:00:00.123456+00:00"
        );
        // Offset conversion crossing into BCE: the sign sits outside the
        // four year digits (`-0001`, not `-001`).
        assert_eq!(
            compat("0000-01-01T00:00:00+01:00"),
            "-0001-12-31T23:00:00+00:00"
        );
    }

    #[test]
    fn rfc3339_compat_round_trips_through_jiff_parser() {
        // Round-trip holds for every form both parsers accept (all writers
        // in this workspace emit these). The BCE case is deliberately
        // excluded: chrono's own to_rfc3339 prints `-0001-…`, which chrono's
        // own parse_from_rfc3339 rejects — the formatter matches those bytes
        // and the asymmetry is inherited from chrono, not introduced.
        for s in [
            "2026-12-01T00:00:00Z",
            "2026-12-01T00:00:00.100Z",
            "2026-12-01T00:00:00.123456Z",
            "1969-12-31T23:59:59.999999999Z",
        ] {
            let out = compat(s);
            let back = Timestamp::from_str(&out).expect("round trip parses");
            assert_eq!(
                back,
                Timestamp::from_str(s).expect("fixture parses"),
                "round trip changed the instant for {s}"
            );
        }
    }

    /// Declared parser-parity drift vs the retired chrono validation chain
    /// (`DateTime::parse_from_rfc3339` → `DateTime::<Utc>` fallback): jiff
    /// accepts Temporal extensions, rejects instants whose UTC conversion
    /// leaves its year range, and collapses leap seconds onto `:59` (pinned
    /// in tachi-params slice 3; repeated here because `normalize_utc_iso`
    /// switches onto the same parser). These assertions pin the NEW behavior
    /// so any future parser change is a visible test flip, not silent drift.
    #[test]
    fn parser_parity_drift_is_pinned() {
        // Loosened: Temporal extensions the old gate rejected.
        assert!(Timestamp::from_str("2026-12-01T00:00Z").is_ok());
        assert!(Timestamp::from_str("2026-12-01T00:00:00Z[UTC]").is_ok());
        // Tightened: jiff's instant ceiling sits INSIDE the last calendar
        // day of 9999 (the doc range ends at 253402207200s), so civil
        // 9999-12-31 — a date the HTTP-date and scoring fallback paths can
        // legitimately hold — fails the UTC conversion. Consumers treat
        // that as "unusable instant", never a panic (astra review, C4).
        assert!(Timestamp::from_str("9999-12-30T22:00:00Z").is_ok());
        assert!(Timestamp::from_str("9999-12-31T00:00:00Z").is_err());
        // Tightened: five/six-digit extended years outside ±9999 are
        // rejected (`+10000-01-01` was parsed by the old chain and refused
        // by the SQLite julianday gate instead; the search as_of tests use
        // the six-digit BCE form below to keep exercising that gate).
        assert!(Timestamp::from_str("+10000-01-01T00:00:00Z").is_err());
        // Leap seconds collapse onto :59 (chrono kept a distinct instant).
        let leap = Timestamp::from_str("2016-12-31T23:59:60Z").expect("leap parses");
        let flat = Timestamp::from_str("2016-12-31T23:59:59Z").expect("flat parses");
        assert_eq!(leap, flat);
        // Offset-less civil time is still rejected by both.
        assert!(Timestamp::from_str("2026-12-01T00:00:00").is_err());
        // BCE years parse ONLY in Temporal's six-digit signed spelling;
        // a bare four-digit negative year is not a valid spelling. The
        // six-digit form canonicalizes to the four-digit-minus-sign
        // rendering (`-004713` → `-4713-…`), which is what reaches — and
        // is refused by — SQLite's julianday range gate.
        assert!(Timestamp::from_str("-004713-01-01T00:00:00Z").is_ok());
        assert_eq!(
            super::millis_z(Timestamp::from_str("-004713-01-01T00:00:00Z").unwrap()),
            "-4713-01-01T00:00:00.000Z"
        );
        assert!(Timestamp::from_str("-4713-01-01T00:00:00Z").is_err());
        assert!(Timestamp::from_str("0000-01-01T00:00:00Z").is_ok());
    }

    #[test]
    fn whole_seconds_between_truncates_like_chrono_num_seconds() {
        // chrono `(later - earlier).num_seconds()` truncates toward zero at
        // the fractional boundary; subtracting epoch-second values would
        // round UP here (1801) and flip TTL/stale decisions (astra review
        // of #2372, C4).
        let earlier = Timestamp::from_str("2026-01-01T12:00:00.900Z").unwrap();
        let later = Timestamp::from_str("2026-01-01T12:30:01.100Z").unwrap();
        assert_eq!(super::whole_seconds_between(later, earlier), 1_800);
        // Negative intervals truncate toward zero as well (clock skew).
        assert_eq!(super::whole_seconds_between(earlier, later), -1_800);
        assert_eq!(super::whole_seconds_between(later, later), 0);
    }

    #[test]
    fn strict_rfc3339_gate_matches_the_julianday_readable_grammar() {
        use super::parse_rfc3339_strict;
        // The two shapes every in-tree writer produces.
        assert!(parse_rfc3339_strict("2026-11-18T12:00:00.000Z").is_some());
        assert!(parse_rfc3339_strict("2026-11-18T12:00:00+08:00").is_some());
        assert!(parse_rfc3339_strict("  2026-11-18T12:00:00Z  ").is_some());
        // Temporal extensions parse under jiff but NOT under SQLite's
        // julianday readers — the strict gate must refuse them.
        assert!(parse_rfc3339_strict("2026-11-18T12:00Z").is_none());
        assert!(parse_rfc3339_strict("2026-11-18T12:00:00Z[UTC]").is_none());
        assert!(parse_rfc3339_strict("-004713-01-01T00:00:00Z").is_none());
        // Fraction longer than nine digits: jiff refuses, chrono ignored the
        // excess, julianday reads it — refuse here (write/read agreement
        // that both old guards would also have made, differently).
        assert!(parse_rfc3339_strict("2026-11-18T12:00:00.1234567890Z").is_none());
    }

    #[test]
    fn epoch_millisecond_accessor_truncates_toward_zero_like_chrono() {
        // Pins jiff's `as_millisecond` rounding for pre-epoch instants:
        // chrono's `timestamp_millis()` truncated toward zero, so sub-second
        // pre-epoch values map to 0, never -1 (astra review of #2372
        // alleged a tie-break change; this pin proves the parity).
        let pre_epoch =
            Timestamp::from_str("1969-12-31T23:59:59.999999Z").expect("pre-epoch parses");
        assert_eq!(pre_epoch.as_millisecond(), 0);
    }
}
