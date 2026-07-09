//! Local worktree merge toolkit — `approve_merge`, safety gates, and cleaner
//! integration. Extracted from `tachi-server` `dispatch_ops/merge` (#833).
//!
//! This is the one slice that also weakens the cyclic SCC: `gh_ops` previously
//! reached into `dispatch_ops` only via `remove_worktree_with_cleaner`; after
//! extraction that call crosses a crate boundary to `tachi_merge_ops` instead,
//! removing `gh_ops -> dispatch_ops` as an intra-crate edge.

#![allow(
    clippy::field_reassign_with_default,
    clippy::if_same_then_else,
    clippy::useless_format,
    clippy::collapsible_str_replace
)]

mod cleaner;
mod handler;
mod preview;
mod repo;
mod safety;

pub use cleaner::{remove_worktree_with_cleaner, resolve_tachi_clean_bin, CleanerRemoveReport};
pub use handler::handle_approve_merge;
pub use safety::{
    evaluate_delete_worktree_safety, validate_merge_branch_name, validate_static_merge_safety,
    worktree_equals_repo_root, DeleteWorktreeSafety,
};
