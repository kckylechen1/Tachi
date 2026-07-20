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
    plan_c_dir_name_from_root, plan_c_global_db_path, plan_c_global_db_path_existing,
    plan_c_legacy_dir_name_from_root, plan_c_project_root_from_local_db, resolve_project_db_path,
};
pub(crate) use home::tachi_home;
pub(crate) use named::{list_named_projects, named_project_for_db_path, named_project_from_path};
pub(crate) use symlink::{
    ensure_plan_c_symlink, plan_c_split_brain, plan_c_split_brain_for_local_db,
};
pub(crate) use types::{PlanCLinkOutcome, PlanCSplitBrain};

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
