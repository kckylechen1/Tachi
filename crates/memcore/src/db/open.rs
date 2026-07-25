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

pub(crate) struct ConnectionAuthorizationState {
    typed_dml: AtomicBool,
    schema_migration: AtomicBool,
    planner_maintenance: AtomicBool,
}

pub(crate) type ReservedReferenceWriteFlag = Arc<ConnectionAuthorizationState>;

enum AuthorizationKind {
    TypedDml,
    SchemaMigration,
    PlannerMaintenance,
}

pub(crate) struct ReservedReferenceWriteAuthorization {
    flag: ReservedReferenceWriteFlag,
    kind: AuthorizationKind,
}

impl Drop for ReservedReferenceWriteAuthorization {
    fn drop(&mut self) {
        match self.kind {
            AuthorizationKind::TypedDml => self.flag.typed_dml.store(false, Ordering::SeqCst),
            AuthorizationKind::SchemaMigration => {
                self.flag.schema_migration.store(false, Ordering::SeqCst)
            }
            AuthorizationKind::PlannerMaintenance => {
                self.flag.planner_maintenance.store(false, Ordering::SeqCst)
            }
        }
    }
}

pub(crate) fn register_reserved_reference_write_guard(
    conn: &Connection,
) -> rusqlite::Result<ReservedReferenceWriteFlag> {
    let flag = Arc::new(ConnectionAuthorizationState {
        typed_dml: AtomicBool::new(false),
        schema_migration: AtomicBool::new(false),
        planner_maintenance: AtomicBool::new(false),
    });
    let function_flag = Arc::clone(&flag);
    conn.create_scalar_function(
        "tachi_reserved_reference_write_enabled",
        0,
        FunctionFlags::SQLITE_UTF8,
        move |_| {
            Ok(i64::from(
                function_flag.typed_dml.load(Ordering::SeqCst)
                    || function_flag.schema_migration.load(Ordering::SeqCst),
            ))
        },
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

fn expected_reference_trigger(raw: *const c_char) -> bool {
    sqlite_identifier_eq(raw, b"memories_reserved_refs_insert_guard")
        || sqlite_identifier_eq(raw, b"memories_reserved_refs_update_guard")
}

fn expected_search_generation_trigger(raw_name: *const c_char, raw_table: *const c_char) -> bool {
    if raw_name.is_null() || raw_table.is_null() {
        return false;
    }
    // SQLite owns these NUL-terminated strings for this authorizer callback.
    let name = unsafe { CStr::from_ptr(raw_name) }.to_str();
    let table = unsafe { CStr::from_ptr(raw_table) }.to_str();
    matches!((name, table), (Ok(name), Ok(table)) if crate::db::search_generation::is_expected_search_generation_trigger_target(name, table))
}

fn schema_mutation(action: c_int) -> bool {
    matches!(
        action,
        rusqlite::ffi::SQLITE_CREATE_INDEX
            | rusqlite::ffi::SQLITE_CREATE_TABLE
            | rusqlite::ffi::SQLITE_CREATE_TEMP_INDEX
            | rusqlite::ffi::SQLITE_CREATE_TEMP_TABLE
            | rusqlite::ffi::SQLITE_CREATE_TEMP_TRIGGER
            | rusqlite::ffi::SQLITE_CREATE_TEMP_VIEW
            | rusqlite::ffi::SQLITE_CREATE_TRIGGER
            | rusqlite::ffi::SQLITE_CREATE_VIEW
            | rusqlite::ffi::SQLITE_CREATE_VTABLE
            | rusqlite::ffi::SQLITE_DROP_INDEX
            | rusqlite::ffi::SQLITE_DROP_TABLE
            | rusqlite::ffi::SQLITE_DROP_TEMP_INDEX
            | rusqlite::ffi::SQLITE_DROP_TEMP_TABLE
            | rusqlite::ffi::SQLITE_DROP_TEMP_TRIGGER
            | rusqlite::ffi::SQLITE_DROP_TEMP_VIEW
            | rusqlite::ffi::SQLITE_DROP_TRIGGER
            | rusqlite::ffi::SQLITE_DROP_VIEW
            | rusqlite::ffi::SQLITE_DROP_VTABLE
            | rusqlite::ffi::SQLITE_ALTER_TABLE
            | rusqlite::ffi::SQLITE_ANALYZE
            | rusqlite::ffi::SQLITE_REINDEX
    )
}

fn protected_memory_authority_column(raw: *const c_char) -> bool {
    // These columns are consumed by namespace routing, write affinity,
    // lifecycle protection/CAS, or trusted-state classification. Content and
    // search-index columns deliberately remain available to compatibility
    // callers; metadata carries the remaining typed authority markers.
    [
        b"id".as_slice(),
        b"path".as_slice(),
        b"category".as_slice(),
        b"topic".as_slice(),
        b"source".as_slice(),
        b"scope".as_slice(),
        b"archived".as_slice(),
        b"valid_from".as_slice(),
        b"valid_until".as_slice(),
        b"revision".as_slice(),
        b"metadata".as_slice(),
        b"retention_policy".as_slice(),
        b"domain".as_slice(),
        b"superseded_by".as_slice(),
        b"idless_identity".as_slice(),
        b"recall_count".as_slice(),
        b"query_diversity".as_slice(),
        b"tier".as_slice(),
    ]
    .iter()
    .any(|expected| sqlite_identifier_eq(raw, expected))
}

unsafe extern "C" fn reserved_reference_authorizer(
    state: *mut c_void,
    action: c_int,
    arg1: *const c_char,
    arg2: *const c_char,
    database: *const c_char,
    accessor: *const c_char,
) -> c_int {
    let (typed_dml, schema_migration, planner_maintenance) = if state.is_null() {
        (false, false, false)
    } else {
        // MemoryStore owns this state for longer than its Connection. Raw
        // fixture handles pass null and therefore remain permanently denied.
        let state = unsafe { &*state.cast::<ConnectionAuthorizationState>() };
        (
            state.typed_dml.load(Ordering::SeqCst),
            state.schema_migration.load(Ordering::SeqCst),
            state.planner_maintenance.load(Ordering::SeqCst),
        )
    };

    let trigger_ddl = matches!(
        action,
        rusqlite::ffi::SQLITE_CREATE_TRIGGER
            | rusqlite::ffi::SQLITE_CREATE_TEMP_TRIGGER
            | rusqlite::ffi::SQLITE_DROP_TRIGGER
            | rusqlite::ffi::SQLITE_DROP_TEMP_TRIGGER
    );
    if trigger_ddl {
        let canonical_migration_trigger = schema_migration
            && matches!(
                action,
                rusqlite::ffi::SQLITE_CREATE_TRIGGER | rusqlite::ffi::SQLITE_DROP_TRIGGER
            )
            && ((expected_reference_trigger(arg1) && sqlite_identifier_eq(arg2, b"memories"))
                || expected_search_generation_trigger(arg1, arg2))
            && sqlite_identifier_eq(database, b"main");
        return if canonical_migration_trigger {
            rusqlite::ffi::SQLITE_OK
        } else {
            rusqlite::ffi::SQLITE_DENY
        };
    }

    if schema_migration {
        return rusqlite::ffi::SQLITE_OK;
    }

    let planner_maintenance_action = action == rusqlite::ffi::SQLITE_ANALYZE
        || (action == rusqlite::ffi::SQLITE_CREATE_TABLE
            && (sqlite_identifier_eq(arg1, b"sqlite_stat1")
                || sqlite_identifier_eq(arg1, b"sqlite_stat4"))
            && sqlite_identifier_eq(database, b"main"));
    if planner_maintenance && planner_maintenance_action {
        return rusqlite::ffi::SQLITE_OK;
    }

    if schema_mutation(action) {
        return rusqlite::ffi::SQLITE_DENY;
    }

    let protected_memory_write = (action == rusqlite::ffi::SQLITE_INSERT
        && sqlite_identifier_eq(arg1, b"memories"))
        || (action == rusqlite::ffi::SQLITE_UPDATE
            && sqlite_identifier_eq(arg1, b"memories")
            && protected_memory_authority_column(arg2));
    let unsafe_pragma =
        action == rusqlite::ffi::SQLITE_PRAGMA && sqlite_identifier_eq(arg1, b"writable_schema");
    let attached_schema = matches!(
        action,
        rusqlite::ffi::SQLITE_ATTACH | rusqlite::ffi::SQLITE_DETACH
    );

    if protected_memory_write {
        // A direct typed statement may mutate protected fields. SQL executed
        // indirectly by a trigger never inherits that authority.
        if typed_dml && accessor.is_null() {
            rusqlite::ffi::SQLITE_OK
        } else {
            rusqlite::ffi::SQLITE_DENY
        }
    } else if unsafe_pragma || attached_schema {
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

fn normalize_trigger_sql(sql: &str) -> String {
    sql.trim_end_matches(|ch: char| ch == ';' || ch.is_whitespace())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn validate_persistent_trigger_inventory(
    conn: &Connection,
    require_complete: bool,
) -> Result<(), MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT name, tbl_name, sql
         FROM main.sqlite_schema
         WHERE type = 'trigger'
         ORDER BY name",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut found = std::collections::HashSet::new();
    for row in rows {
        let (name, table, sql) = row?;
        if crate::db::search_generation::is_canonical_search_generation_trigger(
            &name,
            &table,
            sql.as_deref(),
        ) {
            continue;
        }
        let Some((canonical_name, canonical_sql)) =
            crate::db::schema::expected_reserved_reference_trigger(&name)
        else {
            return Err(MemoryError::InvalidArg(format!(
                "unsafe persistent trigger inventory: unexpected trigger '{name}' on table '{table}'"
            )));
        };
        if name != canonical_name
            || table != "memories"
            || sql.as_deref().map(normalize_trigger_sql)
                != Some(normalize_trigger_sql(canonical_sql))
        {
            return Err(MemoryError::InvalidArg(format!(
                "unsafe persistent trigger inventory: trigger '{name}' does not match the canonical '{canonical_name}' definition"
            )));
        }
        found.insert(canonical_name);
    }

    if require_complete {
        for expected in [
            "memories_reserved_refs_insert_guard",
            "memories_reserved_refs_update_guard",
        ] {
            if !found.contains(expected) {
                return Err(MemoryError::InvalidArg(format!(
                    "unsafe persistent trigger inventory: required trigger '{expected}' is missing"
                )));
            }
        }
    }
    Ok(())
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
    flag.typed_dml
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .map_err(|_| {
            MemoryError::InvalidArg(
                "reserved reference write authorization is already active".to_string(),
            )
        })?;
    Ok(ReservedReferenceWriteAuthorization {
        flag: Arc::clone(flag),
        kind: AuthorizationKind::TypedDml,
    })
}

pub(crate) fn authorize_schema_migration(
    flag: &ReservedReferenceWriteFlag,
) -> Result<ReservedReferenceWriteAuthorization, MemoryError> {
    flag.schema_migration
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .map_err(|_| {
            MemoryError::InvalidArg("schema migration authorization is already active".to_string())
        })?;
    Ok(ReservedReferenceWriteAuthorization {
        flag: Arc::clone(flag),
        kind: AuthorizationKind::SchemaMigration,
    })
}

pub(crate) fn authorize_planner_maintenance(
    flag: &ReservedReferenceWriteFlag,
) -> Result<ReservedReferenceWriteAuthorization, MemoryError> {
    flag.planner_maintenance
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .map_err(|_| {
            MemoryError::InvalidArg(
                "planner maintenance authorization is already active".to_string(),
            )
        })?;
    Ok(ReservedReferenceWriteAuthorization {
        flag: Arc::clone(flag),
        kind: AuthorizationKind::PlannerMaintenance,
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
