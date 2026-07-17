use serde_json::Value;

pub(crate) fn write_owner_only_file(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create parent dir: {e}"))?;
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        file.write_all(bytes)
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        file.sync_all()
            .map_err(|e| format!("fsync {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    sync_parent_dir(path)?;
    Ok(())
}

pub(crate) fn write_owner_only_file_atomic(
    path: &std::path::Path,
    bytes: &[u8],
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create parent dir: {e}"))?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("tachi-file");
    let tmp_path = path.with_file_name(format!(
        "{file_name}.tmp.{}",
        uuid::Uuid::new_v4().as_simple()
    ));

    let result = (|| {
        #[cfg(unix)]
        let mut file = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp_path)
                .map_err(|e| format!("open temp {}: {e}", tmp_path.display()))?
        };
        #[cfg(not(unix))]
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| format!("open temp {}: {e}", tmp_path.display()))?;

        use std::io::Write;
        file.write_all(bytes)
            .map_err(|e| format!("write temp {}: {e}", tmp_path.display()))?;
        file.sync_all()
            .map_err(|e| format!("fsync temp {}: {e}", tmp_path.display()))?;
        drop(file);

        std::fs::rename(&tmp_path, path).map_err(|e| {
            format!(
                "rename temp {} -> {}: {e}",
                tmp_path.display(),
                path.display()
            )
        })?;
        sync_parent_dir(path)
    })();

    if result.is_err() {
        if let Err(error) = std::fs::remove_file(&tmp_path) {
            eprintln!(
                "failed to remove temporary write file {}: {error}",
                tmp_path.display()
            );
        }
    }
    result
}

pub(crate) fn write_run_status_file(
    run_dir: &std::path::Path,
    status: &Value,
) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(status).map_err(|e| format!("serialize json: {e}"))?;
    write_owner_only_file_atomic(&run_dir.join("status.json"), &body)
}

fn sync_parent_dir(path: &std::path::Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        if let Some(parent) = path.parent() {
            let dir = std::fs::File::open(parent)
                .map_err(|e| format!("open parent dir {}: {e}", parent.display()))?;
            dir.sync_all()
                .map_err(|e| format!("fsync parent dir {}: {e}", parent.display()))?;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
