use rusqlite::{Connection, OpenFlags};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use crate::error::MemoryError;

const BUSY_TIMEOUT_MS: u64 = 5_000;

static SQLITE_STARTUP_LOCK: Mutex<()> = Mutex::new(());

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
