use std::future::Future;
use std::sync::{Arc, OnceLock};

use tokio::sync::Semaphore;

static RECALL_BLOCKING_SEATS: OnceLock<Arc<Semaphore>> = OnceLock::new();
static CACHE_HIT_TELEMETRY_SEAT: OnceLock<Arc<Semaphore>> = OnceLock::new();

fn recall_blocking_seats() -> &'static Arc<Semaphore> {
    RECALL_BLOCKING_SEATS.get_or_init(|| {
        // Provisional process-local capacity: one recall may hold a blocking
        // thread while awaiting its provider, while its SQLite and Condvar
        // sections must not occupy Tokio workers. CPU parallelism is a useful
        // conservative bound until production queue/latency measurements
        // justify a separately configurable value.
        let capacity = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        Arc::new(Semaphore::new(capacity))
    })
}

pub(super) async fn run_bounded_recall<T, F, Fut>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = T> + 'static,
{
    run_bounded_recall_with(recall_blocking_seats().clone(), work).await
}

async fn run_bounded_recall_with<T, F, Fut>(
    semaphore: Arc<Semaphore>,
    work: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = T> + 'static,
{
    // Admission happens before spawning. Cancelling while queued therefore
    // creates no blocking task. Once admitted, the owned permit moves into
    // the closure, so cancelling the caller cannot release capacity while the
    // underlying request (including provider awaits) is still running.
    let permit = semaphore
        .acquire_owned()
        .await
        .map_err(|_| "recall blocking admission closed".to_string())?;
    let runtime = tokio::runtime::Handle::current();
    #[cfg(test)]
    let test_context = super::cache::take_recall_cache_test_context();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        #[cfg(test)]
        let _test_context = super::cache::install_recall_cache_test_context(test_context);
        runtime.block_on(work())
    })
    .await
    .map_err(|error| format!("recall blocking worker failed: {error}"))
}

pub(super) fn try_spawn_cache_hit_telemetry(work: impl FnOnce() + Send + 'static) {
    let semaphore = CACHE_HIT_TELEMETRY_SEAT
        .get_or_init(|| Arc::new(Semaphore::new(1)))
        .clone();
    try_spawn_cache_hit_telemetry_with(semaphore, work);
}

fn try_spawn_cache_hit_telemetry_with(
    semaphore: Arc<Semaphore>,
    work: impl FnOnce() + Send + 'static,
) {
    let Ok(permit) = semaphore.try_acquire_owned() else {
        return;
    };
    std::mem::drop(tokio::task::spawn_blocking(move || {
        // Keep the only telemetry seat through unwind as well as normal exit.
        let _permit = permit;
        work();
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_search_ops::search_memory::cache::{
        recall_cache_read_enabled, run_recall_cache_race_hook, RecallCacheRaceHook,
        RecallCacheRacePoint, RecallCacheTestOverride,
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn current_thread_timer_runs_while_recall_holds_blocking_worker() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime")
            .block_on(async {
                let (release_tx, release_rx) = std::sync::mpsc::channel();
                let recall = tokio::spawn(run_bounded_recall_with(
                    Arc::new(Semaphore::new(1)),
                    move || async move {
                        release_rx.recv().expect("release");
                    },
                ));
                tokio::time::timeout(Duration::from_millis(100), async {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    release_tx.send(()).expect("release blocking worker");
                })
                .await
                .expect("Tokio timer must remain responsive");
                recall.await.expect("recall task").expect("recall worker");
            });
    }

    #[test]
    fn cancellation_before_and_after_admission_preserves_capacity_ownership() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime")
            .block_on(async {
                let semaphore = Arc::new(Semaphore::new(1));
                let admitted = Arc::new(AtomicUsize::new(0));
                let (release_tx, release_rx) = std::sync::mpsc::channel();
                let first_admitted = admitted.clone();
                let first = tokio::spawn(run_bounded_recall_with(semaphore.clone(), move || {
                    first_admitted.fetch_add(1, Ordering::SeqCst);
                    async move { release_rx.recv().expect("release admitted work") }
                }));
                while admitted.load(Ordering::SeqCst) != 1 {
                    tokio::task::yield_now().await;
                }

                let waiting_admitted = admitted.clone();
                let waiting = tokio::spawn(run_bounded_recall_with(semaphore.clone(), move || {
                    waiting_admitted.fetch_add(1, Ordering::SeqCst);
                    async {}
                }));
                tokio::task::yield_now().await;
                waiting.abort();
                first.abort();
                assert_eq!(semaphore.available_permits(), 0);
                assert_eq!(admitted.load(Ordering::SeqCst), 1);
                release_tx.send(()).expect("finish admitted work");
                tokio::time::timeout(Duration::from_secs(1), async {
                    while semaphore.available_permits() != 1 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("admitted worker retains then releases permit");
                assert_eq!(admitted.load(Ordering::SeqCst), 1);
            });
    }

    #[tokio::test]
    async fn saturated_cache_hit_telemetry_drops_update() {
        let semaphore = Arc::new(Semaphore::new(1));
        let held = semaphore.clone().acquire_owned().await.expect("hold seat");
        let ran = Arc::new(AtomicBool::new(false));
        let observed = ran.clone();
        try_spawn_cache_hit_telemetry_with(semaphore, move || {
            observed.store(true, Ordering::SeqCst);
        });
        tokio::task::yield_now().await;
        assert!(!ran.load(Ordering::SeqCst));
        drop(held);
    }

    #[tokio::test]
    async fn cache_override_and_race_hook_transfer_to_worker() {
        let _override = RecallCacheTestOverride::enabled();
        let fired = Arc::new(AtomicBool::new(false));
        let observed = fired.clone();
        let _hook = RecallCacheRaceHook::install(
            RecallCacheRacePoint::AfterQueryBeforeValidation,
            move || observed.store(true, Ordering::SeqCst),
        );
        run_bounded_recall_with(Arc::new(Semaphore::new(1)), move || async move {
            assert!(recall_cache_read_enabled());
            run_recall_cache_race_hook(RecallCacheRacePoint::AfterQueryBeforeValidation);
        })
        .await
        .expect("worker");
        assert!(fired.load(Ordering::SeqCst));
    }
}
