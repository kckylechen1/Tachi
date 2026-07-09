//! Discrimination tests for the #484 worktree governor's reclaim path
//! (cross-vendor review on PR #910, CP1/CP2/CP3/CP4/CP6):
//!
//! - open -> close -> list round trip
//! - close (direct) and sweep (reclaim) both refuse a dirty worktree, even
//!   with `--force`
//! - an explicit `--path` outside the managed root is rejected
//! - a path containing `..` traversal is rejected
//!
//! These run as a real subprocess-driving integration test (separate test
//! binary) against the crate's public API so `HOME` / `TACHI_WORKTREES_ROOT`
//! env mutation here can't race with the crate's own unit tests.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use tachi_clean::sweep::{self, SweepOptions};
use tachi_clean::registry;
use tachi_clean::wt_clean::{self, OutputFormat, WtRemoveOptions};
use tachi_clean::wt_open::{self, OpenOptions};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn unique_temp(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        nanos
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn init_git_repo(path: &Path) {
    assert!(Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(path)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["config", "user.email", "tachi-test@example.com"])
        .current_dir(path)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["config", "user.name", "tachi-test"])
        .current_dir(path)
        .status()
        .unwrap()
        .success());
    std::fs::write(path.join("README"), "hello").unwrap();
    assert!(Command::new("git")
        .args(["add", "README"])
        .current_dir(path)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(path)
        .status()
        .unwrap()
        .success());
}

struct EnvGuard {
    home_old: Option<std::ffi::OsString>,
    root_old: Option<std::ffi::OsString>,
}

fn set_env(home: &Path, root: &Path) -> EnvGuard {
    let guard = EnvGuard {
        home_old: std::env::var_os("HOME"),
        root_old: std::env::var_os("TACHI_WORKTREES_ROOT"),
    };
    std::env::set_var("HOME", home);
    std::env::set_var("TACHI_WORKTREES_ROOT", root);
    guard
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.home_old.take() {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match self.root_old.take() {
            Some(v) => std::env::set_var("TACHI_WORKTREES_ROOT", v),
            None => std::env::remove_var("TACHI_WORKTREES_ROOT"),
        }
    }
}

#[test]
fn open_close_list_round_trip() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("wt-lifecycle-roundtrip");
    let home = root.join("home");
    let cache = root.join("cache-worktrees");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    let _env = set_env(&home, &cache);

    let open_report = wt_open::open_worktree(OpenOptions {
        repo_root: repo.clone(),
        path: None,
        branch: Some("tachi/roundtrip/worker".into()),
        base: Some("HEAD".into()),
        task: Some("roundtrip".into()),
        role: Some("worker".into()),
        dispatch_id: None,
        name: Some("roundtrip-leaf".into()),
        dry_run: false,
        output: OutputFormat::Json,
    })
    .expect("open_worktree should not error");
    assert!(
        open_report.errors.is_empty(),
        "open errors: {:?}",
        open_report.errors
    );
    assert!(open_report.opened, "worktree should have opened");
    assert!(open_report.registered, "worktree should have registered");

    let path = PathBuf::from(&open_report.path);
    assert!(path.exists(), "worktree path should exist after open");

    // list: registry must reflect the freshly opened worktree.
    let listed = registry::list_registered_worktrees().expect("list should succeed");
    assert!(
        listed.iter().any(|item| paths_match(&item.path, &path)),
        "wt-list should include the opened worktree: {listed:?}"
    );

    // close: direct force-remove should succeed on a clean worktree.
    wt_clean::run_wt_remove(WtRemoveOptions {
        path: path.clone(),
        force: true,
        output: OutputFormat::Json,
    })
    .expect("close of a clean, registered worktree should succeed");

    assert!(!path.exists(), "worktree path should be gone after close");
    let listed_after = registry::list_registered_worktrees().expect("list should succeed");
    assert!(
        !listed_after.iter().any(|item| paths_match(&item.path, &path)),
        "wt-list should no longer include the closed worktree: {listed_after:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn direct_close_refuses_dirty_worktree() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("wt-lifecycle-close-dirty");
    let home = root.join("home");
    let cache = root.join("cache-worktrees");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    let _env = set_env(&home, &cache);

    let open_report = wt_open::open_worktree(OpenOptions {
        repo_root: repo.clone(),
        path: None,
        branch: Some("tachi/close-dirty/worker".into()),
        base: Some("HEAD".into()),
        task: Some("close-dirty".into()),
        role: Some("worker".into()),
        dispatch_id: None,
        name: Some("close-dirty-leaf".into()),
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();
    assert!(open_report.opened, "setup: worktree should have opened");
    let path = PathBuf::from(&open_report.path);

    // Seed uncommitted work.
    std::fs::write(path.join("uncommitted.txt"), "do not clobber me").unwrap();

    let result = wt_clean::run_wt_remove(WtRemoveOptions {
        path: path.clone(),
        force: true,
        output: OutputFormat::Json,
    });

    assert!(
        result.is_err(),
        "direct close must refuse a dirty worktree with --force"
    );
    let err = result.unwrap_err();
    assert!(
        err.to_lowercase().contains("dirty"),
        "error should explain the dirty refusal: {err}"
    );
    assert!(
        path.exists() && path.join("uncommitted.txt").exists(),
        "dirty worktree and its uncommitted file must survive the refused close"
    );

    // Cleanup: discard the dirty file, then really close it.
    std::fs::remove_file(path.join("uncommitted.txt")).unwrap();
    let _ = wt_clean::run_wt_remove(WtRemoveOptions {
        path: path.clone(),
        force: true,
        output: OutputFormat::Json,
    });
    let _ = std::fs::remove_dir_all(&root);
}

/// CP1/CP2 discrimination: sweep --force must NEVER remove a worktree with
/// uncommitted changes, even once it's old enough / marked enough to be a
/// sweep candidate. RED on the pre-fix sweep code (which only gated on
/// marker + active-process, not git cleanliness): the worktree directory
/// would be gone after this call. GREEN post-fix: it survives.
#[test]
fn sweep_never_removes_dirty_worktree() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("wt-lifecycle-sweep-dirty");
    let home = root.join("home");
    let cache = root.join("cache-worktrees");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    let _env = set_env(&home, &cache);

    let open_report = wt_open::open_worktree(OpenOptions {
        repo_root: repo.clone(),
        path: None,
        branch: Some("tachi/sweep-dirty/worker".into()),
        base: Some("HEAD".into()),
        task: Some("sweep-dirty".into()),
        role: Some("worker".into()),
        dispatch_id: None,
        name: Some("sweep-dirty-leaf".into()),
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();
    assert!(open_report.opened, "setup: worktree should have opened");
    let path = PathBuf::from(&open_report.path);

    // Seed uncommitted work in the managed worktree sweep will scan.
    std::fs::write(path.join("uncommitted.txt"), "do not clobber me").unwrap();

    // max_age_days = 0 makes every marked worktree an immediate sweep
    // candidate regardless of real elapsed time (age >= 0 always fails
    // `age < max_age(0)`), so the test doesn't need to fake mtime/`now`.
    sweep::run_sweep(SweepOptions {
        roots: vec![cache.clone()],
        max_age_days: 0,
        force: true,
        output: OutputFormat::Text,
    })
    .expect("sweep should not hard-error even when it skips a dirty candidate");

    assert!(
        path.exists() && path.join("uncommitted.txt").exists(),
        "sweep --force must never remove a worktree with uncommitted changes; \
         path={} gone or file missing after sweep",
        path.display()
    );

    let worktree_list = Command::new("git")
        .args(["-C", repo.to_str().unwrap(), "worktree", "list"])
        .output()
        .unwrap();
    let listing = String::from_utf8_lossy(&worktree_list.stdout);
    assert!(
        listing.contains(path.to_str().unwrap()),
        "git worktree list should still show the dirty worktree: {listing}"
    );

    // Cleanup: discard the dirty file, then really remove it.
    std::fs::remove_file(path.join("uncommitted.txt")).unwrap();
    let _ = wt_clean::run_wt_remove(WtRemoveOptions {
        path: path.clone(),
        force: true,
        output: OutputFormat::Json,
    });
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn open_rejects_path_outside_managed_root() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("wt-lifecycle-outside-root");
    let home = root.join("home");
    let cache = root.join("cache-worktrees");
    let repo = root.join("repo");
    let outside = root.join("elsewhere").join("escaped-wt");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    init_git_repo(&repo);
    let _env = set_env(&home, &cache);

    let report = wt_open::open_worktree(OpenOptions {
        repo_root: repo,
        path: Some(outside.clone()),
        branch: Some("tachi/outside/worker".into()),
        base: Some("HEAD".into()),
        task: Some("outside".into()),
        role: Some("worker".into()),
        dispatch_id: None,
        name: None,
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();

    assert!(!report.opened, "must not open outside the managed root");
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("managed root") || e.contains("outside")),
        "expected an outside-managed-root refusal, got: {:?}",
        report.errors
    );
    assert!(!outside.exists(), "escaped path must not be created");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn open_rejects_path_traversal() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("wt-lifecycle-traversal");
    let home = root.join("home");
    let cache = root.join("cache-worktrees");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    let _env = set_env(&home, &cache);

    // Lexically escapes the managed root via `..` even though it's rooted
    // under `cache` textually.
    let traversal_path = cache.join("leaf").join("..").join("..").join("escaped");

    let report = wt_open::open_worktree(OpenOptions {
        repo_root: repo,
        path: Some(traversal_path.clone()),
        branch: Some("tachi/traversal/worker".into()),
        base: Some("HEAD".into()),
        task: Some("traversal".into()),
        role: Some("worker".into()),
        dispatch_id: None,
        name: None,
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();

    assert!(!report.opened, "must not open a traversal path");
    assert!(
        report.errors.iter().any(|e| e.contains("traversal") || e.contains("..")),
        "expected a traversal refusal, got: {:?}",
        report.errors
    );

    let _ = std::fs::remove_dir_all(&root);
}

fn paths_match(a: &str, b: &Path) -> bool {
    let a_canon = std::fs::canonicalize(a).unwrap_or_else(|_| PathBuf::from(a));
    let b_canon = std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    a_canon == b_canon
}
