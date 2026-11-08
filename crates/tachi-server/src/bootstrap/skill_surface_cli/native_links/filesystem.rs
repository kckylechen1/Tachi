use serde::{Deserialize, Serialize};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::STORE_NAME;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct LinkIdentity {
    volume: u64,
    file: String,
    // Rename preserves mtime. It distinguishes newly created Unix symlinks even
    // if the filesystem immediately reuses the deleted link's inode number.
    modified: String,
}

pub(super) struct Store {
    pub root: PathBuf,
    pub path: PathBuf,
    _lock: File,
}

impl Store {
    pub fn open(root: PathBuf, create: bool) -> Result<Option<Self>, String> {
        Self::open_inner(root, create, false)
    }

    pub fn open_read_only(root: PathBuf) -> Result<Option<Self>, String> {
        Self::open_inner(root, false, true)
    }

    fn open_inner(root: PathBuf, create: bool, read_only: bool) -> Result<Option<Self>, String> {
        let path = root.join(STORE_NAME);
        if !present(&path)? {
            if !create {
                return Ok(None);
            }
            match create_private_directory(&path) {
                Ok(()) => (),
                Err(_) if present(&path)? => (),
                Err(error) => return Err(error),
            }
        }
        check_directory(&path)?;
        if create {
            crate::utils::sync_parent_dir(&path)?;
        }
        let lock_path = path.join(".lock");
        let mut options = file_options();
        options
            .read(true)
            .write(!read_only)
            .create(create)
            .truncate(false);
        let lock = options
            .open(&lock_path)
            .map_err(|e| format!("open link lock {}: {e}", lock_path.display()))?;
        check_file(&lock, &lock_path)?;
        let result = if read_only {
            lock.try_lock_shared()
        } else {
            lock.try_lock()
        };
        result.map_err(|e| {
            format!(
                "native link directory is busy or cannot be locked ({}): {e}",
                root.display()
            )
        })?;
        Ok(Some(Self {
            root,
            path,
            _lock: lock,
        }))
    }
}

pub(super) fn target_directory(path: &Path) -> Result<PathBuf, String> {
    if !cfg!(any(target_os = "linux", target_os = "macos", windows)) {
        return Err(
            "native Skill directory links currently support Linux, macOS, and Windows only".into(),
        );
    }
    let root = fs::canonicalize(path).map_err(|e| {
        format!(
            "target directory must already exist ({}): {e}",
            path.display()
        )
    })?;
    if root.to_str().is_none() {
        return Err(
            "target directory path must be UTF-8 so it can be recorded in the ownership receipt"
                .into(),
        );
    }
    check_directory(&root)?;
    Ok(root)
}

pub(super) fn present(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("inspect {}: {e}", path.display())),
    }
}

pub(super) fn create_private_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = fs::DirBuilder::new();
    builder
        .create(path)
        .map_err(|e| format!("create link metadata {}: {e}", path.display()))
}

fn check_owner(metadata: &Metadata, path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions or memory arguments.
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            return Err(format!(
                "{} must be owned by the current user and not writable by group or others",
                path.display()
            ));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(format!(
                "link metadata must not be a Windows reparse point: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

pub(super) fn check_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("inspect directory {}: {e}", path.display()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "expected an ordinary directory at {}",
            path.display()
        ));
    }
    check_owner(&metadata, path)
}

fn file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
}

fn check_file(file: &File, path: &Path) -> Result<(), String> {
    let metadata = file
        .metadata()
        .map_err(|e| format!("inspect {}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("expected an ordinary file at {}", path.display()));
    }
    check_owner(&metadata, path)
}

pub(super) fn read_receipt(path: &Path) -> Result<Vec<u8>, String> {
    const LIMIT: u64 = 64 * 1024;
    let file = file_options()
        .read(true)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    check_file(&file, path)?;
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    if bytes.len() as u64 > LIMIT {
        return Err(format!(
            "ownership receipt exceeds {LIMIT} bytes: {}",
            path.display()
        ));
    }
    Ok(bytes)
}

pub(super) fn identity(path: &Path) -> Result<LinkIdentity, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata =
            fs::symlink_metadata(path).map_err(|e| format!("identify {}: {e}", path.display()))?;
        Ok(LinkIdentity {
            volume: metadata.dev(),
            file: metadata.ino().to_string(),
            modified: format!("{}:{}", metadata.mtime(), metadata.mtime_nsec()),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FileIdInfo, GetFileInformationByHandleEx, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
        };
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(|e| format!("open link identity {}: {e}", path.display()))?;
        let mut info = FILE_ID_INFO::default();
        // SAFETY: the handle remains open and the output buffer has the size
        // required for FileIdInfo. OPEN_REPARSE_POINT identifies the link itself.
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                (&mut info as *mut FILE_ID_INFO).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        } == 0
        {
            return Err(format!(
                "filesystem cannot identify directory link {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        if info.FileId.Identifier == [0; 16] {
            return Err("filesystem did not provide a stable directory link identity".into());
        }
        Ok(LinkIdentity {
            volume: info.VolumeSerialNumber,
            file: info
                .FileId
                .Identifier
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            modified: file
                .metadata()
                .map_err(|e| e.to_string())?
                .last_write_time()
                .to_string(),
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err("native directory link identity is unsupported on this platform".into())
    }
}

pub(super) fn create_directory_link(source: &Path, target: &Path) -> Result<(), String> {
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(source, target);
    #[cfg(windows)]
    let result = std::os::windows::fs::symlink_dir(source, target);
    #[cfg(not(any(unix, windows)))]
    let result: std::io::Result<()> = Err(std::io::Error::other("directory symlinks unsupported"));
    result.map_err(|e| {
        let message = format!("create directory link {} -> {}: {e}", target.display(), source.display());
        #[cfg(windows)]
        let message = format!("{message}. Windows requires Developer Mode or permission to create symbolic links; no copy fallback is used");
        message
    })
}

pub(super) fn remove_directory_link(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    let result = fs::remove_dir(path); // Removes the directory symlink, never its contents.
    #[cfg(not(windows))]
    let result = fs::remove_file(path);
    result.map_err(|e| format!("remove managed link {}: {e}", path.display()))
}

pub(super) fn move_exclusive(from: &Path, to: &Path) -> Result<(), String> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let result = crate::utils::atomic_noreplace(from, to);
    #[cfg(windows)]
    let result = {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
        let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: both paths are NUL-terminated. No REPLACE_EXISTING or COPY_ALLOWED
        // flags: a competing destination is preserved, and links are never copied.
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    let result: std::io::Result<()> = Err(std::io::Error::other(
        "exclusive directory link rename unsupported",
    ));
    result.map_err(|e| {
        format!(
            "move {} to {} without replacement: {e}",
            from.display(),
            to.display()
        )
    })
}
