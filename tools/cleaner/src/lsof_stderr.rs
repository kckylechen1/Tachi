//! Which `lsof` stderr diagnostics bear on a given probe target (tachi#1978).
//!
//! Every ownership/holder probe in the workspace treats *any* text on `lsof`'s
//! stderr as "the run cannot be trusted" and fails closed. That is right for
//! the diagnostics that describe the query itself (a `+D` walk that could not
//! descend a subdirectory, a target that could not be stat()ed, a permission
//! error), but lsof's Linux dialect also prints one diagnostic per mount-table
//! entry it cannot stat() at start-up, on **every** invocation and whatever
//! the query:
//!
//! ```text
//! lsof: WARNING: can't stat() tracefs file system /sys/kernel/debug/tracing
//!       Output information may be incomplete.
//! ```
//!
//! As an unprivileged user on a stock Ubuntu host that line is always there
//! (`/sys/kernel/debug` is mode 0700 root), so every probe resolved to
//! "undetermined" and tidy migration, sticky cutover, doctor checkpoint and
//! the cleaner refused on every non-root Linux host.
//!
//! lsof matches open files against a named target by device + inode (taken
//! from the target itself and from each process's descriptors); the mount
//! table only names file systems and resolves a target that *is* a mount
//! point. A mount lsof could not stat therefore cannot hide a holder of a
//! target that is neither that mount point, nor below it, nor above it. That
//! disjoint case — and only that case — is dropped here. Everything else stays
//! and keeps failing closed:
//!
//! * a mount warning whose mount point is the target, an ancestor of it, or
//!   inside it (a `+D` walk crosses it);
//! * a mount warning whose path is not a plain absolute path (relative, `.`/
//!   `..` components, or lsof's `\`/`^` escapes for unprintable bytes) — it
//!   cannot be compared reliably;
//! * a target that cannot be canonicalized — disjointness cannot be proven;
//! * every other diagnostic, including a bare "Output information may be
//!   incomplete." that does not follow a dropped mount warning.
//!
//! `lsof -w` is deliberately **not** used: it silences the `+D` walk's own
//! "can't stat()/opendir()" warnings too, which are exactly the partial-walk
//! signal the holder probes must never lose.

use std::path::{Component, Path, PathBuf};

const MOUNT_STAT_PREFIX: &str = "lsof: WARNING: can't stat() ";
const MOUNT_STAT_FILE_SYSTEM: &str = " file system ";
const INCOMPLETE_CONTINUATION: &str = "Output information may be incomplete.";

/// Return the part of `stderr` that is relevant to a probe of `target`, with
/// lines joined by `\n`. An empty result means every diagnostic was a proven
/// irrelevant mount-table warning; any other result must be handled exactly
/// as the caller handled a non-empty stderr before (fail closed).
pub fn relevant_lsof_stderr(stderr: &str, target: &Path) -> String {
    let targets = comparable_targets(target);
    let mut relevant = Vec::new();
    let mut lines = stderr.lines().peekable();
    while let Some(line) = lines.next() {
        let disjoint = unreadable_mount(line).is_some_and(|mount| {
            !targets.is_empty()
                && targets
                    .iter()
                    .all(|target| !target.starts_with(&mount) && !mount.starts_with(target))
        });
        if disjoint {
            if lines
                .peek()
                .is_some_and(|next| next.trim() == INCOMPLETE_CONTINUATION)
            {
                lines.next();
            }
            continue;
        }
        if !line.trim().is_empty() {
            relevant.push(line);
        }
    }
    relevant.join("\n")
}

/// The target spelled as the caller passed it (when absolute) and canonically.
/// Empty — nothing can be proven disjoint — when canonicalization fails.
fn comparable_targets(target: &Path) -> Vec<PathBuf> {
    let Ok(canonical) = target.canonicalize() else {
        return Vec::new();
    };
    let mut targets = vec![canonical];
    if target.is_absolute() && !targets.contains(&target.to_path_buf()) {
        targets.push(target.to_path_buf());
    }
    targets
}

/// Mount point named by lsof's start-up "can't stat() <fstype> file system
/// <mount>" warning, when the line is exactly that and the path is plain.
fn unreadable_mount(line: &str) -> Option<PathBuf> {
    let rest = line.strip_prefix(MOUNT_STAT_PREFIX)?;
    let (fs_type, mount) = rest.split_once(MOUNT_STAT_FILE_SYSTEM)?;
    if fs_type.is_empty() || fs_type.contains(char::is_whitespace) {
        return None;
    }
    if mount.is_empty() || mount != mount.trim() || mount.contains(['\\', '^']) {
        return None;
    }
    let path = Path::new(mount);
    let rebuilt: PathBuf = path.components().collect();
    // `components()` silently normalizes `//` and interior `.`; a spelling it
    // rewrites is not the plain kernel mount path this comparison relies on.
    let plain = path.is_absolute()
        && rebuilt.as_os_str() == path.as_os_str()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)));
    plain.then(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte-for-byte what lsof 4.95.0 on Ubuntu 24.04 (aarch64, non-root)
    /// writes to stderr for every query (captured on atom-dgx-2).
    const TRACEFS: &str = "lsof: WARNING: can't stat() tracefs file system /sys/kernel/debug/tracing\n      Output information may be incomplete.\n";

    /// A private, existing directory removed on drop.
    struct Target(PathBuf);
    impl Target {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Target {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn target() -> Target {
        let dir = std::env::temp_dir().join(format!("lsof-stderr-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).expect("create target dir");
        Target(dir)
    }

    #[test]
    fn disjoint_mount_warning_is_irrelevant() {
        let dir = target();
        assert_eq!(relevant_lsof_stderr(TRACEFS, dir.path()), "");
        assert_eq!(relevant_lsof_stderr(&TRACEFS.repeat(3), dir.path()), "");
    }

    #[test]
    fn empty_stderr_stays_empty() {
        let dir = target();
        assert_eq!(relevant_lsof_stderr("", dir.path()), "");
    }

    #[test]
    fn mount_inside_the_target_is_relevant() {
        let dir = target();
        let mount = dir.path().canonicalize().unwrap().join("fuse-mount");
        let stderr = format!(
            "lsof: WARNING: can't stat() fuse file system {}\n      Output information may be incomplete.\n",
            mount.display()
        );
        let relevant = relevant_lsof_stderr(&stderr, dir.path());
        assert!(
            relevant.contains("can't stat() fuse file system")
                && relevant.contains(INCOMPLETE_CONTINUATION),
            "a mount below a +D target is part of the walk: {relevant:?}"
        );
    }

    #[test]
    fn mount_at_or_above_the_target_is_relevant() {
        let dir = target();
        let canonical = dir.path().canonicalize().unwrap();
        for mount in [canonical.clone(), canonical.parent().unwrap().to_path_buf()] {
            let stderr = format!(
                "lsof: WARNING: can't stat() nfs file system {}\n",
                mount.display()
            );
            assert_ne!(
                relevant_lsof_stderr(&stderr, dir.path()),
                "",
                "mount {} contains the target",
                mount.display()
            );
        }
    }

    #[test]
    fn root_target_never_drops_a_mount_warning() {
        assert_ne!(relevant_lsof_stderr(TRACEFS, Path::new("/")), "");
    }

    #[test]
    fn uncanonicalizable_target_never_drops_a_mount_warning() {
        let dir = target();
        let missing = dir.path().join("does-not-exist");
        assert_ne!(relevant_lsof_stderr(TRACEFS, &missing), "");
    }

    #[test]
    fn other_diagnostics_survive_next_to_a_dropped_mount_warning() {
        let dir = target();
        for other in [
            "lsof: status error on /x: No such file or directory\n",
            "lsof: WARNING: can't stat() /some/mount\n      Output information may be incomplete.\n",
            "lsof: WARNING: can't opendir(/x/deps): Permission denied\n",
            "      Output information may be incomplete.\n",
        ] {
            let stderr = format!("{TRACEFS}{other}");
            assert_eq!(
                relevant_lsof_stderr(&stderr, dir.path()),
                other.trim_end(),
                "only the tracefs warning may be dropped from {stderr:?}"
            );
        }
    }

    #[test]
    fn unplain_mount_paths_are_relevant() {
        let dir = target();
        for mount in [
            "relative/mount",
            "/a/../sys",
            "/a/./b",
            "//double/slash",
            "/trailing/slash/",
            "/run/user/1000/my\\040dir",
            "/run/^Gbell",
            "/trailing ",
            "",
        ] {
            let stderr = format!("lsof: WARNING: can't stat() fuse file system {mount}\n");
            assert_ne!(
                relevant_lsof_stderr(&stderr, dir.path()),
                "",
                "mount path {mount:?} cannot be proven disjoint"
            );
        }
        let spaced_type = "lsof: WARNING: can't stat() fuse portal file system /run/doc\n";
        assert_ne!(relevant_lsof_stderr(spaced_type, dir.path()), "");
    }
}
