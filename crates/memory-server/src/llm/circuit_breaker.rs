use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
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
    opened_at: RwLock<Option<Instant>>,
}

impl Inner {
    fn new() -> Self {
        Self {
            state: RwLock::new(BreakerState::Closed),
            failure_count: AtomicU32::new(0),
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
            BreakerState::Closed | BreakerState::HalfOpen => true,
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
                        }
                        return true;
                    }
                }
                false
            }
        }
    }

    fn record_success(&self) {
        self.inner.failure_count.store(0, Ordering::Relaxed);
        let mut state = self.inner.state.write().unwrap_or_else(|e| e.into_inner());
        *state = BreakerState::Closed;
    }

    fn record_failure(&self) {
        let count = self.inner.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
        if count >= FAILURE_THRESHOLD {
            let mut state = self.inner.state.write().unwrap_or_else(|e| e.into_inner());
            if *state != BreakerState::Open {
                *state = BreakerState::Open;
                let mut opened_at = self
                    .inner
                    .opened_at
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                *opened_at = Some(Instant::now());
            }
        }
    }

    #[allow(dead_code)]
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

    #[allow(dead_code)]
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
    }
}
