//! Create-only Claude skill publication. No legacy entry is adopted or removed.
//! Directory descriptors keep operations off symlink-substituted parents;
//! exclusive creation never overwrites an existing file, directory or link.
//! This is not a custody registry or a hostile-same-UID namespace certificate.

use std::path::{Path, PathBuf};

#[cfg(unix)]
mod unix {
    use super::*;
    use std::ffi::{CString, OsStr};
    use std::fs::{File, OpenOptions};
    use std::io::{self, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
    use std::path::Component;

    pub(super) struct Directory {
        pub(super) file: File,
        pub(super) path: PathBuf,
    }

    fn component(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path component"))
    }

    fn directory_at(parent: &File, name: &CString) -> io::Result<File> {
        // SAFETY: live parent fd and terminated name; no O_CREAT mode argument.
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
        // SAFETY: successful openat returns a fresh exclusively owned fd.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub(super) fn absolute(path: &Path) -> Result<PathBuf, String> {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(path)
        };
        if path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        {
            return Err("export root must not contain parent traversal".to_string());
        }
        Ok(path)
    }

    pub(super) fn open(path: PathBuf, create: bool) -> Result<Directory, String> {
        let mut current = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/")
            .map_err(|e| e.to_string())?;
        for part in path.components() {
            let Component::Normal(name) = part else {
                continue;
            };
            let name = component(name).map_err(|e| e.to_string())?;
            current = match directory_at(&current, &name) {
                Ok(directory) => directory,
                Err(e) if create && e.kind() == io::ErrorKind::NotFound => {
                    // SAFETY: live directory fd, terminated name and valid mode.
                    if unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) } < 0 {
                        let e = io::Error::last_os_error();
                        if e.kind() != io::ErrorKind::AlreadyExists {
                            return Err(format!("create export root {}: {e}", path.display()));
                        }
                    }
                    directory_at(&current, &name)
                        .map_err(|e| format!("refuse export root {}: {e}", path.display()))?
                }
                Err(e) => return Err(format!("refuse export root {}: {e}", path.display())),
            };
        }
        Ok(Directory {
            file: current,
            path,
        })
    }

    fn validate_binding(directory: &Directory) -> Result<(), String> {
        let current = open(directory.path.clone(), false)?;
        let before = directory.file.metadata().map_err(|e| e.to_string())?;
        let now = current.file.metadata().map_err(|e| e.to_string())?;
        if (before.dev(), before.ino()) != (now.dev(), now.ino()) {
            return Err("export root changed since it was opened; publication refused".to_string());
        }
        Ok(())
    }

    pub(super) fn entry_exists(directory: &Directory, name: &CString) -> io::Result<bool> {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: valid pointers; stat is only written, not read, by fstatat.
        if unsafe {
            libc::fstatat(
                directory.file.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            return Ok(true);
        }
        let e = io::Error::last_os_error();
        if e.kind() == io::ErrorKind::NotFound {
            Ok(false)
        } else {
            Err(e)
        }
    }

    pub(super) fn publish(
        store: &Directory,
        projection: &Directory,
        name: &str,
        content: &str,
    ) -> Result<(PathBuf, PathBuf), String> {
        validate_binding(store)?;
        validate_binding(projection)?;
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err("invalid export name".to_string());
        }
        let name_c = component(OsStr::new(name)).map_err(|e| e.to_string())?;
        if entry_exists(projection, &name_c).map_err(|e| e.to_string())? {
            return Err(format!(
                "ownership_unverified: preserve existing Claude entry {name}"
            ));
        }
        // Do not follow or adopt a pre-existing skill directory, even if it
        // points into the old Tachi store. Only this mkdir grants creation here.
        // SAFETY: valid parent fd, terminated single component and valid mode.
        if unsafe { libc::mkdirat(store.file.as_raw_fd(), name_c.as_ptr(), 0o700) } < 0 {
            return Err(format!(
                "preserve existing or unavailable export {name}: {}",
                io::Error::last_os_error()
            ));
        }
        let skill_directory = directory_at(&store.file, &name_c).map_err(|e| e.to_string())?;
        let leaf = CString::new("SKILL.md").expect("static leaf");
        // SAFETY: valid directory fd, terminated leaf, valid flags/mode. O_EXCL
        // rejects an existing file or symlink without truncating or following it.
        let fd = unsafe {
            libc::openat(
                skill_directory.as_raw_fd(),
                leaf.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        // SAFETY: openat returned a fresh owned fd.
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(content.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        let skill_dir = store.path.join(name);
        let target = CString::new(skill_dir.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        validate_binding(store)?;
        validate_binding(projection)?;
        // SAFETY: terminated paths and live directory fd. symlinkat fails on
        // an existing entry, including one arriving after entry_exists.
        if unsafe {
            libc::symlinkat(
                target.as_ptr(),
                projection.file.as_raw_fd(),
                name_c.as_ptr(),
            )
        } < 0
        {
            return Err(format!(
                "preserve conflicting Claude entry {name}; new export retained: {}",
                io::Error::last_os_error()
            ));
        }
        Ok((skill_dir.join("SKILL.md"), projection.path.join(name)))
    }
}

pub(super) struct Publisher {
    #[cfg(unix)]
    store: unix::Directory,
    #[cfg(unix)]
    projection: unix::Directory,
}

impl Publisher {
    pub(super) fn open(store: &Path, projection: &Path) -> Result<Self, String> {
        #[cfg(unix)]
        {
            let store = unix::absolute(store)?;
            let projection = unix::absolute(projection)?;
            if store == projection {
                return Err("export and Claude projection roots must be distinct".to_string());
            }
            Ok(Self {
                store: unix::open(store, true)?,
                projection: unix::open(projection, true)?,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (store, projection);
            Err("Claude export requires supported symlink-safe directory operations".to_string())
        }
    }

    pub(super) fn publish(
        &mut self,
        name: &str,
        content: &str,
    ) -> Result<(PathBuf, PathBuf), String> {
        #[cfg(unix)]
        {
            unix::publish(&self.store, &self.projection, name, content)
        }
        #[cfg(not(unix))]
        {
            let _ = (name, content);
            Err("Claude export is unavailable".to_string())
        }
    }
}

#[cfg(all(test, unix))]
#[path = "claude_export_fs_tests.rs"]
mod tests;
