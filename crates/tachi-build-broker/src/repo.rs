//! Repo identity for tickets and seats (#894 S2c item 3, round-2).
//!
//! ## Why a ticket needs an identity and not a path
//!
//! The queue is machine-wide (one `hard_state` table in the global store) but a
//! seat is per-repo: `tachi build run --repo X` resolves the seat from `X` and
//! then drains tickets. Round-1 drained the queue *unfiltered* — so a ticket
//! submitted for repo A could be handed to the seat of repo B, which would
//! cheerfully try to `git checkout --detach <A's sha>` inside B's checkout. It
//! either fails (unknown revision) or, if the sha happens to exist in both
//! (shared history, forks), builds the wrong tree and files the receipt against
//! the wrong repo.
//!
//! So the queue is filtered by repo — and the thing it filters on must be an
//! *identity*, not the string the caller typed:
//!
//! - `~/Projects/Sigil` and `/Users/me/Projects/Sigil` are the same repo
//!   (symlink);
//! - `/repo` and `/repo/` are the same repo (trailing slash);
//! - a **linked worktree** of a repo is the same repo for building purposes —
//!   its shas live in the same object store, and the seat (itself a linked
//!   worktree, created from the main root) can check any of them out. Treating
//!   two worktrees of one repo as two repos would give them two seats, two
//!   resident targets, and defeat the whole point of a machine-unique executor.
//!
//! [`repo_identity`] answers with git's *common dir* — the one `.git` directory
//! every worktree of a repo shares — mapped back to the main worktree root. That
//! is the same string for every worktree of a repo and different for every other
//! repo on the machine. When git cannot answer (not a repo, no git on PATH), it
//! falls back to the canonicalized path, and when even that fails, to the
//! trimmed input: a degraded identity is still an identity, and the comparison
//! stays exact-match either way (never a prefix/substring test, which is how a
//! path check turns into a confused-deputy bug).

use std::path::{Path, PathBuf};

/// Resolve a user-supplied repo path to this machine's identity for that repo.
///
/// Absolute, canonical, and stable across the repo's linked worktrees.
pub fn repo_identity(path: &Path) -> Result<String, String> {
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("resolve repo {}: {e}", path.display()))?;

    match git_common_dir(&canonical) {
        Some(common_dir) => {
            // `<main-root>/.git` (normal repo) → the main root. A bare repo has
            // no worktree root; its own path IS the identity.
            let root = if common_dir.file_name().and_then(|n| n.to_str()) == Some(".git") {
                common_dir
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or(common_dir)
            } else {
                common_dir
            };
            let root = root.canonicalize().unwrap_or(root);
            Ok(normalize(&root.display().to_string()))
        }
        // Not a git repo (or git is unavailable): the canonical path is the best
        // identity available. Every ticket and every seat goes through this same
        // function, so both sides degrade the same way and still match.
        None => Ok(normalize(&canonical.display().to_string())),
    }
}

/// Ask git for the directory every worktree of this repo shares.
fn git_common_dir(repo: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }
    Some(PathBuf::from(raw))
}

/// Strip trailing separators so `/repo` and `/repo/` compare equal. (Not a
/// lowercase/normalize-case step: paths are case-sensitive on the platforms this
/// runs on, and folding case would merge two genuinely different dirs.)
fn normalize(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() > 1 {
        trimmed.trim_end_matches('/').to_string()
    } else {
        trimmed.to_string()
    }
}

/// Does this ticket's repo identity match the seat's? `None` = no filter (every
/// repo), which is what the machine-wide views (`status`'s global counts) use.
///
/// Exact match on the normalized identity — never a prefix test. `/repo` must
/// not match `/repo-2`, and `/repo` must not "contain" `/repo/sub`.
pub(crate) fn same_repo(ticket_repo: &str, seat_repo: Option<&str>) -> bool {
    match seat_repo {
        None => true,
        Some(seat) => normalize(ticket_repo) == normalize(seat),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trailing_slash_is_the_same_repo() {
        assert!(same_repo("/repo", Some("/repo/")));
        assert!(same_repo("/repo/", Some("/repo")));
    }

    #[test]
    fn a_prefix_is_not_the_same_repo() {
        // The confused-deputy shape: a prefix/substring test here would let a
        // ticket for /repo-2 (or /repo/sub) be drained by /repo's seat.
        assert!(!same_repo("/repo-2", Some("/repo")));
        assert!(!same_repo("/repo/sub", Some("/repo")));
        assert!(!same_repo("/other", Some("/repo")));
    }

    #[test]
    fn no_filter_matches_every_repo() {
        assert!(same_repo("/repo", None));
        assert!(same_repo("/anything/else", None));
    }

    /// Two linked worktrees of one repo must resolve to ONE identity — otherwise
    /// each worktree would get its own executor seat and its own resident target,
    /// which is exactly the N-targets sprawl the broker exists to end.
    #[test]
    fn linked_worktrees_of_one_repo_share_an_identity() {
        let tmp = crate::test_support::non_skipped_fixture_tempdir("repo-identity-");
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).unwrap();

        let git = |args: &[&str], cwd: &std::path::Path| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(args)
                .output()
                .expect("git runs");
            assert!(
                ok.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&ok.stderr)
            );
        };
        git(&["init", "-q"], &main);
        git(&["config", "user.email", "t@t"], &main);
        git(&["config", "user.name", "t"], &main);
        std::fs::write(main.join("f"), "x").unwrap();
        git(&["add", "."], &main);
        git(&["commit", "-qm", "c"], &main);

        let linked = tmp.path().join("linked");
        git(
            &[
                "worktree",
                "add",
                "--detach",
                linked.to_str().unwrap(),
                "HEAD",
            ],
            &main,
        );

        let from_main = repo_identity(&main).unwrap();
        let from_linked = repo_identity(&linked).unwrap();
        assert_eq!(
            from_main, from_linked,
            "a linked worktree is the same repo: its shas live in the same object store and one \
             seat serves both"
        );
        assert!(same_repo(&from_linked, Some(&from_main)));
    }

    #[test]
    fn a_non_repo_dir_still_gets_a_stable_identity() {
        let tmp = crate::test_support::non_skipped_fixture_tempdir("repo-identity-plain-");
        let a = repo_identity(tmp.path()).unwrap();
        let b = repo_identity(tmp.path()).unwrap();
        assert_eq!(a, b);
        assert!(std::path::Path::new(&a).is_absolute());
    }
}
