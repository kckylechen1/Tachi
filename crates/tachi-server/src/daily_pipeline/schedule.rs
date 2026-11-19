use chrono::{Datelike, Duration as ChronoDuration, FixedOffset, TimeZone, Utc};
use std::time::Duration;

pub(crate) fn next_daily_run_time() -> tokio::time::Instant {
    let tz = shanghai_offset();
    let now_utc = Utc::now();
    let now_local = now_utc.with_timezone(&tz);
    let today_0400 = tz
        .with_ymd_and_hms(
            now_local.year(),
            now_local.month(),
            now_local.day(),
            4,
            0,
            0,
        )
        .single()
        .unwrap_or(now_local);
    let next_local = if now_local < today_0400 {
        today_0400
    } else {
        today_0400 + ChronoDuration::days(1)
    };
    let wait = (next_local.with_timezone(&Utc) - now_utc)
        .to_std()
        .unwrap_or_else(|_| Duration::from_secs(0));
    tokio::time::Instant::now() + wait
}

pub(crate) fn shanghai_offset() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("valid Asia/Shanghai fixed offset")
}

pub(crate) fn shanghai_today() -> String {
    Utc::now()
        .with_timezone(&shanghai_offset())
        .format("%Y-%m-%d")
        .to_string()
}
