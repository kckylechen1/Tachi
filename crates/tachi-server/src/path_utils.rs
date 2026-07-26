mod alias;
mod home;
mod named;
mod symlink;
mod types;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use alias::validate_project_db_relpath;
pub(crate) use alias::{
    canonical_db_leaf_exists_without_symlink, plan_c_alias_db_for_root_in_home,
    plan_c_dir_name_from_root, plan_c_existing_alias_db_for_root_in_home,
    plan_c_existing_named_alias_db_in_home, plan_c_global_db_path, plan_c_global_db_path_existing,
    plan_c_global_db_path_existing_in_home, plan_c_global_db_path_in_home,
    plan_c_legacy_dir_name_from_root, plan_c_previous_dir_name_from_root,
    plan_c_previous_raw_dir_name_from_root, plan_c_project_root_from_local_db,
    resolve_project_db_path,
};

/// Return whether a persisted project identity is safe to address without
/// lossy normalization. This is shared by named-project routing and manifest
/// scope parsing so stale manifest roles cannot bypass the project DB guard.
pub(crate) fn is_canonical_project_identity(project_name: &str) -> bool {
    !project_name.is_empty()
        && project_name.trim() == project_name
        && !project_name.contains('/')
        && !project_name.contains('\\')
        && !project_name.contains("..")
        && !project_name.starts_with('.')
        && !project_name.eq_ignore_ascii_case("unnamed")
        && project_name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

/// Parse a canonical manifest project scope. Prefix-only or malformed scopes
/// do not carry project authority.
pub(crate) fn canonical_project_scope_hint(scope_hint: &str) -> Option<&str> {
    let project_name = scope_hint.strip_prefix("project:")?;
    is_canonical_project_identity(project_name).then_some(project_name)
}

/// Determine whether a manifest entry names a project DB that must be a
/// canonical regular file. Older manifests can retain `DbRole::Unknown` while
/// preserving a canonical `project:<identity>` scope.
pub(crate) fn manifest_db_is_project_protected(entry: &crate::manifest::DbEntry) -> bool {
    entry.role == crate::manifest::DbRole::Project
        || canonical_project_scope_hint(&entry.scope_hint).is_some()
}

/// Classify a manifest DB leaf without changing canonical non-project path
/// semantics. Project entries are canonical data files and must never be
/// symlinks; global and runtime roles retain target-following `exists` behavior.
pub(crate) fn manifest_db_leaf_exists(entry: &crate::manifest::DbEntry) -> Result<bool, String> {
    if manifest_db_is_project_protected(entry) {
        canonical_db_leaf_exists_without_symlink(std::path::Path::new(&entry.path))
    } else {
        Ok(std::path::Path::new(&entry.path).exists())
    }
}
pub(crate) use home::tachi_home;
pub(crate) use named::{
    list_named_projects, list_named_projects_in_home, named_project_for_db_path,
    named_project_for_db_path_in_home, named_project_from_path_in_home,
};
pub(crate) use symlink::{
    ensure_plan_c_symlink, ensure_plan_c_symlink_in_home, inspect_plan_c_alias_for_local_db,
    inspect_plan_c_alias_for_local_db_in_home, inspect_plan_c_alias_in_home,
};
#[cfg(all(test, unix))]
pub(crate) use symlink::{install_plan_c_symlink_hook_for_test, PlanCSymlinkHookGuard};
pub(crate) use types::{
    PlanCAliasInspection, PlanCAliasIntegrity, PlanCLinkOutcome, PlanCSplitBrain,
};

/// Return the process-cached root of the current Git worktree, when available.
pub(crate) fn cached_git_root() -> Option<&'static std::path::PathBuf> {
    static GIT_ROOT: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    GIT_ROOT
        .get_or_init(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .output()
                .ok()
                .filter(|out| out.status.success())
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(std::path::PathBuf::from)
        })
        .as_ref()
}
