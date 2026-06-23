use crate::hub_helpers::review_status_allows_call;
use crate::memory_search_ops::{handle_save_memory, search_memory_rows};
use crate::server_state::{DbScope, MemoryServer};
use crate::tool_params::{
    HybridWeightsParam, ProgressCheckParams, SaveMemoryParams, SearchMemoryParams,
    TachiBoardParams, TachiTaskParams, TaskBriefParams, WikiSearchParams, WikiWriteParams,
};
use chrono::Utc;
use memory_core::{HubCapability, MemoryEntry, MemoryStore};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const DEBUG_CHECKLIST_LIMIT: usize = 4;
const FALLBACK_DEBUG_CHECKLIST: [&str; DEBUG_CHECKLIST_LIMIT] = [
    "Start from the observed error and trace where the invariant first becomes false.",
    "For MCP argument bugs, verify schema -> client serialization -> server deserialization -> handler -> transport in that order.",
    "Do not keep patching the same layer after two failed attempts; reframe or ask another agent.",
    "If stderr/log visibility is weak, add a durable test or inspect the data structure at the API boundary.",
];

const WIKI_DUP_JACCARD_THRESHOLD: f64 = 0.85;

mod feature_briefing;
mod progress_check;
mod support;
mod task_routing;
mod wiki_facade;

#[cfg(test)]
mod tests;

use self::progress_check::*;
use self::support::*;
use self::wiki_facade::*;

#[cfg(test)]
use self::feature_briefing::*;
#[cfg(test)]
use self::task_routing::*;

pub(crate) use self::feature_briefing::{handle_tachi_feature_briefing, handle_tachi_task_brief};
pub(crate) use self::progress_check::handle_tachi_progress_check;
pub(crate) use self::task_routing::{build_task_brief_routing, TaskBriefRouting};
pub(crate) use self::wiki_facade::{handle_tachi_wiki_search, handle_tachi_wiki_write};
