use super::handlers::{
    build_bracket_self_evolution_id, classify_bracket_self_evolution,
    extract_bracket_self_evolution_notes, matches_agent_tag, resolve_capture_target,
};
use super::maintenance::{
    build_distill_edges, classify_distill_guide_type, coherence_bucket_key,
    coherent_distill_buckets, infer_memory_insight, memory_claim_signature,
    scheduled_distill_path_prefix,
};
use super::recall::{parse_compact_context_response, parse_session_capture_response};
use super::{FOUNDRY_DISTILL_SOURCE, FOUNDRY_RELATED_LIMIT};
use crate::manifest::{DbEntry, DbRole, Manifest};
use crate::server_state::DbScope;
use crate::tool_params::{CaptureSessionParams, CompactRollupParams, Message};
use memory_core::MemoryEntry;
use serde_json::json;
use tempfile::tempdir;

fn tachi_home_test_lock() -> &'static std::sync::Mutex<()> {
    crate::utils::global_test_lock()
}

mod bracket_evolution;
mod capture_target;
mod distill_buckets;
mod distill_guides;
mod memory_maintenance;
mod recall_parse_params;
