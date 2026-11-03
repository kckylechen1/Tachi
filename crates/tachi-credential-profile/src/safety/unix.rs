use super::RunTarget;
use std::ffi::{CStr, CString, OsStr};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

fn name(value: &OsStr) -> Result<CString, String> {
    CString::new(value.as_bytes()).map_err(|_| "credential target contains NUL".to_string())
}

fn open_directory(parent: RawFd, child: &CStr) -> std::io::Result<File> {
    // SAFETY: parent is a live directory fd; child is a terminated component.
    let fd = unsafe {
        libc::openat(
            parent,
            child.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: openat returned a new owned fd.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn check_leaf(parent: RawFd, leaf: &CStr, allow_existing: bool) -> Result<(), String> {
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: parent and leaf are live; metadata points to writable stat storage.
    let result = unsafe {
        libc::fstatat(
            parent,
            leaf.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result < 0 {
        let err = std::io::Error::last_os_error();
        return if err.kind() == ErrorKind::NotFound {
            Ok(())
        } else {
            Err(format!("inspect credential file: {err}"))
        };
    }
    // SAFETY: fstatat initialized metadata on success.
    let metadata = unsafe { metadata.assume_init() };
    if metadata.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(
            "config_overlay target must be a regular file, not a symlink or directory".to_string(),
        );
    }
    if !allow_existing {
        return Err(
            "credential target already exists; replacement requires allow_existing within run_dir"
                .to_string(),
        );
    }
    Ok(())
}

pub(super) fn write_run_file(
    target: &RunTarget,
    value: &str,
    mode: u32,
    allow_existing: bool,
) -> Result<(), String> {
    let mut parent = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")
        .map_err(|err| format!("open credential run_dir: {err}"))?;
    for component in target.root.components() {
        if let std::path::Component::Normal(part) = component {
            parent = open_directory(parent.as_raw_fd(), &name(part)?)
                .map_err(|err| format!("open credential run_dir without symlinks: {err}"))?;
        }
    }
    let relative_parent = target
        .relative
        .parent()
        .expect("validated file target has parent");
    for component in relative_parent.components() {
        let child = name(component.as_os_str())?;
        let next = match open_directory(parent.as_raw_fd(), &child) {
            Ok(next) => next,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                // SAFETY: parent is a live fd; child is one validated component.
                let result = unsafe { libc::mkdirat(parent.as_raw_fd(), child.as_ptr(), 0o700) };
                if result < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.kind() != ErrorKind::AlreadyExists {
                        return Err(format!("create credential subdirectory: {err}"));
                    }
                }
                open_directory(parent.as_raw_fd(), &child).map_err(|err| {
                    format!("open credential subdirectory without symlinks: {err}")
                })?
            }
            Err(err) => {
                return Err(format!(
                    "open credential subdirectory without symlinks: {err}"
                ))
            }
        };
        parent = next;
    }
    let leaf = name(target.relative.file_name().expect("validated file name"))?;
    check_leaf(parent.as_raw_fd(), &leaf, allow_existing)?;
    let temporary =
        CString::new(format!(".tachi-tmp-{}", uuid::Uuid::new_v4().as_simple())).unwrap();
    // The variadic mode argument requires C integer promotion on macOS.
    // SAFETY: parent is a live directory fd; temporary is a new single component.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            temporary.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(format!(
            "create temporary credential file: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: openat returned a new owned fd.
    let mut file = unsafe { File::from_raw_fd(fd) };
    let result = (|| -> Result<(), String> {
        file.write_all(value.as_bytes())
            .map_err(|err| format!("write temporary credential file: {err}"))?;
        file.set_permissions(std::fs::Permissions::from_mode(mode))
            .map_err(|err| format!("chmod temporary credential file: {err}"))?;
        file.sync_all()
            .map_err(|err| format!("sync temporary credential file: {err}"))?;
        // Both publication operations are relative to the same held directory.
        // renameat replaces only the leaf entry, never a symlink's destination.
        // linkat fails atomically if a target appeared after the initial check.
        let result = if allow_existing {
            // SAFETY: both names and the owning directory fd remain live.
            unsafe {
                libc::renameat(
                    parent.as_raw_fd(),
                    temporary.as_ptr(),
                    parent.as_raw_fd(),
                    leaf.as_ptr(),
                )
            }
        } else {
            // SAFETY: both names and the owning directory fd remain live.
            unsafe {
                libc::linkat(
                    parent.as_raw_fd(),
                    temporary.as_ptr(),
                    parent.as_raw_fd(),
                    leaf.as_ptr(),
                    0,
                )
            }
        };
        if result < 0 {
            return Err(format!(
                "publish credential file: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    })();
    // On rename success the temporary name is already gone; otherwise this
    // removes its link after publication or an error, without path traversal.
    // SAFETY: parent and temporary remain live until after unlinkat.
    let removed = unsafe { libc::unlinkat(parent.as_raw_fd(), temporary.as_ptr(), 0) };
    if removed < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() != ErrorKind::NotFound && result.is_ok() {
            return Err(format!("remove temporary credential file: {err}"));
        }
    }
    result
}
