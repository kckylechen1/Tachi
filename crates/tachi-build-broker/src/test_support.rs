//! Private test fixture helper ported from `tachi-server::test_support`
//! (#1702 carve 4). Intentional duplication — a shared test-support crate is
//! a separate #1611 Track T side quest.

use std::path::{Path, PathBuf};

/// Create a tempdir under a base that will not be skipped by repo-local DB
/// fixture rules (`/tmp` prefixes and git-worktree ancestors are rejected).
pub(crate) fn non_skipped_fixture_tempdir(prefix: &str) -> tempfile::TempDir {
    let base = non_skipped_fixture_base().join("repo-local-db-fixtures");
    std::fs::create_dir_all(&base).expect("repo-local DB fixture base");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(&base)
        .expect("repo-local DB fixture tempdir")
}

fn non_skipped_fixture_base() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_target = manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(|repo| repo.join("target"));

    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .into_iter()
        .chain(repo_target)
        .chain(dirs::home_dir().map(|home| home.join(".cache/sigil-repo-local-db-fixtures")))
        .map(absolutize)
        .find(|candidate| !has_tmp_skip_prefix(candidate) && !has_git_ancestor(candidate))
        .expect("repo-local DB tests need a fixture base outside temp roots and git worktrees")
}

/// Resolve a candidate to an absolute path *before* it is fed to
/// [`has_git_ancestor`]. `CARGO_TARGET_DIR` can legally hold a relative value
/// (e.g. `target/fixture-base`); this crate's own build convention points it
/// at a shared external cache (see repo `AGENTS.md`), so a checkout routinely
/// has no local `target/` directory at all. When that's true, `has_git_ancestor`'s
/// `path.ancestors().find(|c| c.exists())` walk — run on the relative path
/// exactly as given — finds nothing that exists to canonicalize and reports
/// "no git ancestor", even though the same relative path resolves squarely
/// inside the current git worktree once `std::fs::create_dir_all` joins it
/// against the process cwd at use time. Absolutizing against cwd first makes
/// the ancestor walk see the same directories the filesystem will actually
/// use, so an existing ancestor (at minimum cwd itself) is always available
/// to canonicalize and check for `.git`.
fn absolutize(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    }
}

fn has_tmp_skip_prefix(path: &Path) -> bool {
    let path_lower = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    path_lower.starts_with("/tmp/") || path_lower.starts_with("/private/tmp/")
}

/// Boolean outcome matches server's `find_git_root_from` presence check, without
/// calling server-only utils: canonicalize the first existing ancestor, then
/// walk upward for a `.git` entry (file or directory).
fn has_git_ancestor(path: &Path) -> bool {
    path.ancestors()
        .find(|candidate| candidate.exists())
        .and_then(|candidate| {
            let mut dir = candidate.canonicalize().ok()?;
            if dir.is_file() {
                dir.pop();
            }
            loop {
                if dir.join(".git").exists() {
                    return Some(());
                }
                if !dir.pop() {
                    return None;
                }
            }
        })
        .is_some()
}
