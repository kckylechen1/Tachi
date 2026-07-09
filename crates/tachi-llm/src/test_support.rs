//! Process-wide test lock for env-var isolation.
//!
//! Reentrant on the same thread so `LlmClient::new` (which reads
//! `TACHI_RERANK_*` under the lock) can be called from tests that already
//! hold the lock while mutating those vars.

use std::cell::Cell;
use std::sync::{Mutex, MutexGuard, OnceLock};

thread_local! {
    static LOCK_DEPTH: Cell<usize> = const { Cell::new(0) };
}

fn mutex() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// RAII guard for the process-wide test lock. Nested acquires on the same
/// thread increment a depth counter and do not re-lock the mutex.
pub(crate) struct TestLockGuard {
    /// `Some` only for the outermost acquire on this thread.
    /// Held solely for RAII unlock on drop (never read).
    #[allow(dead_code)]
    owned: Option<MutexGuard<'static, ()>>,
}

impl Drop for TestLockGuard {
    fn drop(&mut self) {
        LOCK_DEPTH.with(|depth| {
            let d = depth.get();
            debug_assert!(d > 0, "TestLockGuard drop with depth 0");
            depth.set(d.saturating_sub(1));
        });
        // `owned` drops here when this is the outermost guard.
    }
}

/// Handle returned by [`global_test_lock`]; call `.lock()` to acquire.
pub(crate) struct GlobalTestLock;

impl GlobalTestLock {
    /// Acquire the process-wide test lock (reentrant on the same thread).
    pub(crate) fn lock(&self) -> TestLockGuard {
        LOCK_DEPTH.with(|depth| {
            if depth.get() == 0 {
                let guard = mutex().lock().unwrap_or_else(|e| e.into_inner());
                depth.set(1);
                TestLockGuard {
                    owned: Some(guard),
                }
            } else {
                depth.set(depth.get() + 1);
                TestLockGuard { owned: None }
            }
        })
    }
}

/// Process-wide lock for tests that mutate shared env vars (`TACHI_RERANK_*`,
/// `HOME`, provider keys, etc.).
///
/// Usage:
/// ```ignore
/// let _guard = crate::test_support::global_test_lock().lock();
/// ```
pub(crate) fn global_test_lock() -> GlobalTestLock {
    GlobalTestLock
}
