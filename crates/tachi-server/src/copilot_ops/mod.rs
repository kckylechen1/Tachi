use crate::memory_search_ops::search_memory_rows;
use crate::server_state::{DbScope, MemoryServer};
use crate::tool_params::{
    build_candidate_knowledge_artifact_fields, build_evidence_refs_v1,
    derive_effective_knowledge_artifact, EffectiveKnowledgeArtifactV1, ProgressCheckParams,
    SaveMemoryParams, SearchMemoryParams, StoreRef, TachiBoardParams, TachiTaskParams,
    TaskBriefParams, WikiApplicabilityStatusV1, WikiKnowledgeScopeV1, WikiLifecycleV1,
    WikiReadPlan, WikiSearchParams, WikiWriteParams,
};
use chrono::Utc;
use memcore::{MemoryEntry, MemoryStore};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[cfg(test)]
use memcore::HubCapability;

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

pub(crate) use self::feature_briefing::handle_tachi_feature_briefing;
#[cfg(test)]
pub(crate) use self::feature_briefing::handle_tachi_task_brief;
pub(crate) use self::progress_check::handle_tachi_progress_check;
pub(crate) use self::support::{
    is_wiki_projection_duplicate, wiki_parent_path, wiki_projection_supersedes_edge,
};
pub(crate) use self::task_routing::{build_task_brief_routing, TaskBriefRouting};
pub(crate) use self::wiki_facade::{
    handle_tachi_wiki_search, handle_tachi_wiki_write,
    handle_tachi_wiki_write_with_model_invocation,
};
