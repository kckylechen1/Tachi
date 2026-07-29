use serde_json::Value;

pub(super) const MAX_APPEND_ONLY_JSONL_LINE_BYTES: usize = 256 * 1024;

/// Atomically replace a file by writing a synced same-directory temp file first.
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
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

pub(crate) fn write_json_file_owner_only(
    path: &std::path::Path,
    value: &Value,
) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(value).map_err(|e| format!("serialize json: {e}"))?;
    write_owner_only_file_atomic(path, &body)
}

pub(crate) fn write_run_status_file(
    run_dir: &std::path::Path,
    status: &Value,
) -> Result<(), String> {
    write_json_file_owner_only(&run_dir.join("status.json"), status)
}

#[cfg(test)]
pub(crate) fn read_to_string_allow_missing(
    path: &std::path::Path,
    label: &str,
) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(Some(raw)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "{label} read failed"
            );
            Err(format!("read {label} {}: {err}", path.display()))
        }
    }
}

pub(crate) fn append_run_event(run_dir: &std::path::Path, event: Value) -> Result<(), String> {
    let line = serde_json::to_string(&event).map_err(|e| format!("serialize event: {e}"))?;
    append_owner_only_jsonl_line(&run_dir.join("events.jsonl"), &line)
}

pub(crate) fn sync_parent_dir(path: &std::path::Path) -> Result<(), String> {
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

pub(crate) fn append_owner_only_jsonl_line(
    path: &std::path::Path,
    line: &str,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create parent dir: {e}"))?;
    }
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    if !line.ends_with('\n') {
        bytes.push(b'\n');
    }
    if bytes.len() > MAX_APPEND_ONLY_JSONL_LINE_BYTES {
        return Err(format!(
            "append-only JSONL line {} exceeds {} byte cap",
            path.display(),
            MAX_APPEND_ONLY_JSONL_LINE_BYTES
        ));
    }

    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("open {}: {e}", path.display()))?
    };
    #[cfg(not(unix))]
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;

    use std::io::Write;
    file.write_all(&bytes)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    file.sync_all()
        .map_err(|e| format!("fsync {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}
