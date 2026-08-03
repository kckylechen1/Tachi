//! Singleton-daemon enforcement via an advisory file lock + PID record.
//!
//! The Tachi daemon must not run more than one process per user/host because
//! multiple foundry workers writing to the same set of project DBs will race
//! the `processed_events` claim table and the `foundry_jobs` queue. We enforce
//! singleton-ness by `flock(LOCK_EX | LOCK_NB)` on a stable lock-file path.
//! If acquisition fails because the file is locked, we read the recorded PID
//! and probe it with `kill(pid, 0)`. The path and PID record are informational:
//! neither establishes daemon liveness. If the previous daemon is gone (POSIX
//! releases `flock` on close, while the stable path remains) we fall through
//! and take it over.
//!
//! Lifetime: the returned [`DaemonLock`] holds the open fd. Dropping it
//! clears its PID record, releases the kernel lock, and closes the fd while
//! preserving the stable lock path. Callers must keep it alive for the
//! daemon's full runtime.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

pub(crate) fn legacy_daemon_lock_path(app_home: &Path) -> PathBuf {
    app_home.join("daemon.lock")
}

pub(crate) fn legacy_daemon_pid_path(app_home: &Path) -> PathBuf {
    app_home.join("daemon.pid")
}

pub(crate) fn scoped_daemon_lock_path(app_home: &Path, global_db_path: &Path) -> PathBuf {
    app_home.join(format!("daemon-{}.lock", daemon_scope_id(global_db_path)))
}

pub(crate) fn scoped_daemon_pid_path(app_home: &Path, global_db_path: &Path) -> PathBuf {
    app_home.join(format!("daemon-{}.pid", daemon_scope_id(global_db_path)))
}

pub(crate) fn daemon_scope_id(global_db_path: &Path) -> String {
    let normalized =
        std::fs::canonicalize(global_db_path).unwrap_or_else(|_| global_db_path.into());
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in normalized.display().to_string().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Errors returned by [`DaemonLock::acquire`].
#[derive(Debug)]
pub enum DaemonLockError {
    /// Another daemon is alive and holding the lock. The reported PID is the
    /// owner recorded in the PID file (may be 0 if the file was empty).
    AlreadyRunning { pid: i32 },
    /// Filesystem or syscall error while opening / locking the file.
    Io(std::io::Error),
}

impl std::fmt::Display for DaemonLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaemonLockError::AlreadyRunning { pid } => {
                write!(f, "another tachi daemon is already running (pid {pid})")
            }
            DaemonLockError::Io(e) => write!(f, "daemon lock io error: {e}"),
        }
    }
}

impl std::error::Error for DaemonLockError {}

impl From<std::io::Error> for DaemonLockError {
    fn from(e: std::io::Error) -> Self {
        DaemonLockError::Io(e)
    }
}

/// RAII handle for the singleton daemon advisory lock.
///
/// While alive, holds an exclusive `flock` on a stable lock-file path.
/// Dropping clears the PID record, releases the lock, and closes the descriptor
/// without unlinking that path, so later acquirers and already-open waiters
/// address the same inode.
pub struct DaemonLock {
    file: File,
}

impl DaemonLock {
    /// Acquire the singleton daemon lock at `path` (typically
    /// `~/.tachi/daemon.pid`). Creates parent directories as needed.
    ///
    /// Returns:
    /// - `Ok(DaemonLock)` if the lock was acquired (either fresh, or by
    ///   stealing from a dead previous owner).
    /// - `Err(AlreadyRunning { pid })` if a live process holds the lock.
    /// - `Err(Io)` for filesystem/syscall failures.
    pub fn acquire(path: impl AsRef<Path>) -> Result<Self, DaemonLockError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;

        Self::acquire_file(file)
    }

    /// Acquire an already-existing singleton daemon lock without creating the
    /// path or its parent directories. Cleanup callers use this to avoid
    /// manufacturing lock files for receipts that have no lock identity.
    pub fn acquire_existing(path: impl AsRef<Path>) -> Result<Self, DaemonLockError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;

        Self::acquire_file(file)
    }

    fn acquire_file(file: File) -> Result<Self, DaemonLockError> {
        // Try non-blocking exclusive lock.
        match try_flock_exclusive(&file)? {
            FlockOutcome::Acquired => {
                write_pid(&file)?;
                Ok(DaemonLock { file })
            }
            FlockOutcome::WouldBlock => {
                // Lock is held by someone. Inspect the recorded PID.
                let recorded = read_pid(&file).unwrap_or(0);
                if recorded > 0 && process_alive(recorded) {
                    Err(DaemonLockError::AlreadyRunning { pid: recorded })
                } else {
                    // Stale/dead owner: POSIX flock is released on close, so
                    // the kernel lock should not actually be held. Loop one
                    // more time; if it still blocks, treat as live to be safe.
                    match try_flock_exclusive(&file)? {
                        FlockOutcome::Acquired => {
                            write_pid(&file)?;
                            Ok(DaemonLock { file })
                        }
                        FlockOutcome::WouldBlock => {
                            Err(DaemonLockError::AlreadyRunning { pid: recorded })
                        }
                    }
                }
            }
        }
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        // Clear the record while this owner still holds flock, then release it.
        // Do not unlink the path: an already-open waiter must remain on this
        // inode, rather than lock an unlinked predecessor while a later
        // acquirer creates and locks a replacement inode. `File` closes
        // immediately after this Drop implementation.
        let _ = clear_pid(&self.file);
        let fd = self.file.as_raw_fd();
        // SAFETY: `fd` comes from a live `File` owned by this `DaemonLock`.
        // `flock(LOCK_UN)` does not dereference Rust memory and only requests
        // kernel unlock for that descriptor. Errors are intentionally ignored
        // during drop because the fd close also releases any held flock.
        unsafe {
            libc::flock(fd, libc::LOCK_UN);
        }
    }
}

enum FlockOutcome {
    Acquired,
    WouldBlock,
}

fn try_flock_exclusive(file: &File) -> Result<FlockOutcome, std::io::Error> {
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is borrowed from a valid open `File`. The call only passes
    // integer flags to the OS and does not hand Rust pointers across FFI.
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(FlockOutcome::Acquired)
    } else {
        let err = std::io::Error::last_os_error();
        if matches!(err.raw_os_error(), Some(code) if code == libc::EWOULDBLOCK) {
            Ok(FlockOutcome::WouldBlock)
        } else {
            Err(err)
        }
    }
}

fn write_pid(file: &File) -> std::io::Result<()> {
    let pid = std::process::id();
    let mut handle = file;
    handle.seek(SeekFrom::Start(0))?;
    handle.set_len(0)?;
    writeln!(handle, "{pid}")?;
    handle.flush()?;
    Ok(())
}

fn clear_pid(file: &File) -> std::io::Result<()> {
    let mut handle = file;
    handle.seek(SeekFrom::Start(0))?;
    handle.set_len(0)?;
    handle.flush()?;
    Ok(())
}

fn read_pid(file: &File) -> std::io::Result<i32> {
    let mut handle = file;
    handle.seek(SeekFrom::Start(0))?;
    let mut s = String::new();
    handle.read_to_string(&mut s)?;
    Ok(s.trim().parse::<i32>().unwrap_or(0))
}

/// Error from [`DualDaemonLock::acquire`]: names *which* of the two lock
/// files is held by a live process, so the caller can report who is
/// blocking the operation instead of a generic "already running".
#[derive(Debug)]
pub(crate) enum DualLockError {
    /// The scoped `daemon-<hash>.lock` (matching the operation's target
    /// global DB) is held by a live process.
    ScopedRunning { pid: i32 },
    /// The legacy `daemon.lock` (pre-scoping daemon versions) is held by a
    /// live process.
    LegacyRunning { pid: i32 },
    /// Filesystem/syscall error while opening or locking either file.
    Io(std::io::Error),
}

impl std::fmt::Display for DualLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DualLockError::ScopedRunning { pid } => write!(
                f,
                "another tachi daemon is already running for this DB (pid {pid}, scoped lock)"
            ),
            DualLockError::LegacyRunning { pid } => write!(
                f,
                "another tachi daemon is already running (pid {pid}, legacy lock)"
            ),
            DualLockError::Io(e) => write!(f, "daemon lock io error: {e}"),
        }
    }
}

impl std::error::Error for DualLockError {}

/// Holds both the scoped (`daemon-<hash>.lock`) and legacy (`daemon.lock`)
/// singleton locks for the duration of an operation that must not race a
/// live daemon under either naming scheme. Tidy `--execute` and VACUUM
/// previously probed only the legacy path, so a daemon running under the
/// current scoped-lock scheme (the common case since the scoped lock was
/// introduced) was invisible to them and concurrent writes could corrupt
/// the DB. Acquiring both closes that gap: any live holder of either file
/// blocks acquisition here, and holding both for the operation's lifetime
/// also blocks a new daemon (under either scheme) from starting mid-operation.
pub(crate) struct DualDaemonLock {
    _scoped: DaemonLock,
    _legacy: DaemonLock,
}

impl DualDaemonLock {
    /// Acquire both locks for `global_db_path` under `app_home`. Returns
    /// `Err(DualLockError::ScopedRunning{..})` / `Err(LegacyRunning{..})`
    /// naming exactly which lock is held by a live process; the scoped
    /// lock is checked first and released again before returning if the
    /// legacy lock turns out to be busy (no partial hold on error).
    pub(crate) fn acquire(app_home: &Path, global_db_path: &Path) -> Result<Self, DualLockError> {
        let scoped_path = scoped_daemon_lock_path(app_home, global_db_path);
        let scoped = match DaemonLock::acquire(&scoped_path) {
            Ok(lock) => lock,
            Err(DaemonLockError::AlreadyRunning { pid }) => {
                return Err(DualLockError::ScopedRunning { pid })
            }
            Err(DaemonLockError::Io(e)) => return Err(DualLockError::Io(e)),
        };

        let legacy_path = legacy_daemon_lock_path(app_home);
        let legacy = match DaemonLock::acquire(&legacy_path) {
            Ok(lock) => lock,
            Err(DaemonLockError::AlreadyRunning { pid }) => {
                // Returning drops `scoped` (a local of this function) on the
                // way out, releasing its flock before the caller sees the
                // legacy conflict — no partial hold survives a failed acquire.
                return Err(DualLockError::LegacyRunning { pid });
            }
            Err(DaemonLockError::Io(e)) => return Err(DualLockError::Io(e)),
        };

        Ok(DualDaemonLock {
            _scoped: scoped,
            _legacy: legacy,
        })
    }
}

/// Error from [`ScopedDaemonLock::acquire`].
#[derive(Debug)]
pub(crate) enum ScopedLockError {
    /// The scoped `daemon-<hash>.lock` for this DB is held by a live process.
    Running { pid: i32 },
    /// Filesystem/syscall error while opening or locking the file.
    Io(std::io::Error),
}

impl std::fmt::Display for ScopedLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScopedLockError::Running { pid } => write!(
                f,
                "another tachi daemon is already running for this DB (pid {pid}, scoped lock)"
            ),
            ScopedLockError::Io(e) => write!(f, "daemon lock io error: {e}"),
        }
    }
}

impl std::error::Error for ScopedLockError {}

/// Holds only the scoped (`daemon-<hash>.lock`) singleton lock for
/// `global_db_path` — no legacy-lock attempt.
///
/// This exists for callers that already run *inside* the lifetime of an
/// outer, wider-scoped [`DualDaemonLock`] held elsewhere in the same
/// process (e.g. `tidy --execute`'s outer lock on `target_db`, held for the
/// whole run in `bootstrap/tidy/command.rs`). `legacy_daemon_lock_path` is a
/// single fixed path per `app_home` — NOT scoped per DB — so a second
/// `DaemonLock::acquire` on it from a *different* file descriptor in the
/// same process is not a no-op: `flock(2)` locks are associated with the
/// open file description, not the process, so a second `flock(LOCK_EX)` via
/// a fresh fd on a file this process already holds via another fd is
/// refused exactly like a foreign holder ("may be denied by a lock that the
/// calling process has already placed via another file descriptor" —
/// flock(2)). Re-attempting the legacy lock here would misreport the
/// process's OWN outer hold as `LegacyRunning { pid: self }` and roll back
/// every migration whose source belongs to a different scope than the
/// target — this is not a redundant safety net, it is a guaranteed
/// self-deadlock. The outer lock already excludes every legacy-scheme
/// daemon for the whole operation; only the scoped lock, which is unique
/// per DB, needs a fresh acquisition here.
pub(crate) struct ScopedDaemonLock {
    _scoped: DaemonLock,
}

impl ScopedDaemonLock {
    pub(crate) fn acquire(app_home: &Path, global_db_path: &Path) -> Result<Self, ScopedLockError> {
        let scoped_path = scoped_daemon_lock_path(app_home, global_db_path);
        match DaemonLock::acquire(&scoped_path) {
            Ok(lock) => Ok(ScopedDaemonLock { _scoped: lock }),
            Err(DaemonLockError::AlreadyRunning { pid }) => Err(ScopedLockError::Running { pid }),
            Err(DaemonLockError::Io(e)) => Err(ScopedLockError::Io(e)),
        }
    }
}

/// Check whether `pid` refers to a live process this user can signal. Uses
/// `kill(pid, 0)` which performs the permission and existence check without
/// delivering a signal. Returns `false` for pid<=1.
pub fn process_alive(pid: i32) -> bool {
    if pid <= 1 {
        return false;
    }
    // SAFETY: `kill(pid, 0)` performs an existence/permission probe and does
    // not deliver a signal. `pid` is range-checked above to avoid special
    // process-group semantics for non-positive values.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    let err = std::io::Error::last_os_error();
    // EPERM means the process exists but we lack permission to signal it —
    // still alive from our perspective.
    matches!(err.raw_os_error(), Some(code) if code == libc::EPERM)
}

/// Read the PID currently recorded in `path` without taking the lock. Returns
/// `None` if the file does not exist or is empty/unparseable.
pub fn read_pid_file(path: impl AsRef<Path>) -> Option<i32> {
    let mut s = String::new();
    let mut f = std::fs::File::open(path).ok()?;
    f.read_to_string(&mut s).ok()?;
    s.trim().parse::<i32>().ok().filter(|p| *p > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn acquire_records_current_pid_on_stable_lock_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("daemon.pid");
        let lock = DaemonLock::acquire(&path).expect("first acquire must succeed");
        let recorded = read_pid_file(&path).expect("pid file must be readable");
        assert_eq!(recorded as u32, std::process::id());
        drop(lock);
        assert!(
            path.exists(),
            "the stable lock path must remain after DaemonLock is dropped"
        );
        assert!(
            read_pid_file(&path).is_none(),
            "dropping DaemonLock must clear the PID record while retaining the lock path"
        );
    }

    #[test]
    fn scoped_daemon_paths_are_stable_per_global_db() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global").join("memory.db");
        let other = dir.path().join("other").join("memory.db");

        let lock = scoped_daemon_lock_path(dir.path(), &global);
        let pid = scoped_daemon_pid_path(dir.path(), &global);
        let other_lock = scoped_daemon_lock_path(dir.path(), &other);

        assert_eq!(lock.parent(), Some(dir.path()));
        assert_eq!(pid.parent(), Some(dir.path()));
        assert_ne!(lock, other_lock);
        assert!(lock
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("daemon-"));
        assert!(lock
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".lock"));
        assert!(pid.file_name().unwrap().to_string_lossy().ends_with(".pid"));
    }

    #[test]
    fn double_acquire_in_same_process_is_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("daemon.pid");
        let _first = DaemonLock::acquire(&path).expect("first acquire");
        let second = DaemonLock::acquire(&path);
        match second {
            Err(DaemonLockError::AlreadyRunning { pid }) => {
                assert_eq!(
                    pid as u32,
                    std::process::id(),
                    "owner pid should match self"
                );
            }
            other => panic!(
                "expected AlreadyRunning, got {}",
                match other {
                    Ok(_) => "Ok(_)".to_string(),
                    Err(e) => format!("Err({e})"),
                }
            ),
        }
    }

    #[test]
    fn stale_pid_file_with_dead_pid_is_taken_over() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("daemon.pid");
        // Write a PID very unlikely to be alive (>= 1<<22 is well past the
        // typical pid_max on macOS/Linux defaults).
        std::fs::write(&path, "4194300\n").unwrap();
        // No flock is held on the file (we never opened it via DaemonLock),
        // so acquire should succeed and overwrite the PID.
        let lock = DaemonLock::acquire(&path).expect("stale takeover must succeed");
        let recorded = read_pid_file(&path).unwrap();
        assert_eq!(recorded as u32, std::process::id());
        drop(lock);
    }

    #[test]
    fn acquire_releases_lock_on_drop() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("daemon.pid");
        {
            let _lock = DaemonLock::acquire(&path).expect("first");
        }
        // After drop, a fresh acquire must succeed.
        let _again = DaemonLock::acquire(&path).expect("re-acquire after drop");
    }

    #[test]
    fn dropping_owner_keeps_waiters_on_the_stable_lock_inode() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("daemon.lock");

        let owner_a = DaemonLock::acquire(&path).expect("owner A acquires the lock path");
        // Open this descriptor while A still owns the path's inode. B waits
        // until after A drops before taking its flock on that exact inode.
        let old_inode = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("B opens A's lock inode before A drops");

        drop(owner_a);
        assert!(matches!(
            try_flock_exclusive(&old_inode),
            Ok(FlockOutcome::Acquired)
        ));

        match DaemonLock::acquire(&path) {
            Err(DaemonLockError::AlreadyRunning { .. }) => {}
            Ok(owner_c) => {
                drop(owner_c);
                panic!("C acquired a newly-created lock path while B still held the old inode");
            }
            Err(error) => panic!("C must be blocked by B's flock, got {error}"),
        }
    }

    #[test]
    fn process_alive_self_is_true() {
        assert!(process_alive(std::process::id() as i32));
    }

    #[test]
    fn process_alive_pid_one_is_false() {
        // pid 1 is conventionally init; we explicitly reject pid<=1 to avoid
        // false positives from EPERM.
        assert!(!process_alive(1));
    }

    #[test]
    fn dual_lock_acquires_both_when_free() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global").join("memory.db");
        std::fs::create_dir_all(global.parent().unwrap()).unwrap();
        std::fs::write(&global, b"db").unwrap();

        let dual = DualDaemonLock::acquire(dir.path(), &global).expect("both locks free");
        let scoped_path = scoped_daemon_lock_path(dir.path(), &global);
        let legacy_path = legacy_daemon_lock_path(dir.path());
        assert!(scoped_path.exists(), "scoped lock file must be created");
        assert!(legacy_path.exists(), "legacy lock file must be created");
        drop(dual);

        // Dropping releases both flocks: a fresh acquire on each path
        // (independently, mimicking a live daemon under either scheme)
        // must succeed again.
        let _scoped_again = DaemonLock::acquire(&scoped_path).expect("scoped released on drop");
        let _legacy_again = DaemonLock::acquire(&legacy_path).expect("legacy released on drop");
    }

    #[test]
    fn dual_lock_scoped_busy_reports_scoped_running() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global").join("memory.db");
        std::fs::create_dir_all(global.parent().unwrap()).unwrap();
        std::fs::write(&global, b"db").unwrap();
        let scoped_path = scoped_daemon_lock_path(dir.path(), &global);
        let _holder = DaemonLock::acquire(&scoped_path).expect("pre-acquire scoped lock");

        let err = DualDaemonLock::acquire(dir.path(), &global)
            .err()
            .expect("scoped lock busy must refuse");
        match err {
            DualLockError::ScopedRunning { pid } => {
                assert_eq!(pid as u32, std::process::id());
            }
            other => panic!("expected ScopedRunning, got {other}"),
        }
    }

    #[test]
    fn dual_lock_legacy_busy_reports_legacy_running_and_releases_scoped() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global").join("memory.db");
        std::fs::create_dir_all(global.parent().unwrap()).unwrap();
        std::fs::write(&global, b"db").unwrap();
        let legacy_path = legacy_daemon_lock_path(dir.path());
        let _holder = DaemonLock::acquire(&legacy_path).expect("pre-acquire legacy lock");

        let err = DualDaemonLock::acquire(dir.path(), &global)
            .err()
            .expect("legacy lock busy must refuse");
        match err {
            DualLockError::LegacyRunning { pid } => {
                assert_eq!(pid as u32, std::process::id());
            }
            other => panic!("expected LegacyRunning, got {other}"),
        }

        // The scoped lock must not remain held after the legacy conflict —
        // no partial acquisition may survive a failed DualDaemonLock::acquire.
        let scoped_path = scoped_daemon_lock_path(dir.path(), &global);
        let _scoped_free = DaemonLock::acquire(&scoped_path)
            .expect("scoped lock must have been released after legacy conflict");
    }

    #[test]
    fn scoped_only_lock_acquires_and_releases_on_drop() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global").join("memory.db");
        std::fs::create_dir_all(global.parent().unwrap()).unwrap();
        std::fs::write(&global, b"db").unwrap();

        let lock = ScopedDaemonLock::acquire(dir.path(), &global).expect("scope free");
        let scoped_path = scoped_daemon_lock_path(dir.path(), &global);
        assert!(scoped_path.exists());
        drop(lock);

        let _again = DaemonLock::acquire(&scoped_path).expect("released on drop");
    }

    #[test]
    fn scoped_only_lock_busy_reports_running() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global").join("memory.db");
        std::fs::create_dir_all(global.parent().unwrap()).unwrap();
        std::fs::write(&global, b"db").unwrap();
        let scoped_path = scoped_daemon_lock_path(dir.path(), &global);
        let _holder = DaemonLock::acquire(&scoped_path).expect("pre-acquire");

        let err = ScopedDaemonLock::acquire(dir.path(), &global)
            .err()
            .expect("busy scope must refuse");
        match err {
            ScopedLockError::Running { pid } => assert_eq!(pid as u32, std::process::id()),
            other => panic!("expected Running, got {other}"),
        }
    }

    #[test]
    fn scoped_lock_succeeds_for_different_db_while_outer_dual_lock_holds_legacy() {
        // Regression for the exact self-deadlock `ScopedDaemonLock` exists to
        // avoid: while an outer `DualDaemonLock` (e.g. `tidy --execute`'s
        // target-scope lock, held for the whole run in
        // `bootstrap/tidy/command.rs`) holds BOTH scoped(target) and legacy,
        // acquiring a scoped-only lock for a DIFFERENT DB's scope must
        // succeed — a second `DualDaemonLock::acquire` here would hit the
        // same legacy-lock file via a fresh fd and incorrectly report
        // `LegacyRunning { pid: self }` (flock is per-fd, not per-process).
        let dir = tempdir().unwrap();
        let target_db = dir.path().join("target").join("memory.db");
        let source_db = dir.path().join("source").join("memory.db");
        std::fs::create_dir_all(target_db.parent().unwrap()).unwrap();
        std::fs::create_dir_all(source_db.parent().unwrap()).unwrap();
        std::fs::write(&target_db, b"t").unwrap();
        std::fs::write(&source_db, b"s").unwrap();

        let _outer = DualDaemonLock::acquire(dir.path(), &target_db)
            .expect("outer lock on target scope must succeed");

        let scoped_source = ScopedDaemonLock::acquire(dir.path(), &source_db).expect(
            "scoped-only acquire for a DIFFERENT scope must succeed even while \
             the outer DualDaemonLock holds the shared legacy lock",
        );
        drop(scoped_source);
    }
}
