//! Test-only timing of the native SQLite VFS sleep; never replaces its busy handler.

use rusqlite::{ffi, Connection, OpenFlags};
use std::ffi::{c_char, c_int, CStr};
use std::sync::{
    atomic::{AtomicI32, AtomicPtr, AtomicU64, AtomicUsize, Ordering},
    OnceLock,
};
use std::time::Instant;

const KEY: &str = "contended-heartbeat-exit";
const VFS_NAME: &CStr = c"tachi_heartbeat_native_sleep_probe";
static START: OnceLock<Instant> = OnceLock::new();
static STAGES: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];
static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
static PARENT: AtomicPtr<ffi::sqlite3_vfs> = AtomicPtr::new(std::ptr::null_mut());
static SLEEP_COUNT: AtomicUsize = AtomicUsize::new(0);
struct Sleep {
    requested_us: AtomicI32,
    entered_us: AtomicU64,
    returned_us: AtomicU64,
    native_result: AtomicI32,
}
static SLEEPS: [Sleep; 64] = [const {
    Sleep {
        requested_us: AtomicI32::new(0),
        entered_us: AtomicU64::new(0),
        returned_us: AtomicU64::new(0),
        native_result: AtomicI32::new(0),
    }
}; 64];

fn elapsed_us() -> u64 {
    START.get().map_or(0, |start| {
        (start.elapsed().as_micros() as u64).saturating_add(1)
    })
}

pub(super) fn mark(key: &str, stage: usize) {
    if key == KEY {
        START.get_or_init(Instant::now);
        let _ =
            STAGES[stage].compare_exchange(0, elapsed_us(), Ordering::Release, Ordering::Relaxed);
    }
}

unsafe extern "C" fn vfs_open(
    _vfs: *mut ffi::sqlite3_vfs,
    filename: *const c_char,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    output_flags: *mut c_int,
) -> c_int {
    let parent = PARENT.load(Ordering::Acquire);
    if parent.is_null() {
        return ffi::SQLITE_MISUSE;
    }
    // SAFETY: registration publishes the process-lived native parent before opening.
    match unsafe { (*parent).xOpen } {
        Some(open) => unsafe { open(parent, filename, file, flags, output_flags) },
        None => ffi::SQLITE_MISUSE,
    }
}

unsafe extern "C" fn vfs_sleep(_vfs: *mut ffi::sqlite3_vfs, requested: c_int) -> c_int {
    let parent = PARENT.load(Ordering::Acquire);
    if parent.is_null() {
        return 0;
    }
    // SAFETY: the registered parent outlives every worker and has immutable callbacks.
    let Some(native_sleep) = (unsafe { (*parent).xSleep }) else {
        return 0;
    };
    let slot = SLEEPS.get(SLEEP_COUNT.fetch_add(1, Ordering::Relaxed));
    if let Some(slot) = slot {
        slot.requested_us.store(requested, Ordering::Relaxed);
        slot.entered_us.store(elapsed_us(), Ordering::Release);
    }
    // No logging, allocation, locks or unwinding on this native callback path.
    let result = unsafe { native_sleep(parent, requested) };
    if let Some(slot) = slot {
        slot.native_result.store(result, Ordering::Relaxed);
        slot.returned_us.store(elapsed_us(), Ordering::Release);
    }
    result
}

fn register() -> Result<(), String> {
    REGISTERED
        .get_or_init(|| {
            START.get_or_init(Instant::now);
            // SAFETY: SQLite initializes and serializes its VFS registry lookup.
            let parent = unsafe { ffi::sqlite3_vfs_find(std::ptr::null()) };
            if parent.is_null() {
                return Err("native SQLite VFS is unavailable".to_string());
            }
            // SQLite mutates pNext under STATIC_MAIN; protect the whole-struct copy too.
            let template = unsafe {
                let mutex = ffi::sqlite3_mutex_alloc(ffi::SQLITE_MUTEX_STATIC_MAIN);
                ffi::sqlite3_mutex_enter(mutex);
                let template = *parent;
                ffi::sqlite3_mutex_leave(mutex);
                template
            };
            if template.xOpen.is_none() || template.xSleep.is_none() {
                return Err("native SQLite VFS lacks open/sleep callbacks".to_string());
            }
            PARENT.store(parent, Ordering::Release);
            let shim = Box::into_raw(Box::new(ffi::sqlite3_vfs {
                pNext: std::ptr::null_mut(),
                zName: VFS_NAME.as_ptr(),
                xOpen: Some(vfs_open),
                xSleep: Some(vfs_sleep),
                ..template
            }));
            // Intentionally process-lived: an assertion can unwind before the worker joins.
            // pAppData and file layout stay native; only this named connection selects us.
            let rc = unsafe { ffi::sqlite3_vfs_register(shim, 0) };
            if rc != ffi::SQLITE_OK {
                return Err(format!("register heartbeat sleep VFS: SQLite code {rc}"));
            }
            Ok(())
        })
        .clone()
}

pub(super) fn open(path: &std::path::Path, key: &str) -> Result<Connection, String> {
    let result = if key == KEY {
        register()?;
        Connection::open_with_flags_and_vfs(path, OpenFlags::default(), VFS_NAME)
    } else {
        Connection::open(path)
    };
    result.map_err(|error| format!("open ingest heartbeat database: {error}"))
}

pub(super) fn execute(
    connection: &Connection,
    key: &str,
    worker: &str,
    owner: &str,
) -> rusqlite::Result<usize> {
    let sql = "UPDATE processed_events \
        SET created_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now') \
        WHERE event_hash = ?1 AND worker = ?2 AND event_id = ?3";
    if key != KEY {
        return connection.execute(sql, rusqlite::params![key, worker, owner]);
    }
    mark(key, 10);
    let prepared = connection.prepare(sql);
    mark(key, 11);
    let mut statement = prepared?;
    mark(key, 12);
    let result = statement.execute(rusqlite::params![key, worker, owner]);
    mark(key, 13);
    mark(key, 14);
    drop(statement);
    mark(key, 15);
    result
}

pub(super) fn dump() {
    let names = [
        "worker_enter",
        "notify",
        "execute_return",
        "connection_drop",
        "worker_return",
        "async_resumed",
        "joiner_enter",
        "join_ready",
        "test_drop",
        "test_join",
        "prepare_enter",
        "prepare_return",
        "step_reset_enter",
        "step_reset_return",
        "statement_drop_enter",
        "statement_drop_return",
    ];
    let stages: Vec<_> = names
        .iter()
        .zip(STAGES.iter())
        .map(|(name, stamp)| (*name, stamp.load(Ordering::Acquire)))
        .collect();
    eprintln!("HEARTBEAT_PROBE_US {stages:?}");
    let parent = PARENT.load(Ordering::Acquire);
    if !parent.is_null() {
        // SAFETY: the native VFS and its static name outlive this test process.
        let name = unsafe { CStr::from_ptr((*parent).zName) };
        eprintln!("HEARTBEAT_NATIVE_VFS {name:?}");
    }
    for (index, slot) in SLEEPS.iter().enumerate() {
        let entered = slot.entered_us.load(Ordering::Acquire);
        if entered != 0 {
            let returned = slot.returned_us.load(Ordering::Acquire);
            let result = (returned != 0).then(|| slot.native_result.load(Ordering::Relaxed));
            eprintln!("HEARTBEAT_SLEEP_US index={index} requested={} entered={entered} returned={returned} native_result={result:?}", slot.requested_us.load(Ordering::Relaxed));
        }
    }
    eprintln!(
        "HEARTBEAT_SLEEP_COUNT {} capacity={}",
        SLEEP_COUNT.load(Ordering::Relaxed),
        SLEEPS.len()
    );
}
