//! Singleton-daemon enforcement via advisory file lock + PID file.
//!
//! The Tachi daemon must not run more than one process per user/host because
//! multiple foundry workers writing to the same set of project DBs will race
//! the `processed_events` claim table and the `foundry_jobs` queue. We enforce
//! singleton-ness by `flock(LOCK_EX | LOCK_NB)` on `~/.tachi/daemon.pid`. If
//! acquisition fails because the file is locked, we read the recorded PID and
//! probe it with `kill(pid, 0)`. If the previous daemon is gone (process died
//! without releasing flock — POSIX guarantees release on close, but a crashed
//! process still leaves the file on disk) we fall through and take it over.
//!
//! Lifetime: the returned [`DaemonLock`] holds the open fd. Dropping it
//! releases the kernel lock and best-effort removes the PID file. Callers must
//! keep it alive for the daemon's full runtime.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

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
/// While alive, holds an exclusive `flock` on the PID file. Dropping releases
/// the lock and removes the PID file (best-effort).
pub struct DaemonLock {
    file: File,
    path: PathBuf,
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

        // Try non-blocking exclusive lock.
        match try_flock_exclusive(&file)? {
            FlockOutcome::Acquired => {
                write_pid(&file)?;
                Ok(DaemonLock { file, path })
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
                            Ok(DaemonLock { file, path })
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
        // Releasing the fd implicitly drops the flock. Remove the PID file
        // best-effort so a future operator inspection does not see a stale
        // PID for an exited process.
        let fd = self.file.as_raw_fd();
        // SAFETY: `fd` comes from a live `File` owned by this `DaemonLock`.
        // `flock(LOCK_UN)` does not dereference Rust memory and only requests
        // kernel unlock for that descriptor. Errors are intentionally ignored
        // during drop because the fd close also releases any held flock.
        unsafe {
            libc::flock(fd, libc::LOCK_UN);
        }
        let _ = std::fs::remove_file(&self.path);
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

fn read_pid(file: &File) -> std::io::Result<i32> {
    let mut handle = file;
    handle.seek(SeekFrom::Start(0))?;
    let mut s = String::new();
    handle.read_to_string(&mut s)?;
    Ok(s.trim().parse::<i32>().unwrap_or(0))
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
    fn acquire_creates_pid_file_with_current_pid() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("daemon.pid");
        let lock = DaemonLock::acquire(&path).expect("first acquire must succeed");
        let recorded = read_pid_file(&path).expect("pid file must be readable");
        assert_eq!(recorded as u32, std::process::id());
        drop(lock);
        assert!(
            !path.exists(),
            "PID file should be removed when DaemonLock is dropped"
        );
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
    fn process_alive_self_is_true() {
        assert!(process_alive(std::process::id() as i32));
    }

    #[test]
    fn process_alive_pid_one_is_false() {
        // pid 1 is conventionally init; we explicitly reject pid<=1 to avoid
        // false positives from EPERM.
        assert!(!process_alive(1));
    }
}
