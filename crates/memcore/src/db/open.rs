use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, OpenFlags};
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::error::MemoryError;

const BUSY_TIMEOUT_MS: u64 = 5_000;
const LOCK_RETRY_ATTEMPTS: usize = 6;
const LOCK_RETRY_INITIAL_BACKOFF_MS: u64 = 10;
const LOCK_RETRY_MAX_BACKOFF_MS: u64 = 250;
const LOCK_RETRY_MAX_ELAPSED_MS: u64 = 30_000;

/// Page cache size, negative = size in KiB (SQLite convention). Matches the
/// writer-side `PRAGMA cache_size = -16000` in `schema/ddl.rs`'s
/// `CONNECTION_PRAGMA_SQL` — the value was already computed there but never
/// reached read-only handles opened through this module.
const READ_CACHE_SIZE_KIB: i64 = -16_000;
/// mmap the DB file for reads (256 MB) so hot pages are served from the OS
/// page cache instead of round-tripping through SQLite's pager on every
/// read. Read-only handles never write, so this carries none of the
/// crash-consistency caveats `mmap_size` has on a writer connection.
const READ_MMAP_SIZE_BYTES: i64 = 256 * 1024 * 1024;

static SQLITE_STARTUP_LOCK: Mutex<()> = Mutex::new(());

pub(crate) type ReservedReferenceWriteFlag = Arc<AtomicBool>;

pub(crate) struct ReservedReferenceWriteAuthorization {
    flag: ReservedReferenceWriteFlag,
}

impl Drop for ReservedReferenceWriteAuthorization {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

pub(crate) fn register_reserved_reference_write_guard(
    conn: &Connection,
) -> rusqlite::Result<ReservedReferenceWriteFlag> {
    let flag = Arc::new(AtomicBool::new(false));
    let function_flag = Arc::clone(&flag);
    conn.create_scalar_function(
        "tachi_reserved_reference_write_enabled",
        0,
        FunctionFlags::SQLITE_UTF8,
        move |_| Ok(i64::from(function_flag.load(Ordering::SeqCst))),
    )?;
    Ok(flag)
}

fn sqlite_identifier_eq(raw: *const c_char, expected: &[u8]) -> bool {
    if raw.is_null() {
        return false;
    }
    // SQLite owns these NUL-terminated strings for the duration of the call.
    unsafe { CStr::from_ptr(raw) }
        .to_bytes()
        .eq_ignore_ascii_case(expected)
}

fn protected_reference_trigger(raw: *const c_char) -> bool {
    sqlite_identifier_eq(raw, b"memories_reserved_refs_insert_guard")
        || sqlite_identifier_eq(raw, b"memories_reserved_refs_update_guard")
}

unsafe extern "C" fn reserved_reference_authorizer(
    state: *mut c_void,
    action: c_int,
    arg1: *const c_char,
    arg2: *const c_char,
    _database: *const c_char,
    _accessor: *const c_char,
) -> c_int {
    let authorized = if state.is_null() {
        false
    } else {
        // MemoryStore owns this AtomicBool for longer than its Connection. Raw
        // fixture handles pass null and therefore remain permanently denied.
        unsafe { &*state.cast::<AtomicBool>() }.load(Ordering::SeqCst)
    };
    if authorized {
        return rusqlite::ffi::SQLITE_OK;
    }

    let trigger_ddl = matches!(
        action,
        rusqlite::ffi::SQLITE_CREATE_TRIGGER
            | rusqlite::ffi::SQLITE_CREATE_TEMP_TRIGGER
            | rusqlite::ffi::SQLITE_DROP_TRIGGER
            | rusqlite::ffi::SQLITE_DROP_TEMP_TRIGGER
    ) && protected_reference_trigger(arg1);
    let protected_memory_write = (action == rusqlite::ffi::SQLITE_INSERT
        && sqlite_identifier_eq(arg1, b"memories"))
        || (action == rusqlite::ffi::SQLITE_UPDATE
            && sqlite_identifier_eq(arg1, b"memories")
            && sqlite_identifier_eq(arg2, b"metadata"));
    let protected_memory_schema = (action == rusqlite::ffi::SQLITE_DROP_TABLE
        && sqlite_identifier_eq(arg1, b"memories"))
        || (action == rusqlite::ffi::SQLITE_ALTER_TABLE && sqlite_identifier_eq(arg2, b"memories"));
    let unsafe_pragma =
        action == rusqlite::ffi::SQLITE_PRAGMA && sqlite_identifier_eq(arg1, b"writable_schema");
    let attached_schema = matches!(
        action,
        rusqlite::ffi::SQLITE_ATTACH | rusqlite::ffi::SQLITE_DETACH
    );

    if trigger_ddl
        || protected_memory_write
        || protected_memory_schema
        || unsafe_pragma
        || attached_schema
    {
        rusqlite::ffi::SQLITE_DENY
    } else {
        rusqlite::ffi::SQLITE_OK
    }
}

pub(crate) fn install_reserved_reference_authorizer(
    conn: &Connection,
    authorization: Option<&ReservedReferenceWriteFlag>,
) -> rusqlite::Result<()> {
    let state = authorization
        .map(|flag| Arc::as_ptr(flag).cast_mut().cast::<c_void>())
        .unwrap_or(std::ptr::null_mut());
    let result = unsafe {
        rusqlite::ffi::sqlite3_set_authorizer(
            conn.handle(),
            Some(reserved_reference_authorizer),
            state,
        )
    };
    if result == rusqlite::ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(result),
            Some("install reserved-reference connection authorizer".to_string()),
        ))
    }
}

pub(crate) fn ensure_reserved_reference_write_guard(conn: &Connection) -> Result<(), MemoryError> {
    if conn
        .query_row(
            "SELECT tachi_reserved_reference_write_enabled()",
            [],
            |row| row.get::<_, i64>(0),
        )
        .is_err()
    {
        let _deny_by_default = register_reserved_reference_write_guard(conn)?;
    }
    Ok(())
}

pub(crate) fn authorize_reserved_reference_write(
    flag: &ReservedReferenceWriteFlag,
) -> Result<ReservedReferenceWriteAuthorization, MemoryError> {
    flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .map_err(|_| {
            MemoryError::InvalidArg(
                "reserved reference write authorization is already active".to_string(),
            )
        })?;
    Ok(ReservedReferenceWriteAuthorization {
        flag: Arc::clone(flag),
    })
}

/// Process-wide count of explicit application-level lock-retry backoffs —
/// i.e. how many times `retry_memory_locked` observed a BUSY/LOCKED error and
/// slept before retrying. This is NOT time spent inside SQLite's own opaque
/// `busy_timeout` handler (see that function's doc comment); it is a plain
/// counter for benchmarks/diagnostics that want a retry count without wiring
/// up a tracing subscriber. It counts across every store in the process, not
/// scoped to one DB — callers isolating one workload's retries should
/// snapshot this before and after and diff.
static LOCK_RETRY_BACKOFF_COUNT: AtomicU64 = AtomicU64::new(0);

/// Read the process-wide lock-retry backoff counter (see
/// `LOCK_RETRY_BACKOFF_COUNT`'s doc comment).
pub fn lock_retry_backoff_count() -> u64 {
    LOCK_RETRY_BACKOFF_COUNT.load(Ordering::Relaxed)
}

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
    configure_read_only_connection(&conn)?;
    Ok(conn)
}

pub(crate) fn configure_connection(conn: &Connection) -> Result<(), MemoryError> {
    conn.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))?;
    Ok(())
}

/// Read-only-only PRAGMAs. `cache_size` and `mmap_size` are both legal to set
/// on a read-only handle (they configure this connection's local page
/// cache/mmap window, not the DB file), unlike `journal_mode`/`foreign_keys`,
/// which are writer/schema concerns intentionally NOT added here.
fn configure_read_only_connection(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute_batch(&format!(
        "PRAGMA cache_size = {READ_CACHE_SIZE_KIB};
         PRAGMA mmap_size = {READ_MMAP_SIZE_BYTES};"
    ))?;
    Ok(())
}

/// Retry a write operation through SQLite BUSY/LOCKED with explicit
/// application-level backoff. `op` (e.g. "upsert", "gc_tables") and
/// `db_label` (the store's manifest label, e.g. "global"/"project" — never a
/// filesystem path) are structured diagnostic tags only: no DB path, query
/// text, memory content, or credential ever appears in the emitted events
/// (kckylechen1/tachi#1093).
///
/// The `explicit_backoff_ms` field this reports is ONLY the time this loop
/// spent in its own `std::thread::sleep(backoff)` calls between attempts. It
/// deliberately does NOT include time spent blocked inside SQLite's own
/// `busy_timeout` handler (configured in `configure_connection`, up to
/// `BUSY_TIMEOUT_MS` per `operation()` call) — that time is opaque to
/// rusqlite (no callback hook is installed to observe it) and is not
/// reported here rather than mislabeled as something we measured.
pub(crate) fn retry_memory_locked<T>(
    op: &str,
    db_label: &str,
    mut operation: impl FnMut() -> Result<T, MemoryError>,
) -> Result<T, MemoryError> {
    let started_at = std::time::Instant::now();
    let mut backoff = Duration::from_millis(LOCK_RETRY_INITIAL_BACKOFF_MS);
    let max_backoff = Duration::from_millis(LOCK_RETRY_MAX_BACKOFF_MS);
    let max_elapsed = Duration::from_millis(LOCK_RETRY_MAX_ELAPSED_MS);
    let mut explicit_backoff = Duration::ZERO;

    for attempt in 1..=LOCK_RETRY_ATTEMPTS {
        match operation() {
            Ok(value) => {
                if attempt > 1 {
                    tracing::debug!(
                        op,
                        db_label,
                        attempts = attempt,
                        explicit_backoff_ms = explicit_backoff.as_millis() as u64,
                        "memcore lock retry: recovered from database busy/locked"
                    );
                }
                return Ok(value);
            }
            Err(error)
                if memory_error_is_locked(&error)
                    && attempt < LOCK_RETRY_ATTEMPTS
                    && started_at.elapsed().saturating_add(backoff) < max_elapsed =>
            {
                tracing::debug!(
                    op,
                    db_label,
                    attempt,
                    backoff_ms = backoff.as_millis() as u64,
                    "memcore lock retry: database busy/locked, backing off"
                );
                LOCK_RETRY_BACKOFF_COUNT.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(backoff);
                explicit_backoff += backoff;
                backoff = (backoff * 2).min(max_backoff);
            }
            Err(error) => {
                if attempt > 1 {
                    tracing::debug!(
                        op,
                        db_label,
                        attempts = attempt,
                        explicit_backoff_ms = explicit_backoff.as_millis() as u64,
                        "memcore lock retry: giving up after retries"
                    );
                }
                return Err(error);
            }
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

/// Public: classify whether a `rusqlite::Error` represents a transient
/// BUSY/LOCKED condition (another process — typically a live daemon — holds
/// the file lock) rather than a genuine open/query failure. Pure
/// classification, no side effects; exposed so CLI tooling outside this
/// crate (e.g. `tachi migrate`, kckylechen1/tachi#1223) can skip-and-report a
/// library another process currently holds instead of treating contention as
/// a hard error.
pub fn sqlite_error_is_locked(error: &rusqlite::Error) -> bool {
    use rusqlite::ffi::ErrorCode;
    matches!(
        error,
        rusqlite::Error::SqliteFailure(err, _)
            if matches!(err.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}
