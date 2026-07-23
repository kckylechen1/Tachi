//! Suite-scoped temp fixture root for tachi-server tests.
//!
//! Layout: `$TMPDIR/tachi-tests/run-<pid>-<uuid>/…`
//!
//! A single Once GC at first use reclaims kill -9 / panic leftovers by deleting
//! **sibling** `run-*` dirs and loose files under the suite root whose mtime is
//! older than 1h. The current process's run dir is never GC'd (cross-process
//! safety for long-lived nextest workers). See tachi#1405 / HyperTachi#68.

use std::path::{Path, PathBuf};

/// Suite-scoped temp fixture root name under `$TMPDIR`.
pub(crate) const TEST_FIXTURE_ROOT_NAME: &str = "tachi-tests";

/// Max age before a sibling under the suite root is eligible for GC.
pub(crate) const TEST_FIXTURE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

/// `$TMPDIR/tachi-tests/` (suite root; not the per-run dir).
pub(crate) fn suite_fixture_root() -> PathBuf {
    std::env::temp_dir().join(TEST_FIXTURE_ROOT_NAME)
}

/// Remove direct children of `suite_root` whose mtime is older than `max_age`
/// relative to `now`, skipping `keep` (the current run dir). Returns the number
/// of entries successfully removed.
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
        let stale = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|mtime| now.duration_since(mtime).ok())
            .is_some_and(|age| age > max_age);
        if !stale {
            continue;
        }
        let ok = if path.is_dir() {
            std::fs::remove_dir_all(&path).is_ok()
        } else {
            std::fs::remove_file(&path).is_ok()
        };
        if ok {
            removed += 1;
        }
    }
    removed
}

/// Per-process run dir: `$TMPDIR/tachi-tests/run-<pid>-<uuid>/`.
///
/// Created on first use. Runs stale-sibling GC once against the suite root,
/// never deleting this run dir.
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

        let stale_run = suite.join("run-stale-sibling");
        std::fs::create_dir_all(&stale_run).expect("stale run");
        std::fs::write(stale_run.join("inner.txt"), b"x").expect("inner");
        let stale_loose = suite.join("loose-orphan.txt");
        std::fs::write(&stale_loose, b"s").expect("loose");

        let mtime = std::fs::metadata(&stale_loose)
            .and_then(|m| m.modified())
            .expect("mtime");
        // Synthetic "now" makes keep + stale look older than max_age. `keep` is
        // still preserved by path, proving the cross-process safety latch.
        let now = mtime + TEST_FIXTURE_MAX_AGE + std::time::Duration::from_secs(5);
        let removed = gc_stale_test_fixtures(suite, TEST_FIXTURE_MAX_AGE, now, Some(&keep));
        assert_eq!(removed, 2, "stale run dir + loose file must go");
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
        assert_eq!(test_fixture_root(), run_dir);
        assert!(run_dir.is_dir());
    }
}
