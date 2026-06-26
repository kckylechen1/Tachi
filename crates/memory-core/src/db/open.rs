use rusqlite::{Connection, OpenFlags};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use crate::error::MemoryError;

const BUSY_TIMEOUT_MS: u64 = 5_000;
const LOCK_RETRY_ATTEMPTS: usize = 6;
const LOCK_RETRY_INITIAL_BACKOFF_MS: u64 = 10;
const LOCK_RETRY_MAX_BACKOFF_MS: u64 = 250;
const LOCK_RETRY_MAX_ELAPSED_MS: u64 = 30_000;

static SQLITE_STARTUP_LOCK: Mutex<()> = Mutex::new(());

/// Serialize in-process SQLite open+schema initialization.
///
/// SQLite handles cross-process coordination through file locks/WAL, but a
/// single process can still stampede several stores through schema init during
/// startup. Keeping that phase single-file inside this process removes one
/// avoidable source of `database is locked` errors while preserving concurrent
/// read pools after startup.
pub(crate) fn acquire_startup_lock() -> MutexGuard<'static, ()> {
    SQLITE_STARTUP_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn open_read_write(db_path: &str) -> Result<Connection, MemoryError> {
    let conn = Connection::open(db_path)?;
    configure_connection(&conn)?;
    Ok(conn)
}

pub(crate) fn open_read_only(db_path: &str) -> Result<Connection, MemoryError> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    configure_connection(&conn)?;
    Ok(conn)
}

pub(crate) fn configure_connection(conn: &Connection) -> Result<(), MemoryError> {
    conn.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))?;
    Ok(())
}

pub(crate) fn retry_memory_locked<T>(
    mut operation: impl FnMut() -> Result<T, MemoryError>,
) -> Result<T, MemoryError> {
    let started_at = std::time::Instant::now();
    let mut backoff = Duration::from_millis(LOCK_RETRY_INITIAL_BACKOFF_MS);
    let max_backoff = Duration::from_millis(LOCK_RETRY_MAX_BACKOFF_MS);
    let max_elapsed = Duration::from_millis(LOCK_RETRY_MAX_ELAPSED_MS);

    for attempt in 1..=LOCK_RETRY_ATTEMPTS {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error)
                if memory_error_is_locked(&error)
                    && attempt < LOCK_RETRY_ATTEMPTS
                    && started_at.elapsed().saturating_add(backoff) < max_elapsed =>
            {
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(max_backoff);
            }
            Err(error) => return Err(error),
        }
    }

    unreachable!("retry loop returns on every final attempt")
}

fn memory_error_is_locked(error: &MemoryError) -> bool {
    let MemoryError::Sqlite(error) = error else {
        return false;
    };
    sqlite_error_is_locked(error)
}

pub(crate) fn sqlite_error_is_locked(error: &rusqlite::Error) -> bool {
    use rusqlite::ffi::ErrorCode;
    matches!(
        error,
        rusqlite::Error::SqliteFailure(err, _)
            if matches!(err.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}
