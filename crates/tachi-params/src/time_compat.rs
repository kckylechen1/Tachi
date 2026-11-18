// time_compat — the chrono-parity timestamp surface for tachi-params during
// the chrono→jiff migration (#2361). Persisted and wire-visible strings keep
// chrono's `to_rfc3339` normal form (AutoSi fraction, `+00:00` offset) so the
// migration is byte-stable for every reader; jiff's `Timestamp::to_string()`
// (Z spelling, trailing-zero-trimmed fraction) must not leak into stored or
// wire formats.

/// Format a timestamp exactly as chrono's `DateTime::<Utc>::to_rfc3339` did:
/// `YYYY-MM-DDTHH:MM:SS[.fff|.ffffff|.fffffffff]+00:00`, fraction digits by
/// 0/3/6/9 alignment (never trimmed), four year digits with a leading sign
/// for BCE years (chrono prints `-0001`, Rust's `{:04}` would print `-001`).
pub(crate) fn rfc3339_compat(ts: jiff::Timestamp) -> String {
    let dt = ts.to_zoned_datetime(jiff::tz::TimeZone::UTC).datetime();
    let nanos = dt.subsec_nanosecond().unsigned_abs();
    let fraction = if nanos == 0 {
        String::new()
    } else if nanos.is_multiple_of(1_000_000) {
        format!(".{:03}", nanos / 1_000_000)
    } else if nanos.is_multiple_of(1_000) {
        format!(".{:06}", nanos / 1_000)
    } else {
        format!(".{:09}", nanos)
    };
    let year = dt.year();
    let year_str = if year < 0 {
        format!("-{:04}", -(year as i32))
    } else {
        format!("{:04}", year)
    };
    format!(
        "{year_str}-{:02}-{:02}T{:02}:{:02}:{:02}{fraction}+00:00",
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

#[cfg(test)]
mod tests {
    use super::rfc3339_compat;
    use std::str::FromStr;

    fn fmt(s: &str) -> String {
        rfc3339_compat(jiff::Timestamp::from_str(s).expect("fixture parses"))
    }

    #[test]
    fn formatter_matches_chrono_normal_form() {
        // Mirrors chrono to_rfc3339 outputs for the same instants; the
        // taskintent golden (`2026-12-01T00:00:00+00:00`) pins this on the
        // wire, these pin the fraction and sign shapes.
        assert_eq!(fmt("2026-12-01T00:00:00Z"), "2026-12-01T00:00:00+00:00");
        assert_eq!(
            fmt("2026-12-01T00:00:00.100Z"),
            "2026-12-01T00:00:00.100+00:00"
        );
        assert_eq!(
            fmt("2026-12-01T00:00:00.123456Z"),
            "2026-12-01T00:00:00.123456+00:00"
        );
        assert_eq!(
            fmt("1969-12-31T23:59:59.999999999Z"),
            "1969-12-31T23:59:59.999999999+00:00"
        );
        // Offset conversion crossing into BCE: chrono prints sign + four
        // digits; naive {:04} counts the sign in the width (review finding).
        assert_eq!(
            fmt("0000-01-01T00:00:00+01:00"),
            "-0001-12-31T23:00:00+00:00"
        );
    }

    #[test]
    fn formatter_round_trips_through_jiff_parser() {
        // Round-trip holds for every form both parsers accept (all tachi
        // writers emit these). The BCE case is deliberately excluded:
        // chrono's own to_rfc3339 prints `-0001-…`, which chrono's own
        // parse_from_rfc3339 also rejects — the formatter matches those
        // bytes, and the asymmetry is inherited, not introduced.
        for s in [
            "2026-12-01T00:00:00Z",
            "2026-12-01T00:00:00.100Z",
            "2026-12-01T00:00:00.123456Z",
            "1969-12-31T23:59:59.999999999Z",
        ] {
            let out = fmt(s);
            let back = jiff::Timestamp::from_str(&out).expect("round trip parses");
            assert_eq!(
                back,
                jiff::Timestamp::from_str(s).expect("fixture parses"),
                "round trip changed the instant for {s}"
            );
        }
    }

    /// Declared parser-parity drift vs the retired
    /// `chrono::DateTime::parse_from_rfc3339` validation gates (gpt-6.2-astra
    /// review, 2026-11-18). These assertions pin the NEW behavior so any
    /// future parser change is a visible test flip, not silent drift. No
    /// tachi writer can produce the divergent forms: all writers emit the
    /// chrono normal form above, which both parsers accept identically.
    #[test]
    fn parser_parity_drift_is_pinned() {
        use jiff::Timestamp;
        // Loosened: Temporal extensions the old gate rejected.
        assert!(Timestamp::from_str("-000001-12-31T23:00:00Z").is_ok());
        assert!(Timestamp::from_str("2026-12-01T00:00Z").is_ok());
        assert!(Timestamp::from_str("2026-12-01T00:00:00Z[UTC]").is_ok());
        // Tightened: instant leaves jiff's year range after UTC conversion.
        assert!(Timestamp::from_str("9999-12-31T23:30:00-01:00").is_err());
        // Leap seconds collapse onto :59 (chrono kept a distinct instant).
        let leap = Timestamp::from_str("2016-12-31T23:59:60Z").expect("leap parses");
        let flat = Timestamp::from_str("2016-12-31T23:59:59Z").expect("flat parses");
        assert_eq!(leap, flat);
        // Offset-less civil time is still rejected by both.
        assert!(Timestamp::from_str("2026-12-01T00:00:00").is_err());
    }
}
