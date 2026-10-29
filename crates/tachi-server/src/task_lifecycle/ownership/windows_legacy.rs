//! Windows compatibility for unsealed legacy flows, never ownership authority.
//! Directory handles prevent rename; reparse entries are refused at open.
//! Child reads retain their own handles. Writes require the mutation-file
//! lock and recheck the marker; directory sharing does not lock child data.
use super::UNAVAILABLE;
use crate::utils::read_reserved_field;
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const BACKUP_SEMANTICS: u32 = 0x0200_0000;
const REPARSE_POINT: u32 = 0x400;
const SHARE_READ: u32 = 1;
const SHARE_WRITE: u32 = 2;
const LIMIT: usize = 1024 * 1024;

struct Directory {
    path: PathBuf,
    handles: Vec<File>,
}

impl Directory {
    fn open(path: &Path, create: bool) -> Result<Option<Self>, String> {
        let parent = path.parent().ok_or(UNAVAILABLE)?;
        let name = path.file_name().ok_or(UNAVAILABLE)?;
        if create {
            // The parent is the configured legacy runs root, not a project grant.
            std::fs::create_dir_all(parent).map_err(|_| UNAVAILABLE)?;
        }
        let parent = match parent.canonicalize() {
            Ok(parent) => parent,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(UNAVAILABLE.into()),
        };
        let mut opened = Self {
            path: PathBuf::new(),
            handles: Vec::new(),
        };
        for component in parent.components() {
            match component {
                Component::Prefix(_) => opened.path.push(component),
                Component::RootDir | Component::Normal(_) => {
                    opened.path.push(component);
                    opened.pin_current(false)?.ok_or(UNAVAILABLE)?;
                }
                _ => return Err(UNAVAILABLE.into()),
            }
        }
        opened.path.push(name);
        if opened.pin_current(create)?.is_none() {
            return Ok(None);
        }
        Ok(Some(opened))
    }

    fn pin_current(&mut self, create: bool) -> Result<Option<()>, String> {
        if create {
            match std::fs::create_dir(&self.path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(UNAVAILABLE.into()),
            }
        }
        let file = match OpenOptions::new()
            .access_mode(0)
            .share_mode(SHARE_READ)
            .custom_flags(BACKUP_SEMANTICS | OPEN_REPARSE_POINT)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(UNAVAILABLE.into()),
        };
        let metadata = file.metadata().map_err(|_| UNAVAILABLE)?;
        if !metadata.is_dir() || metadata.file_attributes() & REPARSE_POINT != 0 {
            return Err(UNAVAILABLE.into());
        }
        self.handles.push(file);
        Ok(Some(()))
    }

    fn status(&self) -> Result<Option<File>, String> {
        let mut file = match open_regular(&self.path.join("status.json"), false)? {
            Some(file) => file,
            None => return Ok(None),
        };
        if read_reserved_field::<serde::de::IgnoredAny>(&mut file, "ownership", LIMIT)?.is_some() {
            return Err(UNAVAILABLE.into());
        }
        Ok(Some(file))
    }

    fn fence(&self) -> Result<File, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .share_mode(SHARE_READ | SHARE_WRITE)
            .custom_flags(OPEN_REPARSE_POINT)
            .open(self.path.join(".status-mutation.lock"))
            .map_err(|_| UNAVAILABLE)?;
        regular(&file)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    #[cfg(test)]
                    crate::managed_run_control::test_hooks::run_status_io_hook(
                        crate::managed_run_control::test_hooks::StatusIoHookStage::LockWouldBlock,
                        &self.path,
                    );
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => return Err(UNAVAILABLE.into()),
            }
        }
    }
}

fn regular(file: &File) -> Result<(), String> {
    let metadata = file.metadata().map_err(|_| UNAVAILABLE)?;
    if !metadata.is_file() || metadata.file_attributes() & REPARSE_POINT != 0 {
        return Err(UNAVAILABLE.into());
    }
    Ok(())
}

fn open_regular(path: &Path, append: bool) -> Result<Option<File>, String> {
    let mut options = OpenOptions::new();
    options
        .read(!append)
        .append(append)
        .create(append)
        .share_mode(SHARE_READ)
        .custom_flags(OPEN_REPARSE_POINT);
    match options.open(path) {
        Ok(file) => {
            regular(&file)?;
            Ok(Some(file))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(UNAVAILABLE.into()),
    }
}

fn read_bytes(file: File) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    file.take(LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UNAVAILABLE)?;
    if bytes.len() > LIMIT {
        return Err(UNAVAILABLE.into());
    }
    Ok(bytes)
}

fn decode(file: File) -> Result<Value, String> {
    serde_json::from_slice(&read_bytes(file)?).map_err(|_| UNAVAILABLE.into())
}

pub(super) fn seal(run_dir: &Path) -> Result<Option<super::FlowSeal>, String> {
    let Some(directory) = Directory::open(run_dir, false)? else {
        return Ok(None);
    };
    let Some(mut status) = open_regular(&directory.path.join("status.json"), false)? else {
        return Ok(None);
    };
    read_reserved_field(&mut status, "ownership", LIMIT)
}

pub(super) fn preflight(run_dir: &Path) -> Result<(), String> {
    if let Some(directory) = Directory::open(run_dir, false)? {
        directory.status()?;
    }
    Ok(())
}

fn read_component_bytes(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let Some(directory) = Directory::open(path.parent().ok_or(UNAVAILABLE)?, false)? else {
        return Ok(None);
    };
    let status = directory.status()?;
    if path.file_name().is_some_and(|name| name == "status.json") {
        return status.map(read_bytes).transpose();
    }
    let file = open_regular(
        &directory.path.join(path.file_name().ok_or(UNAVAILABLE)?),
        false,
    )?;
    if status.is_none() && file.is_some() {
        return Err(UNAVAILABLE.into());
    }
    file.map(read_bytes).transpose()
}

pub(super) fn read_json(path: &Path) -> Result<Option<Value>, String> {
    read_component_bytes(path)?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(|_| UNAVAILABLE.into()))
        .transpose()
}

pub(super) fn read_events(run_dir: &Path) -> Result<Option<Vec<u8>>, String> {
    read_component_bytes(&run_dir.join("events.jsonl"))
}

pub(super) fn read_verification(run_dir: &Path) -> Result<Option<Value>, String> {
    let Some(directory) = Directory::open(run_dir, false)? else {
        return Ok(None);
    };
    let _status = directory.status()?;
    open_regular(&directory.path.join("verification.json"), false)?
        .map(decode)
        .transpose()
}

pub(super) fn write(flow: &Path, path: &Path, bytes: &[u8], append: bool) -> Result<(), String> {
    let relative = path.strip_prefix(flow).map_err(|_| UNAVAILABLE)?;
    let components = relative.components().collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(UNAVAILABLE.into());
    }
    let mut directory = Directory::open(flow, true)?.ok_or(UNAVAILABLE)?;
    // Refuse even before creating the mutation lock on a sealed flow.
    directory.status()?;
    let _fence = directory.fence()?;
    let status = directory.status()?;
    for component in &components[..components.len() - 1] {
        directory.path.push(component.as_os_str());
        directory.pin_current(true)?.ok_or(UNAVAILABLE)?;
    }
    directory
        .path
        .push(components.last().ok_or(UNAVAILABLE)?.as_os_str());
    if relative == Path::new("status.json") {
        if append
            || serde_json::from_slice::<Value>(bytes)
                .map_err(|_| UNAVAILABLE)?
                .get("ownership")
                .is_some()
        {
            return Err(UNAVAILABLE.into());
        }
        // Cooperating writers remain excluded by the mandatory fence while
        // the old status handle closes for the existing atomic replacement.
        drop(status);
    }
    if append {
        if bytes.len() > 256 * 1024 {
            return Err(UNAVAILABLE.into());
        }
        let mut file = open_regular(&directory.path, true)?.ok_or(UNAVAILABLE)?;
        file.write_all(bytes).map_err(|_| UNAVAILABLE)?;
        file.sync_all().map_err(|_| UNAVAILABLE)?;
        Ok(())
    } else {
        crate::utils::write_owner_only_file_atomic(&directory.path, bytes)
    }
    #[test]
    fn optional_absence_never_bypasses_marker_or_events_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("flow_events");
        std::fs::create_dir(&dir).unwrap();
        assert_eq!(read_events(&dir).unwrap(), None);
        let event = b"{\"kind\":\"legacy\"}\n";
        std::fs::write(dir.join("events.jsonl"), event).unwrap();
        assert!(read_events(&dir).is_err());
        std::fs::write(dir.join("status.json"), "{}").unwrap();
        assert_eq!(
            read_events(&dir).unwrap().as_deref(),
            Some(event.as_slice())
        );
        std::fs::remove_file(dir.join("events.jsonl")).unwrap();
        for status in [
            r#"{"ownership":null}"#,
            r#"{"ownership":{},"ownership":{}}"#,
            r#"{"ownership":{"version":1,"claim_id":"c","canonical_project":"alpha","flow_id":"flow_events"}}"#,
            "{",
        ] {
            std::fs::write(dir.join("status.json"), status).unwrap();
            assert!(
                read_events(&dir).is_err(),
                "absent events must not bypass marker denial"
            );
        }
    }
}

#[cfg(test)]
mod tests;
