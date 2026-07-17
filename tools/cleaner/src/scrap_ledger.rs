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
//! Best-effort by design: a ledger write failure must never fail (or
//! silently undo) a removal that has already happened; callers surface it
//! as a warning, not an error.

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

/// Append a scrap record for a worktree that was just removed. Best-effort:
/// callers must not fail the removal itself on a ledger write error.
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
    let ledger = ledger_path()?;
    if !ledger.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&ledger).map_err(|err| format!("read ledger: {err}"))?;
    let needle = path.display().to_string();
    let mut found = None;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<ScrapRecord>(line) {
            if record.path == needle {
                found = Some(record); // keep scanning: last write wins
            }
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
}
