use std::fs;
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
mod unix;

pub(super) fn is_high_risk_credential_target(path: &Path) -> bool {
    let raw = path.to_string_lossy();
    raw.ends_with("/.claude.json")
        || raw.contains("/.claude/")
        || raw.contains("/.claude-code-router/")
}

struct RunTarget {
    root: PathBuf,
    relative: PathBuf,
}

impl RunTarget {
    fn path(&self) -> PathBuf {
        self.root.join(&self.relative)
    }
}

// This check also serves dry-run planning. The writer independently anchors all
// operations to a directory fd and never follows a descendant symlink.
fn resolve_run_target(target: &Path, run_dir: Option<&Path>) -> Result<RunTarget, String> {
    let run_dir = run_dir.ok_or_else(|| {
        "config_overlay requires a caller-granted run_dir; persistent file targets are retired"
            .to_string()
    })?;
    if !cfg!(unix) {
        return Err("config_overlay file output requires Unix directory-fd confinement; use env or config_content_env on this platform".to_string());
    }
    if target
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("config_overlay target must not contain '..'".to_string());
    }
    let root = fs::canonicalize(run_dir)
        .map_err(|err| format!("resolve credential run_dir '{}': {err}", run_dir.display()))?;
    if !root.is_dir() {
        return Err("credential run_dir must be an existing directory".to_string());
    }
    let granted_path = if run_dir.is_absolute() {
        run_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| format!("resolve current directory: {err}"))?
            .join(run_dir)
    };
    let relative = if target.is_absolute() {
        target
            .strip_prefix(&granted_path)
            .or_else(|_| target.strip_prefix(&root))
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
    Ok(RunTarget { root, relative })
}

pub(super) fn checked_run_target(target: &Path, run_dir: Option<&Path>) -> Result<PathBuf, String> {
    resolve_run_target(target, run_dir).map(|resolved| resolved.path())
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

pub(super) fn write_run_file(
    target: &Path,
    run_dir: Option<&Path>,
    value: &str,
    chmod: Option<&str>,
    allow_existing: bool,
) -> Result<(), String> {
    let resolved = resolve_run_target(target, run_dir)?;
    let mode = credential_file_mode(chmod)?;
    #[cfg(unix)]
    {
        unix::write_run_file(&resolved, value, mode, allow_existing)
    }
    #[cfg(not(unix))]
    {
        let _ = (resolved, value, mode, allow_existing);
        Err("config_overlay file output is unavailable on this platform".to_string())
    }
}
