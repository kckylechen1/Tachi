use jiff::civil::{time, Time};
use jiff::tz::{Offset, TimeZone};
use jiff::{SignedDuration, Timestamp};
use std::time::Duration;

pub(crate) fn next_daily_run_time() -> tokio::time::Instant {
    // Wall clock FIRST, then the monotonic sample: sampling the monotonic
    // clock before the wall clock can, under descheduling between the two
    // samples, produce a deadline computed from a stale monotonic base —
    // firing the job up to that gap EARLY (astra round 3, finding 3). The
    // pre-migration code sampled in this same order.
    let wait = wait_for_shanghai_run(Timestamp::now(), time(4, 0, 0, 0), false);
    tokio::time::Instant::now() + wait
}

fn wait_for_shanghai_run(now: Timestamp, scheduled: Time, sunday_only: bool) -> Duration {
    let zone = shanghai_offset();
    let local = zone.to_datetime(now);
    let days = if sunday_only {
        (7 - i64::from(local.date().weekday().to_sunday_zero_offset())) % 7
    } else {
        0
    };
    let days = if days == 0 && local.time() >= scheduled {
        if sunday_only {
            7
        } else {
            1
        }
    } else {
        days
    };
    local
        .date()
        .checked_add(SignedDuration::from_secs(days * 86_400))
        .and_then(|date| zone.to_zoned_datetime(date.to_datetime(scheduled)))
        .ok()
        .and_then(|run| Duration::try_from(run.timestamp().duration_since(now)).ok())
        .unwrap_or_default()
}

fn shanghai_offset() -> TimeZone {
    TimeZone::fixed(Offset::from_seconds(8 * 3600).expect("valid Asia/Shanghai fixed offset"))
}

pub(crate) fn shanghai_today() -> String {
    shanghai_offset()
        .to_datetime(Timestamp::now())
        .date()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shanghai_runs_advance_at_the_local_cutoff() {
        let utc_saturday = "2026-11-14T20:59:59.750Z".parse().unwrap();
        assert_eq!(
            shanghai_offset()
                .to_datetime(utc_saturday)
                .date()
                .to_string(),
            "2026-11-15"
        );
        assert_eq!(
            wait_for_shanghai_run(utc_saturday, time(5, 0, 0, 0), true),
            Duration::from_millis(250)
        );
        let cutoff = "2026-11-14T21:00:00Z".parse().unwrap();
        assert_eq!(
            wait_for_shanghai_run(cutoff, time(5, 0, 0, 0), true),
            Duration::from_secs(7 * 86_400)
        );
        assert_eq!(
            wait_for_shanghai_run(cutoff, time(4, 0, 0, 0), false),
            Duration::from_secs(23 * 3600)
        );
    }
}
