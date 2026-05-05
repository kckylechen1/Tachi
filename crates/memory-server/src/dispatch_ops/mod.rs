use crate::MemoryServer;
use crate::SaveMemoryParams;
use crate::SearchMemoryParams;
use crate::TachiBoardParams;
use crate::TachiDispatchParams;
use chrono::Utc;
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

mod board;
mod dispatch;
mod kanban_helpers;
mod mcp_config;
mod merge;
mod prompt;
mod subprocess;

// Re-exports preserving the legacy public surface so external callers
// (`tools.rs`, `shell_ops.rs`, `complete_ops.rs`, `tests.rs`) keep
// resolving symbols via `crate::dispatch_ops::<name>`.
pub(crate) use board::handle_tachi_board;
pub(crate) use dispatch::handle_tachi_dispatch;
pub(crate) use kanban_helpers::update_kanban_state;
#[cfg(test)]
pub(crate) use kanban_helpers::should_cleanup_run;
pub(crate) use merge::handle_approve_merge;
