//! Handle-relative run artifacts on local Windows NTFS volumes. Every ancestor
//! stays open without delete sharing; neither reopening nor atomic replacement
//! resolves the caller's pathname after the anchor has been accepted.

use serde_json::Value;
use std::collections::HashMap;
use std::ffi::{c_void, OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Component, Path, PathBuf, Prefix};
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime};
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    NtCreateFile, FILE_COMPLETE_IF_OPLOCKED, FILE_CREATE, FILE_DIRECTORY_FILE,
    FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_IF, FILE_OPEN_REPARSE_POINT,
    FILE_SYNCHRONOUS_IO_NONALERT, FILE_WRITE_THROUGH,
};
use windows_sys::Win32::Foundation::{
    LocalFree, RtlNtStatusToDosError, ERROR_NO_TOKEN, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, UNICODE_STRING,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    CopySid, EqualSid, GetAce, GetLengthSid, GetSecurityDescriptorControl, GetTokenInformation,
    IsValidAcl, IsValidSid, TokenUser, ACCESS_ALLOWED_ACE, ACL, DACL_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PSID, SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FileDispositionInfo, FileIdInfo, FileRenameInfo, GetFileInformationByHandle,
    GetFileInformationByHandleEx, GetFileType, GetFinalPathNameByHandleW,
    GetVolumeInformationByHandleW, SetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, DELETE,
    FILE_ALL_ACCESS, FILE_ATTRIBUTE_DEVICE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO,
    FILE_RENAME_INFO_0, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
    FILE_TYPE_DISK, OPEN_EXISTING, READ_CONTROL, SYNCHRONIZE, VOLUME_NAME_DOS, VOLUME_NAME_GUID,
};
use windows_sys::Win32::System::SystemServices::{ACCESS_ALLOWED_ACE_TYPE, FILE_PERSISTENT_ACLS};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

const STATUS_LIMIT: usize = 1024 * 1024;
const REPORT_LIMIT: usize = 64 * 1024;
const FENCE_NAME: &str = ".status-mutation.lock";
const FENCE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(crate) enum AnchoredRunStatusOpenError {
    Absent,
    AliasRefused,
    InvalidPath,
    Io(String),
}

impl std::fmt::Display for AnchoredRunStatusOpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Absent => formatter.write_str("run directory is absent"),
            Self::AliasRefused => formatter.write_str("run directory alias is refused"),
            Self::InvalidPath => formatter.write_str("run directory is not a local absolute path"),
            Self::Io(detail) => formatter.write_str(detail),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct FileIdentity {
    volume: u64,
    file: [u8; 16],
}

struct DirectoryAuthority {
    // The final element is the run directory. Earlier elements pin all parents.
    directories: Vec<File>,
}

impl DirectoryAuthority {
    fn directory(&self) -> &File {
        self.directories
            .last()
            .expect("an anchor always has a root")
    }

    fn validate(&self) -> Result<(), String> {
        for directory in &self.directories {
            validate_directory(directory).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct AnchoredRunStatus {
    directory: Arc<DirectoryAuthority>,
    run_dir: PathBuf,
    lock: Arc<Mutex<()>>,
}

pub(crate) struct StatusTextRead {
    pub(crate) text: String,
    pub(crate) modified: Option<SystemTime>,
}

impl AnchoredRunStatus {
    pub(crate) fn open(run_dir: &Path) -> Result<Self, AnchoredRunStatusOpenError> {
        let absolute = if run_dir.is_absolute() {
            run_dir.to_path_buf()
        } else {
            // A drive-relative or root-relative path depends on ambient Windows
            // drive state; only ordinary relative names inherit current_dir.
            if !matches!(run_dir.components().next(), Some(Component::Normal(_))) {
                return Err(AnchoredRunStatusOpenError::InvalidPath);
            }
            std::env::current_dir()
                .map_err(map_open_error)?
                .join(run_dir)
        };
        let mut components = absolute.components();
        let drive = match components.next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
                _ => return Err(AnchoredRunStatusOpenError::InvalidPath),
            },
            _ => return Err(AnchoredRunStatusOpenError::InvalidPath),
        };
        if components.next() != Some(Component::RootDir) {
            return Err(AnchoredRunStatusOpenError::InvalidPath);
        }
        let root_path = PathBuf::from(format!(r"\\?\{}:\", char::from(drive)));
        let root = open_root_directory(&root_path).map_err(map_open_error)?;
        validate_directory(&root)?;
        validate_local_ntfs(&root).map_err(AnchoredRunStatusOpenError::Io)?;
        let mut directories = vec![root];
        for component in components {
            let name = match component {
                Component::Normal(name) => name,
                Component::CurDir => continue,
                Component::ParentDir if directories.len() > 1 => {
                    // Resolve '..' only through already validated handles. A
                    // preceding junction was refused before reaching this step.
                    drop(directories.pop());
                    continue;
                }
                _ => return Err(AnchoredRunStatusOpenError::InvalidPath),
            };
            let child = open_relative(
                directories.last().expect("root is retained"),
                name,
                FILE_READ_ATTRIBUTES | FILE_TRAVERSE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                FILE_OPEN,
                FILE_DIRECTORY_FILE,
                None,
            )
            .map_err(map_open_error)?;
            validate_directory(&child)?;
            directories.push(child);
        }
        if directories.len() == 1 {
            return Err(AnchoredRunStatusOpenError::InvalidPath);
        }
        let identity = file_identity(directories.last().expect("run directory is retained"))
            .map_err(|error| AnchoredRunStatusOpenError::Io(error.to_string()))?;
        Ok(Self {
            directory: Arc::new(DirectoryAuthority { directories }),
            run_dir: absolute,
            lock: lock_for_identity(identity),
        })
    }

    pub(crate) fn open_for_read(run_dir: &Path) -> Result<Option<Self>, String> {
        match Self::open(run_dir) {
            Ok(anchor) => Ok(Some(anchor)),
            Err(AnchoredRunStatusOpenError::Absent) => Ok(None),
            Err(error) => Err(format!("anchor status directory: {error}")),
        }
    }

    /// Windows callers use the same strict no-reparse walk for ordinary writes.
    pub(crate) fn open_ordinary(run_dir: &Path) -> Result<Self, String> {
        let mut anchor =
            Self::open(run_dir).map_err(|error| format!("anchor status directory: {error}"))?;
        // Ordinary status writers and their fault keys use the canonical DOS
        // spelling. Obtain it from the accepted handle without reopening paths.
        anchor.run_dir = directory_display_path(anchor.directory.directory())?;
        Ok(anchor)
    }

    pub(crate) fn lock(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.lock)
    }

    pub(crate) fn status_path(&self) -> PathBuf {
        self.run_dir.join("status.json")
    }

    /// Separate opens share the physical mutex, but never share an accepted
    /// registry authority merely because their reusable file IDs match.
    pub(crate) fn same_opened_directory(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.directory, &other.directory)
    }

    pub(crate) fn read_json(&self) -> Result<Option<Value>, String> {
        self.read_json_bounded(STATUS_LIMIT)
    }

    pub(crate) fn read_json_bounded(&self, limit: usize) -> Result<Option<Value>, String> {
        self.read_status_with_metadata(limit)?
            .map(|read| {
                serde_json::from_str(&read.text).map_err(|error| format!("parse status: {error}"))
            })
            .transpose()
    }

    pub(crate) fn read_status_with_metadata(
        &self,
        limit: usize,
    ) -> Result<Option<StatusTextRead>, String> {
        self.read_named_text("status.json", limit.min(STATUS_LIMIT), false)
    }

    pub(crate) fn read_mcp_binding(&self) -> Result<Option<String>, String> {
        self.read_named_text("mcp-tasks.json", STATUS_LIMIT, true)
            .map(|read| read.map(|read| read.text))
    }

    pub(crate) fn read_mcp_result_report(&self) -> Result<Option<String>, String> {
        self.read_named_text("result.md", REPORT_LIMIT, false)
            .map(|read| read.map(|read| read.text))
    }

    /// A regular report marks availability even when its content exceeds the
    /// report read cap or is not UTF-8. No report bytes are read by this probe.
    pub(crate) fn result_report_len(&self) -> Result<Option<u64>, String> {
        Ok(self
            .open_named_regular("result.md", false)?
            .map(|(_, information)| {
                (u64::from(information.nFileSizeHigh) << 32) | u64::from(information.nFileSizeLow)
            }))
    }

    fn open_named_regular(
        &self,
        name: &'static str,
        private: bool,
    ) -> Result<Option<(File, BY_HANDLE_FILE_INFORMATION)>, String> {
        self.directory.validate()?;
        let file = match open_relative(
            self.directory.directory(),
            OsStr::new(name),
            GENERIC_READ | if private { READ_CONTROL } else { 0 },
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
            None,
        ) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "open run artifact without following aliases: {error}"
                ))
            }
        };
        let information = validate_regular(&file)?;
        if private {
            validate_private(&file)?;
        }
        Ok(Some((file, information)))
    }

    fn read_named_text(
        &self,
        name: &'static str,
        limit: usize,
        private: bool,
    ) -> Result<Option<StatusTextRead>, String> {
        let Some((file, information)) = self.open_named_regular(name, private)? else {
            return Ok(None);
        };
        let length =
            (u64::from(information.nFileSizeHigh) << 32) | u64::from(information.nFileSizeLow);
        if length > limit as u64 {
            return Err(format!("run artifact exceeds named limit of {limit} bytes"));
        }
        let modified = file
            .metadata()
            .map_err(|error| format!("read run artifact metadata: {error}"))?
            .modified()
            .ok();
        let mut text = String::new();
        (&file)
            .take(limit as u64 + 1)
            .read_to_string(&mut text)
            .map_err(|error| format!("read run artifact as UTF-8: {error}"))?;
        if text.len() > limit {
            return Err(format!("run artifact exceeds named limit of {limit} bytes"));
        }
        validate_regular(&file)?;
        if private {
            validate_private(&file)?;
        }
        Ok(Some(StatusTextRead { text, modified }))
    }

    /// Call only while holding this physical directory's process mutex.
    pub(crate) fn acquire_fence(&self) -> Result<StatusMutationFence, String> {
        self.acquire_fence_until(Instant::now() + FENCE_TIMEOUT)
    }

    fn acquire_fence_until(&self, deadline: Instant) -> Result<StatusMutationFence, String> {
        self.directory.validate()?;
        let security = PrivateSecurity::current()?;
        let file = open_relative(
            self.directory.directory(),
            OsStr::new(FENCE_NAME),
            GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_OPEN_IF,
            FILE_NON_DIRECTORY_FILE | FILE_WRITE_THROUGH,
            Some(&security),
        )
        .map_err(|error| format!("open status mutation fence: {error}"))?;
        validate_private(&file)?;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err("status mutation fence timed out".into());
                    }
                    std::thread::sleep(Duration::from_millis(10).min(deadline - now));
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(format!("acquire status mutation fence: {error}"));
                }
            }
        }
        let fence = StatusMutationFence {
            file,
            directory: Arc::clone(&self.directory),
        };
        fence.validate(self)?;
        Ok(fence)
    }

    pub(crate) fn write_atomic(
        &self,
        bytes: &[u8],
        fence: &StatusMutationFence,
    ) -> Result<(), String> {
        if bytes.len() > STATUS_LIMIT {
            return Err("status exceeds the 1 MiB limit".into());
        }
        self.write_named_atomic("status.json", bytes, fence)
    }

    pub(crate) fn write_mcp_binding(
        &self,
        bytes: &[u8],
        fence: &StatusMutationFence,
    ) -> Result<(), String> {
        if bytes.len() > STATUS_LIMIT {
            return Err("MCP result binding exceeds the 1 MiB limit".into());
        }
        // An existing binding must satisfy the same admission policy as reads.
        // Replacement itself is handle-relative and never follows that entry.
        self.read_mcp_binding()?;
        self.write_named_atomic("mcp-tasks.json", bytes, fence)
    }

    fn write_named_atomic(
        &self,
        name: &'static str,
        bytes: &[u8],
        fence: &StatusMutationFence,
    ) -> Result<(), String> {
        fence.validate(self)?;
        let security = PrivateSecurity::current()?;
        let temp_name = format!("{name}.tmp.{}", uuid::Uuid::new_v4().as_simple());
        let mut file = open_relative(
            self.directory.directory(),
            OsStr::new(&temp_name),
            GENERIC_WRITE | READ_CONTROL | DELETE,
            FILE_SHARE_READ,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE | FILE_WRITE_THROUGH,
            Some(&security),
        )
        .map_err(|error| format!("create private anchored temp: {error}"))?;
        let mut renamed = false;
        let result = (|| {
            validate_private(&file)?;
            file.write_all(bytes)
                .map_err(|error| format!("write anchored temp: {error}"))?;
            file.sync_all()
                .map_err(|error| format!("flush anchored temp: {error}"))?;
            fence.validate(self)?;
            rename_relative(&file, self.directory.directory(), name)?;
            renamed = true;
            // Request NTFS write-through and flush the published file's data
            // and metadata before returning a successful receipt.
            file.sync_all().map_err(|error| {
                format!("run artifact replaced but durability flush failed: {error}")
            })
        })();
        if result.is_err() && !renamed {
            // Cleanup targets the created file handle, never a substituted name.
            mark_delete(&file);
        }
        result
    }
}

pub(crate) struct StatusMutationFence {
    file: File,
    directory: Arc<DirectoryAuthority>,
}

impl StatusMutationFence {
    fn validate(&self, anchor: &AnchoredRunStatus) -> Result<(), String> {
        if !Arc::ptr_eq(&self.directory, &anchor.directory) {
            return Err("status mutation fence belongs to a different opened directory".into());
        }
        self.directory.validate()?;
        validate_private(&self.file)?;
        let named = open_relative(
            self.directory.directory(),
            OsStr::new(FENCE_NAME),
            FILE_READ_ATTRIBUTES | READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
            None,
        )
        .map_err(|error| format!("inspect status mutation fence entry: {error}"))?;
        validate_private(&named)?;
        if file_identity(&self.file).map_err(|error| error.to_string())?
            != file_identity(&named).map_err(|error| error.to_string())?
        {
            return Err("status mutation fence entry was replaced".into());
        }
        Ok(())
    }
}

fn lock_for_identity(identity: FileIdentity) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<FileIdentity, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(&identity).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(identity, Arc::downgrade(&lock));
    lock
}

fn map_open_error(error: io::Error) -> AnchoredRunStatusOpenError {
    if error.kind() == io::ErrorKind::NotFound {
        AnchoredRunStatusOpenError::Absent
    } else {
        AnchoredRunStatusOpenError::Io(error.to_string())
    }
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut name: Vec<u16> = path.as_os_str().encode_wide().collect();
    if name.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains NUL",
        ));
    }
    name.push(0);
    Ok(name)
}

fn directory_display_path(directory: &File) -> Result<PathBuf, String> {
    // SAFETY: the retained handle is valid; a zero-size buffer requests its size.
    let required = unsafe {
        GetFinalPathNameByHandleW(directory.as_raw_handle(), null_mut(), 0, VOLUME_NAME_DOS)
    };
    if required == 0 || required > 32_768 {
        return Err("opened run directory has no bounded canonical DOS name".into());
    }
    let mut name = vec![0u16; required as usize];
    // SAFETY: the writable buffer has the capacity supplied to the API.
    let length = unsafe {
        GetFinalPathNameByHandleW(
            directory.as_raw_handle(),
            name.as_mut_ptr(),
            required,
            VOLUME_NAME_DOS,
        )
    };
    if length == 0 || length >= required {
        return Err("opened run directory canonical DOS name changed during inspection".into());
    }
    Ok(PathBuf::from(OsString::from_wide(&name[..length as usize])))
}

fn open_root_directory(path: &Path) -> io::Result<File> {
    let name = wide_path(path)?;
    // SAFETY: the string is terminated and all pointers remain valid for the call.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            FILE_READ_ATTRIBUTES | FILE_TRAVERSE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned one owned, non-inheritable file handle.
    Ok(unsafe { File::from_raw_handle(handle) })
}

fn open_relative(
    directory: &File,
    name: &OsStr,
    access: u32,
    share: u32,
    disposition: u32,
    options: u32,
    security: Option<&PrivateSecurity>,
) -> io::Result<File> {
    let mut name: Vec<u16> = name.encode_wide().collect();
    if name.is_empty()
        || name.len() > 255
        || name.iter().any(|value| matches!(*value, 0 | 47 | 58 | 92))
        || name == [46]
        || name == [46, 46]
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid fixed basename",
        ));
    }
    let length = (name.len() * size_of::<u16>()) as u16;
    let object_name = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: name.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: directory.as_raw_handle(),
        ObjectName: &object_name,
        Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        SecurityDescriptor: security.map_or(null(), |value| value.descriptor.0.cast()),
        SecurityQualityOfService: null(),
    };
    let mut handle = null_mut();
    let mut status = IO_STATUS_BLOCK::default();
    // SAFETY: this is one validated component relative to a retained handle;
    // synchronous I/O retains no pointers beyond the call. Security is installed
    // by creation itself; existing objects are never truncated by these modes.
    let result = unsafe {
        NtCreateFile(
            &mut handle,
            access | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            &attributes,
            &mut status,
            null(),
            FILE_ATTRIBUTE_NORMAL,
            share,
            disposition,
            options
                | FILE_SYNCHRONOUS_IO_NONALERT
                | FILE_OPEN_REPARSE_POINT
                | FILE_COMPLETE_IF_OPLOCKED,
            null(),
            0,
        )
    };
    if result != 0 {
        // Oplock-break-in-progress is a positive status with a handle. Refuse
        // it too, so acquiring a mutation fence never waits for an oplock owner.
        if result > 0 && !handle.is_null() && handle != INVALID_HANDLE_VALUE {
            // SAFETY: a returned non-null handle is owned by this call.
            drop(unsafe { File::from_raw_handle(handle) });
        }
        // SAFETY: converting an NTSTATUS has no pointer or lifetime preconditions.
        return Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(result) } as i32,
        ));
    }
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::other("NtCreateFile returned no file handle"));
    }
    // SAFETY: NtCreateFile returned a fresh non-inheritable handle.
    Ok(unsafe { File::from_raw_handle(handle) })
}

fn file_information(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: file is retained and information is a writable correctly sized buffer.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(information)
}

fn file_identity(file: &File) -> io::Result<FileIdentity> {
    let mut information = FILE_ID_INFO::default();
    // SAFETY: the buffer matches FileIdInfo and the handle is live.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&mut information as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if information.FileId.Identifier == [0; 16] {
        return Err(io::Error::other(
            "filesystem did not provide a stable file identity",
        ));
    }
    Ok(FileIdentity {
        volume: information.VolumeSerialNumber,
        file: information.FileId.Identifier,
    })
}

fn validate_directory(file: &File) -> Result<(), AnchoredRunStatusOpenError> {
    let information = file_information(file).map_err(map_open_error)?;
    // SAFETY: GetFileType only inspects the retained handle.
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK
        || information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || information.dwFileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DEVICE)
            != 0
    {
        return Err(AnchoredRunStatusOpenError::AliasRefused);
    }
    Ok(())
}

fn validate_local_ntfs(root: &File) -> Result<(), String> {
    let mut filesystem = [0u16; 32];
    let mut flags = 0;
    let mut volume_path = [0u16; 512];
    // SAFETY: output buffers are writable and the handle is retained. A GUID
    // volume name is unavailable for UNC shares; no network fallback is admitted.
    let local = unsafe {
        GetFinalPathNameByHandleW(
            root.as_raw_handle(),
            volume_path.as_mut_ptr(),
            volume_path.len() as u32,
            VOLUME_NAME_GUID,
        )
    };
    // SAFETY: the optional output pointers are null and the two buffers are valid.
    let volume = unsafe {
        GetVolumeInformationByHandleW(
            root.as_raw_handle(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            &mut flags,
            filesystem.as_mut_ptr(),
            filesystem.len() as u32,
        )
    };
    let filesystem_end = filesystem
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(filesystem.len());
    if local == 0
        || local as usize >= volume_path.len()
        || volume == 0
        || flags & FILE_PERSISTENT_ACLS == 0
        || String::from_utf16_lossy(&filesystem[..filesystem_end]) != "NTFS"
    {
        return Err(
            "managed run artifacts require a local NTFS volume with persistent ACLs".into(),
        );
    }
    Ok(())
}

fn validate_regular(file: &File) -> Result<BY_HANDLE_FILE_INFORMATION, String> {
    let information = file_information(file).map_err(|error| error.to_string())?;
    // SAFETY: GetFileType only inspects the retained handle.
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK
        || information.dwFileAttributes
            & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DEVICE)
            != 0
    {
        return Err("run artifact is not an admitted regular file".into());
    }
    Ok(information)
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: each non-null allocation comes from a LocalAlloc-based API.
        unsafe { LocalFree(self.0) };
    }
}

struct UserSid(Vec<usize>);

impl UserSid {
    fn pointer(&self) -> PSID {
        self.0.as_ptr().cast_mut().cast()
    }

    fn current() -> Result<Self, String> {
        let mut token: HANDLE = null_mut();
        // SAFETY: pseudo-handles are valid for the call and token is writable.
        if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_TOKEN as i32) {
                return Err(format!("open effective Windows identity: {error}"));
            }
            // SAFETY: the process pseudo-handle is valid and token is writable.
            if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
                return Err(format!(
                    "open Windows identity: {}",
                    io::Error::last_os_error()
                ));
            }
        }
        // SAFETY: the successful token open returned an owned handle.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut length = 0;
        // SAFETY: this is the documented buffer-size query.
        unsafe {
            GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut length)
        };
        if !(size_of::<TOKEN_USER>() as u32..=64 * 1024).contains(&length) {
            return Err("Windows identity returned an invalid token size".into());
        }
        let mut buffer = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
        // SAFETY: usize storage provides alignment and at least length bytes.
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            )
        } == 0
        {
            return Err(format!(
                "read Windows identity: {}",
                io::Error::last_os_error()
            ));
        }
        // SAFETY: a successful TokenUser query populated an aligned TOKEN_USER.
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        // SAFETY: the SID pointer belongs to the live token-information buffer.
        if unsafe { IsValidSid(user.User.Sid) } == 0 {
            return Err("Windows identity returned an invalid SID".into());
        }
        // SAFETY: the SID was validated above.
        let length = unsafe { GetLengthSid(user.User.Sid) };
        let mut sid = Self(vec![0usize; (length as usize).div_ceil(size_of::<usize>())]);
        // SAFETY: the destination has at least length writable bytes.
        if unsafe { CopySid(length, sid.0.as_mut_ptr().cast(), user.User.Sid) } == 0 {
            return Err(format!(
                "copy Windows identity: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(sid)
    }

    fn text(&self) -> Result<String, String> {
        let mut text = null_mut();
        // SAFETY: this SID is validated and text is a writable output pointer.
        if unsafe { ConvertSidToStringSidW(self.pointer(), &mut text) } == 0 {
            return Err(format!(
                "format Windows identity: {}",
                io::Error::last_os_error()
            ));
        }
        let allocation = LocalAllocation(text.cast());
        let mut length = 0;
        // SAFETY: ConvertSidToStringSidW returned a terminated allocated string.
        while unsafe { *text.add(length) } != 0 {
            length += 1;
        }
        // SAFETY: length covers only initialized UTF-16 before the terminator.
        let result = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
            .map_err(|error| format!("decode Windows identity: {error}"));
        drop(allocation);
        result
    }
}

struct PrivateSecurity {
    descriptor: LocalAllocation,
}

impl PrivateSecurity {
    fn current() -> Result<Self, String> {
        let owner = UserSid::current()?.text()?;
        Self::from_sddl(&format!("O:{owner}D:P(A;;FA;;;{owner})"))
    }

    fn from_sddl(text: &str) -> Result<Self, String> {
        let text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor = null_mut();
        // SAFETY: the input is terminated and descriptor is an output pointer.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(format!("build private ACL: {}", io::Error::last_os_error()));
        }
        Ok(Self {
            descriptor: LocalAllocation(descriptor),
        })
    }
}

fn validate_private(file: &File) -> Result<(), String> {
    if validate_regular(file)?.nNumberOfLinks != 1 {
        return Err("private run artifact has multiple hard links".into());
    }
    let user = UserSid::current()?;
    let mut owner = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: the file is open with READ_CONTROL and all output pointers are valid.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    let _descriptor = LocalAllocation(descriptor);
    if status != 0 || descriptor.is_null() || owner.is_null() || dacl.is_null() {
        return Err(format!("inspect private run artifact ACL: Win32 {status}"));
    }
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: every pointer lies in the security descriptor returned by Windows.
    let valid = unsafe {
        IsValidSid(owner) != 0
            && EqualSid(owner, user.pointer()) != 0
            && IsValidAcl(dacl) != 0
            && GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) != 0
            && control & SE_DACL_PROTECTED != 0
            && (*dacl).AceCount == 1
    };
    if !valid {
        return Err("private run artifact owner or protected DACL is not current-user-only".into());
    }
    let mut ace = null_mut();
    // SAFETY: a valid ACL with one entry was established above.
    if unsafe { GetAce(dacl, 0, &mut ace) } == 0 || ace.is_null() {
        return Err("private run artifact DACL has no valid owner ACE".into());
    }
    // SAFETY: a valid ACL's first ACE starts with ACE_HEADER. The type/size
    // guards precede all access to the ACCESS_ALLOWED_ACE payload and its SID.
    let admitted = unsafe {
        let header = &*ace.cast::<windows_sys::Win32::Security::ACE_HEADER>();
        if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE
            || header.AceFlags != 0
            || usize::from(header.AceSize) < offset_of!(ACCESS_ALLOWED_ACE, SidStart) + 8
        {
            false
        } else {
            let allowed = &*ace.cast::<ACCESS_ALLOWED_ACE>();
            let sid: PSID = (&allowed.SidStart as *const u32).cast_mut().cast();
            let sid_length = 8 + 4 * usize::from(*sid.cast::<u8>().add(1));
            allowed.Mask == FILE_ALL_ACCESS
                && sid_length + offset_of!(ACCESS_ALLOWED_ACE, SidStart)
                    <= usize::from(header.AceSize)
                && IsValidSid(sid) != 0
                && GetLengthSid(sid) as usize + offset_of!(ACCESS_ALLOWED_ACE, SidStart)
                    <= usize::from(header.AceSize)
                && EqualSid(sid, user.pointer()) != 0
        }
    };
    if !admitted {
        return Err("private run artifact DACL grants access beyond its current owner".into());
    }
    Ok(())
}

fn rename_relative(file: &File, directory: &File, name: &str) -> Result<(), String> {
    // Only the two fixed artifact basenames reach this helper.
    let name: Vec<u16> = name.encode_utf16().collect();
    let bytes = offset_of!(FILE_RENAME_INFO, FileName) + name.len() * size_of::<u16>();
    let mut buffer = vec![0usize; bytes.div_ceil(size_of::<usize>())];
    let information = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: usize storage is aligned, has enough bytes for the variable tail,
    // and both file/directory handles remain live for the synchronous rename.
    unsafe {
        (*information).Anonymous = FILE_RENAME_INFO_0 {
            ReplaceIfExists: true,
        };
        (*information).RootDirectory = directory.as_raw_handle();
        (*information).FileNameLength = (name.len() * size_of::<u16>()) as u32;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*information).FileName).cast::<u16>(),
            name.len(),
        );
        if SetFileInformationByHandle(
            file.as_raw_handle(),
            FileRenameInfo,
            information.cast(),
            bytes as u32,
        ) == 0
        {
            return Err(format!(
                "replace anchored run artifact: {}",
                io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

fn mark_delete(file: &File) {
    let information = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: the source file has DELETE access and this fixed-size buffer lives
    // through the call. Failure leaves only the private temp, not a target write.
    unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&information as *const FILE_DISPOSITION_INFO).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        );
    }
}

#[cfg(test)]
mod tests;
