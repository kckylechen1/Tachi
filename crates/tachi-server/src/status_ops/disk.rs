//! Disk headroom block for `tachi_status` (#484 slice 2).
//!
//! Surfaces free-space pressure for the two paths that matter to the
//! managed-worktree governor: the worktrees root (many linked worktrees,
//! each a full checkout) and the shared cargo target dir (one shared build
//! cache all managed worktrees are provisioned to point at — see
//! `tools/cleaner/src/wt_open.rs`'s `provision_shared_cargo_target_config`).
//!
//! Probing is injectable (`collect_disk_status_with_probe`) so tests can
//! assert the warning-threshold logic without depending on the real
//! filesystem's free space.

use std::path::{Path, PathBuf};

/// Warn when free space on a probed volume drops below this percentage...
pub(crate) const DISK_FREE_PERCENT_WARN_THRESHOLD: f64 = 10.0;
/// ...or below this many free bytes (20 GB), whichever fires first.
pub(crate) const DISK_FREE_BYTES_WARN_THRESHOLD: u64 = 20 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct DiskVolumeStatus {
    /// Which managed path this volume backs (`worktrees_root` /
    /// `shared_target_dir`), not the mount point name.
    pub(crate) label: &'static str,
    pub(crate) path: String,
    pub(crate) free_bytes: Option<u64>,
    pub(crate) total_bytes: Option<u64>,
    pub(crate) free_percent: Option<f64>,
    /// Set when free space is below either warning threshold.
    pub(crate) warning: Option<String>,
    /// Set when the path or filesystem couldn't be resolved/probed at all
    /// (e.g. `TACHI_WORKTREES_ROOT`/`HOME` unset, or `statvfs` failed).
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct DiskStatus {
    pub(crate) worktrees_root: DiskVolumeStatus,
    pub(crate) shared_target_dir: DiskVolumeStatus,
}

/// `(free_bytes, total_bytes)` for the filesystem holding `path`, or an
/// error string. Injectable seam for tests.
pub(crate) type DiskUsageProbe = dyn Fn(&Path) -> Result<(u64, u64), String>;

/// Real collection: resolves the managed worktrees root and shared cargo
/// target dir (both may not exist on disk yet) and probes each with
/// `statvfs`.
pub(crate) fn collect_disk_status() -> DiskStatus {
    collect_disk_status_with_probe(
        tachi_clean::wt_open::default_worktrees_root(),
        tachi_clean::wt_open::default_shared_cargo_target_dir(),
        &real_disk_usage,
    )
}

pub(crate) fn collect_disk_status_with_probe(
    worktrees_root: Result<PathBuf, String>,
    shared_target_dir: Result<PathBuf, String>,
    probe: &DiskUsageProbe,
) -> DiskStatus {
    DiskStatus {
        worktrees_root: probe_volume("worktrees_root", worktrees_root, probe),
        shared_target_dir: probe_volume("shared_target_dir", shared_target_dir, probe),
    }
}

fn probe_volume(
    label: &'static str,
    path_result: Result<PathBuf, String>,
    probe: &DiskUsageProbe,
) -> DiskVolumeStatus {
    let path = match path_result {
        Ok(path) => path,
        Err(err) => {
            return DiskVolumeStatus {
                label,
                path: String::new(),
                free_bytes: None,
                total_bytes: None,
                free_percent: None,
                warning: None,
                error: Some(err),
            }
        }
    };
    let path_display = path.display().to_string();
    match probe(&path) {
        Ok((free_bytes, total_bytes)) => {
            let free_percent = if total_bytes > 0 {
                Some((free_bytes as f64 / total_bytes as f64) * 100.0)
            } else {
                None
            };
            DiskVolumeStatus {
                label,
                path: path_display,
                free_bytes: Some(free_bytes),
                total_bytes: Some(total_bytes),
                free_percent,
                warning: disk_warning(free_bytes, free_percent),
                error: None,
            }
        }
        Err(err) => DiskVolumeStatus {
            label,
            path: path_display,
            free_bytes: None,
            total_bytes: None,
            free_percent: None,
            warning: None,
            error: Some(err),
        },
    }
}

fn disk_warning(free_bytes: u64, free_percent: Option<f64>) -> Option<String> {
    let low_percent = free_percent
        .map(|p| p < DISK_FREE_PERCENT_WARN_THRESHOLD)
        .unwrap_or(false);
    let low_bytes = free_bytes < DISK_FREE_BYTES_WARN_THRESHOLD;
    if !low_percent && !low_bytes {
        return None;
    }
    let gb = free_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let percent_str = free_percent
        .map(|p| format!("{p:.1}% free"))
        .unwrap_or_else(|| "% free unknown (total size unavailable)".to_string());
    Some(format!(
        "low disk space: {gb:.1} GB free, {percent_str} (warn below {} GB or {}%)",
        DISK_FREE_BYTES_WARN_THRESHOLD / (1024 * 1024 * 1024),
        DISK_FREE_PERCENT_WARN_THRESHOLD as u64,
    ))
}

/// Nearest existing ancestor of `path` (inclusive) — the managed worktrees
/// root / shared target dir may not have been created yet, but `statvfs`
/// needs a path that exists.
fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut probe = path.to_path_buf();
    loop {
        if probe.exists() {
            return Some(probe);
        }
        if !probe.pop() {
            return None;
        }
    }
}

fn real_disk_usage(path: &Path) -> Result<(u64, u64), String> {
    use std::os::unix::ffi::OsStrExt;

    let probe_path = nearest_existing_ancestor(path)
        .ok_or_else(|| format!("no existing ancestor for {}", path.display()))?;
    let c_path = std::ffi::CString::new(probe_path.as_os_str().as_bytes())
        .map_err(|err| format!("path contains NUL byte: {err}"))?;

    // SAFETY: `stat` is a plain-old-data out-param zeroed before the call;
    // `c_path` is a valid NUL-terminated C string for the duration of the
    // call. Same unsafe-FFI shape as the other `libc::` call sites in this
    // crate (e.g. `daemon_lock.rs`, `vault_ops/session.rs`).
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if rc != 0 {
        return Err(format!(
            "statvfs({}) failed: {}",
            probe_path.display(),
            std::io::Error::last_os_error()
        ));
    }
    // `f_frsize` (fragment size) is the POSIX-correct multiplier for
    // `f_blocks`/`f_bavail`, not `f_bsize` (preferred I/O size) — they
    // coincide on most filesystems but aren't guaranteed to.
    let block_size = stat.f_frsize as u64;
    let free_bytes = (stat.f_bavail as u64).saturating_mul(block_size);
    let total_bytes = (stat.f_blocks as u64).saturating_mul(block_size);
    Ok((free_bytes, total_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_probe(free: u64, total: u64) -> impl Fn(&Path) -> Result<(u64, u64), String> {
        move |_path: &Path| Ok((free, total))
    }

    #[test]
    fn healthy_volume_has_no_warning() {
        let status = collect_disk_status_with_probe(
            Ok(PathBuf::from("/tmp/worktrees-root")),
            Ok(PathBuf::from("/tmp/shared-target")),
            &ok_probe(100 * 1024 * 1024 * 1024, 500 * 1024 * 1024 * 1024),
        );
        assert!(status.worktrees_root.warning.is_none());
        assert!(status.shared_target_dir.warning.is_none());
        assert_eq!(status.worktrees_root.free_bytes, Some(100 * 1024 * 1024 * 1024));
        assert!((status.worktrees_root.free_percent.unwrap() - 20.0).abs() < 0.01);
    }

    #[test]
    fn low_percent_triggers_warning_even_with_plenty_of_absolute_bytes() {
        // 25 GB free of 1 TB total = 2.5% free — below the 10% threshold —
        // even though 25 GB alone is above the 20 GB absolute threshold.
        let free = 25 * 1024 * 1024 * 1024_u64;
        let total = 1000 * 1024 * 1024 * 1024_u64;
        let status = collect_disk_status_with_probe(
            Ok(PathBuf::from("/tmp/worktrees-root")),
            Ok(PathBuf::from("/tmp/shared-target")),
            &ok_probe(free, total),
        );
        assert!(
            status.worktrees_root.warning.is_some(),
            "expected a low-percent warning: {:?}",
            status.worktrees_root
        );
    }

    #[test]
    fn low_absolute_bytes_triggers_warning_even_with_healthy_percent() {
        // 10 GB free of 50 GB total = 20% free — above the 10% threshold —
        // but 10 GB alone is below the 20 GB absolute threshold.
        let free = 10 * 1024 * 1024 * 1024_u64;
        let total = 50 * 1024 * 1024 * 1024_u64;
        let status = collect_disk_status_with_probe(
            Ok(PathBuf::from("/tmp/worktrees-root")),
            Ok(PathBuf::from("/tmp/shared-target")),
            &ok_probe(free, total),
        );
        assert!(
            status.worktrees_root.warning.is_some(),
            "expected a low-bytes warning: {:?}",
            status.worktrees_root
        );
    }

    #[test]
    fn unresolvable_path_surfaces_as_error_not_panic() {
        let status = collect_disk_status_with_probe(
            Err("HOME unset".to_string()),
            Ok(PathBuf::from("/tmp/shared-target")),
            &ok_probe(100 * 1024 * 1024 * 1024, 500 * 1024 * 1024 * 1024),
        );
        assert_eq!(status.worktrees_root.error.as_deref(), Some("HOME unset"));
        assert!(status.worktrees_root.free_bytes.is_none());
        assert!(status.worktrees_root.warning.is_none());
    }

    #[test]
    fn probe_failure_surfaces_as_error() {
        let status = collect_disk_status_with_probe(
            Ok(PathBuf::from("/tmp/worktrees-root")),
            Ok(PathBuf::from("/tmp/shared-target")),
            &|_path: &Path| Err("statvfs failed: boom".to_string()),
        );
        assert!(status.worktrees_root.error.is_some());
        assert!(status.shared_target_dir.error.is_some());
    }

    #[test]
    fn real_disk_usage_probes_an_existing_path() {
        // Discrimination against the real libc::statvfs call site: any
        // existing path (temp_dir always exists) must yield nonzero total
        // bytes without panicking.
        let (free, total) = real_disk_usage(&std::env::temp_dir()).expect("statvfs should work");
        assert!(total > 0, "total_bytes should be nonzero: {total}");
        assert!(free <= total, "free ({free}) should not exceed total ({total})");
    }

    #[test]
    fn real_disk_usage_walks_up_to_nearest_existing_ancestor() {
        // A not-yet-created leaf under an existing dir must still resolve
        // (statvfs probes the nearest existing ancestor).
        let missing = std::env::temp_dir().join("tachi-disk-status-does-not-exist-484");
        let _ = std::fs::remove_dir_all(&missing);
        let (_, total) = real_disk_usage(&missing).expect("should walk up to an existing ancestor");
        assert!(total > 0);
    }
}
