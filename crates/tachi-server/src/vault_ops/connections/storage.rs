use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::{
    validate_catalog, validate_connection, Connection, ConnectionCatalog, MAX_CATALOG_BYTES,
};

/// Include the DB filename so two databases in one directory stay isolated.
pub(crate) fn catalog_path(global_db: &Path) -> PathBuf {
    let db = global_db
        .canonicalize()
        .unwrap_or_else(|_| global_db.to_path_buf());
    let mut name = db.file_name().unwrap_or_default().to_os_string();
    name.push(".connections.json");
    db.with_file_name(name)
}

fn owner_file_options(write: bool) -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true);
    if write {
        options.write(true).create(true).truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .mode(0o600);
    }
    options
}

fn check_file(file: &File) -> Result<(), String> {
    let metadata = file
        .metadata()
        .map_err(|_| "Cannot inspect connection catalog".to_string())?;
    if !metadata.is_file() {
        return Err("Connection catalog must be a regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no arguments or memory safety preconditions.
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            return Err("Connection catalog must be owned by the current user and not writable by other users".into());
        }
    }
    Ok(())
}

pub(crate) fn load_catalog(global_db: &Path) -> Result<ConnectionCatalog, String> {
    let path = catalog_path(global_db);
    // Also reject symlinks on platforms without O_NOFOLLOW.
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err("Connection catalog must not be a symlink".into())
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err("Connection catalog must be a regular file".into())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ConnectionCatalog::default())
        }
        Err(_) => return Err("Cannot inspect connection catalog".into()),
        _ => {}
    }
    let file = owner_file_options(false)
        .open(&path)
        .map_err(|_| "Cannot read connection catalog".to_string())?;
    check_file(&file)?;
    let mut bytes = Vec::new();
    file.take(MAX_CATALOG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read connection catalog".to_string())?;
    if bytes.len() > MAX_CATALOG_BYTES as usize {
        return Err("Connection catalog is too large".into());
    }
    let catalog = serde_json::from_slice(&bytes).map_err(|error: serde_json::Error| {
        format!(
            "Invalid connection catalog at line {}, column {}",
            error.line(),
            error.column()
        )
    })?;
    validate_catalog(&catalog)?;
    Ok(catalog)
}

fn update_catalog(
    global_db: &Path,
    update: impl FnOnce(&mut ConnectionCatalog) -> Result<(), String>,
) -> Result<ConnectionCatalog, String> {
    let path = catalog_path(global_db);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|_| "Cannot create connection catalog directory".to_string())?;
    }
    let mut lock_name = path.as_os_str().to_os_string();
    lock_name.push(".lock");
    let lock_path = PathBuf::from(lock_name);
    if std::fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err("Connection catalog lock must not be a symlink".into());
    }
    let lock = owner_file_options(true)
        .open(lock_path)
        .map_err(|_| "Cannot open connection catalog lock".to_string())?;
    check_file(&lock)?;
    lock.lock()
        .map_err(|_| "Cannot lock connection catalog".to_string())?;
    let mut catalog = load_catalog(global_db)?;
    update(&mut catalog)?;
    catalog.revision = catalog
        .revision
        .checked_add(1)
        .ok_or_else(|| "Connection catalog revision exhausted".to_string())?;
    catalog
        .connections
        .sort_by(|left, right| left.name.cmp(&right.name));
    validate_catalog(&catalog)?;
    let bytes = serde_json::to_vec_pretty(&catalog)
        .map_err(|_| "Cannot encode connection catalog".to_string())?;
    if bytes.len() > MAX_CATALOG_BYTES as usize {
        return Err("Connection catalog is too large".into());
    }
    crate::utils::write_owner_only_file_atomic(&path, &bytes)
        .map_err(|_| "Cannot write connection catalog".to_string())?;
    // Keep the stable lock inode: removing it would allow concurrent writers
    // to lock different files while replacing the same catalog.
    Ok(catalog)
}

pub(crate) fn put_connection(
    global_db: &Path,
    connection: Connection,
    replace: bool,
) -> Result<ConnectionCatalog, String> {
    validate_connection(&connection)?;
    update_catalog(global_db, move |catalog| {
        if let Some(existing) = catalog
            .connections
            .iter_mut()
            .find(|entry| entry.name == connection.name)
        {
            if !replace {
                return Err("Connection already exists; explicit replacement is required".into());
            }
            *existing = connection;
        } else {
            catalog.connections.push(connection);
        }
        Ok(())
    })
}

pub(crate) fn remove_connection(global_db: &Path, name: &str) -> Result<ConnectionCatalog, String> {
    update_catalog(global_db, |catalog| {
        let index = catalog
            .connections
            .iter()
            .position(|entry| entry.name == name)
            .ok_or_else(|| "Connection is not registered".to_string())?;
        catalog.connections.remove(index);
        Ok(())
    })
}
