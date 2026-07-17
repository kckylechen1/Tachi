//! Scrap ledger (tachi#1118 same-path re-entry gate; freeze boundary 3 in
//! the 2026-07-16 leader adjudication comment).
//!
//! Every successful worktree removal (`wt-remove` direct close, or `sweep`
//! reclaim) appends a `{path, branch, scrapped_at}` record here. `wt-open`
//! consults it before creating a new worktree: a scrapped tree is
//! re-opened only under a NEW branch name AND a NEW path, never the exact
//! path a surviving writer might still hold a reference to.
//!
//! 2026-07-15 live incident (documented on tachi#1118): a tree was removed
//! and rebuilt at the SAME path, and a still-alive writer from before the
//! rebuild kept writing into the new checkout via an absolute path it had
//! cached — the smoking gun was a "fix" to a dangling brace the zombie had
//! left mid-migration landing in the *new* tree. Refusing same-path reuse
//! closes that interleaving route.
//!
//! Fail-closed integrity boundary (tachi#1212 fix-round for the 2026-07-17
//! cross-vendor review): callers now RECORD the scrap BEFORE performing the
//! destructive `git worktree remove`, not after, and abort the removal if
//! the record write fails — a removal that "succeeds" but leaves the ledger
//! with no memory of it is exactly what would let a scrapped path/branch
//! silently become reusable. This does not contradict "never undo a removal
//! that already happened": with recording-before-removal, the removal has
//! NOT happened yet at the point a write failure is detected, so there is
//! nothing to undo — the tree is simply left in place, same as any other
//! refused close-predicate outcome.
//!
//! Reads are fail-closed too: a malformed (unparsable) line anywhere in the
//! ledger makes a lookup inconclusive for every query, not just the record
//! it belongs to — mirrors the `HolderEvidence::Unknown` precedent
//! (`holder.rs`): "could not positively rule this out" must never collapse
//! to "therefore no match", the same earned-not-defaulted discipline this
//! module's callers apply to OS-view holder evidence.

use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScrapRecord {
    pub path: String,
    pub branch: String,
    pub scrapped_at: String,
}

fn ledger_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| "neither HOME nor USERPROFILE is set".to_string())?;
    Ok(PathBuf::from(home)
        .join(".tachi")
        .join("worktrees_scrapped.jsonl"))
}

/// Append a scrap record for a worktree that is ABOUT TO BE removed.
/// Callers must call this BEFORE running the destructive `git worktree
/// remove` and must abort the removal (never proceed) when this returns
/// `Err` — the fail-closed integrity boundary this ledger exists to
/// provide (tachi#1212 fix-round). A write failure never undoes anything,
/// because at the point it is detected the removal has not happened yet.
pub fn record_scrap(path: &Path, branch: &str) -> Result<(), String> {
    let ledger = ledger_path()?;
    if let Some(parent) = ledger.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("create ledger dir: {err}"))?;
    }
    let record = ScrapRecord {
        path: path.display().to_string(),
        branch: branch.to_string(),
        scrapped_at: chrono::Utc::now().to_rfc3339(),
    };
    let line =
        serde_json::to_string(&record).map_err(|err| format!("serialize scrap record: {err}"))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&ledger)
        .map_err(|err| format!("open ledger: {err}"))?;
    writeln!(file, "{line}").map_err(|err| format!("write ledger: {err}"))
}

/// Look up the most recent scrap record whose recorded path lexically
/// equals `path` (both sides are absolute paths produced by this same
/// tool; a scrapped path no longer exists on disk once removed, so this
/// intentionally compares the literal string form rather than
/// `canonicalize`, which would fail on a path that is no longer there).
pub fn find_scrap_by_path(path: &Path) -> Result<Option<ScrapRecord>, String> {
    let needle = path.display().to_string();
    scan_ledger(|record| record.path == needle)
}

/// Look up the most recent scrap record whose recorded branch exactly
/// equals `branch`, regardless of path. Used to enforce the "NEW branch
/// name AND NEW path" half of the freeze contract's same-path re-entry gate
/// (tachi#1212 fix-round, codex checkpoint 1): a scrapped branch reused at a
/// different path is just as much a re-entry route as the same path reused
/// under a different branch would be, since either one lets a surviving
/// writer's cached reference (by path OR by branch name) collide with the
/// freshly reopened tree.
pub fn find_scrap_by_branch(branch: &str) -> Result<Option<ScrapRecord>, String> {
    scan_ledger(|record| record.branch == branch)
}

/// Shared scan over the ledger, returning the most recent record matching
/// `matches`. Fail-closed on malformed content (tachi#1212 fix-round): a
/// line that fails to parse as a [`ScrapRecord`] makes the WHOLE lookup
/// inconclusive (`Err`), not just that one line's record silently
/// disappearing — an unparsable line could have been the very match a
/// caller is depending on to refuse a re-entry, and silently skipping it
/// would defeat the re-entry gate on exactly the corrupted-ledger case that
/// most needs it to hold. Callers must treat `Err` here the same way
/// `HolderEvidence::Unknown` is treated: never equivalent to "no match
/// found".
fn scan_ledger(matches: impl Fn(&ScrapRecord) -> bool) -> Result<Option<ScrapRecord>, String> {
    let ledger = ledger_path()?;
    if !ledger.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&ledger).map_err(|err| format!("read ledger: {err}"))?;
    let mut found = None;
    for (lineno, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let record = serde_json::from_str::<ScrapRecord>(line).map_err(|err| {
            format!(
                "corrupt scrap ledger at {} line {}: {err}; refusing to trust an incomplete \
                 scan rather than silently ignore a line that may have been the match",
                ledger.display(),
                lineno + 1
            )
        })?;
        if matches(&record) {
            found = Some(record); // keep scanning: last write wins
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct HomeGuard(Option<std::ffi::OsString>);
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    fn set_home() -> (HomeGuard, PathBuf) {
        let old = std::env::var_os("HOME");
        let root = std::env::temp_dir().join(format!(
            "tachi-clean-scrap-ledger-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("HOME", &root);
        (HomeGuard(old), root)
    }

    #[test]
    fn round_trips_a_scrap_record_by_path() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let (_home_guard, root) = set_home();

        let path = PathBuf::from("/tmp/example/worktree-a");
        assert!(find_scrap_by_path(&path).unwrap().is_none());

        record_scrap(&path, "tachi/example/worker-abc").unwrap();

        let found = find_scrap_by_path(&path).unwrap();
        assert!(found.is_some(), "just-recorded scrap must be found");
        assert_eq!(found.unwrap().branch, "tachi/example/worker-abc");

        // A distinct path must never match.
        assert!(
            find_scrap_by_path(&PathBuf::from("/tmp/example/worktree-b"))
                .unwrap()
                .is_none()
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn later_record_for_the_same_path_wins() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let (_home_guard, root) = set_home();

        let path = PathBuf::from("/tmp/example/worktree-c");
        record_scrap(&path, "tachi/first/worker").unwrap();
        record_scrap(&path, "tachi/second/worker").unwrap();

        let found = find_scrap_by_path(&path).unwrap().unwrap();
        assert_eq!(found.branch, "tachi/second/worker");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn finds_a_scrap_record_by_branch_regardless_of_path() {
        // Discrimination for tachi#1212 checkpoint 1: the re-entry gate
        // must also catch an old branch name reused at a brand-new path,
        // not just the same path reused under a new branch.
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let (_home_guard, root) = set_home();

        record_scrap(
            &PathBuf::from("/tmp/example/branch-lookup-path1"),
            "tachi/branch-reuse/worker",
        )
        .unwrap();

        let found = find_scrap_by_branch("tachi/branch-reuse/worker")
            .unwrap()
            .expect("branch lookup must find the record regardless of the queried path");
        assert_eq!(found.path, "/tmp/example/branch-lookup-path1");

        assert!(
            find_scrap_by_branch("tachi/never-scrapped/worker")
                .unwrap()
                .is_none(),
            "an unrelated branch must never match"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_malformed_ledger_line_fails_closed_instead_of_being_silently_skipped() {
        // Discrimination for tachi#1212 checkpoint 4 (fail-open on the
        // integrity boundary): a corrupt line anywhere in the ledger must
        // make the WHOLE lookup inconclusive, never silently degrade to
        // "that record doesn't exist" the way a `filter_map`-style skip
        // would. RED on the pre-fix code, which used `if let Ok(record) =
        // ...` to drop unparsable lines with no error at all.
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let (_home_guard, root) = set_home();

        record_scrap(
            &PathBuf::from("/tmp/example/before-corruption"),
            "tachi/before/worker",
        )
        .unwrap();

        let ledger = root.join(".tachi").join("worktrees_scrapped.jsonl");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&ledger)
            .unwrap();
        writeln!(file, "{{not valid json at all").unwrap();
        drop(file);

        let err = find_scrap_by_path(&PathBuf::from("/tmp/example/before-corruption"))
            .expect_err("a corrupt line must fail the lookup closed, not silently skip it");
        assert!(
            err.contains("corrupt scrap ledger"),
            "error should name the corruption, got: {err}"
        );

        let err = find_scrap_by_branch("tachi/before/worker")
            .expect_err("branch lookup must fail closed on the same corrupt ledger");
        assert!(err.contains("corrupt scrap ledger"), "got: {err}");

        let _ = std::fs::remove_dir_all(&root);
    }
}
