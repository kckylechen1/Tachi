use memcore::anchored_fs::{AnchoredDirectory, DirectoryIdentity};
use std::ffi::{CStr, CString, OsStr};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};

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

fn leaf_stat(parent: RawFd, leaf: &CStr) -> std::io::Result<libc::stat> {
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: parent and leaf are live; metadata points to writable stat storage.
    if unsafe {
        libc::fstatat(
            parent,
            leaf.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fstatat initialized metadata on success.
    Ok(unsafe { metadata.assume_init() })
}

fn check_leaf(
    parent: RawFd,
    leaf: &CStr,
    allow_existing: bool,
) -> Result<Option<(u64, u64)>, String> {
    let metadata = match leaf_stat(parent, leaf) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("inspect credential file: {err}")),
    };
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
    Ok(Some(stat_identity(&metadata)))
}

#[allow(clippy::unnecessary_cast)] // dev_t differs between macOS and Linux.
fn stat_identity(stat: &libc::stat) -> (u64, u64) {
    (stat.st_dev as u64, stat.st_ino)
}

pub(crate) struct RunWriter {
    directory: File,
    pub(crate) identity: DirectoryIdentity,
}

impl RunWriter {
    pub(crate) fn open(canonical_run_dir: &Path) -> Result<Self, String> {
        let anchored = AnchoredDirectory::open_absolute(canonical_run_dir)
            .map_err(|err| format!("open credential run_dir without symlinks: {err}"))?;
        let identity = anchored
            .identity()
            .map_err(|err| format!("identify credential run_dir: {err}"))?;
        // The shared anchor does not expose its fd for renameat/linkat. Open a
        // publication fd and admit it only if it names the very same directory.
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(canonical_run_dir)
            .map_err(|err| format!("open credential run_dir for publication: {err}"))?;
        let metadata = directory
            .metadata()
            .map_err(|err| format!("identify credential publication directory: {err}"))?;
        if (metadata.dev(), metadata.ino()) != (identity.device, identity.inode) {
            return Err("credential run_dir changed before publication".into());
        }
        Ok(Self {
            directory,
            identity,
        })
    }

    pub(crate) fn existing_identity(&self, relative: &Path) -> Option<(u64, u64)> {
        let mut parent = self.directory.try_clone().ok()?;
        for component in relative.parent()?.components() {
            let Component::Normal(child) = component else {
                return None;
            };
            parent = open_directory(parent.as_raw_fd(), &name(child).ok()?).ok()?;
        }
        if relative.is_absolute() {
            return None;
        }
        let leaf = name(relative.file_name()?).ok()?;
        check_leaf(parent.as_raw_fd(), &leaf, true).ok().flatten()
    }

    pub(crate) fn stage_file(
        &self,
        relative: &Path,
        allow_existing: bool,
        previous_overlay: Option<(u64, u64)>,
    ) -> Result<PublishedRunFile, String> {
        let mut parent = self
            .directory
            .try_clone()
            .map_err(|err| format!("duplicate credential run_dir descriptor: {err}"))?;
        let relative_parent = relative.parent().expect("validated file target has parent");
        for component in relative_parent.components() {
            let child = name(component.as_os_str())?;
            let next = match open_directory(parent.as_raw_fd(), &child) {
                Ok(next) => next,
                Err(err) if err.kind() == ErrorKind::NotFound => {
                    // SAFETY: parent is a live fd; child is one validated component.
                    let result =
                        unsafe { libc::mkdirat(parent.as_raw_fd(), child.as_ptr(), 0o700) };
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
        let leaf = name(relative.file_name().expect("validated file name"))?;
        let original = previous_overlay.or(check_leaf(parent.as_raw_fd(), &leaf, allow_existing)?);
        let backup = original.map(|_| {
            CString::new(format!(".tachi-bak-{}", uuid::Uuid::new_v4().as_simple())).unwrap()
        });
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
        let file = unsafe { File::from_raw_fd(fd) };
        Ok(PublishedRunFile {
            parent,
            leaf,
            file,
            temporary,
            backup,
            original,
            backup_linked: false,
            published: false,
        })
    }
}

fn unlink(parent: &File, leaf: &CStr) -> std::io::Result<()> {
    // SAFETY: parent is live and leaf is one NUL-terminated component.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), leaf.as_ptr(), 0) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn unlink_if_present(parent: &File, leaf: &CStr) -> std::io::Result<()> {
    match unlink(parent, leaf) {
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn link(parent: &File, from: &CStr, to: &CStr) -> std::io::Result<()> {
    let fd = parent.as_raw_fd();
    // SAFETY: both names are single components in the held parent directory.
    if unsafe { libc::linkat(fd, from.as_ptr(), fd, to.as_ptr(), 0) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn rename(parent: &File, from: &CStr, to: &CStr) -> std::io::Result<()> {
    let fd = parent.as_raw_fd();
    // SAFETY: both names are single components in the held parent directory.
    if unsafe { libc::renameat(fd, from.as_ptr(), fd, to.as_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) struct PublishedRunFile {
    parent: File,
    leaf: CString,
    file: File,
    temporary: CString,
    backup: Option<CString>,
    pub(crate) original: Option<(u64, u64)>,
    backup_linked: bool,
    published: bool,
}

impl PublishedRunFile {
    pub(crate) fn identity(&self) -> std::io::Result<(u64, u64)> {
        let metadata = self.file.metadata()?;
        Ok((metadata.dev(), metadata.ino()))
    }

    pub(crate) fn temporary_name(&self) -> &OsStr {
        OsStr::from_bytes(self.temporary.to_bytes())
    }

    pub(crate) fn backup_name(&self) -> Option<&OsStr> {
        self.backup
            .as_deref()
            .map(|name| OsStr::from_bytes(name.to_bytes()))
    }

    fn matches(&self, name: &CStr) -> std::io::Result<bool> {
        match leaf_stat(self.parent.as_raw_fd(), name) {
            Ok(stat) => Ok(stat_identity(&stat) == self.identity()?),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// Verify that `canonical_run_dir` still has `expected_root` and that walking
    /// `relative.parent()` without following symlinks still reaches `self.parent`.
    fn verify_within_run_root(
        &self,
        canonical_run_dir: &Path,
        expected_root: DirectoryIdentity,
        relative: &Path,
    ) -> Result<(), String> {
        let reopened = RunWriter::open(canonical_run_dir)?;
        if reopened.identity != expected_root {
            return Err("credential run_dir changed before publication".into());
        }
        let mut current = reopened.directory;
        let relative_parent = relative.parent().expect("validated file target has parent");
        for component in relative_parent.components() {
            let Component::Normal(part) = component else {
                return Err("credential target parent is not an ordinary path".into());
            };
            let child = name(part)?;
            current = open_directory(current.as_raw_fd(), &child)
                .map_err(|err| format!("credential subdirectory moved outside run_dir: {err}"))?;
        }
        let expected = current
            .metadata()
            .map_err(|err| format!("identify credential parent directory: {err}"))?;
        let held = self
            .parent
            .metadata()
            .map_err(|err| format!("identify staged credential parent directory: {err}"))?;
        if (expected.dev(), expected.ino()) != (held.dev(), held.ino()) {
            return Err("credential subdirectory moved outside run_dir before publication".into());
        }
        Ok(())
    }

    /// Write only after SQLite has committed records for every possible name.
    pub(crate) fn write_file(
        &mut self,
        canonical_run_dir: &Path,
        expected_root: DirectoryIdentity,
        relative: &Path,
        value: &str,
        mode: u32,
    ) -> Result<(), String> {
        self.verify_within_run_root(canonical_run_dir, expected_root, relative)?;
        if check_leaf(self.parent.as_raw_fd(), &self.leaf, self.original.is_some())?
            != self.original
        {
            return Err("credential target changed before publication".into());
        }
        self.publish(value, mode)
            .map_err(|err| format!("publish credential file: {err}"))?;
        self.verify_within_run_root(canonical_run_dir, expected_root, relative)
    }

    fn publish(&mut self, value: &str, mode: u32) -> std::io::Result<()> {
        if !self.matches(&self.temporary)? {
            return Err(std::io::Error::other("temporary credential file changed"));
        }
        self.file.write_all(value.as_bytes())?;
        self.file
            .set_permissions(std::fs::Permissions::from_mode(mode))?;
        self.file.sync_all()?;

        if let Some(backup) = &self.backup {
            link(&self.parent, &self.leaf, backup)?;
            self.backup_linked = true;
            let stat = leaf_stat(self.parent.as_raw_fd(), backup)?;
            if Some(stat_identity(&stat)) != self.original {
                return Err(std::io::Error::other(
                    "credential target changed during replacement",
                ));
            }
            rename(&self.parent, &self.temporary, &self.leaf)?;
        } else {
            link(&self.parent, &self.temporary, &self.leaf)?;
        }
        self.published = true;
        unlink_if_present(&self.parent, &self.temporary)?;
        self.parent.sync_all()
    }

    pub(crate) fn commit(self) -> std::io::Result<()> {
        if let Some(backup) = &self.backup {
            // Cleanup may already have removed this tracked backup.
            match leaf_stat(self.parent.as_raw_fd(), backup) {
                Ok(stat) if Some(stat_identity(&stat)) == self.original => {
                    unlink_if_present(&self.parent, backup)?;
                }
                Err(err) if err.kind() == ErrorKind::NotFound => {}
                Err(err) => return Err(err),
                Ok(_) => return Err(std::io::Error::other("credential backup changed")),
            }
        }
        self.parent.sync_all()
    }

    pub(crate) fn undo(self) -> std::io::Result<()> {
        if self.published {
            if !self.matches(&self.leaf)? {
                return Err(std::io::Error::other("published credential file changed"));
            }
            if let Some(backup) = &self.backup {
                rename(&self.parent, backup, &self.leaf)?;
            } else {
                unlink(&self.parent, &self.leaf)?;
            }
        } else if self.backup_linked {
            if let Some(backup) = &self.backup {
                unlink_if_present(&self.parent, backup)?;
            }
        }
        if self.matches(&self.temporary)? {
            unlink_if_present(&self.parent, &self.temporary)?;
        }
        self.parent.sync_all()
    }
}

impl Drop for PublishedRunFile {
    fn drop(&mut self) {
        // Before the write-ahead commit this is an empty staging file.
        if self.matches(&self.temporary).unwrap_or(false) {
            let _ = unlink_if_present(&self.parent, &self.temporary);
        }
    }
}
