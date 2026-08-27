//! Descriptor-anchored filesystem operations for same-UID hostile paths.
//!
//! Every component is opened relative to an already-open directory with
//! `O_NOFOLLOW`. Exact-file removal first atomically moves the directory entry
//! to a fresh sibling name, then inspects and removes that captured object.

#[cfg(unix)]
mod imp {
    use std::ffi::{CString, OsStr};
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Component, Path};

    pub struct AnchoredDirectory {
        fd: OwnedFd,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CreateFileOutcome {
        Created,
        Exists,
    }

    impl AnchoredDirectory {
        pub fn open_absolute(path: &Path) -> io::Result<Self> {
            if !path.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "anchored directory path must be absolute: {}",
                        path.display()
                    ),
                ));
            }
            let slash = CString::new("/").expect("slash has no interior NUL");
            // SAFETY: slash is a valid absolute C path and no create mode is consumed.
            let root = unsafe {
                libc::open(
                    slash.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            };
            if root < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: root is a fresh descriptor owned by this function.
            let mut current = unsafe { OwnedFd::from_raw_fd(root) };
            for component in path.components() {
                match component {
                    Component::RootDir => {}
                    Component::Normal(name) => current = open_directory_at(&current, name)?,
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("unsupported anchored path component in {}", path.display()),
                        ));
                    }
                }
            }
            Ok(Self { fd: current })
        }

        pub fn open_or_create_directory(&self, name: &OsStr) -> io::Result<Self> {
            match open_directory_at(&self.fd, name) {
                Ok(fd) => Ok(Self { fd }),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let name = c_name(name)?;
                    // SAFETY: the parent descriptor is live and name is one owned component.
                    let created =
                        unsafe { libc::mkdirat(self.fd.as_raw_fd(), name.as_ptr(), 0o700) };
                    if created != 0 {
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::AlreadyExists {
                            return Err(error);
                        }
                    }
                    open_directory_at_c(&self.fd, &name).map(|fd| Self { fd })
                }
                Err(error) => Err(error),
            }
        }

        pub fn open_directory(&self, name: &OsStr) -> io::Result<Self> {
            open_directory_at(&self.fd, name).map(|fd| Self { fd })
        }

        pub fn create_file_exclusive(
            &self,
            name: &OsStr,
            contents: &[u8],
        ) -> io::Result<CreateFileOutcome> {
            let name = c_name(name)?;
            let temporary =
                CString::new(format!(".tachi-write-{}", uuid::Uuid::new_v4().as_simple()))
                    .expect("generated write name has no interior NUL");
            // SAFETY: the directory descriptor is live, name is one component, and
            // O_CREAT consumes the supplied mode.
            let fd = unsafe {
                libc::openat(
                    self.fd.as_raw_fd(),
                    temporary.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600 as libc::c_uint,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: fd is fresh and exclusively owned.
            let mut file = unsafe { File::from_raw_fd(fd) };
            let write_result = file.write_all(contents).and_then(|()| file.sync_all());
            drop(file);
            if let Err(error) = write_result {
                let _ = self.unlink(&temporary);
                return Err(error);
            }
            match rename_noreplace_at(&self.fd, &temporary, &name) {
                Ok(()) => {
                    self.sync()?;
                    Ok(CreateFileOutcome::Created)
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    self.unlink(&temporary)?;
                    Ok(CreateFileOutcome::Exists)
                }
                Err(error) => {
                    let _ = self.unlink(&temporary);
                    Err(error)
                }
            }
        }

        pub fn remove_file_if_exact(&self, name: &OsStr, expected: &[u8]) -> io::Result<bool> {
            let name = c_name(name)?;
            let captured = CString::new(format!(
                ".tachi-rollback-{}",
                uuid::Uuid::new_v4().as_simple()
            ))
            .expect("generated rollback name has no interior NUL");
            match rename_noreplace_at(&self.fd, &name, &captured) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error),
            }

            let matches = self.captured_file_matches(&captured, expected);
            match matches {
                Ok(true) => {
                    // SAFETY: captured is one component under the anchored directory;
                    // unlinkat removes the captured entry and never follows a symlink.
                    if unsafe { libc::unlinkat(self.fd.as_raw_fd(), captured.as_ptr(), 0) } != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    self.sync()?;
                    Ok(true)
                }
                Ok(false) => {
                    rename_noreplace_at(&self.fd, &captured, &name)?;
                    self.sync()?;
                    Ok(false)
                }
                Err(inspect_error) => match rename_noreplace_at(&self.fd, &captured, &name) {
                    Ok(()) => Err(inspect_error),
                    Err(restore_error) => Err(io::Error::other(format!(
                        "inspect captured file failed ({inspect_error}); restoring its original name also failed ({restore_error}); preserved as {}",
                        captured.to_string_lossy()
                    ))),
                },
            }
        }

        fn captured_file_matches(&self, name: &CString, expected: &[u8]) -> io::Result<bool> {
            // SAFETY: name is one component under a live descriptor; O_NOFOLLOW
            // refuses a captured symlink.
            let fd = unsafe {
                libc::openat(
                    self.fd.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                let error = io::Error::last_os_error();
                if matches!(error.raw_os_error(), Some(libc::ELOOP)) {
                    return Ok(false);
                }
                return Err(error);
            }
            // SAFETY: fd is fresh and exclusively owned.
            let mut file = unsafe { File::from_raw_fd(fd) };
            if !file.metadata()?.is_file() {
                return Ok(false);
            }
            let mut observed = Vec::new();
            file.read_to_end(&mut observed)?;
            Ok(observed == expected)
        }

        fn sync(&self) -> io::Result<()> {
            // SAFETY: dup returns a fresh descriptor or -1.
            let fd = unsafe { libc::dup(self.fd.as_raw_fd()) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: fd is fresh and exclusively owned.
            File::from(unsafe { OwnedFd::from_raw_fd(fd) }).sync_all()
        }

        fn unlink(&self, name: &CString) -> io::Result<()> {
            // SAFETY: name is one component under the anchored directory and
            // unlinkat never follows its final symlink.
            if unsafe { libc::unlinkat(self.fd.as_raw_fd(), name.as_ptr(), 0) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }

    fn c_name(name: &OsStr) -> io::Result<CString> {
        if name.as_bytes().contains(&b'/') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "anchored child name must be one path component",
            ));
        }
        CString::new(name.as_bytes())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
    }

    fn open_directory_at(parent: &impl AsRawFd, name: &OsStr) -> io::Result<OwnedFd> {
        open_directory_at_c(parent, &c_name(name)?)
    }

    fn open_directory_at_c(parent: &impl AsRawFd, name: &CString) -> io::Result<OwnedFd> {
        // SAFETY: parent is a live directory descriptor and name is one component.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is fresh and exclusively owned.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    #[cfg(target_os = "macos")]
    fn rename_noreplace_at(
        directory: &impl AsRawFd,
        from: &CString,
        to: &CString,
    ) -> io::Result<()> {
        // SAFETY: both names are one component under the same live directory.
        let result = unsafe {
            libc::renameatx_np(
                directory.as_raw_fd(),
                from.as_ptr(),
                directory.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(target_os = "linux")]
    fn rename_noreplace_at(
        directory: &impl AsRawFd,
        from: &CString,
        to: &CString,
    ) -> io::Result<()> {
        // SAFETY: both names are one component under the same live directory.
        let result = unsafe {
            libc::renameat2(
                directory.as_raw_fd(),
                from.as_ptr(),
                directory.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn rename_noreplace_at(
        _directory: &impl AsRawFd,
        _from: &CString,
        _to: &CString,
    ) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace rename is unavailable on this platform",
        ))
    }
}

#[cfg(not(unix))]
mod imp {
    use std::ffi::OsStr;
    use std::io;
    use std::path::Path;

    pub struct AnchoredDirectory;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CreateFileOutcome {
        Created,
        Exists,
    }

    impl AnchoredDirectory {
        pub fn open_absolute(_path: &Path) -> io::Result<Self> {
            unsupported()
        }

        pub fn open_or_create_directory(&self, _name: &OsStr) -> io::Result<Self> {
            unsupported()
        }

        pub fn open_directory(&self, _name: &OsStr) -> io::Result<Self> {
            unsupported()
        }

        pub fn create_file_exclusive(
            &self,
            _name: &OsStr,
            _contents: &[u8],
        ) -> io::Result<CreateFileOutcome> {
            unsupported()
        }

        pub fn remove_file_if_exact(&self, _name: &OsStr, _expected: &[u8]) -> io::Result<bool> {
            unsupported()
        }
    }

    fn unsupported<T>() -> io::Result<T> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "descriptor-anchored filesystem operations are unavailable on this platform",
        ))
    }
}

pub use imp::{AnchoredDirectory, CreateFileOutcome};
