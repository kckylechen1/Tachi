//! Scratch-only regression contracts for explicitly budgeted opens.

use memcore::{DbOpenContext, MemoryError, MemoryStore};
use std::time::Duration;

fn is_busy(error: &MemoryError) -> bool {
    matches!(error, MemoryError::Sqlite(error) if memcore::db::sqlite_error_is_locked(error))
}

#[test]
fn zero_budget_refuses_before_creating_a_store() {
    let root = tempfile::tempdir().expect("owned fixture");
    let path = root.path().join("tachi-memory.db");
    let outcome = MemoryStore::open_with_context_and_busy_timeout(
        path.to_str().unwrap(),
        &DbOpenContext::create_fresh(),
        Duration::ZERO,
    );
    let error = match outcome {
        Ok(store) => {
            drop(store);
            panic!("an exhausted admission budget must not create a store");
        }
        Err(error) => error,
    };
    assert!(is_busy(&error), "expected typed contention refusal: {error}");
    assert!(!path.exists());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn ordinary_open_still_creates_a_fresh_store() {
    let root = tempfile::tempdir().expect("owned fixture");
    let path = root.path().join("tachi-memory.db");
    drop(MemoryStore::open(path.to_str().unwrap()).expect("unbudgeted control"));
    assert!(path.exists());
}

#[cfg(feature = "test-support")]
#[test]
fn budget_expires_while_another_open_owns_the_process_startup_mutex() {
    use std::sync::mpsc;
    use std::time::Instant;

    let root = tempfile::tempdir().expect("owned fixture");
    let held_path = root.path().join("held.db");
    let waiting_path = root.path().join("waiting.db");
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let _hook = MemoryStore::install_startup_ownership_hook_for_tests(
        held_path.to_str().unwrap(),
        || {},
        move || {
            held_tx.send(()).expect("announce actual startup ownership");
            // A bounded fixture wait also releases on sender drop/unwind.
            let _ = release_rx.recv_timeout(Duration::from_secs(20));
        },
    );
    let holder = std::thread::spawn(move || {
        MemoryStore::open(held_path.to_str().unwrap()).map(drop)
    });
    let held = held_rx.recv_timeout(Duration::from_secs(10));
    if held.is_err() {
        let _ = release_tx.send(());
        let _ = holder.join();
        panic!("fixture did not reach actual startup ownership: {held:?}");
    }
    let (done_tx, done_rx) = mpsc::channel();
    let waiting = waiting_path.clone();
    let waiter = std::thread::spawn(move || {
        let started = Instant::now();
        let result = MemoryStore::open_with_context_and_busy_timeout(
            waiting.to_str().unwrap(),
            &DbOpenContext::create_fresh(),
            Duration::from_millis(100),
        )
        .map(drop);
        let _ = done_tx.send((result, started.elapsed()));
    });
    // Twenty times the requested waiting budget: hang backstop, not a benchmark.
    let before_release = done_rx.recv_timeout(Duration::from_secs(2));
    let path_existed_before_release = waiting_path.exists();
    // Always release and join both workers before any regression assertion.
    let _ = release_tx.send(());
    let holder_result = holder.join();
    let waiter_result = waiter.join();
    holder_result.expect("holder joined").expect("ordinary holder open");
    waiter_result.expect("waiter joined");
    let (outcome, _) = before_release.expect("budget must expire before holder release");
    let error = outcome.expect_err("blocked budgeted open must refuse");
    assert!(is_busy(&error), "expected typed contention refusal: {error}");
    assert!(!path_existed_before_release);
    assert!(!waiting_path.exists(), "refusal must leave no delayed creator");
}
