//! Process-wide SQLite extension registration.
//!
//! SQLite auto-extensions are global to the process, not a connection.  The
//! `libsimple` crate therefore must be registered exactly once before any
//! connection opens; concurrent registration races with extension loading and
//! can fail with `SQLITE_BUSY`.

use rusqlite::Result as SqlResult;
use std::sync::{Mutex, OnceLock};

static SIMPLE_AUTO_EXTENSION_REGISTERED: OnceLock<()> = OnceLock::new();
static SIMPLE_AUTO_EXTENSION_REGISTRATION_LOCK: Mutex<()> = Mutex::new(());

/// Register libsimple's tokenizer auto-extension once for this process.
///
/// The lock protects the check-and-register transition.  A failed registration
/// deliberately leaves the marker unset so the next caller receives a real
/// retry instead of a false success.
pub fn enable_simple_auto_extension() -> SqlResult<()> {
    register_once(
        &SIMPLE_AUTO_EXTENSION_REGISTERED,
        &SIMPLE_AUTO_EXTENSION_REGISTRATION_LOCK,
        libsimple::enable_auto_extension,
    )
}

fn register_once(
    registered: &OnceLock<()>,
    registration_lock: &Mutex<()>,
    register: impl FnOnce() -> SqlResult<()>,
) -> SqlResult<()> {
    if registered.get().is_some() {
        return Ok(());
    }

    // A panic while registering must not turn a future caller into a false
    // success. Recover the mutex and leave the marker unset for a real retry.
    let _guard = registration_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if registered.get().is_some() {
        return Ok(());
    }

    register()?;
    registered
        .set(())
        .expect("registration marker must be unset while its lock is held");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn concurrent_registration_attempts_are_coalesced() {
        let registered = Arc::new(OnceLock::new());
        let registration_lock = Arc::new(Mutex::new(()));
        let attempts = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(std::sync::Barrier::new(32));

        std::thread::scope(|scope| {
            for _ in 0..32 {
                let registered = Arc::clone(&registered);
                let registration_lock = Arc::clone(&registration_lock);
                let attempts = Arc::clone(&attempts);
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    register_once(&registered, &registration_lock, || {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .expect("registration succeeds");
                });
            }
        });

        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "the process-global registration must run once even under concurrent opens"
        );
    }

    #[test]
    fn failed_registration_is_not_cached_as_success() {
        let registered = OnceLock::new();
        let registration_lock = Mutex::new(());

        let first = register_once(&registered, &registration_lock, || {
            Err(rusqlite::Error::InvalidQuery)
        });
        assert!(first.is_err(), "the registration error reaches the caller");
        assert!(
            registered.get().is_none(),
            "failed registration must remain retryable"
        );

        register_once(&registered, &registration_lock, || Ok(())).expect("retry succeeds");
        assert!(registered.get().is_some());
    }
}
