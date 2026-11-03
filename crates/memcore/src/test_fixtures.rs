//! Suite-scoped temp fixture root for memcore tests.
//!
//! Layout: `$TMPDIR/memcore-tests/run-<pid>-<uuid>/…`
//!
//! Once GC: loose files by mtime>1h; `run-<pid>-*` dirs only when owning pid
//! is dead **and** mtime>1h. Current run dir never GC'd. See tachi#1405.

use rusqlite::{
    backup::{Backup, StepResult},
    Connection, OpenFlags,
};
use std::path::{Path, PathBuf};

/// The caller owns a fresh private directory for one immutable test run and
/// removes it after all test children exit. Without it, keep fresh initialization.
pub(crate) fn memory_connection() -> Connection {
    match std::env::var_os("MEMCORE_TEST_TEMPLATE_DIR") {
        Some(root) => template_connection(Path::new(&root), || {}),
        None => fresh_memory_connection(),
    }
}

fn register_extensions() {
    crate::db::enable_simple_auto_extension().unwrap();
    crate::db::register_sqlite_vec();
}

fn fresh_memory_connection() -> Connection {
    register_extensions();
    let conn = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    crate::db::try_load_sqlite_vec(&conn);
    conn
}

fn template_path(root: &Path) -> PathBuf {
    assert!(
        root.is_absolute() && root.is_dir(),
        "invalid test template directory"
    );
    let executable = std::env::current_exe().unwrap();
    let mut name = executable.file_name().unwrap().to_os_string();
    name.push(".sqlite");
    root.join(name)
}

fn template_exists(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            assert!(
                metadata.file_type().is_file(),
                "test template is not a regular file"
            );
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => panic!("inspect test template: {error}"),
    }
}

fn copy_database(source: &Connection, target: &mut Connection) {
    // Both are private or immutable: contention is an error, not a retry loop.
    let backup = Backup::new(source, target).unwrap();
    assert_eq!(backup.step(-1).unwrap(), StepResult::Done);
}

fn template_connection(root: &Path, before_publish: impl FnOnce()) -> Connection {
    register_extensions();
    let path = template_path(root);
    if !template_exists(&path) {
        let source = fresh_memory_connection();
        let scratch = tempfile::NamedTempFile::new_in(root).unwrap();
        {
            let mut target = Connection::open(scratch.path()).unwrap();
            copy_database(&source, &mut target);
        }
        before_publish();
        // Concurrent cold workers may each build a complete snapshot. Never
        // expose a partial database or replace the first published snapshot.
        match scratch.persist_noclobber(&path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                assert!(template_exists(&path));
            }
            Err(error) => panic!("publish test template: {error}"),
        }
    }
    let source = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut conn = Connection::open_in_memory().unwrap();
    copy_database(&source, &mut conn);
    crate::db::ensure_reserved_reference_write_guard(&conn).unwrap();
    crate::db::apply_connection_pragmas(&conn).unwrap();
    crate::db::try_load_sqlite_vec(&conn);
    conn
}

pub(crate) const TEST_FIXTURE_ROOT_NAME: &str = "memcore-tests";

pub(crate) const TEST_FIXTURE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

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
#[cfg(unix)]
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
                let owner_pid = parse_run_dir_pid(name);
                #[cfg(unix)]
                let retain_owner = owner_pid.is_some_and(process_alive);
                // A platform without a liveness probe cannot prove this owner exited.
                #[cfg(not(unix))]
                let retain_owner = owner_pid.is_some();
                if retain_owner {
                    continue;
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

pub(crate) fn test_fixture_path(name: impl AsRef<Path>) -> PathBuf {
    test_fixture_root().join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_preserves_schema_seeds_connection_policy_and_isolation() {
        let root = tempfile::tempdir().unwrap();
        let fresh = fresh_memory_connection();
        let clone = template_connection(root.path(), || {});
        fn schema(conn: &Connection) -> Vec<(String, String, Option<String>)> {
            conn.prepare("SELECT type, name, sql FROM sqlite_schema ORDER BY type, name")
                .unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        }
        assert_eq!(schema(&fresh), schema(&clone));
        for (kind, table, _) in schema(&fresh) {
            if kind == "table" {
                let sql = format!("SELECT count(*) FROM \"{}\"", table.replace('"', "\"\""));
                let count =
                    |conn: &Connection| conn.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap();
                assert_eq!(count(&fresh), count(&clone), "seed rows in {table}");
            }
        }
        for pragma in ["foreign_keys", "cache_size", "busy_timeout", "user_version"] {
            let read = |conn: &Connection| {
                conn.query_row(&format!("PRAGMA {pragma}"), [], |r| r.get::<_, i64>(0))
                    .unwrap()
            };
            assert_eq!(read(&fresh), read(&clone), "{pragma}");
        }
        assert_eq!(clone.path(), Some(""));
        let mode: String = clone
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "memory");
        let guard: i64 = clone
            .query_row("SELECT tachi_reserved_reference_write_enabled()", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(guard, 0);
        clone
            .execute(
                "INSERT INTO hard_state(namespace,key) VALUES('fixture-isolation','one')",
                [],
            )
            .unwrap();
        clone
            .execute("CREATE TABLE fixture_only(value TEXT)", [])
            .unwrap();
        let second = template_connection(root.path(), || panic!("warm template rebuilt"));
        let count: i64 = second
            .query_row(
                "SELECT count(*) FROM hard_state WHERE namespace='fixture-isolation'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(schema(&fresh), schema(&second));
    }

    #[test]
    fn invalid_template_fails_without_rebuilding() {
        let root = tempfile::tempdir().unwrap();
        let path = template_path(root.path());
        std::fs::write(&path, b"not a SQLite database").unwrap();
        assert!(
            std::panic::catch_unwind(|| template_connection(root.path(), || panic!("rebuilt")))
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"not a SQLite database");
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(
            std::panic::catch_unwind(|| template_connection(root.path(), || panic!("rebuilt")))
                .is_err()
        );
    }

    fn wait_for_paths(paths: &[PathBuf]) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !paths.iter().all(|path| path.exists()) {
            assert!(
                std::time::Instant::now() < deadline,
                "fixture cohort did not reach barrier"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn template_publish_child() {
        let Some(root) = std::env::var_os("MEMCORE_TEMPLATE_CHILD_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let id = std::env::var("MEMCORE_TEMPLATE_CHILD_ID").unwrap();
        let conn = template_connection(&root, || {
            std::fs::write(root.join(format!("ready-{id}")), []).unwrap();
            wait_for_paths(&[root.join("publish")]);
        });
        conn.execute(
            "INSERT INTO hard_state(namespace,key) VALUES('fixture-isolation','child')",
            [],
        )
        .unwrap();
        let second = template_connection(&root, || panic!("warm template rebuilt"));
        let count: i64 = second
            .query_row(
                "SELECT count(*) FROM hard_state WHERE namespace='fixture-isolation'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn concurrent_processes_publish_complete_isolated_templates() {
        struct Children(Vec<std::process::Child>);
        impl Drop for Children {
            fn drop(&mut self) {
                for child in &mut self.0 {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        let root = tempfile::tempdir().unwrap();
        let mut children = Children(Vec::new());
        for id in 0..2 {
            children.0.push(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "test_fixtures::tests::template_publish_child",
                        "--nocapture",
                    ])
                    .env("MEMCORE_TEMPLATE_CHILD_ROOT", root.path())
                    .env("MEMCORE_TEMPLATE_CHILD_ID", id.to_string())
                    .spawn()
                    .unwrap(),
            );
        }
        wait_for_paths(
            &(0..2)
                .map(|id| root.path().join(format!("ready-{id}")))
                .collect::<Vec<_>>(),
        );
        assert!(
            !template_exists(&template_path(root.path())),
            "partial snapshot published"
        );
        std::fs::write(root.path().join("publish"), []).unwrap();
        for child in &mut children.0 {
            assert!(child.wait().unwrap().success());
        }
        let conn = template_connection(root.path(), || panic!("published template rebuilt"));
        let integrity: String = conn
            .query_row("PRAGMA quick_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
    }

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

    #[cfg(unix)]
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

    #[cfg(not(unix))]
    #[test]
    fn platform_refusal_fixture_gc_retains_unproven_owner_while_reclaiming_legacy_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let owned = dir.path().join("run-2000000001-owner");
        std::fs::create_dir(&owned).unwrap();
        std::fs::write(owned.join("sentinel"), b"preserve").unwrap();
        let legacy = dir.path().join("legacy");
        std::fs::create_dir(&legacy).unwrap();
        let now =
            std::time::SystemTime::now() + TEST_FIXTURE_MAX_AGE + std::time::Duration::from_secs(5);
        assert_eq!(
            gc_stale_test_fixtures(dir.path(), TEST_FIXTURE_MAX_AGE, now, None),
            1
        );
        assert_eq!(std::fs::read(owned.join("sentinel")).unwrap(), b"preserve");
        assert!(!legacy.exists());
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
