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

    #[cfg(test)]
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
}
