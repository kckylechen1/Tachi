//! Keep long synchronous filesystem / subprocess work from pinning an async
//! executor worker (audit E4).
//!
//! Two shapes exist in the crate:
//!
//! * `tokio::task::spawn_blocking(..).await` — the default when the caller can
//!   hand owned data to the blocking pool and gaining an `.await` point is
//!   harmless.
//! * [`block_off_core`] — for call sites that must NOT gain a new cancellation
//!   point (non-RAII resources are held across the call, or a `Drop` guard
//!   must stay the only cleanup path) or that run from `Drop`, where nothing
//!   can be awaited. The work still runs synchronously in the calling task and
//!   in the same order; only the executor core is handed to another thread
//!   while it blocks.

use tokio::runtime::{Handle, RuntimeFlavor};

/// Run `f` synchronously in the current task. On a multi-thread tokio runtime
/// the current worker first hands its core (and its queued tasks) to another
/// thread via [`tokio::task::block_in_place`], so the executor keeps polling
/// other tasks while `f` blocks. On a current-thread runtime, or outside any
/// runtime, `f` runs inline exactly as a direct call would
/// (`block_in_place` panics on a current-thread runtime).
///
/// Unlike `spawn_blocking(..).await` this adds no `.await` point, so the
/// caller's cancellation behaviour and ordering are unchanged. Not for use
/// inside a `tokio::task::LocalSet` (the crate has none).
pub(crate) fn block_off_core<T>(f: impl FnOnce() -> T) -> T {
    match Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

/// Admit before spawning, and keep the seat until the native work terminates.
/// Cancelling queued work spawns nothing; cancelling admitted work must not
/// refund capacity while it still holds a thread, file or store lock.
pub(crate) async fn run_bounded_blocking<T: Send + 'static>(
    seats: std::sync::Arc<tokio::sync::Semaphore>,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let permit = seats
        .acquire_owned()
        .await
        .map_err(|_| "blocking admission closed".to_string())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|error| format!("blocking worker failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{block_off_core, run_bounded_blocking};
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn bounded_work_retains_its_seat_after_cancellation_and_recovers() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let seats = Arc::new(tokio::sync::Semaphore::new(1));
        let starts = Arc::new(AtomicUsize::new(0));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first_starts = starts.clone();
        let first = tokio::spawn(run_bounded_blocking(seats.clone(), move || {
            first_starts.fetch_add(1, Ordering::SeqCst);
            entered_tx.send(()).expect("entered work");
            release_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("bounded release");
        }));
        tokio::time::timeout(Duration::from_secs(2), entered_rx)
            .await
            .expect("admitted work")
            .expect("entry signal");
        let queued_starts = starts.clone();
        let mut queued = Box::pin(run_bounded_blocking(seats.clone(), move || {
            queued_starts.fetch_add(1, Ordering::SeqCst);
        }));
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(queued.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(queued);
        first.abort();
        assert_eq!(seats.available_permits(), 0);
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        release_tx.send(()).expect("release admitted worker");
        tokio::time::timeout(Duration::from_secs(2), async {
            while seats.available_permits() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("finished worker refunds seat");
        let _ = first.await;
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "cancelled queued work never starts"
        );
        assert_eq!(
            run_bounded_blocking(seats.clone(), || 7)
                .await
                .expect("recovery"),
            7
        );
        assert!(
            run_bounded_blocking::<()>(seats.clone(), || panic!("fixture panic"))
                .await
                .is_err()
        );
        assert_eq!(seats.available_permits(), 1, "unwind refunds seat");
    }

    /// Largest wall gap seen by a 1 ms ticker while `work` runs on the same
    /// runtime. Inline blocking on the runtime's only worker shows up as one
    /// gap about as long as the blocking work.
    async fn max_probe_gap_while<F>(work: F) -> Duration
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe_done = done.clone();
        let probe = tokio::spawn(async move {
            let mut max_gap = Duration::ZERO;
            let mut last = Instant::now();
            while !probe_done.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(1)).await;
                let now = Instant::now();
                max_gap = max_gap.max(now - last);
                last = now;
            }
            max_gap
        });
        // Let the probe start ticking before the blocking work begins.
        tokio::time::sleep(Duration::from_millis(20)).await;
        tokio::spawn(work).await.expect("work task");
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        probe.await.expect("probe task")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn block_off_core_keeps_single_worker_runtime_responsive() {
        let gap = max_probe_gap_while(async {
            block_off_core(|| std::thread::sleep(Duration::from_millis(400)));
        })
        .await;
        assert!(
            gap < Duration::from_millis(200),
            "a 400 ms block_off_core section must not stall the only worker; max gap {gap:?}"
        );
    }

    /// Discriminator for the test above: the same sleep called directly on
    /// the only worker does stall the probe.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn inline_blocking_stalls_single_worker_runtime() {
        let gap = max_probe_gap_while(async {
            std::thread::sleep(Duration::from_millis(400));
        })
        .await;
        assert!(
            gap >= Duration::from_millis(300),
            "inline blocking should stall the only worker; max gap {gap:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn block_off_core_runs_inline_on_current_thread_runtime() {
        let caller = std::thread::current().id();
        let ran_on = block_off_core(|| std::thread::current().id());
        assert_eq!(ran_on, caller);
    }

    #[test]
    fn block_off_core_runs_outside_a_runtime() {
        assert_eq!(block_off_core(|| 7), 7);
    }
}
