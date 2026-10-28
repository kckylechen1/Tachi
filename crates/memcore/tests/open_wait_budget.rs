//! Scratch-only regression contracts for explicitly budgeted opens.

use memcore::{DbOpenContext, MemoryError, MemoryStore};
use std::time::Duration;

fn is_busy(error: &MemoryError) -> bool {
    matches!(error, MemoryError::Sqlite(error) if memcore::db::sqlite_error_is_locked(error))
}

/// Directory-lock contention draws on the same budget (#2143): while
/// another handle holds the parent directory's exclusive `flock` (what
/// `acquire_cross_process_startup_lock` takes), a budgeted open must
/// refuse with the typed busy error and create nothing.
#[test]
fn budget_expires_while_another_handle_holds_the_directory_startup_lock() {
    let root = tempfile::tempdir().expect("owned fixture");
    let path = root.path().join("tachi-memory.db");
    let dir = std::fs::File::open(root.path()).expect("open directory for flock");
    dir.try_lock().expect("hold the directory startup lock");
    let error = match MemoryStore::open_with_context_and_busy_timeout(
        path.to_str().unwrap(),
        &DbOpenContext::create_fresh(),
        Duration::from_millis(50),
    ) {
        Ok(store) => {
            drop(store);
            panic!("directory-held budgeted open must refuse");
        }
        Err(error) => error,
    };
    assert!(
        is_busy(&error),
        "expected typed contention refusal: {error}"
    );
    assert!(!path.exists());
    let _ = dir.unlock();
}

/// A budgeted open that SUCCEEDS must restore the ordinary busy policy
/// before the retained handle escapes (#2143): the small deadline bounded
/// admission, not the handle's later writes.
#[test]
fn successful_budgeted_open_restores_ordinary_busy_timeout_on_the_retained_handle() {
    let root = tempfile::tempdir().expect("owned fixture");
    let path = root.path().join("tachi-memory.db");
    let store = MemoryStore::open_with_context_and_busy_timeout(
        path.to_str().unwrap(),
        &DbOpenContext::create_fresh(),
        Duration::from_secs(30),
    )
    .expect("budgeted open succeeds");
    let observed: i64 = store
        .connection()
        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
        .expect("read busy_timeout on the retained handle");
    assert_eq!(
        observed, 5000,
        "the retained handle must carry the ordinary 5s busy timeout, not the admission budget"
    );
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
    assert!(
        is_busy(&error),
        "expected typed contention refusal: {error}"
    );
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
    let holder =
        std::thread::spawn(move || MemoryStore::open(held_path.to_str().unwrap()).map(drop));
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
    holder_result
        .expect("holder joined")
        .expect("ordinary holder open");
    waiter_result.expect("waiter joined");
    let (outcome, _) = before_release.expect("budget must expire before holder release");
    let error = outcome.expect_err("blocked budgeted open must refuse");
    assert!(
        is_busy(&error),
        "expected typed contention refusal: {error}"
    );
    assert!(!path_existed_before_release);
    assert!(
        !waiting_path.exists(),
        "refusal must leave no delayed creator"
    );
}
