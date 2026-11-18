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
        // Tightened: instant leaves jiff's year range after UTC conversion.
        assert!(Timestamp::from_str("9999-12-31T23:30:00-01:00").is_err());
        // Tightened: years past 9999 are outside jiff entirely. The old
        // chrono chain parsed `+10000-01-01` and let SQLite's julianday
        // range gate reject it; jiff refuses at the parse, so the two
        // rejection messages differ for this input family (the search
        // as_of tests re-anchored on a low-end julianday fixture because
        // of exactly this).
        assert!(Timestamp::from_str("+10000-01-01T00:00:00Z").is_err());
        // Leap seconds collapse onto :59 (chrono kept a distinct instant).
        let leap = Timestamp::from_str("2016-12-31T23:59:60Z").expect("leap parses");
        let flat = Timestamp::from_str("2016-12-31T23:59:59Z").expect("flat parses");
        assert_eq!(leap, flat);
        // Offset-less civil time is still rejected by both.
        assert!(Timestamp::from_str("2026-12-01T00:00:00").is_err());
    }
}
