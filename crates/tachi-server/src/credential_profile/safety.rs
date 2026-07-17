use std::fs;
use std::io::{ErrorKind, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn mode_from_chmod(chmod: Option<&str>) -> Result<u32, String> {
    let raw = chmod.unwrap_or("0600");
    u32::from_str_radix(raw, 8).map_err(|e| format!("invalid chmod '{raw}': {e}"))
}

pub(super) fn is_high_risk_credential_target(path: &Path) -> bool {
    let raw = path.to_string_lossy();
    raw.ends_with("/.claude.json")
        || raw.contains("/.claude/")
        || raw.contains("/.claude-code-router/")
}

pub(super) fn ensure_safe_credential_target(path: &Path) -> Result<(), String> {
    if is_high_risk_credential_target(path) {
        return Err(format!(
            "refusing high-risk credential target '{}'; use a narrower generated credential path",
            path.display()
        ));
    }
    Ok(())
}

pub(super) fn write_file_atomic(
    target: &Path,
    value: &str,
    chmod: Option<&str>,
    allow_existing: bool,
) -> Result<(), String> {
    ensure_safe_credential_target(target)?;
    if target.exists() && !allow_existing {
        return Err(format!(
            "target '{}' already exists; rerun with allow_existing after reviewing backup policy",
            target.display()
        ));
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create '{}': {e}", parent.display()))?;
    }

    let cleanup_file = |path: &Path, label: &str| -> Result<(), String> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!("remove {label} '{}': {err}", path.display())),
        }
    };
    let cleanup_temp_and_backup = |temp: &Path, backup: Option<&Path>| {
        if let Err(error) = cleanup_file(temp, "temp credential file") {
            tracing::warn!(error = %error, path = %temp.display(), "cleanup temp credential file failed");
        }
        if let Some(backup) = backup {
            if let Err(error) = cleanup_file(backup, "temporary credential backup") {
                tracing::warn!(error = %error, path = %backup.display(), "cleanup credential backup failed");
            }
        }
    };

    let mut backup_path = None;
    if target.exists() {
        let backup = target.with_extension(format!(
            "{}.tachi-bak-{}",
            target
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("bak"),
            chrono::Utc::now().timestamp()
        ));
        fs::copy(target, &backup).map_err(|e| {
            format!(
                "backup existing target '{}' to '{}': {e}",
                target.display(),
                backup.display()
            )
        })?;
        #[cfg(unix)]
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o600)).map_err(|e| {
            if let Err(cleanup_error) = cleanup_file(&backup, "temporary credential backup") {
                tracing::warn!(
                    error = %cleanup_error,
                    path = %backup.display(),
                    "cleanup temporary credential backup after chmod failure failed"
                );
            }
            format!(
                "chmod temporary credential backup '{}': {e}",
                backup.display()
            )
        })?;
        backup_path = Some(backup);
    }

    let temp = target.with_extension(format!(
        "{}.tachi-tmp-{}",
        target
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("tmp"),
        uuid::Uuid::new_v4().as_simple()
    ));
    let write_result = (|| -> Result<(), String> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .map_err(|e| format!("create temp credential file '{}': {e}", temp.display()))?;
        file.write_all(value.as_bytes())
            .map_err(|e| format!("write temp credential file '{}': {e}", temp.display()))?;
        file.sync_all()
            .map_err(|e| format!("sync temp credential file '{}': {e}", temp.display()))?;
        Ok(())
    })();
    if let Err(err) = write_result {
        cleanup_temp_and_backup(&temp, backup_path.as_deref());
        return Err(err);
    }

    #[cfg(unix)]
    {
        let mode = match mode_from_chmod(chmod) {
            Ok(mode) => mode,
            Err(err) => {
                cleanup_temp_and_backup(&temp, backup_path.as_deref());
                return Err(err);
            }
        };
        if let Err(err) = fs::set_permissions(&temp, fs::Permissions::from_mode(mode)) {
            cleanup_temp_and_backup(&temp, backup_path.as_deref());
            return Err(format!(
                "chmod temp credential file '{}': {err}",
                temp.display()
            ));
        }
    }

    fs::rename(&temp, target).map_err(|e| {
        cleanup_temp_and_backup(&temp, backup_path.as_deref());
        format!(
            "move temp credential file '{}' to '{}': {e}",
            temp.display(),
            target.display()
        )
    })?;
    if let Some(backup) = backup_path.as_deref() {
        cleanup_file(backup, "temporary credential backup")?;
    }
    Ok(())
}
