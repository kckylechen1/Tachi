use std::fs;
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
pub(super) mod unix;

pub(super) fn is_high_risk_credential_target(path: &Path) -> bool {
    let raw = path.to_string_lossy();
    raw.ends_with("/.claude.json")
        || raw.contains("/.claude/")
        || raw.contains("/.claude-code-router/")
}

pub(super) fn refuse_parent_dir(path: &Path, label: &str) -> Result<(), String> {
    if path.components().any(|part| part == Component::ParentDir) {
        return Err(format!("{label} must not contain '..'"));
    }
    Ok(())
}

pub(super) fn absolute_run_dir(run_dir: &Path) -> Result<PathBuf, String> {
    refuse_parent_dir(run_dir, "credential run_dir")?;
    if run_dir.is_absolute() {
        return Ok(run_dir.to_path_buf());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(run_dir))
        .map_err(|err| format!("resolve current directory: {err}"))
}

pub(super) struct RunDirectory {
    granted: PathBuf,
    pub(super) canonical: PathBuf,
}

impl RunDirectory {
    pub(super) fn resolve(run_dir: &Path) -> Result<Self, String> {
        let granted = absolute_run_dir(run_dir)?;
        let canonical = fs::canonicalize(&granted)
            .map_err(|err| format!("resolve credential run_dir '{}': {err}", run_dir.display()))?;
        Ok(Self { granted, canonical })
    }
}

// Planning inspects the path for diagnostics. Publication anchors its own
// traversal to directory descriptors and never follows a descendant symlink.
pub(super) fn checked_run_target(
    target: &Path,
    run_dir: Result<Option<&RunDirectory>, &str>,
) -> Result<PathBuf, String> {
    refuse_parent_dir(target, "config_overlay target")?;
    if !cfg!(unix) {
        return Err("config_overlay file output requires Unix directory-fd confinement; use env or config_content_env on this platform".to_string());
    }
    let run_dir = run_dir?.ok_or_else(|| {
        "config_overlay requires a caller-granted run_dir; persistent file targets are retired"
            .to_string()
    })?;
    let root = &run_dir.canonical;
    if !root.is_dir() {
        return Err("credential run_dir must be an existing directory".to_string());
    }
    if root.to_str().is_none() {
        return Err("credential run_dir must be valid UTF-8 for file overlays".to_string());
    }
    let relative = if target.is_absolute() {
        target
            .strip_prefix(&run_dir.granted)
            .or_else(|_| target.strip_prefix(root))
            .map_err(|_| {
                "config_overlay target must be inside the caller-granted run_dir".to_string()
            })?
    } else {
        target
    };
    let relative = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(Ok(part)),
            Component::CurDir => None,
            _ => Some(Err(
                "config_overlay target must be relative to run_dir".to_string()
            )),
        })
        .collect::<Result<PathBuf, String>>()?;
    if relative.as_os_str().is_empty() {
        return Err("config_overlay target must name a file inside run_dir".to_string());
    }
    let absolute_target = root.join(&relative);
    let mut current = root.clone();
    for part in relative.components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(
                    "config_overlay target must not traverse or replace a symlink".to_string(),
                );
            }
            Ok(metadata) if current != absolute_target && !metadata.is_dir() => {
                return Err("config_overlay target parent must be a directory".to_string());
            }
            Ok(metadata) if current == absolute_target && !metadata.is_file() => {
                return Err("config_overlay target must be a regular file".to_string());
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
            Err(err) => {
                return Err(format!(
                    "inspect credential target '{}': {err}",
                    current.display()
                ))
            }
        }
    }
    Ok(absolute_target)
}

pub(super) fn credential_file_mode(chmod: Option<&str>) -> Result<u32, String> {
    let raw = chmod.unwrap_or("0600");
    let mode =
        u32::from_str_radix(raw, 8).map_err(|err| format!("invalid chmod '{raw}': {err}"))?;
    if mode & !0o600 != 0 || mode & 0o400 == 0 {
        return Err("credential config_overlay chmod must be 0400 or 0600".to_string());
    }
    Ok(mode)
}
