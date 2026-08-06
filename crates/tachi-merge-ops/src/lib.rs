//! Local worktree cleanup bridge used by GitHub safe-merge flows.
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

pub use cleaner::{remove_worktree_with_cleaner, resolve_tachi_clean_bin, CleanerRemoveReport};
