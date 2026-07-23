//! Suite-scoped temp fixture root for tachi-server tests.
//!
//! All fixtures live under `$TMPDIR/tachi-tests/` so a single Once GC at first
//! use can reclaim kill -9 / panic leftovers (mtime > 1h). See tachi#1405 /
//! HyperTachi#68.

use std::path::{Path, PathBuf};

/// Suite-scoped temp fixture root name under `$TMPDIR`.
pub(crate) const TEST_FIXTURE_ROOT_NAME: &str = "tachi-tests";

/// Max age before a fixture under [`test_fixture_root`] is eligible for GC.
pub(crate) const TEST_FIXTURE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

/// Remove direct children of `root` whose mtime is older than `max_age`
/// relative to `now`. Returns the number of entries successfully removed.
pub(crate) fn gc_stale_test_fixtures(
    root: &Path,
    max_age: std::time::Duration,
    now: std::time::SystemTime,
) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
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

/// `$TMPDIR/tachi-tests/`, created on first use. Runs stale-fixture GC once
/// per test binary (Once).
pub(crate) fn test_fixture_root() -> PathBuf {
    static GC: std::sync::Once = std::sync::Once::new();
    let root = std::env::temp_dir().join(TEST_FIXTURE_ROOT_NAME);
    let _ = std::fs::create_dir_all(&root);
    GC.call_once(|| {
        let _ = gc_stale_test_fixtures(&root, TEST_FIXTURE_MAX_AGE, std::time::SystemTime::now());
    });
    root
}

/// Join `name` under [`test_fixture_root`] (triggers Once GC on first use).
pub(crate) fn test_fixture_path(name: impl AsRef<Path>) -> PathBuf {
    test_fixture_root().join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gc_stale_test_fixtures_removes_old_keeps_young() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let stale = root.join("stale.txt");
        let nested = root.join("stale-dir");
        std::fs::create_dir_all(&nested).expect("stale dir");
        std::fs::write(nested.join("inner.txt"), b"x").expect("inner");
        std::fs::write(&stale, b"s").expect("stale");

        let mtime = std::fs::metadata(&stale)
            .and_then(|m| m.modified())
            .expect("mtime");
        let now = mtime + TEST_FIXTURE_MAX_AGE + std::time::Duration::from_secs(5);
        let removed = gc_stale_test_fixtures(root, TEST_FIXTURE_MAX_AGE, now);
        assert_eq!(removed, 2, "file + dir older than max_age must go");
        assert!(!stale.exists());
        assert!(!nested.exists());

        let young = root.join("young.txt");
        std::fs::write(&young, b"y").expect("young");
        let young_mtime = std::fs::metadata(&young)
            .and_then(|m| m.modified())
            .expect("young mtime");
        let kept = gc_stale_test_fixtures(
            root,
            TEST_FIXTURE_MAX_AGE,
            young_mtime + std::time::Duration::from_secs(60),
        );
        assert_eq!(kept, 0);
        assert!(young.exists());
    }

    #[test]
    fn test_fixture_path_lives_under_suite_root() {
        let path = test_fixture_path("probe-fixture.txt");
        assert_eq!(
            path.parent().and_then(|p| p.file_name()),
            Some(std::ffi::OsStr::new(TEST_FIXTURE_ROOT_NAME))
        );
        assert!(test_fixture_root().is_dir());
    }
}
