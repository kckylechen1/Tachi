use std::path::{Path, PathBuf};

/// Create repo-local DB fixtures outside OS temp roots. Production skip logic
/// intentionally drops `/.tachi/memory.db` under `/tmp` and `/private/tmp`.
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
        .find(|candidate| !has_tmp_skip_prefix(candidate))
        .unwrap_or_else(|| PathBuf::from("target"))
}

fn has_tmp_skip_prefix(path: &Path) -> bool {
    let path_lower = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    path_lower.starts_with("/tmp/") || path_lower.starts_with("/private/tmp/")
}

pub(crate) fn assert_repo_local_db_fixture_not_skipped(path: &Path) {
    assert!(
        crate::manifest::should_skip_path(path).is_none(),
        "repo-local DB test fixture must not be hidden by temporary-workspace skip rules: {}",
        path.display()
    );
}
