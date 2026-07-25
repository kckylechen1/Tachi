//! Suite-scoped temp fixture root for tachi-server tests.
//!
//! Layout: `$TMPDIR/tachi-tests/run-<pid>-<uuid>/…`
//!
//! Once GC at first use reclaims kill -9 / panic leftovers:
//! - **loose files** / non-`run-<pid>-*` dirs: mtime > 1h
//! - **`run-<pid>-<uuid>` dirs**: only if owning pid is **not alive**
//!   (`kill(pid, 0)` fails) **and** mtime > 1h
//! - current process run dir is never GC'd
//!
//! See tachi#1405 / HyperTachi#68.

use std::path::{Path, PathBuf};

/// Suite-scoped temp fixture root name under `$TMPDIR`.
pub(crate) const TEST_FIXTURE_ROOT_NAME: &str = "tachi-tests";

/// Max age before a sibling under the suite root is eligible for GC.
pub(crate) const TEST_FIXTURE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

/// `$TMPDIR/tachi-tests/` (suite root; not the per-run dir).
pub(crate) fn suite_fixture_root() -> PathBuf {
    std::env::temp_dir().join(TEST_FIXTURE_ROOT_NAME)
}

/// Parse `run-<pid>-<uuid>` → pid. Returns `None` for malformed / legacy names.
pub(crate) fn parse_run_dir_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("run-")?;
    let (pid_str, uuid_part) = rest.split_once('-')?;
    if uuid_part.is_empty() {
        return None;
    }
    pid_str.parse().ok().filter(|pid| *pid > 1)
}

/// `kill(pid, 0)` existence probe. `EPERM` still counts as alive.
pub(crate) fn process_alive(pid: u32) -> bool {
    if pid <= 1 {
        return false;
    }
    // SAFETY: signal 0 is an existence/permission probe; does not deliver.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    let err = std::io::Error::last_os_error();
    matches!(err.raw_os_error(), Some(code) if code == libc::EPERM)
}

fn is_mtime_stale(
    entry: &std::fs::DirEntry,
    max_age: std::time::Duration,
    now: std::time::SystemTime,
) -> bool {
    entry
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|mtime| now.duration_since(mtime).ok())
        .is_some_and(|age| age > max_age)
}

/// Remove eligible children of `suite_root`, skipping `keep` (current run dir).
///
/// - Loose files / unparseable dirs: mtime > `max_age`
/// - `run-<pid>-<uuid>` dirs: owning pid dead **and** mtime > `max_age`
pub(crate) fn gc_stale_test_fixtures(
    suite_root: &Path,
    max_age: std::time::Duration,
    now: std::time::SystemTime,
    keep: Option<&Path>,
) -> usize {
    let Ok(entries) = std::fs::read_dir(suite_root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if keep.is_some_and(|k| k == path.as_path()) {
            continue;
        }
        if !is_mtime_stale(&entry, max_age, now) {
            continue;
        }
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if let Some(pid) = parse_run_dir_pid(name) {
                    if process_alive(pid) {
                        // Long-lived nextest worker still owns this namespace.
                        continue;
                    }
                }
            }
            if std::fs::remove_dir_all(&path).is_ok() {
                removed += 1;
            }
        } else if std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Per-process run dir: `$TMPDIR/tachi-tests/run-<pid>-<uuid>/`.
pub(crate) fn test_fixture_root() -> PathBuf {
    static RUN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    RUN.get_or_init(|| {
        let suite = suite_fixture_root();
        let _ = std::fs::create_dir_all(&suite);
        let run = suite.join(format!(
            "run-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let _ = gc_stale_test_fixtures(
            &suite,
            TEST_FIXTURE_MAX_AGE,
            std::time::SystemTime::now(),
            Some(run.as_path()),
        );
        let _ = std::fs::create_dir_all(&run);
        run
    })
    .clone()
}

/// Join `name` under [`test_fixture_root`] (triggers Once GC on first use).
pub(crate) fn test_fixture_path(name: impl AsRef<Path>) -> PathBuf {
    test_fixture_root().join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gc_stale_test_fixtures_removes_old_siblings_keeps_young_and_keep() {
        let dir = tempfile::tempdir().expect("tempdir");
        let suite = dir.path();
        let keep = suite.join("run-keep-alive");
        std::fs::create_dir_all(&keep).expect("keep dir");
        std::fs::write(keep.join("live.txt"), b"live").expect("live");

        // Legacy / unparseable dir name — GC'd by mtime alone.
        let stale_run = suite.join("run-stale-sibling");
        std::fs::create_dir_all(&stale_run).expect("stale run");
        std::fs::write(stale_run.join("inner.txt"), b"x").expect("inner");
        let stale_loose = suite.join("loose-orphan.txt");
        std::fs::write(&stale_loose, b"s").expect("loose");

        let mtime = std::fs::metadata(&stale_loose)
            .and_then(|m| m.modified())
            .expect("mtime");
        let now = mtime + TEST_FIXTURE_MAX_AGE + std::time::Duration::from_secs(5);
        let removed = gc_stale_test_fixtures(suite, TEST_FIXTURE_MAX_AGE, now, Some(&keep));
        assert_eq!(removed, 2, "stale legacy run dir + loose file must go");
        assert!(keep.exists(), "current run dir must never be GC'd");
        assert!(keep.join("live.txt").exists());
        assert!(!stale_run.exists());
        assert!(!stale_loose.exists());

        let young_run = suite.join("run-young-sibling");
        std::fs::create_dir_all(&young_run).expect("young run");
        std::fs::write(young_run.join("y.txt"), b"y").expect("young");
        let young_mtime = std::fs::metadata(&young_run)
            .and_then(|m| m.modified())
            .expect("young mtime");
        let kept = gc_stale_test_fixtures(
            suite,
            TEST_FIXTURE_MAX_AGE,
            young_mtime + std::time::Duration::from_secs(60),
            Some(&keep),
        );
        assert_eq!(kept, 0);
        assert!(young_run.exists(), "young sibling run must survive");
    }

    #[test]
    fn gc_skips_run_dir_when_owning_pid_alive_deletes_when_dead() {
        let dir = tempfile::tempdir().expect("tempdir");
        let suite = dir.path();

        let live = suite.join(format!(
            "run-{}-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            std::process::id()
        ));
        std::fs::create_dir_all(&live).expect("live run");
        std::fs::write(live.join("held.txt"), b"held").expect("held");

        // Extremely unlikely to be a live pid on this host.
        let dead_pid = 2_000_000_001u32;
        assert!(
            !process_alive(dead_pid),
            "fixture assumes pid {dead_pid} is dead"
        );
        let dead = suite.join(format!(
            "run-{dead_pid}-aaaaaaaa-bbbb-cccc-dddd-ffffffffffff"
        ));
        std::fs::create_dir_all(&dead).expect("dead run");
        std::fs::write(dead.join("gone.txt"), b"gone").expect("gone");

        let mtime = std::fs::metadata(&live)
            .and_then(|m| m.modified())
            .expect("mtime");
        let now = mtime + TEST_FIXTURE_MAX_AGE + std::time::Duration::from_secs(5);
        let removed = gc_stale_test_fixtures(suite, TEST_FIXTURE_MAX_AGE, now, None);
        assert_eq!(removed, 1, "only the dead-pid run dir should be removed");
        assert!(
            live.exists(),
            "live-pid run dir must survive even when mtime-old"
        );
        assert!(live.join("held.txt").exists());
        assert!(!dead.exists(), "dead-pid run dir must be GC'd");
    }

    #[test]
    fn parse_run_dir_pid_extracts_pid() {
        assert_eq!(
            parse_run_dir_pid("run-12345-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
            Some(12345)
        );
        assert_eq!(parse_run_dir_pid("run-stale-sibling"), None);
        assert_eq!(parse_run_dir_pid("loose.txt"), None);
        assert_eq!(parse_run_dir_pid("run-1-uuid"), None, "pid<=1 rejected");
    }

    #[test]
    fn test_fixture_path_lives_under_per_run_suite_root() {
        let path = test_fixture_path("probe-fixture.txt");
        let run_dir = path.parent().expect("parent");
        let suite_dir = run_dir.parent().expect("suite");
        assert_eq!(
            suite_dir.file_name(),
            Some(std::ffi::OsStr::new(TEST_FIXTURE_ROOT_NAME))
        );
        let run_name = run_dir.file_name().and_then(|s| s.to_str()).expect("run");
        assert!(
            run_name.starts_with("run-"),
            "expected run-* namespace, got {run_name}"
        );
        assert_eq!(
            parse_run_dir_pid(run_name),
            Some(std::process::id()),
            "run dir must encode this process pid"
        );
        assert_eq!(test_fixture_root(), run_dir);
        assert!(run_dir.is_dir());
    }
}
