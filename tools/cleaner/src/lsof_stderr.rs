//! Which `lsof` stderr bytes bear on a given probe target (tachi#1978).
//!
//! Every ownership/holder probe in the workspace treats *any* relevant text on
//! `lsof`'s stderr as "the run cannot be trusted" and fails closed. That is
//! right for diagnostics that describe the query itself (a `+D` walk that
//! could not descend a subdirectory, a target that could not be stat()ed, a
//! permission error). But on Linux, lsof stats every mount-table entry at
//! start-up, and as an unprivileged user on a stock Ubuntu host it cannot stat
//! the `tracefs` mount under the root-only `/sys/kernel/debug`, so it prints
//! this pair on **every** invocation whatever the query (byte-for-byte from
//! lsof 4.95.0 on Ubuntu 24.04, atom-dgx-2):
//!
//! ```text
//! lsof: WARNING: can't stat() tracefs file system /sys/kernel/debug/tracing
//!       Output information may be incomplete.
//! ```
//!
//! That made every probe "undetermined" on every non-root Linux host.
//!
//! lsof matches open files against a named target by device + inode (taken
//! from the target itself and from each process's descriptors); the mount
//! table only names file systems and resolves a target that *is* a mount
//! point. A tracefs mount lsof could not stat therefore cannot hide a holder of
//! a target that is neither that mount point, nor below it, nor above it.
//!
//! Exactly that — and nothing wider — is dropped, and only on Linux:
//!
//! * the line is valid UTF-8 and is exactly `lsof: WARNING: can't stat()
//!   tracefs file system <mount>` (no other file-system type: only the
//!   observed tracefs warning has been shown to be this benign start-up
//!   failure);
//! * it is immediately followed by the exact continuation line
//!   `      Output information may be incomplete.` (six spaces), as lsof
//!   printed it on the host — a lone warning line is not the observed shape;
//! * `<mount>` is a plain absolute path (no `\`/`^` escapes, no `.`/`..`,
//!   `//` or trailing `/`);
//! * every target spelling (each caller path made absolute, and its canonical
//!   form) is disjoint from `<mount>`: not equal, not an ancestor, not inside;
//!   a target that cannot be canonicalized drops nothing.
//!
//! Every other non-blank line is returned with its bytes unchanged and keeps
//! failing closed. The Linux result is not byte-identical to the input:
//! blank (whitespace-only) lines are omitted and the kept lines are re-joined
//! with `\n` without a trailing delimiter — only emptiness and the kept text
//! matter to the classifiers. On every other OS the input is returned
//! byte-for-byte: this is a Linux lsof-dialect behavior and macOS filtering
//! must not move.
//!
//! `lsof -w` is deliberately **not** used: it silences the `+D` walk's own
//! "can't stat()/opendir()" warnings too, which are exactly the partial-walk
//! signal the holder probes must never lose.

use std::path::Path;

/// Return the part of `stderr` that bears on a probe of `targets`, as raw
/// bytes. An empty result means every diagnostic was a proven-irrelevant
/// tracefs mount warning; anything else must be handled exactly like a
/// non-empty stderr (fail closed). Identity on non-Linux platforms.
pub fn relevant_lsof_stderr(stderr: &[u8], targets: &[&Path]) -> Vec<u8> {
    #[cfg(target_os = "linux")]
    {
        linux::drop_disjoint_tracefs_warnings(stderr, targets)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = targets;
        stderr.to_vec()
    }
}

/// lsof's default column header, which is what every probe gets: none of
/// them selects columns (`-F`, `-o`, `-s`, `-K`, ...). Identical on lsof 4.95.0
/// (Ubuntu 24.04, atom-dgx-2) and lsof 4.91 (macOS):
/// `COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME`.
pub const LSOF_DEFAULT_HEADER: [&str; 9] = [
    "COMMAND", "PID", "USER", "FD", "TYPE", "DEVICE", "SIZE/OFF", "NODE", "NAME",
];

/// Whether `stdout`'s first line is exactly lsof's default column header
/// ([`LSOF_DEFAULT_HEADER`], any column spacing). Every classifier requires
/// this before it treats the first line as a header to skip: an unvalidated
/// first line must never be discarded (cold review round 1 finding 1), and a
/// line that merely starts `COMMAND PID` is not a header (round 2).
pub fn starts_with_lsof_header(stdout: &str) -> bool {
    stdout
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .eq(LSOF_DEFAULT_HEADER)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod linux {
    use std::path::{Component, Path, PathBuf};

    const TRACEFS_PREFIX: &str = "lsof: WARNING: can't stat() tracefs file system ";
    const INCOMPLETE_CONTINUATION: &[u8] = b"      Output information may be incomplete.";

    pub(super) fn drop_disjoint_tracefs_warnings(stderr: &[u8], targets: &[&Path]) -> Vec<u8> {
        let comparable = comparable_targets(targets);
        let lines: Vec<&[u8]> = stderr.split(|byte| *byte == b'\n').collect();
        let mut relevant: Vec<&[u8]> = Vec::new();
        let mut index = 0;
        while index < lines.len() {
            let line = lines[index];
            let droppable = !comparable.is_empty()
                && lines.get(index + 1).copied() == Some(INCOMPLETE_CONTINUATION)
                && tracefs_mount(line).is_some_and(|mount| {
                    comparable
                        .iter()
                        .all(|target| !target.starts_with(&mount) && !mount.starts_with(target))
                });
            if droppable {
                index += 2;
                continue;
            }
            if !line.iter().all(u8::is_ascii_whitespace) {
                relevant.push(line);
            }
            index += 1;
        }
        relevant.join(&b'\n')
    }

    /// Every spelling a target is known by: each caller path made absolute
    /// (without resolving symlinks) and its canonical form. Empty — nothing
    /// can be proven disjoint — when any target fails to canonicalize.
    fn comparable_targets(targets: &[&Path]) -> Vec<PathBuf> {
        let mut spellings = Vec::new();
        for target in targets {
            let Ok(canonical) = target.canonicalize() else {
                return Vec::new();
            };
            let Ok(absolute) = std::path::absolute(target) else {
                return Vec::new();
            };
            for spelling in [canonical, absolute] {
                if !spellings.contains(&spelling) {
                    spellings.push(spelling);
                }
            }
        }
        spellings
    }

    /// Mount point named by the exact tracefs start-up warning, when the line
    /// is valid UTF-8, is exactly that warning, and the path is plain.
    fn tracefs_mount(line: &[u8]) -> Option<PathBuf> {
        let line = std::str::from_utf8(line).ok()?;
        let mount = line.strip_prefix(TRACEFS_PREFIX)?;
        if mount.is_empty() || mount != mount.trim() || mount.contains(['\\', '^']) {
            return None;
        }
        let path = Path::new(mount);
        let rebuilt: PathBuf = path.components().collect();
        // `components()` silently normalizes `//`, interior `.` and a trailing
        // `/`; a spelling it rewrites is not the plain kernel mount path.
        let plain = path.is_absolute()
            && rebuilt.as_os_str() == path.as_os_str()
            && path
                .components()
                .all(|component| matches!(component, Component::RootDir | Component::Normal(_)));
        plain.then(|| path.to_path_buf())
    }

    #[cfg(all(test, unix))]
    mod tests {
        use super::*;

        /// Byte-for-byte what lsof 4.95.0 on Ubuntu 24.04 (aarch64, non-root)
        /// writes to stderr for every query (captured on atom-dgx-2).
        const TRACEFS: &[u8] = b"lsof: WARNING: can't stat() tracefs file system /sys/kernel/debug/tracing\n      Output information may be incomplete.\n";

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

        fn relevant(stderr: &[u8], target: &Path) -> String {
            String::from_utf8_lossy(&drop_disjoint_tracefs_warnings(stderr, &[target])).into_owned()
        }

        #[test]
        fn disjoint_tracefs_pair_is_irrelevant() {
            let dir = target();
            assert_eq!(relevant(TRACEFS, dir.path()), "");
            assert_eq!(relevant(&TRACEFS.repeat(3), dir.path()), "");
            assert_eq!(relevant(b"", dir.path()), "");
        }

        #[test]
        fn tracefs_warning_without_its_continuation_is_relevant() {
            let dir = target();
            let lone =
                b"lsof: WARNING: can't stat() tracefs file system /sys/kernel/debug/tracing\n";
            assert_ne!(relevant(lone, dir.path()), "");
            let other_indent = b"lsof: WARNING: can't stat() tracefs file system /sys/kernel/debug/tracing\n  Output information may be incomplete.\n";
            assert_ne!(relevant(other_indent, dir.path()), "");
        }

        #[test]
        fn other_file_system_types_are_relevant() {
            let dir = target();
            for fs_type in ["nfs", "fuse", "fuse.portal", "overlay", "nsfs", "debugfs"] {
                let stderr = format!(
                    "lsof: WARNING: can't stat() {fs_type} file system /unrelated\n      Output information may be incomplete.\n"
                );
                assert_ne!(
                    relevant(stderr.as_bytes(), dir.path()),
                    "",
                    "{fs_type} is not the observed tracefs warning"
                );
            }
        }

        #[test]
        fn tracefs_mount_at_above_or_inside_the_target_is_relevant() {
            let dir = target();
            let canonical = dir.path().canonicalize().unwrap();
            for mount in [
                canonical.clone(),
                canonical.parent().unwrap().to_path_buf(),
                canonical.join("inner"),
            ] {
                let stderr = format!(
                    "lsof: WARNING: can't stat() tracefs file system {}\n      Output information may be incomplete.\n",
                    mount.display()
                );
                assert_ne!(
                    relevant(stderr.as_bytes(), dir.path()),
                    "",
                    "mount {} overlaps the target",
                    mount.display()
                );
            }
        }

        #[test]
        fn root_or_uncanonicalizable_target_drops_nothing() {
            assert_ne!(relevant(TRACEFS, Path::new("/")), "");
            let dir = target();
            assert_ne!(relevant(TRACEFS, &dir.path().join("missing")), "");
        }

        #[test]
        fn every_caller_spelling_is_compared() {
            // A symlinked alias of the target: the alias's parent directory is
            // disjoint from the canonical target but contains the alias.
            let dir = target();
            let real = dir.path().join("real");
            let alias_parent = dir.path().join("aliases");
            std::fs::create_dir_all(&real).unwrap();
            std::fs::create_dir_all(&alias_parent).unwrap();
            let alias = alias_parent.join("db-dir");
            std::os::unix::fs::symlink(&real, &alias).unwrap();
            let stderr = format!(
                "lsof: WARNING: can't stat() tracefs file system {}\n      Output information may be incomplete.\n",
                alias_parent.display()
            );
            let only_canonical =
                drop_disjoint_tracefs_warnings(stderr.as_bytes(), &[real.as_path()]);
            assert!(
                only_canonical.is_empty(),
                "control: disjoint from the real path"
            );
            let with_alias = drop_disjoint_tracefs_warnings(stderr.as_bytes(), &[alias.as_path()]);
            assert!(
                !with_alias.is_empty(),
                "the alias spelling lies under the mount"
            );
        }

        #[test]
        fn non_utf8_lines_are_relevant() {
            let dir = target();
            let mut stderr =
                b"lsof: WARNING: can't stat() tracefs file system /tmp/m-\xff\n".to_vec();
            stderr.extend_from_slice(b"      Output information may be incomplete.\n");
            let kept = drop_disjoint_tracefs_warnings(&stderr, &[dir.path()]);
            assert!(kept.starts_with(b"lsof: WARNING"), "{kept:?}");
            assert!(kept.contains(&0xff), "bytes are kept verbatim: {kept:?}");
        }

        #[test]
        fn other_diagnostics_survive_next_to_a_dropped_pair() {
            let dir = target();
            for other in [
                "lsof: status error on /x: No such file or directory",
                "lsof: WARNING: can't stat() /some/mount",
                "lsof: WARNING: can't opendir(/x/deps): Permission denied",
                "      Output information may be incomplete.",
            ] {
                let mut stderr = TRACEFS.to_vec();
                stderr.extend_from_slice(other.as_bytes());
                stderr.push(b'\n');
                assert_eq!(relevant(&stderr, dir.path()), other, "from {stderr:?}");
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
                let stderr = format!(
                    "lsof: WARNING: can't stat() tracefs file system {mount}\n      Output information may be incomplete.\n"
                );
                assert_ne!(
                    relevant(stderr.as_bytes(), dir.path()),
                    "",
                    "mount path {mount:?} cannot be proven disjoint"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round 2: only lsof's complete default header, as the first line, is a
    /// header. (Round 1 accepted any first line starting `COMMAND PID`, e.g.
    /// `COMMAND PID USER` — tightened, lead-authorized.)
    #[test]
    fn only_the_full_default_header_as_the_first_line_is_a_header() {
        // Real headers, as printed by lsof 4.95.0 (DGX2) and 4.91 (macOS).
        assert!(starts_with_lsof_header(
            "COMMAND    PID       USER   FD   TYPE DEVICE SIZE/OFF     NODE NAME\n"
        ));
        assert!(starts_with_lsof_header(
            "COMMAND   PID       USER   FD   TYPE DEVICE SIZE/OFF      NODE NAME\nbash 1 u 3r REG 1,17 2 9 /x\n"
        ));
        for not_a_header in [
            "COMMAND PID USER\nx 1 u\n",
            "COMMAND PID nonsense\n",
            "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME EXTRA\n",
            "COMMAND PID USER FD TYPE DEVICE SIZE NODE NAME\n",
            "garbage\n",
            "\nCOMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n",
            "COMMANDS PID\n",
            "",
        ] {
            assert!(!starts_with_lsof_header(not_a_header), "{not_a_header:?}");
        }
    }

    const TRACEFS: &[u8] = b"lsof: WARNING: can't stat() tracefs file system /sys/kernel/debug/tracing\n      Output information may be incomplete.\n";

    /// Finding 8 of the round-1 cold review: the filter is a Linux lsof-dialect
    /// accommodation; on every other OS the stderr is returned byte-for-byte.
    #[test]
    fn filtering_happens_only_on_linux() {
        let dir = std::env::temp_dir();
        let out = relevant_lsof_stderr(TRACEFS, &[dir.as_path()]);
        if cfg!(target_os = "linux") {
            assert!(out.is_empty(), "{out:?}");
        } else {
            assert_eq!(out, TRACEFS);
        }
    }
}
