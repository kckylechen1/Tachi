mod cleaner;
mod handler;
mod preview;
mod repo;
mod safety;

pub(crate) use cleaner::remove_worktree_with_cleaner;
pub(crate) use handler::handle_approve_merge;

#[cfg(test)]
pub(crate) use cleaner::resolve_tachi_clean_bin;
#[cfg(test)]
pub(crate) use safety::{
    evaluate_delete_worktree_safety, validate_merge_branch_name, validate_static_merge_safety,
    worktree_equals_repo_root,
};
