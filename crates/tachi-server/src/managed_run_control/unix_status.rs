use serde_json::Value;
use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub(crate) enum AnchoredRunStatusOpenError {
    Absent,
    AliasRefused,
    InvalidPath,
    Io(String),
}

impl AnchoredRunStatusOpenError {
    pub(crate) fn cancellation_reason(&self) -> &'static str {
        match self {
            // Preserve the existing public reason for a run-directory symlink,
            // including an alias whose target happens to remain under runs/.
            Self::AliasRefused => "run_directory_outside_runs_root",
            Self::Io(detail) => {
                tracing::warn!(error = %detail, "managed status directory could not be anchored");
                "absent_same_daemon_handle"
            }
            Self::Absent | Self::InvalidPath => "absent_same_daemon_handle",
        }
    }
}

/// Stable Unix authority for one managed run's canonical `status.json`.
///
/// The directory descriptor, not a resolved pathname, owns every subsequent
/// status read and atomic replacement. The status mutex is keyed by the same
/// opened directory's device/inode identity, so aliases share serialization
/// while a replacement directory cannot inherit the original lock.
pub(crate) struct AnchoredRunStatus {
    directory: Arc<File>,
    run_dir: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl AnchoredRunStatus {
    pub(crate) fn open(run_dir: &Path) -> Result<Self, AnchoredRunStatusOpenError> {
        let run_name = run_dir
            .file_name()
            .ok_or(AnchoredRunStatusOpenError::InvalidPath)?;
        let parent = run_dir
            .parent()
            .ok_or(AnchoredRunStatusOpenError::InvalidPath)?;
        let canonical_parent = parent.canonicalize().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                AnchoredRunStatusOpenError::Absent
            } else {
                AnchoredRunStatusOpenError::Io(format!(
                    "resolve managed runs root {}: {error}",
                    parent.display()
                ))
            }
        })?;
        let parent_directory = open_directory_path_no_follow(&canonical_parent)?;
        let child =
            open_directory_at(&parent_directory, run_name).map_err(map_run_directory_open_error)?;
        let directory = File::from(child);
        let metadata = directory.metadata().map_err(|error| {
            AnchoredRunStatusOpenError::Io(format!(
                "inspect managed run directory {}: {error}",
                run_dir.display()
            ))
        })?;
        if !metadata.is_dir() {
            return Err(AnchoredRunStatusOpenError::AliasRefused);
        }
        let lock =
            crate::dispatch_ops::status_json_lock_for_identity(metadata.dev(), metadata.ino());
        let anchored = Self {
            directory: Arc::new(directory),
            run_dir: run_dir.to_path_buf(),
            lock,
        };
        #[cfg(test)]
        super::test_hooks::run_status_io_hook(
            super::test_hooks::StatusIoHookStage::AfterDirectoryValidation,
            &anchored.run_dir,
        );
        Ok(anchored)
    }

    pub(crate) fn lock(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.lock)
    }

    pub(crate) fn status_path(&self) -> PathBuf {
        self.run_dir.join("status.json")
    }

    pub(crate) fn read_json(&self) -> Result<Option<Value>, String> {
        let Some(mut file) = self.open_status_file()? else {
            return Ok(None);
        };
        let metadata = file.metadata().map_err(|error| {
            format!(
                "inspect opened managed status {}: {error}",
                self.status_path().display()
            )
        })?;
        if !metadata.is_file() {
            return Err(format!(
                "managed status is not a regular file: {}",
                self.status_path().display()
            ));
        }
        let mut raw = String::new();
        file.read_to_string(&mut raw).map_err(|error| {
            format!(
                "read opened managed status {}: {error}",
                self.status_path().display()
            )
        })?;
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|error| format!("parse {}: {error}", self.status_path().display()))
    }

    pub(crate) fn write_atomic(&self, bytes: &[u8]) -> Result<(), String> {
        let temp_name = CString::new(format!(
            "status.json.tmp.{}",
            uuid::Uuid::new_v4().as_simple()
        ))
        .expect("generated status temp name has no interior NUL");
        let status_name = CString::new("status.json").expect("status name has no interior NUL");
        // SAFETY: the directory descriptor is live, both names are owned
        // NUL-terminated components, and O_CREAT consumes the supplied mode.
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                temp_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(format!(
                "open anchored temp for {}: {}",
                self.status_path().display(),
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: `fd` is freshly returned by openat and exclusively owned.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = (|| {
            file.write_all(bytes).map_err(|error| {
                format!(
                    "write anchored temp for {}: {error}",
                    self.status_path().display()
                )
            })?;
            file.sync_all().map_err(|error| {
                format!(
                    "fsync anchored temp for {}: {error}",
                    self.status_path().display()
                )
            })?;
            drop(file);
            #[cfg(test)]
            super::test_hooks::run_status_io_hook(
                super::test_hooks::StatusIoHookStage::BeforeAtomicRename,
                &self.run_dir,
            );
            // SAFETY: source and destination are single components resolved
            // against the same live directory descriptor.
            let renamed = unsafe {
                libc::renameat(
                    self.directory.as_raw_fd(),
                    temp_name.as_ptr(),
                    self.directory.as_raw_fd(),
                    status_name.as_ptr(),
                )
            };
            if renamed != 0 {
                return Err(format!(
                    "rename anchored temp onto {}: {}",
                    self.status_path().display(),
                    std::io::Error::last_os_error()
                ));
            }
            self.directory.sync_all().map_err(|error| {
                format!(
                    "fsync anchored directory for {}: {error}",
                    self.status_path().display()
                )
            })
        })();
        if result.is_err() {
            // SAFETY: the temp name is a valid component under the same live
            // directory descriptor; failure cleanup does not follow links.
            unsafe {
                libc::unlinkat(self.directory.as_raw_fd(), temp_name.as_ptr(), 0);
            }
        }
        result
    }

    fn open_status_file(&self) -> Result<Option<File>, String> {
        let status_name = CString::new("status.json").expect("status name has no interior NUL");
        // SAFETY: the directory descriptor is live, the status name is an
        // owned NUL-terminated component, and no O_CREAT mode is consumed.
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                status_name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd >= 0 {
            // SAFETY: `fd` is freshly returned by openat and exclusively owned.
            return Ok(Some(unsafe { File::from_raw_fd(fd) }));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        if matches!(
            error.raw_os_error(),
            Some(libc::ELOOP) | Some(libc::ENOTDIR)
        ) {
            return Err(format!(
                "refusing symlinked managed status {}",
                self.status_path().display()
            ));
        }
        Err(format!(
            "open anchored managed status {}: {error}",
            self.status_path().display()
        ))
    }
}

fn map_run_directory_open_error(error: std::io::Error) -> AnchoredRunStatusOpenError {
    if error.kind() == std::io::ErrorKind::NotFound {
        AnchoredRunStatusOpenError::Absent
    } else if matches!(
        error.raw_os_error(),
        Some(libc::ELOOP) | Some(libc::ENOTDIR)
    ) {
        AnchoredRunStatusOpenError::AliasRefused
    } else {
        AnchoredRunStatusOpenError::Io(error.to_string())
    }
}

fn open_directory_path_no_follow(path: &Path) -> Result<OwnedFd, AnchoredRunStatusOpenError> {
    let slash = CString::new("/").expect("slash has no interior NUL");
    // SAFETY: slash is a valid NUL-terminated absolute path and no O_CREAT
    // mode is consumed.
    let root_fd = unsafe {
        libc::open(
            slash.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(AnchoredRunStatusOpenError::Io(format!(
            "open filesystem root: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: `root_fd` is freshly returned by open and exclusively owned.
    let mut directory = unsafe { OwnedFd::from_raw_fd(root_fd) };
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory =
                    open_directory_at(&directory, name).map_err(map_run_directory_open_error)?;
            }
            _ => return Err(AnchoredRunStatusOpenError::InvalidPath),
        }
    }
    Ok(directory)
}

fn open_directory_at(parent: &impl AsRawFd, name: &OsStr) -> std::io::Result<OwnedFd> {
    let name = CString::new(name.as_bytes())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    // SAFETY: `parent` is a live directory descriptor, `name` is an owned
    // NUL-terminated component, and no O_CREAT mode is consumed.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fd` is freshly returned by openat and exclusively owned.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}
