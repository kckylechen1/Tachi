use crate::MemoryServer;
use crate::SaveMemoryParams;
use crate::TachiDispatchParams;
use chrono::Utc;
use serde_json::json;
use std::time::Duration;
use tokio::process::Command;

mod acp_native;
mod acpx;
mod board;
mod dispatch;
mod dispatch_v2;
mod harness;
mod kanban_helpers;
mod mcp_config;
mod merge;
mod prompt;
mod subprocess;

// Re-exports preserving the legacy public surface so external callers
// (`tools.rs`, `shell_ops.rs`, `complete_ops.rs`, `tests.rs`) keep
// resolving symbols via `crate::dispatch_ops::<name>`.
pub(crate) use acpx::run_acpx_control_from_status;
pub(crate) use board::{collect_run_task_for_server, handle_tachi_board};
#[cfg(test)]
pub(crate) use dispatch::apply_unlocked_vault_env;
pub(crate) use dispatch::handle_tachi_dispatch;
#[cfg(test)]
pub(crate) use dispatch::new_dispatch_id;
pub(crate) use dispatch::recover_orphaned_dispatch_runs;
pub(crate) use harness::{harness_server_attach_ready, probe_harness_server_status};
#[cfg(test)]
pub(crate) use kanban_helpers::should_cleanup_run;
pub(crate) use kanban_helpers::update_kanban_state;
pub(crate) use merge::handle_approve_merge;
#[cfg(test)]
pub(crate) use merge::{
    evaluate_delete_worktree_safety, resolve_tachi_clean_bin, validate_merge_branch_name,
    validate_static_merge_safety, worktree_equals_repo_root,
};
#[cfg(test)]
pub(crate) use prompt::{assemble_prompt, assemble_prompt_with_trace};
