//! Tool implementations for the Memory MCP server.
//!
//! Extracted from `main.rs` (Phase 4 of v1.0 cleanup) to keep the crate root
//! focused on bootstrap/state and delegate the ~120 `#[tool]` wrappers here.
//! Every method in this file is a thin shim that delegates to a `handle_*`
//! function in one of the `*_ops` siblings — no business logic lives here.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration as StdDuration;

use crate::capability_ops::handle_prepare_capability_bundle;
use crate::copilot_ops::{
    handle_tachi_feature_briefing, handle_tachi_wiki_search, handle_tachi_wiki_write,
};
use crate::event_ops::handle_tachi_event;
use crate::gh_ops::handle_tachi_gh;
use crate::hub_ops::{handle_hub_discover, handle_run_skill, handle_skill_from_pattern};
use crate::kanban::{
    handle_check_inbox, handle_post_card, handle_update_card, CheckInboxParams, PostCardParams,
    UpdateCardParams,
};
use crate::tool_params::*;
use crate::verify_ops::handle_tachi_verify;
use crate::wiki_ops::{
    collect_wiki_browse_value, collect_wiki_read_value_for_plan, collect_wiki_search_value,
    handle_wiki_browse, handle_wiki_ingest, handle_wiki_lint, handle_wiki_read_for_plan,
    handle_wiki_search,
};
use crate::MemoryServer;

/// `tachi_task(action='status')` timeout_secs default/cap for the acpx
/// control-plane call (see `task_facade::handle_tachi_task_status`).
/// `pub(crate)` so `cli_client::transport::daemon_call_timeout` can derive
/// the outer RPC timeout from the *same* numbers instead of a hand-mirrored
/// copy that can drift out of sync (see #970, #1028). The `wait`/`cancel`
/// long-poll/control actions left `tachi_task` in #1319-C2; their timeout
/// consts were removed with them.
pub(crate) const TASK_CONTROL_TIMEOUT_DEFAULT_SECS: u64 = 30;
pub(crate) const TASK_CONTROL_TIMEOUT_CAP_SECS: u64 = 300;

// Machine-readable fold-alias registry. Consumed today by the fold-contract
// and tripwire tests; kept as a production module (not test-gated) so future
// runtime surfaces (e.g. a deprecation listing) can read it. Until such a
// runtime consumer exists, suppress dead_code only in non-test builds — test
// builds keep the lint strict so genuinely-unused entries still surface.
mod a2a_facade;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod alias_manifest;
mod component_facade;
mod continuity_facade;
mod dispatch_complete_defaults;
mod dispatch_facade;
pub(crate) mod formatting;
mod handoff_facade;
mod hub_facade;
mod memory_facade;
mod peer_facade;
mod pipeline_facade;
mod runtime_context_facade;
mod sandbox_facade;
mod skill_discovery;
mod skill_facade;
mod task_facade;
mod task_router;
mod tune_facade;
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

#[tool_router(router = copilot_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Prepare a task brief before non-trivial work: relevant wiki lessons, memory hits, intent, selected_sops, tool_plan, lightweight skill suggestions, and debugging checklist."
    )]
    pub(crate) async fn tachi_task_brief(
        &self,
        Parameters(params): Parameters<TaskBriefParams>,
    ) -> Result<String, String> {
        crate::copilot_ops::handle_tachi_task_brief(self, params).await
    }

    #[tool(
        description = "Check whether an agent is stuck after repeated attempts. Returns reframe advice, relevant wiki hits, and an ask-codex prompt when useful. Pass flow_id to append progress.jsonl."
    )]
    pub(crate) async fn tachi_unstick(
        &self,
        Parameters(params): Parameters<ProgressCheckParams>,
    ) -> Result<String, String> {
        crate::copilot_ops::handle_tachi_progress_check(self, params).await
    }
}

#[tool_router(router = kanban_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(description = "Post a kanban card from one agent to another.")]
    pub(crate) async fn post_card(
        &self,
        Parameters(params): Parameters<PostCardParams>,
    ) -> Result<String, String> {
        handle_post_card(self, params).await
    }

    #[tool(description = "Check kanban inbox for a target agent.")]
    pub(crate) async fn check_inbox(
        &self,
        Parameters(params): Parameters<CheckInboxParams>,
    ) -> Result<String, String> {
        handle_check_inbox(self, params).await
    }

    #[tool(description = "Update status of a kanban card.")]
    pub(crate) async fn update_card(
        &self,
        Parameters(params): Parameters<UpdateCardParams>,
    ) -> Result<String, String> {
        handle_update_card(self, params).await
    }
}
