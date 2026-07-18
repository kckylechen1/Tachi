use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const FAILURE_THRESHOLD: u32 = 5;
const OPEN_DURATION: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

struct Inner {
    state: RwLock<BreakerState>,
    failure_count: AtomicU32,
    half_open_probe_claimed: AtomicBool,
    opened_at: RwLock<Option<Instant>>,
}

impl Inner {
    fn new() -> Self {
        Self {
            state: RwLock::new(BreakerState::Closed),
            failure_count: AtomicU32::new(0),
            half_open_probe_claimed: AtomicBool::new(false),
            opened_at: RwLock::new(None),
        }
    }
}

#[derive(Clone)]
pub(crate) struct CircuitBreaker {
    inner: Arc<Inner>,
}

impl CircuitBreaker {
    fn new() -> Self {
        Self {
            inner: Arc::new(Inner::new()),
        }
    }

    fn allow_request(&self) -> bool {
        let state = *self.inner.state.read().unwrap_or_else(|e| e.into_inner());
        match state {
            BreakerState::Closed => true,
            BreakerState::HalfOpen => self.claim_half_open_probe(),
            BreakerState::Open => {
                let opened_at = self
                    .inner
                    .opened_at
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(at) = *opened_at {
                    if at.elapsed() >= OPEN_DURATION {
                        drop(opened_at);
                        let mut state = self.inner.state.write().unwrap_or_else(|e| e.into_inner());
                        if *state == BreakerState::Open {
                            *state = BreakerState::HalfOpen;
                            self.inner
                                .half_open_probe_claimed
                                .store(false, Ordering::Release);
                        }
                        return self.claim_half_open_probe();
                    }
                }
                false
            }
        }
    }

    fn claim_half_open_probe(&self) -> bool {
        self.inner
            .half_open_probe_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn record_success(&self) {
        self.inner.failure_count.store(0, Ordering::Relaxed);
        self.inner
            .half_open_probe_claimed
            .store(false, Ordering::Release);
        let mut opened_at = self
            .inner
            .opened_at
            .write()
            .unwrap_or_else(|e| e.into_inner());
        *opened_at = None;
        let mut state = self.inner.state.write().unwrap_or_else(|e| e.into_inner());
        *state = BreakerState::Closed;
    }

    fn record_failure(&self) {
        let count = self.inner.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
        if count >= FAILURE_THRESHOLD {
            let mut state = self.inner.state.write().unwrap_or_else(|e| e.into_inner());
            if *state != BreakerState::Open {
                *state = BreakerState::Open;
                self.inner
                    .half_open_probe_claimed
                    .store(false, Ordering::Release);
                let mut opened_at = self
                    .inner
                    .opened_at
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                *opened_at = Some(Instant::now());
            }
        }
    }

    /// Human-readable breaker state. Used both by tests and by the
    /// production lane-outage status surface (#1197) — `tachi_status`
    /// reports this per lane so a degraded provider shows up as `"open"`
    /// instead of a silent stall.
    pub(crate) fn state_name(&self) -> &'static str {
        match *self.inner.state.read().unwrap_or_else(|e| e.into_inner()) {
            BreakerState::Closed => "closed",
            BreakerState::Open => "open",
            BreakerState::HalfOpen => "half_open",
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct CircuitBreakerRegistry {
    breakers: Arc<RwLock<HashMap<String, CircuitBreaker>>>,
}

impl CircuitBreakerRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn get_or_create(&self, key: &str) -> CircuitBreaker {
        let read = self.breakers.read().unwrap_or_else(|e| e.into_inner());
        if let Some(b) = read.get(key) {
            return b.clone();
        }
        drop(read);
        let mut write = self.breakers.write().unwrap_or_else(|e| e.into_inner());
        write
            .entry(key.to_string())
            .or_insert_with(CircuitBreaker::new)
            .clone()
    }

    pub(crate) fn allow(&self, key: &str) -> bool {
        self.get_or_create(key).allow_request()
    }

    pub(crate) fn record_success(&self, key: &str) {
        self.get_or_create(key).record_success();
    }

    pub(crate) fn record_failure(&self, key: &str) {
        self.get_or_create(key).record_failure();
    }

    /// Read-only breaker state for a key, without creating an entry when one
    /// doesn't exist yet (an absent breaker behaves as `"closed"` — never
    /// having failed is not the same as being degraded). Used by the
    /// lane-outage status surface (#1197) so polling status never mutates
    /// breaker state as a side effect.
    pub(crate) fn state_name(&self, key: &str) -> &'static str {
        self.breakers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .map(CircuitBreaker::state_name)
            .unwrap_or("closed")
    }

    /// Introspection helper for unit tests / future status surfaces.
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> Vec<(String, &'static str)> {
        self.breakers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(k, v)| (k.clone(), v.state_name()))
            .collect()
    }
}

/// Tracks whether a lane's **entire configured provider chain** (primary +
/// fallback tiers) has been exhausted on the most recent call — i.e. a full
/// lane outage, distinct from an ordinary within-tier retry/failure (#1197).
/// A lane recovering via its fallback provider (any tier succeeding) clears
/// the streak: the lane is healthy from the caller's point of view even if
/// degraded to a secondary provider.
#[derive(Clone, Default)]
pub(crate) struct LaneOutageTracker {
    entries: Arc<RwLock<HashMap<&'static str, LaneOutageEntry>>>,
}

#[derive(Clone, Default)]
struct LaneOutageEntry {
    consecutive_chain_failures: u32,
    last_outage_at: Option<String>,
    last_error: Option<String>,
}

impl LaneOutageTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record that every configured tier failed this call. `now_utc` is an
    /// RFC3339 timestamp string (caller-supplied so this module doesn't need
    /// its own clock dependency).
    pub(crate) fn record_chain_exhausted(&self, lane: &'static str, now_utc: String, error: String) {
        let mut entries = self.entries.write().unwrap_or_else(|e| e.into_inner());
        let entry = entries.entry(lane).or_default();
        entry.consecutive_chain_failures += 1;
        entry.last_outage_at = Some(now_utc);
        entry.last_error = Some(error);
    }

    /// Any tier succeeding resets the outage streak for this lane.
    pub(crate) fn record_chain_success(&self, lane: &'static str) {
        let mut entries = self.entries.write().unwrap_or_else(|e| e.into_inner());
        entries.remove(lane);
    }

    /// `(consecutive_chain_failures, last_outage_at, last_error)` for `lane`;
    /// all-zero/`None` when the lane has never had a full-chain outage (or
    /// has recovered since).
    pub(crate) fn snapshot_for(&self, lane: &'static str) -> (u32, Option<String>, Option<String>) {
        let entries = self.entries.read().unwrap_or_else(|e| e.into_inner());
        match entries.get(lane) {
            Some(entry) => (
                entry.consecutive_chain_failures,
                entry.last_outage_at.clone(),
                entry.last_error.clone(),
            ),
            None => (0, None, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breaker_opens_after_threshold_failures() {
        let registry = CircuitBreakerRegistry::new();
        for _ in 0..FAILURE_THRESHOLD {
            assert!(registry.allow("test"));
            registry.record_failure("test");
        }
        assert!(!registry.allow("test"), "breaker should be open");
    }

    #[test]
    fn breaker_resets_on_success() {
        let registry = CircuitBreakerRegistry::new();
        for _ in 0..FAILURE_THRESHOLD {
            registry.record_failure("test");
        }
        assert!(!registry.allow("test"));
        registry.record_success("test");
        assert!(
            registry.allow("test"),
            "breaker should be closed after success"
        );
    }

    #[test]
    fn breaker_independent_per_key() {
        let registry = CircuitBreakerRegistry::new();
        for _ in 0..FAILURE_THRESHOLD {
            registry.record_failure("lane_a");
        }
        assert!(!registry.allow("lane_a"));
        assert!(registry.allow("lane_b"), "lane_b should be unaffected");

        let snap = registry.snapshot();
        let by_key: std::collections::HashMap<_, _> = snap.into_iter().collect();
        assert_eq!(by_key.get("lane_a").copied(), Some("open"));
        // lane_b may be absent until first allow; force create
        assert!(registry.allow("lane_b"));
        let snap = registry.snapshot();
        let by_key: std::collections::HashMap<_, _> = snap.into_iter().collect();
        assert_eq!(by_key.get("lane_b").copied(), Some("closed"));
    }

    #[test]
    fn half_open_allows_only_one_probe() {
        let registry = CircuitBreakerRegistry::new();
        for _ in 0..FAILURE_THRESHOLD {
            registry.record_failure("test");
        }

        let breaker = registry.get_or_create("test");
        {
            let mut opened_at = breaker
                .inner
                .opened_at
                .write()
                .unwrap_or_else(|e| e.into_inner());
            *opened_at = Some(Instant::now() - OPEN_DURATION);
        }

        assert!(registry.allow("test"), "first half-open probe is allowed");
        assert!(
            !registry.allow("test"),
            "second half-open request is fast-rejected until probe resolves"
        );
    }

    #[test]
    fn state_name_of_missing_key_reports_closed_without_creating_entry() {
        let registry = CircuitBreakerRegistry::new();
        assert_eq!(registry.state_name("never-touched"), "closed");
        // Polling status must not be observable as a side effect: no entry
        // should have been created just by asking.
        assert!(registry.snapshot().is_empty());
    }

    #[test]
    fn state_name_of_reflects_open_breaker() {
        let registry = CircuitBreakerRegistry::new();
        for _ in 0..FAILURE_THRESHOLD {
            registry.record_failure("chat:extract");
        }
        assert_eq!(registry.state_name("chat:extract"), "open");
    }

    #[test]
    fn lane_outage_tracker_records_and_clears_on_success() {
        let tracker = LaneOutageTracker::new();
        assert_eq!(
            tracker.snapshot_for("extract"),
            (0, None, None),
            "never-outaged lane reports zero streak"
        );

        tracker.record_chain_exhausted(
            "extract",
            "2026-07-18T00:00:00.000Z".to_string(),
            "all providers exhausted".to_string(),
        );
        let (count, at, err) = tracker.snapshot_for("extract");
        assert_eq!(count, 1);
        assert_eq!(at.as_deref(), Some("2026-07-18T00:00:00.000Z"));
        assert_eq!(err.as_deref(), Some("all providers exhausted"));

        tracker.record_chain_exhausted(
            "extract",
            "2026-07-18T00:01:00.000Z".to_string(),
            "still exhausted".to_string(),
        );
        assert_eq!(tracker.snapshot_for("extract").0, 2, "streak accumulates");

        tracker.record_chain_success("extract");
        assert_eq!(
            tracker.snapshot_for("extract"),
            (0, None, None),
            "any tier succeeding clears the outage streak"
        );
    }

    #[test]
    fn lane_outage_tracker_is_independent_per_lane() {
        let tracker = LaneOutageTracker::new();
        tracker.record_chain_exhausted("extract", "t".to_string(), "e".to_string());
        assert_eq!(tracker.snapshot_for("extract").0, 1);
        assert_eq!(
            tracker.snapshot_for("distill"),
            (0, None, None),
            "distill must be unaffected by extract's outage"
        );
    }
}
