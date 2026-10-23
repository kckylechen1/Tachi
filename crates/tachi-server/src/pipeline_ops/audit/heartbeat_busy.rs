//! Cancellation and one wall-clock wait budget for the heartbeat-only connection.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub(super) struct Cancellation {
    stopped: Mutex<bool>,
    changed: Condvar,
    #[cfg(test)]
    waiting: Option<std::sync::mpsc::Sender<()>>,
}

impl Cancellation {
    pub(super) fn new() -> Self {
        Self {
            stopped: Mutex::new(false),
            changed: Condvar::new(),
            #[cfg(test)]
            waiting: None,
        }
    }

    pub(super) fn stop(&self) {
        // A poisoned state must still wake an in-flight waiter; it cannot retry.
        let mut stopped = self
            .stopped
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *stopped = true;
        self.changed.notify_all();
    }

    fn wait(&self, deadline: Instant, delay: Duration) -> bool {
        let Ok(stopped) = self.stopped.lock() else {
            return false;
        };
        let now = Instant::now();
        if *stopped || now >= deadline {
            return false;
        }
        let delay = delay.min(deadline.saturating_duration_since(now));
        #[cfg(test)]
        if let Some(waiting) = self.waiting.as_ref() {
            let _ = waiting.send(());
        }
        // The predicate and stop's notification share this mutex, including the
        // transition into wait, so stopping before or during wait cannot be lost.
        let Ok((stopped, _)) = self
            .changed
            .wait_timeout_while(stopped, delay, |stopped| !*stopped)
        else {
            return false;
        };
        !*stopped && Instant::now() < deadline
    }
}

#[derive(Clone)]
struct Context {
    cancel: Arc<Cancellation>,
    deadline: Instant,
}

thread_local! {
    static ACTIVE: RefCell<Option<Context>> = const { RefCell::new(None) };
}

pub(super) struct Scope {
    previous: Option<Context>,
    // A callback context must be restored on the thread that installed it.
    _thread: PhantomData<Rc<()>>,
}

impl Scope {
    pub(super) fn enter(cancel: Arc<Cancellation>, budget: Duration) -> Result<Self, String> {
        let context = Context {
            cancel,
            deadline: Instant::now() + budget,
        };
        let previous = ACTIVE
            .try_with(|active| {
                active
                    .try_borrow_mut()
                    .map(|mut active| active.replace(context))
                    .map_err(|_| "heartbeat SQLite wait context is already borrowed".to_string())
            })
            .map_err(|_| "heartbeat SQLite wait context is unavailable".to_string())??;
        Ok(Self {
            previous,
            _thread: PhantomData,
        })
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        ACTIVE.with(|active| *active.borrow_mut() = self.previous.take());
    }
}

pub(super) fn busy_handler(count: i32) -> bool {
    // rusqlite's safe fn-pointer API cannot capture state. Clone it out of TLS
    // before waiting, so no RefCell borrow survives a wait or unwind.
    let context = ACTIVE
        .try_with(|active| active.try_borrow().ok().and_then(|active| active.clone()))
        .ok()
        .flatten();
    let Some(context) = context else {
        return false;
    };
    // Keep SQLite's backoff shape, but replace nominal accumulated sleeps with
    // the refresh's single monotonic deadline, even for a new locking event.
    const DELAYS_MS: [u64; 12] = [1, 2, 5, 10, 15, 20, 25, 25, 25, 50, 50, 100];
    let index = (count.max(0) as usize).min(DELAYS_MS.len() - 1);
    context
        .cancel
        .wait(context.deadline, Duration::from_millis(DELAYS_MS[index]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn observed_cancel() -> (Arc<Cancellation>, mpsc::Receiver<()>) {
        let (waiting, received) = mpsc::channel();
        (
            Arc::new(Cancellation {
                stopped: Mutex::new(false),
                changed: Condvar::new(),
                waiting: Some(waiting),
            }),
            received,
        )
    }

    #[test]
    fn stop_before_busy_callback_refuses_retry() {
        let cancel = Arc::new(Cancellation::new());
        cancel.stop();
        let _scope = Scope::enter(cancel, super::super::INGEST_HEARTBEAT_SQLITE_TIMEOUT)
            .expect("install heartbeat wait context");
        assert!(!busy_handler(0), "stopped heartbeat must not retry");
    }

    #[test]
    fn poisoned_cancellation_refuses_retry_and_can_still_be_stopped() {
        let cancel = Arc::new(Cancellation::new());
        let poison = Arc::clone(&cancel);
        assert!(std::thread::spawn(move || {
            let _guard = poison.stopped.lock().unwrap();
            panic!("exercise poisoned cancellation state");
        })
        .join()
        .is_err());
        let _scope = Scope::enter(Arc::clone(&cancel), Duration::from_secs(10)).unwrap();
        assert!(!busy_handler(0), "poison cannot authorize a retry");
        cancel.stop();
        assert!(!busy_handler(0), "stop also remains fail-closed");
    }

    #[test]
    fn stop_wakes_an_inflight_wait_without_waiting_for_its_timeout() {
        let (cancel, waiting) = observed_cancel();
        let worker_cancel = Arc::clone(&cancel);
        let (finished, result) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let retry = worker_cancel.wait(
                Instant::now() + Duration::from_secs(5),
                Duration::from_secs(5),
            );
            let _ = finished.send(retry);
        });
        let observed = waiting.recv_timeout(Duration::from_secs(1));
        cancel.stop();
        let result = result.recv_timeout(Duration::from_secs(1));
        worker.join().expect("wait worker exits");
        observed.expect("worker enters wait with cancellation mutex held");
        assert!(!result.expect("stop wakes in-flight wait"));
    }

    #[test]
    fn a_new_locking_event_cannot_restart_an_expired_refresh_budget() {
        let _scope = Scope::enter(Arc::new(Cancellation::new()), Duration::from_millis(5))
            .expect("install heartbeat wait context");
        assert!(!busy_handler(11), "first wait exhausts refresh budget");
        assert!(
            !busy_handler(0),
            "callback count zero cannot reset deadline"
        );
        assert!(!busy_handler(0), "repeated events share the same deadline");
    }

    #[test]
    fn callback_context_restores_after_error_panic_and_thread_reuse() {
        std::thread::spawn(|| {
            assert!(!busy_handler(0), "missing context refuses retry");
            let outer = Arc::new(Cancellation::new());
            let outer_scope = Scope::enter(outer, Duration::from_secs(10)).unwrap();
            let failed = (|| -> Result<(), ()> {
                let inner = Arc::new(Cancellation::new());
                inner.stop();
                let _inner = Scope::enter(inner, Duration::from_secs(10)).unwrap();
                assert!(!busy_handler(0), "stopped inner context is active");
                Err(())
            })();
            assert!(failed.is_err());
            assert!(busy_handler(0), "error restores live outer context");
            let panicked = std::panic::catch_unwind(|| {
                let inner = Arc::new(Cancellation::new());
                inner.stop();
                let _inner = Scope::enter(inner, Duration::from_secs(10)).unwrap();
                assert!(!busy_handler(0), "stopped panic scope is active");
                panic!("exercise callback scope unwind");
            });
            assert!(panicked.is_err());
            assert!(busy_handler(0), "panic restores live outer context");
            drop(outer_scope);
            assert!(!busy_handler(0), "next job inherits no callback context");
            {
                let _next =
                    Scope::enter(Arc::new(Cancellation::new()), Duration::from_secs(10)).unwrap();
                assert!(busy_handler(0), "same thread can run another refresh");
            }
            assert!(!busy_handler(0), "successful job also restores TLS");
        })
        .join()
        .expect("context worker exits");
    }

    #[test]
    fn sqlite_retries_successfully_when_writer_unlocks_inside_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("heartbeat.db");
        let locker = rusqlite::Connection::open(&path).unwrap();
        locker
            .execute_batch("CREATE TABLE heartbeat (value INTEGER); BEGIN IMMEDIATE")
            .unwrap();
        let (cancel, waiting) = observed_cancel();
        let worker = std::thread::spawn(move || {
            let _scope =
                Scope::enter(cancel, super::super::INGEST_HEARTBEAT_SQLITE_TIMEOUT).unwrap();
            let connection = rusqlite::Connection::open(path).unwrap();
            connection.busy_handler(Some(busy_handler)).unwrap();
            connection.execute("INSERT INTO heartbeat VALUES (1)", [])
        });
        let observed = waiting.recv_timeout(Duration::from_secs(1));
        locker.execute_batch("ROLLBACK").unwrap();
        let result = worker.join().expect("SQLite worker exits");
        observed.expect("SQLite reached the installed busy handler");
        assert_eq!(result.expect("unlocked writer succeeds within budget"), 1);
        assert_eq!(
            locker
                .query_row("SELECT COUNT(*) FROM heartbeat", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}
