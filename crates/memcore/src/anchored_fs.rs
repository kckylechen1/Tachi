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
    use std::sync::Arc;

    #[derive(Debug, Clone)]
    pub struct AnchoredDirectory {
        fd: Arc<OwnedFd>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DirectoryIdentity {
        pub device: u64,
        pub inode: u64,
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
            Ok(Self {
                fd: Arc::new(current),
            })
        }

        pub fn open_or_create_directory(&self, name: &OsStr) -> io::Result<Self> {
            match open_directory_at(self.fd.as_ref(), name) {
                Ok(fd) => Ok(Self { fd: Arc::new(fd) }),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let name = c_name(name)?;
                    // SAFETY: the parent descriptor is live and name is one owned component.
                    let created = unsafe {
                        libc::mkdirat(self.fd.as_ref().as_raw_fd(), name.as_ptr(), 0o700)
                    };
                    if created != 0 {
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::AlreadyExists {
                            return Err(error);
                        }
                    }
                    open_directory_at_c(self.fd.as_ref(), &name).map(|fd| Self { fd: Arc::new(fd) })
                }
                Err(error) => Err(error),
            }
        }

        pub fn open_directory(&self, name: &OsStr) -> io::Result<Self> {
            open_directory_at(self.fd.as_ref(), name).map(|fd| Self { fd: Arc::new(fd) })
        }

        pub fn matches_absolute_path(&self, path: &Path) -> io::Result<bool> {
            let other = Self::open_absolute(path)?;
            Ok(self.identity()? == other.identity()?)
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
                    self.fd.as_ref().as_raw_fd(),
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
            match rename_noreplace_at(self.fd.as_ref(), &temporary, &name) {
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
            match rename_noreplace_at(self.fd.as_ref(), &name, &captured) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error),
            }

            let matches = self.captured_file_matches(&captured, expected);
            match matches {
                Ok(true) => {
                    // SAFETY: captured is one component under the anchored directory;
                    // unlinkat removes the captured entry and never follows a symlink.
                    if unsafe { libc::unlinkat(self.fd.as_ref().as_raw_fd(), captured.as_ptr(), 0) }
                        != 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                    self.sync()?;
                    Ok(true)
                }
                Ok(false) => {
                    rename_noreplace_at(self.fd.as_ref(), &captured, &name)?;
                    self.sync()?;
                    Ok(false)
                }
                Err(inspect_error) => {
                    match rename_noreplace_at(self.fd.as_ref(), &captured, &name) {
                    Ok(()) => Err(inspect_error),
                    Err(restore_error) => Err(io::Error::other(format!(
                        "inspect captured file failed ({inspect_error}); restoring its original name also failed ({restore_error}); preserved as {}",
                        captured.to_string_lossy()
                    ))),
                    }
                }
            }
        }

        fn captured_file_matches(&self, name: &CString, expected: &[u8]) -> io::Result<bool> {
            // SAFETY: name is one component under a live descriptor; O_NOFOLLOW
            // refuses a captured symlink.
            let fd = unsafe {
                libc::openat(
                    self.fd.as_ref().as_raw_fd(),
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
            let fd = unsafe { libc::dup(self.fd.as_ref().as_raw_fd()) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: fd is fresh and exclusively owned.
            File::from(unsafe { OwnedFd::from_raw_fd(fd) }).sync_all()
        }

        pub fn identity(&self) -> io::Result<DirectoryIdentity> {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: stat points to writable storage and fd is live.
            if unsafe { libc::fstat(self.fd.as_ref().as_raw_fd(), stat.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: fstat succeeded and initialized stat.
            let stat = unsafe { stat.assume_init() };
            #[cfg(target_os = "linux")]
            let device = stat.st_dev;
            #[cfg(not(target_os = "linux"))]
            let device = stat.st_dev as u64;
            Ok(DirectoryIdentity {
                device,
                inode: stat.st_ino,
            })
        }

        /// Make this already-open directory the child's cwd without reopening
        /// its mutable pathname between validation and spawn.
        pub fn anchor_command_cwd(&self, command: &mut std::process::Command) -> io::Result<()> {
            use std::os::unix::process::CommandExt;

            // Avoid any pathname lookup of the managed worktree in the child;
            // pre_exec replaces this harmless stable cwd with fchdir.
            command.current_dir("/");
            let authority = self.clone();
            // SAFETY: fchdir is async-signal-safe and the captured descriptor
            // remains live in the command until the child has executed.
            unsafe {
                command.pre_exec(move || {
                    if libc::fchdir(authority.fd.as_ref().as_raw_fd()) == 0 {
                        Ok(())
                    } else {
                        Err(io::Error::last_os_error())
                    }
                });
            }
            Ok(())
        }

        fn unlink(&self, name: &CString) -> io::Result<()> {
            // SAFETY: name is one component under the anchored directory and
            // unlinkat never follows its final symlink.
            if unsafe { libc::unlinkat(self.fd.as_ref().as_raw_fd(), name.as_ptr(), 0) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }

    fn c_name(name: &OsStr) -> io::Result<CString> {
        if name.is_empty()
            || name == OsStr::new(".")
            || name == OsStr::new("..")
            || name.as_bytes().contains(&b'/')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "anchored child name must be one ordinary path component",
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

    #[derive(Debug, Clone)]
    pub struct AnchoredDirectory;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DirectoryIdentity {
        pub device: u64,
        pub inode: u64,
    }

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

        pub fn matches_absolute_path(&self, _path: &Path) -> io::Result<bool> {
            unsupported()
        }

        pub fn identity(&self) -> io::Result<DirectoryIdentity> {
            unsupported()
        }

        pub fn anchor_command_cwd(&self, _command: &mut std::process::Command) -> io::Result<()> {
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

pub use imp::{AnchoredDirectory, CreateFileOutcome, DirectoryIdentity};

#[cfg(all(test, unix))]
mod tests {
    use super::AnchoredDirectory;
    use std::ffi::OsStr;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn anchored_identity_matches_metadata_and_survives_path_replacement() {
        let root = tempfile::tempdir().unwrap();
        let managed = root.path().join("managed");
        let captured = root.path().join("captured");
        std::fs::create_dir(&managed).unwrap();
        let authority = AnchoredDirectory::open_absolute(&managed.canonicalize().unwrap()).unwrap();
        let metadata = std::fs::metadata(&managed).unwrap();
        let identity = authority.identity().unwrap();
        assert_eq!(identity.device, metadata.dev());
        assert_eq!(identity.inode, metadata.ino());

        std::fs::rename(&managed, &captured).unwrap();
        std::fs::create_dir(&managed).unwrap();
        let replacement =
            AnchoredDirectory::open_absolute(&managed.canonicalize().unwrap()).unwrap();
        let replacement_metadata = std::fs::metadata(&managed).unwrap();
        let replacement_identity = replacement.identity().unwrap();
        assert_eq!(replacement_identity.device, replacement_metadata.dev());
        assert_eq!(replacement_identity.inode, replacement_metadata.ino());
        assert_eq!(authority.identity().unwrap(), identity);
        assert_eq!(std::fs::metadata(&captured).unwrap().ino(), identity.inode);
        assert_ne!(replacement_identity, identity);
    }

    #[test]
    fn anchored_children_reject_empty_dot_dotdot_and_slash() {
        let root = tempfile::tempdir().unwrap();
        let anchored = AnchoredDirectory::open_absolute(&root.path().canonicalize().unwrap())
            .expect("anchor temp directory");
        for invalid in ["", ".", "..", "nested/name"] {
            let error = anchored
                .open_directory(OsStr::new(invalid))
                .expect_err("invalid component must be refused");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn anchored_child_cwd_survives_same_path_directory_replacement() {
        let root = tempfile::tempdir().unwrap();
        let managed = root.path().join("managed");
        let captured = root.path().join("captured");
        std::fs::create_dir(&managed).unwrap();
        let authority = AnchoredDirectory::open_absolute(&managed.canonicalize().unwrap()).unwrap();
        std::fs::rename(&managed, &captured).unwrap();
        std::fs::create_dir(&managed).unwrap();

        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "touch child-was-here"]);
        authority.anchor_command_cwd(&mut command).unwrap();
        assert!(command.status().unwrap().success());

        assert!(captured.join("child-was-here").exists());
        assert!(!managed.join("child-was-here").exists());
    }

    #[test]
    fn reanchoring_a_copied_command_preserves_the_anchored_directory() {
        let root = tempfile::tempdir().unwrap();
        let managed = root.path().join("managed");
        let captured = root.path().join("captured");
        std::fs::create_dir(&managed).unwrap();
        let authority = AnchoredDirectory::open_absolute(&managed.canonicalize().unwrap()).unwrap();

        let mut original = std::process::Command::new("/bin/true");
        authority.anchor_command_cwd(&mut original).unwrap();
        let copied_cwd = original.get_current_dir().unwrap().to_path_buf();

        std::fs::rename(&managed, &captured).unwrap();
        std::fs::create_dir(&managed).unwrap();

        // Mirrors a containment wrapper which rebuilds Command and copies only
        // the cwd. Reapplying the authority after wrapping restores the hook.
        let mut wrapped = std::process::Command::new("/bin/sh");
        wrapped
            .args(["-c", "touch copied-wrapper-was-here"])
            .current_dir(copied_cwd);
        authority.anchor_command_cwd(&mut wrapped).unwrap();
        assert!(wrapped.status().unwrap().success());
        assert!(captured.join("copied-wrapper-was-here").exists());
        assert!(!managed.join("copied-wrapper-was-here").exists());
    }
}
