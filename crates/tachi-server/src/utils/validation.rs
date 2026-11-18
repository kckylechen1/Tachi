pub(crate) struct Rfc3339Stamp<'a> {
    pub(crate) key: (i64, u32),
    local: &'a str,
    offset_minutes: i32,
}

impl<'a> Rfc3339Stamp<'a> {
    // Stored health evidence uses the historical RFC3339 grammar, not Temporal or
    // SQLite: space/lowercase, ±23:59, U+2212, leap seconds and long fractions.
    pub(crate) fn parse(raw: &'a str) -> Option<Self> {
        static SHAPE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(
            r"\A([0-9]{4})-([0-9]{2})-([0-9]{2})[Tt ]([0-9]{2}):([0-9]{2}):([0-5][0-9]|60)(?:\.([0-9]+))?(?:[Zz]|([+−-])([01][0-9]|2[0-3]):([0-5][0-9]))\z",
        ).expect("valid health timestamp grammar")
        });
        let fields = SHAPE.captures(raw)?;
        let (key, offset_minutes) = stamp_fields(&fields)?;
        Some(Self {
            key,
            local: &raw[..19],
            offset_minutes,
        })
    }

    pub(crate) fn after(&self, now: jiff::Timestamp) -> bool {
        self.key > timestamp_key(now)
    }

    pub(crate) fn order_key(raw: &str) -> Option<(i64, u32)> {
        Rfc3339Stamp::parse(raw).map(|stamp| stamp.key)
    }

    pub(crate) fn whole_seconds_until(&self, now: jiff::Timestamp) -> i64 {
        if let Ok(then) = stamp_instant(self.key) {
            return memcore::time_compat::whole_seconds_between(now, then);
        }
        let (seconds, nanos) = self.key;
        let elapsed =
            now.as_nanosecond() - (i128::from(seconds) * 1_000_000_000 + i128::from(nanos));
        // Legacy civil subtraction counted the leap if the UTC time of day passed it.
        let passed_leap = nanos >= 1_000_000_000
            && timestamp_key(now).0.rem_euclid(86_400) > seconds.rem_euclid(86_400);
        ((elapsed + i128::from(passed_leap) * 1_000_000_000) / 1_000_000_000) as i64
    }

    pub(crate) fn canonical(&self) -> String {
        let nanos = self.key.1 % 1_000_000_000;
        let fraction = match nanos {
            0 => String::new(),
            _ if nanos.is_multiple_of(1_000_000) => format!(".{:03}", nanos / 1_000_000),
            _ if nanos.is_multiple_of(1_000) => format!(".{:06}", nanos / 1_000),
            _ => format!(".{nanos:09}"),
        };
        let minutes = self.offset_minutes.unsigned_abs();
        format!(
            "{}T{}{fraction}{}{:02}:{:02}",
            &self.local[..10],
            &self.local[11..],
            if self.offset_minutes < 0 { '-' } else { '+' },
            minutes / 60,
            minutes % 60
        )
    }
}

pub(crate) fn timestamp_key(now: jiff::Timestamp) -> (i64, u32) {
    let nanos = now.as_nanosecond();
    (
        nanos.div_euclid(1_000_000_000) as i64,
        nanos.rem_euclid(1_000_000_000) as u32,
    )
}

fn stamp_fields(fields: &regex::Captures<'_>) -> Option<((i64, u32), i32)> {
    let year: i32 = fields[1].parse().ok()?;
    if !(-262_143..=262_142).contains(&year) {
        return None;
    }
    // Gregorian dates repeat every 400 years; validate historical signed years
    // even when a review receipt is outside jiff's civil or instant range.
    let date = jiff::civil::Date::new(
        year.rem_euclid(400) as i16,
        fields[2].parse().ok()?,
        fields[3].parse().ok()?,
    )
    .ok()?;
    let second: i8 = fields[6].parse().ok()?;
    if second > 60 {
        return None;
    }
    let time = jiff::civil::Time::new(
        fields[4].parse().ok()?,
        fields[5].parse().ok()?,
        second.min(59),
        0,
    )
    .ok()?;
    let offset_minutes = match fields.get(8) {
        None => 0,
        Some(sign) => {
            let minutes = fields[9].parse::<i32>().ok()? * 60 + fields[10].parse::<i32>().ok()?;
            if sign.as_str() == "+" {
                minutes
            } else {
                -minutes
            }
        }
    };
    let fraction = fields.get(7).map_or("", |digits| digits.as_str());
    let nanos = fraction
        .bytes()
        .take(9)
        .chain(std::iter::repeat(b'0'))
        .take(9)
        .fold(0_u32, |value, digit| value * 10 + u32::from(digit - b'0'));
    // A civil key includes year 9999 outside jiff's Timestamp ceiling. Its
    // second component keeps :60 ordered strictly between :59 and :00.
    let epoch = jiff::civil::date(1970, 1, 1).to_datetime(jiff::civil::Time::MIN);
    let seconds = date.to_datetime(time).duration_since(epoch).as_secs()
        + i64::from(year.div_euclid(400)) * 146_097 * 86_400
        - i64::from(offset_minutes) * 60;
    let local_day = (seconds + i64::from(offset_minutes) * 60).div_euclid(86_400);
    let utc_day = seconds.div_euclid(86_400);
    if matches!((year, date.month(), date.day()), (-262_143, 1, 1)) && utc_day < local_day
        || matches!((year, date.month(), date.day()), (262_142, 12, 31)) && utc_day > local_day
    {
        return None;
    }
    Some((
        (seconds, nanos + u32::from(second == 60) * 1_000_000_000),
        offset_minutes,
    ))
}

pub(crate) fn parse_rfc3339(raw: &str) -> Result<jiff::Timestamp, &'static str> {
    let stamp = Rfc3339Stamp::parse(raw).ok_or("invalid RFC3339 syntax")?;
    stamp_instant(stamp.key)
}

pub(crate) fn relaxed_datetime_key(raw: &str) -> Option<(i64, u32)> {
    // DateTime<Utc>::from_str also accepted unpadded fields, UTC, optional
    // offset colons and whitespace. Temporal annotations were never valid.
    static SHAPE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(concat!(
            r"\A\s*([+-][0-9]+|[0-9]{1,4})\s*-\s*([0-9]{1,2})\s*-\s*([0-9]{1,2})",
            r"[Tt ]\s*([0-9]{1,2})\s*:\s*([0-9]{1,2})\s*:\s*([0-9]{1,2})(?:\.([0-9]+))?\s*",
            r"(?:[Zz]|[Uu][Tt][Cc]|([+−-])([01][0-9]|2[0-3])[:\s]*([0-5][0-9]))\s*\z"
        ))
        .expect("legacy datetime grammar")
    });
    stamp_fields(&SHAPE.captures(raw)?).map(|(key, _)| key)
}

pub(crate) fn parse_relaxed_datetime(raw: &str) -> Result<jiff::Timestamp, &'static str> {
    stamp_instant(relaxed_datetime_key(raw).ok_or("invalid datetime syntax")?)
}

fn stamp_instant((seconds, nanos): (i64, u32)) -> Result<jiff::Timestamp, &'static str> {
    if nanos >= 1_000_000_000 {
        return Err("RFC3339 leap second cannot be represented as an instant");
    }
    jiff::Timestamp::new(seconds, nanos as i32).map_err(|_| "RFC3339 instant out of range")
}

pub(crate) fn normalize_supported_values<'a>(
    raw_values: &[String],
    supported: &'a [&'a str],
    value_label: &str,
) -> Result<Vec<&'a str>, String> {
    if raw_values.is_empty() {
        return Ok(supported.to_vec());
    }

    let mut out = Vec::new();
    for raw in raw_values {
        let normalized = raw.trim().to_ascii_lowercase();
        let Some(supported_value) = supported
            .iter()
            .copied()
            .find(|candidate| *candidate == normalized)
        else {
            return Err(format!(
                "unsupported {value_label} '{raw}'. Supported: {}",
                supported.join(", ")
            ));
        };
        if !out.contains(&supported_value) {
            out.push(supported_value);
        }
    }
    Ok(out)
}

/// Trusted command list for caller-supplied `agent=custom` dispatch.
pub(crate) fn is_trusted_command(cmd: &str) -> bool {
    tachi_dispatch::is_trusted_dispatch_command(cmd)
}
