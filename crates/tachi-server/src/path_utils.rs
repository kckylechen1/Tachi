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
    canonical_db_leaf_exists_without_symlink, plan_c_alias_db_for_root, plan_c_dir_name_from_root,
    plan_c_existing_alias_db_for_root, plan_c_existing_named_alias_db, plan_c_global_db_path,
    plan_c_global_db_path_existing, plan_c_legacy_dir_name_from_root,
    plan_c_previous_dir_name_from_root, plan_c_previous_raw_dir_name_from_root,
    plan_c_project_root_from_local_db, resolve_project_db_path,
};
pub(crate) use home::tachi_home;
pub(crate) use named::{list_named_projects, named_project_for_db_path, named_project_from_path};
pub(crate) use symlink::{
    ensure_plan_c_symlink, inspect_plan_c_alias, inspect_plan_c_alias_for_local_db,
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
