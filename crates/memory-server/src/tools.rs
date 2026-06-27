//! Tool implementations for the Memory MCP server.
//!
//! Extracted from `main.rs` (Phase 4 of v1.0 cleanup) to keep the crate root
//! focused on bootstrap/state and delegate the ~120 `#[tool]` wrappers here.
//! Every method in this file is a thin shim that delegates to a `handle_*`
//! function in one of the `*_ops` siblings — no business logic lives here.

use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration as StdDuration, Instant};

use crate::arena_ops::handle_tachi_arena;
use crate::capability_ops::handle_prepare_capability_bundle;
use crate::copilot_ops::{
    handle_tachi_feature_briefing, handle_tachi_wiki_search, handle_tachi_wiki_write,
};
use crate::event_ops::handle_tachi_event;
use crate::gh_ops::handle_tachi_gh;
use crate::hub_ops::{handle_hub_discover, handle_run_skill, handle_skill_from_pattern};
use crate::tool_params::*;
use crate::verify_ops::handle_tachi_verify;
use crate::wiki_ops::{
    collect_wiki_browse_value, collect_wiki_read_value, collect_wiki_search_value,
    handle_wiki_browse, handle_wiki_ingest, handle_wiki_lint, handle_wiki_read, handle_wiki_search,
};
use crate::MemoryServer;

const TASK_WAIT_INITIAL_POLL_DELAY: StdDuration = StdDuration::from_millis(250);
const TASK_WAIT_MAX_POLL_DELAY: StdDuration = StdDuration::from_secs(2);

mod agent_profile_facade;
mod continuity_facade;
mod copilot_facade;
mod dispatch_complete_defaults;
mod dispatch_facade;
mod domain_facade;
mod formatting;
mod graph_state_facade;
mod handoff_facade;
mod hub_facade;
mod kanban_facade;
mod memory_facade;
mod pack_facade;
mod pipeline_facade;
mod runtime_context_facade;
mod sandbox_facade;
mod skill_discovery;
mod skill_facade;
mod task_facade;
mod task_router;
mod vault_facade;
mod wiki_facade;
mod workflow_facade;

#[cfg(test)]
mod tests;

use self::dispatch_complete_defaults::*;
use self::formatting::*;
use self::skill_discovery::*;
use self::skill_facade::*;
use self::task_facade::*;
use self::task_router::*;

pub(crate) use self::task_facade::build_task_pr_status_gh_params;
#[cfg(test)]
pub(crate) use self::task_facade::resolve_task_pr_status_target;
