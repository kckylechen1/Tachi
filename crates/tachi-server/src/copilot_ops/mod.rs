use crate::memory_search_ops::search_memory_rows;
use crate::server_state::{DbScope, MemoryServer};
use crate::tool_params::{
    build_candidate_knowledge_artifact_fields, build_evidence_refs_v1,
    derive_effective_knowledge_artifact, EffectiveKnowledgeArtifactV1, SaveMemoryParams,
    SearchMemoryParams, StoreRef, TachiBoardParams, TachiTaskParams, WikiApplicabilityStatusV1,
    WikiKnowledgeScopeV1, WikiLifecycleV1, WikiReadPlan, WikiSearchParams, WikiWriteParams,
};
use chrono::Utc;
use memcore::{MemoryEntry, MemoryStore};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

mod feature_briefing;
mod support;
mod task_routing;
mod wiki_facade;

#[cfg(test)]
mod tests;

use self::support::*;
pub(crate) use self::support::{
    guide_applies_to, rank_applicable_guides, GuideApplicabilityContext,
};
use self::wiki_facade::*;

#[cfg(test)]
use self::feature_briefing::*;
#[cfg(test)]
use self::task_routing::*;

pub(crate) use self::feature_briefing::{feature_briefing, handle_tachi_feature_briefing};
pub(crate) use self::task_routing::{build_task_brief_routing, TaskBriefRouting};
pub(crate) use self::wiki_facade::{
    handle_tachi_wiki_search, handle_tachi_wiki_write,
    handle_tachi_wiki_write_with_model_invocation,
};
