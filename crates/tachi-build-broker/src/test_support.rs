//! Private test-only fixture root helper.
//!
//! Duplicated from `tachi-server::test_support::non_skipped_fixture_tempdir`
//! (#1702 carve-4). A shared test-support crate is a separate #1611 Track T
//! side quest — this leaf preserves identical `CARGO_TARGET_DIR` base
//! resolution rather than substituting `tempfile::tempdir()`.

use std::path::{Path, PathBuf};

/// Create repo-local DB fixtures outside OS temp roots and git worktrees.
/// Production skip logic intentionally drops `/.tachi/memory.db` under
/// `/tmp` and `/private/tmp`; root-resolution tests additionally need a path
/// that cannot walk upward into a repository through Cargo's in-tree target.
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

fn has_git_ancestor(path: &Path) -> bool {
    path.ancestors()
        .find(|candidate| candidate.exists())
        .and_then(find_git_root_from)
        .is_some()
}

/// Local copy of the git-root walk used by the server helper (duplicated so
/// this crate has no `tachi-server` test edge). Linked-worktree `.git` files
/// resolve to the primary checkout root.
fn find_git_root_from(path: impl AsRef<Path>) -> Option<PathBuf> {
    let mut dir = path.as_ref().canonicalize().ok()?;
    if dir.is_file() {
        dir.pop();
    }
    loop {
        if dir.join(".git").exists() {
            return normalize_git_root(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn normalize_git_root(root: PathBuf) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Some(root);
    }
    if !dot_git.is_file() {
        return Some(root);
    }

    if let Some(primary) = linked_worktree_primary_root(&root, &dot_git) {
        return Some(primary);
    }
    Some(root)
}

fn linked_worktree_primary_root(root: &Path, dot_git: &Path) -> Option<PathBuf> {
    let raw = std::fs::read_to_string(dot_git).ok()?;
    let gitdir = raw.trim().strip_prefix("gitdir:")?.trim();
    let gitdir = resolve_relative_path(root, gitdir).canonicalize().ok()?;
    let commondir_raw = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let commondir = resolve_relative_path(&gitdir, commondir_raw.trim())
        .canonicalize()
        .ok()?;
    if commondir.file_name().and_then(|name| name.to_str()) == Some(".git") {
        commondir.parent()?.canonicalize().ok()
    } else {
        None
    }
}

fn resolve_relative_path(base: &Path, raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}
