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

use tachi_clean::registry;
use tachi_clean::sweep::{self, SweepOptions};
use tachi_clean::wt_clean::{self, OutputFormat, WtRemoveOptions};
use tachi_clean::wt_open::{self, CargoTargetPolicy, OpenOptions};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn unique_temp(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("{prefix}-{}-{}", std::process::id(), nanos));
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

/// The cleaner's persisted-holder gate reads the configured global DB before
/// considering destructive worktree cleanup.  Lifecycle fixtures model a
/// legacy installation with no ExecEnv rows, not an unavailable runtime, so
/// create the valid empty DB that deterministically yields NotApplicable.
fn initialize_empty_global_db(home: &Path) {
    let db = home.join(".tachi").join("global").join("memory.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    memcore::MemoryStore::open(db.to_str().unwrap()).expect("fixture global DB should initialize");
}

fn set_env(home: &Path, root: &Path) -> EnvGuard {
    initialize_empty_global_db(home);
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
        cargo_target: CargoTargetPolicy::Shared,
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
        !listed_after
            .iter()
            .any(|item| paths_match(&item.path, &path)),
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
        cargo_target: CargoTargetPolicy::Shared,
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

/// tachi#1118 freeze boundary 1/2 discrimination: a live, OS-view
/// attributed process holder must refuse the close predicate, and the
/// refusal must carry structured attribution (ppid), not just a raw
/// `lsof` line. RED on the pre-fix code for a genuinely behavioral
/// reason: pre-fix `active_processes`'s refusal message is built only
/// from raw `lsof +D` output rows, whose columns are
/// `COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME` — there is no
/// PPID column and no "ppid=" text anywhere in that message, so this
/// assertion fails on origin/main even though origin/main also refuses
/// the removal. GREEN post-fix: the OS-view `holder::probe_holders` +
/// `ps`-attribution path names pid/ppid/tty/cwd explicitly.
///
/// Fixed (tachi#1212 fix-round, codex checkpoint 3): the original version
/// of this test held a NEWLY WRITTEN, untracked `held.txt` open, which
/// `git status --porcelain` reports as `?? held.txt` — the dirty-entries
/// guard in `plan_wt_remove` runs BEFORE the holder probe and fires first
/// on that untracked file, so the function returns the DIRTY refusal
/// message (which never contains "ppid=") and the `err.contains("ppid=")`
/// assertion below could never pass; the holder-attribution code path was
/// never actually reached. Holding the already-tracked, already-committed
/// `README` open instead keeps `git status --porcelain` clean so the
/// holder probe is the check that actually fires.
///
/// RED-MAIN fix-round follow-up: the setup's own "is it clean yet" check
/// (`status_before` below) must apply the SAME "clean" definition
/// `plan_wt_remove` uses, not a raw, unfiltered `git status --porcelain`.
/// `wt_open::open_worktree` (registration enabled) unconditionally writes
/// `.tachi-worktree.json` into every freshly opened worktree before this
/// test ever touches `README`; that marker is untracked by design and is
/// excluded from "dirty" by `wt_clean::dirty_entries_excluding_marker`
/// (`pub(crate)`, unreachable from this separate-crate integration test).
/// A raw status check here saw that marker and (falsely) called the setup
/// dirty, tripping this assertion regardless of the holder-attribution
/// logic under test. `porcelain_entries_excluding_marker` mirrors the
/// production exclusion so this setup check agrees with what
/// `plan_wt_remove` actually treats as clean.
#[test]
fn direct_close_refuses_a_live_holder_with_attributed_pid_family() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("wt-lifecycle-live-holder");
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
        branch: Some("tachi/live-holder/worker".into()),
        base: Some("HEAD".into()),
        task: Some("live-holder".into()),
        role: Some("worker".into()),
        dispatch_id: None,
        name: Some("live-holder-leaf".into()),
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();
    assert!(open_report.opened, "setup: worktree should have opened");
    let path = PathBuf::from(&open_report.path);

    // Hold the already-tracked, already-committed README open — NOT a
    // newly written file — so `git status --porcelain` stays clean and the
    // dirty-entries guard doesn't intercept before the holder probe runs.
    let tracked_file = path.join("README");
    assert!(
        tracked_file.exists(),
        "setup: README should already be tracked/committed by init_git_repo"
    );
    let handle = std::fs::File::open(&tracked_file).unwrap();
    let status_before = porcelain_entries_excluding_marker(&path);
    assert!(
        status_before.is_empty(),
        "test setup bug: holding a tracked file open must not itself make the worktree dirty \
         (entries: {status_before:?})"
    );

    let result = wt_clean::run_wt_remove(WtRemoveOptions {
        path: path.clone(),
        force: true,
        output: OutputFormat::Json,
    });

    assert!(
        result.is_err(),
        "a live OS-view process holder must refuse the close, even with --force"
    );
    let err = result.unwrap_err();
    assert!(
        !err.to_lowercase().contains("dirty"),
        "this must be a HOLDER refusal, not a dirty-entries refusal (test setup regression): {err}"
    );
    assert!(
        err.contains("ppid="),
        "refusal must carry OS-view attribution (ppid=...), not just a raw lsof line: {err}"
    );
    assert!(
        err.contains(&std::process::id().to_string()),
        "refusal must name this process's own pid in the held family: {err}"
    );
    assert!(
        path.exists() && tracked_file.exists(),
        "the held worktree must survive the refused close"
    );

    drop(handle);
    let _ = wt_clean::run_wt_remove(WtRemoveOptions {
        path: path.clone(),
        force: true,
        output: OutputFormat::Json,
    });
    let _ = std::fs::remove_dir_all(&root);
}

/// tachi#1118 freeze boundary 3 discrimination: a scrapped worktree must
/// reopen only under a NEW branch + NEW path, never the exact path a
/// surviving writer might still hold a reference to. RED on the pre-fix
/// code for a genuinely behavioral reason: pre-fix `open_worktree` only
/// checks `path.exists()` (false once the path has been removed) with no
/// memory of a prior scrap, so reopening at the identical path succeeds
/// on origin/main. GREEN post-fix: the scrap ledger refuses it.
#[test]
fn reopen_refuses_the_exact_path_of_a_scrapped_worktree() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("wt-lifecycle-same-path-reentry");
    let home = root.join("home");
    let cache = root.join("cache-worktrees");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    let _env = set_env(&home, &cache);

    let first_open = wt_open::open_worktree(OpenOptions {
        repo_root: repo.clone(),
        path: None,
        branch: Some("tachi/reentry/first".into()),
        base: Some("HEAD".into()),
        task: Some("reentry".into()),
        role: Some("worker".into()),
        dispatch_id: None,
        name: Some("reentry-leaf".into()),
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();
    assert!(first_open.opened, "setup: first open should succeed");
    let scrapped_path = PathBuf::from(&first_open.path);

    wt_clean::run_wt_remove(WtRemoveOptions {
        path: scrapped_path.clone(),
        force: true,
        output: OutputFormat::Json,
    })
    .expect("setup: scrapping the clean worktree should succeed");
    assert!(
        !scrapped_path.exists(),
        "setup: path must be gone after scrap"
    );

    // Reopening at the EXACT same path (even with a different branch) must
    // be refused: the same path is a re-entry route for a surviving writer.
    let reentry = wt_open::open_worktree(OpenOptions {
        repo_root: repo.clone(),
        path: Some(scrapped_path.clone()),
        branch: Some("tachi/reentry/second".into()),
        base: Some("HEAD".into()),
        task: Some("reentry".into()),
        role: Some("worker".into()),
        dispatch_id: None,
        name: None,
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();

    assert!(
        !reentry.opened,
        "reopening at a scrapped path must be refused"
    );
    assert!(
        reentry
            .errors
            .iter()
            .any(|e| e.contains("scrapped") || e.contains("same path")),
        "expected a same-path re-entry refusal, got: {:?}",
        reentry.errors
    );
    assert!(
        !scrapped_path.exists(),
        "a refused reopen must not leave anything on disk at the scrapped path"
    );

    // A genuinely NEW path for the same repo must still succeed (the gate
    // is path-specific, not a blanket lockout).
    let fresh = wt_open::open_worktree(OpenOptions {
        repo_root: repo.clone(),
        path: None,
        branch: Some("tachi/reentry/third".into()),
        base: Some("HEAD".into()),
        task: Some("reentry".into()),
        role: Some("worker".into()),
        dispatch_id: None,
        name: Some("reentry-fresh-leaf".into()),
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();
    assert!(
        fresh.opened,
        "a brand-new path must not be blocked by an unrelated scrap record: {:?}",
        fresh.errors
    );

    let _ = wt_clean::run_wt_remove(WtRemoveOptions {
        path: PathBuf::from(&fresh.path),
        force: true,
        output: OutputFormat::Json,
    });
    let _ = std::fs::remove_dir_all(&root);
}

/// tachi#1118 freeze boundary 3 discrimination, second half (tachi#1212
/// fix-round, codex checkpoint 1): a scrapped BRANCH reused at a brand-new
/// path must also be refused, not just the same path reused under a new
/// branch. RED on the pre-fix code: `open_worktree` only consulted the
/// scrap ledger by path, so `open_worktree(scrapped_branch, new_path)`
/// sailed through. GREEN post-fix: the branch-keyed ledger lookup refuses
/// it.
#[test]
fn reopen_refuses_an_old_branch_name_reused_at_a_new_path() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("wt-lifecycle-branch-reentry");
    let home = root.join("home");
    let cache = root.join("cache-worktrees");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    let _env = set_env(&home, &cache);

    let scrapped_branch = "tachi/branch-reentry/scrapped";

    let first_open = wt_open::open_worktree(OpenOptions {
        repo_root: repo.clone(),
        path: None,
        branch: Some(scrapped_branch.into()),
        base: Some("HEAD".into()),
        task: Some("branch-reentry".into()),
        role: Some("worker".into()),
        dispatch_id: None,
        name: Some("branch-reentry-first-leaf".into()),
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();
    assert!(first_open.opened, "setup: first open should succeed");
    let first_path = PathBuf::from(&first_open.path);

    wt_clean::run_wt_remove(WtRemoveOptions {
        path: first_path.clone(),
        force: true,
        output: OutputFormat::Json,
    })
    .expect("setup: scrapping the clean worktree should succeed");

    // Reopening the SAME branch at a DIFFERENT (brand-new) path must be
    // refused: the freeze contract requires NEW branch AND NEW path, not
    // either alone.
    let reentry = wt_open::open_worktree(OpenOptions {
        repo_root: repo.clone(),
        path: None,
        branch: Some(scrapped_branch.into()),
        base: Some("HEAD".into()),
        task: Some("branch-reentry".into()),
        role: Some("worker".into()),
        dispatch_id: None,
        name: Some("branch-reentry-second-leaf".into()),
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();

    assert!(
        !reentry.opened,
        "reopening a scrapped branch at a new path must still be refused"
    );
    assert!(
        reentry.errors.iter().any(|e| e.contains("scrapped branch")),
        "expected a same-branch re-entry refusal, got: {:?}",
        reentry.errors
    );
    let reentry_path = PathBuf::from(&reentry.path);
    assert!(
        !reentry_path.exists(),
        "a refused branch re-entry must not leave anything on disk"
    );

    // A genuinely NEW branch name at a genuinely new path must still
    // succeed (the gate is branch-specific, not a blanket lockout on the
    // repo).
    let fresh = wt_open::open_worktree(OpenOptions {
        repo_root: repo.clone(),
        path: None,
        branch: Some("tachi/branch-reentry/fresh".into()),
        base: Some("HEAD".into()),
        task: Some("branch-reentry".into()),
        role: Some("worker".into()),
        dispatch_id: None,
        name: Some("branch-reentry-fresh-leaf".into()),
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();
    assert!(
        fresh.opened,
        "a brand-new branch name must not be blocked by an unrelated scrap record: {:?}",
        fresh.errors
    );

    let _ = wt_clean::run_wt_remove(WtRemoveOptions {
        path: PathBuf::from(&fresh.path),
        force: true,
        output: OutputFormat::Json,
    });
    let _ = std::fs::remove_dir_all(&root);
}

/// tachi#1118 freeze boundary 3, write-lane entry gate discrimination
/// (codex checkpoint 3: this gate — wt_open.rs, the `git status
/// --porcelain` check immediately after `git worktree add` succeeds — had
/// NO dedicated behavioral test before this fix-round). Simulated
/// deterministically via a `post-checkout` git hook that writes a file
/// into the just-created worktree: `git worktree add` genuinely invokes
/// `post-checkout` with the new worktree as its cwd (empirically
/// confirmed against the installed git), so this is a real end-to-end
/// reproduction of "a surviving writer interleaved with this open" — not
/// a test-only injection point grafted onto production code.
#[test]
fn open_refuses_a_worktree_left_dirty_by_a_post_checkout_hook() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("wt-lifecycle-entry-gate");
    let home = root.join("home");
    let cache = root.join("cache-worktrees");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);

    let hooks_dir = repo.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join("post-checkout");
    std::fs::write(
        &hook_path,
        "#!/bin/sh\necho interleaved-write > ./interloper.txt\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&hook_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&hook_path, perms).unwrap();

    let _env = set_env(&home, &cache);

    let report = wt_open::open_worktree(OpenOptions {
        repo_root: repo.clone(),
        path: None,
        branch: Some("tachi/entry-gate/worker".into()),
        base: Some("HEAD".into()),
        task: Some("entry-gate".into()),
        role: Some("worker".into()),
        dispatch_id: None,
        name: Some("entry-gate-leaf".into()),
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();

    assert!(
        !report.opened,
        "a worktree left dirty immediately after `git worktree add` must be refused, not \
         handed off to a write lane: {:?}",
        report.errors
    );
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("write-lane entry gate")),
        "expected the write-lane entry gate refusal, got: {:?}",
        report.errors
    );
    let path = PathBuf::from(&report.path);
    assert!(
        path.exists() && path.join("interloper.txt").exists(),
        "detection only (tachi#1062 stays sealed): the dirty tree must be LEFT IN PLACE for \
         inspection, never auto-deleted"
    );
    assert!(
        !report.registered,
        "a refused entry-gate open must never be registered"
    );
    assert!(
        !registry::registry_contains(&path),
        "a refused entry-gate open must not appear in the worktree registry"
    );

    // Cleanup: remove the raw git worktree directly (bypassing tachi's own
    // dirty guard, which would otherwise correctly refuse this cleanup
    // too) so the temp root can be reclaimed.
    let _ = Command::new("git")
        .args([
            "-C",
            repo.to_str().unwrap(),
            "worktree",
            "remove",
            "--force",
            path.to_str().unwrap(),
        ])
        .status();
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
        cargo_target: CargoTargetPolicy::Shared,
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

/// tachi#1212 fix-round, codex checkpoint 5 discrimination: sweep's
/// scrap-ledger recording and wt-open's re-entry lookup must agree on the
/// canonical form of a path, or a symlinked root (macOS `/tmp` ->
/// `/private/tmp`) silently defeats the re-entry gate for anything reclaimed
/// via `sweep` instead of a direct `wt-remove`. Deliberately anchors the
/// managed root under a LITERAL `/tmp/...` path (not `std::env::temp_dir()`,
/// which on macOS is usually already a resolved `TMPDIR` outside `/tmp`) so
/// this test actually exercises the divergence the review named. RED on the
/// pre-fix sweep code (recorded `candidate.path` raw, e.g. `/tmp/...`, while
/// `wt_open`'s lookup canonicalizes the query to `/private/tmp/...`): the
/// reopen below would have SUCCEEDED. GREEN post-fix: sweep canonicalizes
/// before recording, so the lookup matches and the reopen is refused.
#[test]
fn sweep_records_a_canonical_path_matching_wt_open_reentry_lookup() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let root = PathBuf::from(format!(
        "/tmp/wt-lifecycle-sweep-canon-{}-{}",
        std::process::id(),
        nanos
    ));
    let _ = std::fs::remove_dir_all(&root);
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
        branch: Some("tachi/sweep-canon/worker".into()),
        base: Some("HEAD".into()),
        task: Some("sweep-canon".into()),
        role: Some("worker".into()),
        dispatch_id: None,
        name: Some("sweep-canon-leaf".into()),
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();
    assert!(
        open_report.opened,
        "setup: worktree should have opened: {:?}",
        open_report.errors
    );
    let path = PathBuf::from(&open_report.path);
    assert!(
        path.starts_with("/tmp"),
        "test setup must place the worktree under a literal /tmp path to exercise the macOS \
         symlink divergence, got {}",
        path.display()
    );

    // max_age_days = 0 makes every marked worktree an immediate sweep
    // candidate regardless of real elapsed time.
    sweep::run_sweep(SweepOptions {
        roots: vec![cache.clone()],
        max_age_days: 0,
        force: true,
        output: OutputFormat::Text,
    })
    .expect("sweep should reclaim a clean, unheld, aged-out worktree");
    assert!(
        !path.exists(),
        "setup: sweep should have reclaimed the worktree"
    );

    // Reopening at the exact same path must be refused. This only proves
    // the fix if sweep recorded the CANONICAL form of the path — the same
    // form wt_open's lookup canonicalizes the query to.
    let reentry = wt_open::open_worktree(OpenOptions {
        repo_root: repo.clone(),
        path: Some(path.clone()),
        branch: Some("tachi/sweep-canon/second".into()),
        base: Some("HEAD".into()),
        task: Some("sweep-canon".into()),
        role: Some("worker".into()),
        dispatch_id: None,
        name: None,
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();

    assert!(
        !reentry.opened,
        "reopening the exact path sweep just scrapped must be refused, even though sweep (not \
         wt-remove) reclaimed it: {:?}",
        reentry.errors
    );
    assert!(
        reentry.errors.iter().any(|e| e.contains("scrapped")),
        "expected a same-path re-entry refusal, got: {:?}",
        reentry.errors
    );

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
        cargo_target: CargoTargetPolicy::Shared,
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
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();

    assert!(!report.opened, "must not open a traversal path");
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("traversal") || e.contains("..")),
        "expected a traversal refusal, got: {:?}",
        report.errors
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `git status --porcelain` entries for `worktree_root`, excluding the
/// Tachi marker file (`.tachi-worktree.json`) that `wt_open::open_worktree`
/// (via `registry::register_worktree`) unconditionally writes into a freshly
/// opened, registered worktree — a legitimate untracked file, not evidence
/// of dirtiness. This mirrors the production definition of "clean" used by
/// `wt_clean::dirty_entries_excluding_marker` (which this integration test,
/// a separate crate, cannot call directly since it is `pub(crate)`); a raw,
/// unfiltered `git status --porcelain` here would spuriously flag every
/// registered worktree as dirty regardless of what the test itself does.
fn porcelain_entries_excluding_marker(worktree_root: &Path) -> Vec<String> {
    let out = Command::new("git")
        .args([
            "-C",
            worktree_root.to_str().unwrap(),
            "status",
            "--porcelain",
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| {
            let path = line.get(3..).unwrap_or(line).trim();
            path != ".tachi-worktree.json"
        })
        .map(|line| line.to_string())
        .collect()
}

fn paths_match(a: &str, b: &Path) -> bool {
    let a_canon = std::fs::canonicalize(a).unwrap_or_else(|_| PathBuf::from(a));
    let b_canon = std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    a_canon == b_canon
}
