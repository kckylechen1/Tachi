use super::apply::*;
use super::classify::*;
use super::plan::*;
use super::types::*;

use crate::physical_db_identity::{classify_paths, open_read_only_connection};
use memcore::db::migrations::{read_schema_version, EXPECTED_SCHEMA_VERSION};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

pub(crate) struct PreviewConnection {
    pub(crate) connection: Option<rusqlite::Connection>,
    pub(crate) staging_dir: Option<PathBuf>,
}

impl Deref for PreviewConnection {
    type Target = rusqlite::Connection;

    fn deref(&self) -> &Self::Target {
        self.connection
            .as_ref()
            .expect("preview connection is live while borrowed")
    }
}

impl Drop for PreviewConnection {
    fn drop(&mut self) {
        self.connection.take();
        if let Some(staging_dir) = self.staging_dir.take() {
            let _ = std::fs::remove_dir_all(staging_dir);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreviewFileFingerprint {
    pub(crate) length: u64,
    pub(crate) modified: Option<std::time::SystemTime>,
}

pub(crate) fn preview_file_fingerprint(
    path: &Path,
) -> Result<Option<PreviewFileFingerprint>, String> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(Some(PreviewFileFingerprint {
            length: metadata.len(),
            modified: metadata.modified().ok(),
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "cannot stat preview source {}: {error}",
            path.display()
        )),
    }
}

pub(crate) fn preview_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

pub(crate) fn preview_source_fingerprints(
    path: &Path,
) -> Result<[Option<PreviewFileFingerprint>; 3], String> {
    Ok([
        preview_file_fingerprint(path)?,
        preview_file_fingerprint(&preview_sidecar_path(path, "-wal"))?,
        preview_file_fingerprint(&preview_sidecar_path(path, "-shm"))?,
    ])
}

pub(crate) fn create_private_preview_directory(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        let metadata = std::fs::symlink_metadata(path)?;
        let owner = unsafe { libc::geteuid() };
        if metadata.file_type().is_symlink()
            || !metadata.file_type().is_dir()
            || metadata.uid() != owner
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "preview staging directory is not an owner-only 0700 directory",
            ));
        }
    }
    Ok(())
}

pub(crate) fn preview_staging_dir_in(root: &Path) -> Result<PathBuf, String> {
    let process_id = std::process::id();
    for attempt in 0..32u64 {
        let sequence = PREVIEW_SNAPSHOT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!(
            "sigil-wiki-corpus-preview-{process_id}-{sequence}-{attempt}"
        ));
        match create_private_preview_directory(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "cannot create private preview staging directory {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Err("cannot allocate a private preview staging directory".to_string())
}

pub(crate) fn preview_staging_dir() -> Result<PathBuf, String> {
    preview_staging_dir_in(&std::env::temp_dir())
}

pub(crate) fn copy_preview_file(
    source: &Path,
    destination: &Path,
    required: bool,
) -> Result<(), String> {
    match std::fs::copy(source, destination) {
        Ok(_) => Ok(()),
        Err(error) if !required && error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "cannot snapshot preview source {}: {error}",
            source.display()
        )),
    }
}

pub(crate) fn open_preview_connection(source: &Path) -> Result<PreviewConnection, String> {
    let before = preview_source_fingerprints(source)?;
    if before[0].is_none() {
        return Err(format!(
            "preview source {} does not exist",
            source.display()
        ));
    }
    let staging_dir = preview_staging_dir()?;
    let staged_path = staging_dir.join("memory.db");
    let result = (|| {
        copy_preview_file(source, &staged_path, true)?;
        copy_preview_file(
            &preview_sidecar_path(source, "-wal"),
            &preview_sidecar_path(&staged_path, "-wal"),
            false,
        )?;
        copy_preview_file(
            &preview_sidecar_path(source, "-shm"),
            &preview_sidecar_path(&staged_path, "-shm"),
            false,
        )?;
        let after = preview_source_fingerprints(source)?;
        if before != after {
            return Err(format!(
                "preview source {} changed while staging an immutable snapshot",
                source.display()
            ));
        }
        let connection = open_read_only_connection(&staged_path)
            .map_err(|error| format!("cannot open immutable preview snapshot: {error}"))?;
        Ok(connection)
    })();
    match result {
        Ok(connection) => Ok(PreviewConnection {
            connection: Some(connection),
            staging_dir: Some(staging_dir),
        }),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging_dir);
            Err(error)
        }
    }
}

pub(crate) fn backup_file_name(physical_id: &str) -> String {
    format!("wiki-corpus-v1-{}.db", digest_string(physical_id))
}

pub(crate) fn quick_check(conn: &Connection) -> Result<String, String> {
    let result: String = conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|error| error.to_string())?;
    if result.eq_ignore_ascii_case("ok") {
        Ok(result)
    } else {
        Err(format!("SQLite quick_check returned '{result}'"))
    }
}

pub(crate) fn regular_non_symlink_metadata(path: &Path) -> Result<std::fs::Metadata, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        format!(
            "cannot inspect protected Wiki corpus path {}: {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(format!(
            "protected Wiki corpus path {} must be a regular non-symlink file",
            path.display()
        ));
    }
    Ok(metadata)
}

pub(crate) fn regular_directory_metadata(path: &Path) -> Result<std::fs::Metadata, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        format!(
            "cannot inspect backup directory {}: {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(format!(
            "backup directory {} must be a regular non-symlink directory",
            path.display()
        ));
    }
    Ok(metadata)
}

pub(crate) fn open_file_no_follow(
    path: &Path,
    read: bool,
    write: bool,
    create_new: bool,
) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(read).write(write).create_new(create_new);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

pub(crate) fn verify_retained_backups(backups: &[RetainedBackup]) -> Result<(), String> {
    for backup in backups {
        backup.verify()?;
    }
    Ok(())
}

pub(crate) fn maybe_replace_retained_backup_before_source_mutation(
    backups: &[RetainedBackup],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let replacement_path = match race_hook.as_ref() {
        Some(CorpusRaceHook::ReplaceRetainedBackupBeforeSourceMutation { replacement_path }) => {
            replacement_path.clone()
        }
        _ => return Ok(()),
    };
    let backup = backups
        .first()
        .ok_or_else(|| "race hook requires a retained rollback backup".to_string())?;
    race_hook.take();
    atomic_exchange_paths(&backup.path, &replacement_path).map_err(|error| {
        format!(
            "cannot inject retained backup replacement for {}: {error}",
            backup.path.display()
        )
    })
}

pub(crate) fn verify_backups_before_source_mutation(
    backups: &[RetainedBackup],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    maybe_replace_retained_backup_before_source_mutation(backups, race_hook)?;
    verify_retained_backups(backups)
}

pub(crate) fn verify_sqlite_connection_retained_identity(
    connection: &Connection,
    retained: &RetainedPathFile,
    retained_path: &Path,
) -> Result<(), String> {
    retained.verify_path(retained_path)?;
    let opened_path: String = connection
        .query_row("PRAGMA database_list", [], |row| row.get(2))
        .map_err(|error| format!("cannot resolve opened backup database path: {error}"))?;
    let opened_metadata = regular_non_symlink_metadata(Path::new(&opened_path))?;
    if retained_file_identity(&opened_metadata)? != retained.identity {
        return Err(format!(
            "opened SQLite backup handle is detached from retained object: {}",
            retained_path.display()
        ));
    }
    retained.verify_path(retained_path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArtifactRacePoint {
    Backup,
    Manifest,
}

pub(crate) fn maybe_replace_reserved_artifact(
    path: &Path,
    point: ArtifactRacePoint,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let should_replace = matches!(
        (race_hook.as_ref(), point),
        (
            Some(CorpusRaceHook::ReplaceBackupTempWithNormalFile),
            ArtifactRacePoint::Backup
        ) | (
            Some(CorpusRaceHook::ReplaceManifestTempWithNormalFile),
            ArtifactRacePoint::Manifest
        )
    );
    if !should_replace {
        return Ok(());
    }
    race_hook.take();
    std::fs::remove_file(path).map_err(|error| {
        format!(
            "cannot inject protected artifact replacement at {}: {error}",
            path.display()
        )
    })?;
    let mut replacement = open_file_no_follow(path, false, true, true).map_err(|error| {
        format!(
            "cannot inject ordinary protected artifact replacement at {}: {error}",
            path.display()
        )
    })?;
    replacement
        .write_all(b"ordinary-file-race-replacement")
        .map_err(|error| format!("cannot write race replacement: {error}"))?;
    replacement
        .sync_all()
        .map_err(|error| format!("cannot sync race replacement: {error}"))?;
    Ok(())
}

pub(crate) fn maybe_replace_existing_backup_after_retain(
    backup_path: &Path,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let replacement_path = match race_hook.as_ref() {
        Some(CorpusRaceHook::ReplaceExistingBackupAfterRetain { replacement_path }) => {
            replacement_path.clone()
        }
        _ => return Ok(()),
    };
    race_hook.take();
    atomic_exchange_paths(backup_path, &replacement_path).map_err(|error| {
        format!(
            "cannot inject existing backup replacement for {}: {error}",
            backup_path.display()
        )
    })
}

pub(crate) fn read_file_no_follow(path: &Path) -> Result<Vec<u8>, String> {
    let mut file = open_file_no_follow(path, true, false, false)
        .map_err(|error| format!("cannot read protected path {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read protected path {}: {error}", path.display()))?;
    Ok(bytes)
}

pub(crate) fn open_sqlite_no_follow(path: &Path, flags: OpenFlags) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NOFOLLOW)
}

pub(crate) fn canonical_parent_open_path(path: &Path) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("protected SQLite path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("protected SQLite path has no file name: {}", path.display()))?;
    Ok(std::fs::canonicalize(parent)
        .map_err(|error| format!("cannot canonicalize protected SQLite parent: {error}"))?
        .join(file_name))
}

/// Rename a completed private artifact into its deterministic name without
/// replacing an entry that another process may have reserved. macOS and Linux
/// provide the required kernel primitive; the hard-link fallback is also
/// exclusive and is only used on platforms without either primitive.
pub(crate) fn atomic_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let from = CString::new(from.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in source path")
        })?;
        let to = CString::new(to.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in target path")
        })?;
        let rc = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let from = CString::new(from.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in source path")
        })?;
        let to = CString::new(to.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in target path")
        })?;
        let rc = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        std::fs::hard_link(from, to)?;
        std::fs::remove_file(from)
    }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
pub(crate) fn atomic_exchange_paths(left: &Path, right: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let left = CString::new(left.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in left swap path")
    })?;
    let right = CString::new(right.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in right swap path")
    })?;
    let rc = unsafe {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            libc::renamex_np(left.as_ptr(), right.as_ptr(), libc::RENAME_SWAP)
        }
        #[cfg(target_os = "linux")]
        {
            libc::renameat2(
                libc::AT_FDCWD,
                left.as_ptr(),
                libc::AT_FDCWD,
                right.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        }
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
pub(crate) fn atomic_exchange_paths(_left: &Path, _right: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic path exchange is unavailable on this platform",
    ))
}

pub(crate) fn verify_retained_artifact_bytes(
    retained: &RetainedPathFile,
    path: &Path,
    bytes: &[u8],
) -> Result<(), String> {
    retained.verify_path(path)?;
    let installed = read_file_no_follow(path)?;
    retained.verify_path(path)?;
    if installed != bytes {
        return Err(format!(
            "protected artifact {} failed post-install byte verification",
            path.display()
        ));
    }
    Ok(())
}

pub(crate) fn verify_existing_artifact_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    regular_non_symlink_metadata(path)?;
    let retained = RetainedPathFile::open(path, true, false, false)?;
    verify_retained_artifact_bytes(&retained, path, bytes)
}

pub(crate) fn write_private_artifact_with_hook(
    path: &Path,
    bytes: &[u8],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    if std::fs::symlink_metadata(path).is_ok() {
        return verify_existing_artifact_bytes(path, bytes).map_err(|error| {
            format!(
                "protected artifact {} already exists with different or unstable content: {error}",
                path.display()
            )
        });
    }

    let temporary = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("artifact")
    ));
    let retained = match open_file_no_follow(&temporary, true, true, true) {
        Ok(file) => {
            let mut retained = RetainedPathFile::from_file(&temporary, file)?;
            retained
                .file
                .write_all(bytes)
                .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
            retained
                .file
                .sync_all()
                .map_err(|error| format!("cannot sync {}: {error}", temporary.display()))?;
            retained
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            regular_non_symlink_metadata(&temporary)?;
            let retained = RetainedPathFile::open(&temporary, true, true, false)?;
            verify_retained_artifact_bytes(&retained, &temporary, bytes).map_err(|error| {
                format!(
                    "protected artifact reservation {} contains different or unstable content: {error}",
                    temporary.display()
                )
            })?;
            retained
        }
        Err(error) => {
            return Err(format!(
                "cannot reserve protected artifact {}: {error}",
                temporary.display()
            ));
        }
    };

    maybe_replace_reserved_artifact(&temporary, ArtifactRacePoint::Manifest, race_hook)?;
    retained.verify_path(&temporary)?;

    match atomic_noreplace(&temporary, path) {
        Ok(()) => {
            retained.file.sync_all().map_err(|error| {
                format!("cannot sync installed artifact {}: {error}", path.display())
            })?;
            verify_retained_artifact_bytes(&retained, path, bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            retained.verify_path(&temporary)?;
            verify_existing_artifact_bytes(path, bytes).map_err(|error| {
                format!(
                    "protected artifact {} belongs to different or unstable content: {error}",
                    path.display()
                )
            })
        }
        Err(error) => Err(format!(
            "cannot atomically reserve protected artifact {}: {error}",
            path.display()
        )),
    }
}

pub(crate) fn verify_backup(
    source: &StoreScan,
    backup_path: &Path,
    logical_store_refs: Vec<String>,
    expected: &PlanStoreFingerprint,
) -> Result<RetainedBackup, String> {
    regular_non_symlink_metadata(backup_path)?;
    let retained = RetainedPathFile::open(backup_path, true, false, false)?;
    let receipt =
        verify_retained_backup(&retained, source, backup_path, logical_store_refs, expected)?;
    Ok(RetainedBackup {
        receipt,
        path: backup_path.to_path_buf(),
        retained,
    })
}

#[cfg(test)]
pub(crate) fn create_or_verify_backup(
    source: &StoreScan,
    backup_dir: &Path,
    logical_store_refs: Vec<String>,
    expected: &PlanStoreFingerprint,
) -> Result<BackupReceipt, String> {
    let physical_id = source
        .physical
        .as_ref()
        .map(|physical| physical.physical_id.clone())
        .unwrap_or_default();
    let mut race_hook = None;
    create_or_verify_backup_with_hook(
        source,
        backup_dir,
        &backup_file_name(&physical_id),
        logical_store_refs,
        expected,
        &mut race_hook,
    )
    .map(|backup| backup.receipt)
}

pub(crate) fn verify_retained_backup(
    retained: &RetainedPathFile,
    source: &StoreScan,
    backup_path: &Path,
    logical_store_refs: Vec<String>,
    expected: &PlanStoreFingerprint,
) -> Result<BackupReceipt, String> {
    let source_physical = source
        .physical
        .as_ref()
        .ok_or_else(|| "backup source has no physical identity".to_string())?;
    retained.verify_path(backup_path)?;
    let source_retained =
        RetainedPathFile::open(Path::new(&source_physical.open_path), true, false, false)?;
    if retained.identity == source_retained.identity {
        return Err(format!(
            "backup path {} resolves to the source physical database",
            backup_path.display()
        ));
    }
    source_retained.verify_path(Path::new(&source_physical.open_path))?;
    let backup_inventory = classify_paths([backup_path.to_path_buf()]);
    if let Some(unresolved) = backup_inventory.unresolved_paths.first() {
        return Err(format!(
            "backup path {} has no readable physical identity: {}",
            backup_path.display(),
            unresolved.error
        ));
    }
    let backup_physical = backup_inventory
        .stores
        .into_iter()
        .next()
        .ok_or_else(|| "backup has no physical identity".to_string())?;
    retained.verify_path(backup_path)?;
    let backup_open_path = canonical_parent_open_path(Path::new(&backup_physical.open_path))?;
    let conn = open_sqlite_no_follow(&backup_open_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| error.to_string())?;
    verify_sqlite_connection_retained_identity(&conn, retained, backup_path)?;
    let schema = read_schema_version(&conn).map_err(|error| error.to_string())?;
    if schema != EXPECTED_SCHEMA_VERSION {
        return Err(format!(
            "backup {} schema mismatch: stored {}, expected {}",
            backup_path.display(),
            schema,
            EXPECTED_SCHEMA_VERSION
        ));
    }
    let quick_check = quick_check(&conn)?;
    let (rows, _, vector_table_present) = load_rows(&conn)?;
    let backup_digest = row_digest(&rows, vector_table_present);
    if rows.len() != expected.total_memory_rows
        || backup_digest != expected.row_digest
        || vector_table_present != expected.vector_table_present
    {
        return Err(format!(
            "backup {} row evidence mismatch: rows={}/{} digest={}/{} vector_table={}/{}",
            backup_path.display(),
            rows.len(),
            expected.total_memory_rows,
            backup_digest,
            expected.row_digest,
            vector_table_present,
            expected.vector_table_present
        ));
    }
    verify_sqlite_connection_retained_identity(&conn, retained, backup_path)?;
    retained.verify_path(backup_path)?;
    source_retained.verify_path(Path::new(&source_physical.open_path))?;
    Ok(BackupReceipt {
        logical_store_refs,
        source_physical_id: source_physical.physical_id.clone(),
        source_canonical_path: source_physical.canonical_path.clone(),
        source_open_path: source_physical.open_path.clone(),
        backup_path: backup_path.display().to_string(),
        backup_physical_id: backup_physical.physical_id,
        schema,
        total_memory_rows: rows.len(),
        source_row_digest: expected.row_digest.clone(),
        backup_row_digest: backup_digest,
        vector_table_present,
        source_identity_verified: true,
        backup_identity_verified: true,
        quick_check,
        verified: true,
    })
}

pub(crate) fn create_or_verify_backup_with_hook(
    source: &StoreScan,
    backup_dir: &Path,
    backup_file_name: &str,
    logical_store_refs: Vec<String>,
    expected: &PlanStoreFingerprint,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<RetainedBackup, String> {
    regular_directory_metadata(backup_dir)?;
    let backup_dir = std::fs::canonicalize(backup_dir)
        .map_err(|error| format!("cannot canonicalize backup directory: {error}"))?;
    let source_physical = source
        .physical
        .as_ref()
        .ok_or_else(|| "backup source has no physical identity".to_string())?;
    let backup_path = backup_dir.join(backup_file_name);
    if std::fs::symlink_metadata(&backup_path).is_ok() {
        regular_non_symlink_metadata(&backup_path)?;
        let retained = RetainedPathFile::open(&backup_path, true, false, false)?;
        maybe_replace_existing_backup_after_retain(&backup_path, race_hook)?;
        let receipt = verify_retained_backup(
            &retained,
            source,
            &backup_path,
            logical_store_refs,
            expected,
        )?;
        return Ok(RetainedBackup {
            receipt,
            path: backup_path,
            retained,
        });
    }

    if source.row_digest != expected.row_digest
        || source.report.counts.total_memory_rows != expected.total_memory_rows
        || source.vector_table_present != expected.vector_table_present
    {
        return Err(format!(
            "cannot create original backup for {} after source state changed",
            source.spec.logical_store.reference()
        ));
    }

    let temporary = backup_path.with_extension("db.tmp");
    let retained = match std::fs::symlink_metadata(&temporary) {
        Ok(_) => {
            regular_non_symlink_metadata(&temporary)?;
            let retained = RetainedPathFile::open(&temporary, true, true, false)?;
            verify_retained_backup(
                &retained,
                source,
                &temporary,
                logical_store_refs.clone(),
                expected,
            )?;
            retained
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let reservation =
                open_file_no_follow(&temporary, true, true, true).map_err(|error| {
                    format!("cannot reserve backup {}: {error}", temporary.display())
                })?;
            let retained = RetainedPathFile::from_file(&temporary, reservation)?;
            maybe_replace_reserved_artifact(&temporary, ArtifactRacePoint::Backup, race_hook)?;
            retained.verify_path(&temporary)?;
            let source_conn = open_preview_connection(Path::new(&source_physical.open_path))
                .map_err(|error| format!("cannot open backup source: {error}"))?;
            retained.verify_path(&temporary)?;
            let temporary_open_path = canonical_parent_open_path(&temporary)?;
            let mut destination = open_sqlite_no_follow(
                &temporary_open_path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
            )
            .map_err(|error| format!("cannot create backup {}: {error}", temporary.display()))?;
            retained.verify_path(&temporary)?;
            {
                let backup = rusqlite::backup::Backup::new(&source_conn, &mut destination)
                    .map_err(|error| format!("cannot initialize SQLite backup: {error}"))?;
                backup
                    .run_to_completion(128, Duration::from_millis(100), None)
                    .map_err(|error| format!("SQLite backup failed: {error}"))?;
            }
            drop(destination);
            retained
                .file
                .sync_all()
                .map_err(|error| format!("cannot sync backup {}: {error}", temporary.display()))?;
            verify_retained_backup(
                &retained,
                source,
                &temporary,
                logical_store_refs.clone(),
                expected,
            )?;
            retained
        }
        Err(error) => {
            return Err(format!(
                "cannot inspect backup reservation {}: {error}",
                temporary.display()
            ));
        }
    };

    retained.verify_path(&temporary)?;
    match atomic_noreplace(&temporary, &backup_path) {
        Ok(()) => {
            let receipt = verify_retained_backup(
                &retained,
                source,
                &backup_path,
                logical_store_refs,
                expected,
            )?;
            Ok(RetainedBackup {
                receipt,
                path: backup_path,
                retained,
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            retained.verify_path(&temporary)?;
            verify_backup(source, &backup_path, logical_store_refs, expected)
        }
        Err(error) => Err(format!(
            "cannot atomically reserve backup {}: {error}",
            backup_path.display()
        )),
    }
}

#[cfg(test)]
pub(crate) fn write_manifest_if_needed(
    path: &Path,
    manifest: &BackupManifest,
) -> Result<(), String> {
    let mut race_hook = None;
    write_manifest_if_needed_with_hook(path, manifest, &mut race_hook)
}

pub(crate) fn write_manifest_if_needed_with_hook(
    path: &Path,
    manifest: &BackupManifest,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let value = serde_json::to_value(manifest).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())?;
    if std::fs::symlink_metadata(path).is_ok() {
        regular_non_symlink_metadata(path)?;
        let existing = read_file_no_follow(path)?;
        let existing_value: Value = serde_json::from_slice(&existing)
            .map_err(|error| format!("existing backup manifest is invalid: {error}"))?;
        if existing_value != value {
            return Err(format!(
                "deterministic backup manifest {} belongs to a different plan or evidence",
                path.display()
            ));
        }
        return Ok(());
    }
    write_private_artifact_with_hook(path, &bytes, race_hook)
}

pub(crate) fn read_manifest_if_present(path: &Path) -> Result<Option<BackupManifest>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            regular_non_symlink_metadata(path)?;
            let bytes = read_file_no_follow(path)?;
            serde_json::from_slice(&bytes).map(Some).map_err(|error| {
                format!("invalid Wiki corpus manifest {}: {error}", path.display())
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "cannot inspect Wiki corpus manifest {}: {error}",
            path.display()
        )),
    }
}

pub(crate) fn plan_item_completed(
    scans: &[StoreScan],
    plan: &WikiCorpusPlan,
    item: &PlanItem,
) -> bool {
    let Ok(source) = find_scan(scans, &item.source_store_ref) else {
        return false;
    };
    let Some(row) = source.raw_row(&item.source_id) else {
        return false;
    };
    match item.action.as_str() {
        "reclassify_in_place" => receipt_matches(row, item, &plan.plan_id, &["reclassified"]),
        "copy_to_shared_and_supersede" => {
            let Some(target_id) = item.target_id.as_deref() else {
                return false;
            };
            if !receipt_matches(row, item, &plan.plan_id, &["source_superseded"])
                || !row.is_superseded_by(target_id)
            {
                return false;
            }

            // The source receipt and immutable supersession edge are not
            // sufficient no-op evidence: the deterministic target must still
            // be present and canonical. `canonical_target_matches_plan`
            // intentionally ignores mutable enrichment while binding the
            // target receipt, copy identity, and active lifecycle.
            let Ok(target) = find_scan(scans, &item.target_store_ref) else {
                return false;
            };
            let Some(target_row) = target.raw_row(target_id) else {
                return false;
            };
            canonical_target_matches_plan(target_row, item, &plan.plan_id)
        }
        _ => false,
    }
}

pub(crate) fn existing_outcome(item: &PlanItem) -> MigrationOutcome {
    MigrationOutcome {
        source_store_ref: item.source_store_ref.clone(),
        source_id: item.source_id.clone(),
        target_store_ref: item.target_store_ref.clone(),
        target_id: item.target_id.clone(),
        action: item.action.clone(),
        outcome: "existing_no_op".to_string(),
        phases: if item.action == "reclassify_in_place" {
            vec!["reclassified".to_string()]
        } else {
            vec!["target_copied".to_string(), "source_superseded".to_string()]
        },
    }
}

pub(crate) fn retain_plan_backups(
    scans: &[StoreScan],
    plan: &WikiCorpusPlan,
    backup_dir: &Path,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<Vec<RetainedBackup>, String> {
    let mut groups = BTreeMap::<String, (Vec<String>, &StoreScan)>::new();
    for scan in scans {
        let Some(physical) = scan.physical.as_ref() else {
            continue;
        };
        let entry = groups
            .entry(physical.physical_id.clone())
            .or_insert_with(|| (Vec::new(), scan));
        entry
            .0
            .push(scan.spec.logical_store.reference().to_string());
    }

    let mut backups = Vec::new();
    for (physical_id, (mut logical_refs, source)) in groups {
        logical_refs.sort();
        logical_refs.dedup();
        check_authority(source, None)?;
        let expected = plan
            .store_fingerprints
            .iter()
            .find(|fingerprint| {
                fingerprint.logical_store_ref == source.spec.logical_store.reference()
            })
            .ok_or_else(|| {
                format!(
                    "plan has no original backup evidence for {}",
                    source.spec.logical_store.reference()
                )
            })?;
        backups.push(create_or_verify_backup_with_hook(
            source,
            backup_dir,
            &backup_file_name(&physical_id),
            logical_refs,
            expected,
            race_hook,
        )?);
    }
    backups.sort_by(|left, right| {
        left.receipt
            .source_physical_id
            .cmp(&right.receipt.source_physical_id)
    });
    verify_retained_backups(&backups)?;
    Ok(backups)
}
