//! Phase 1 — Daily Batch Distill.
//!
//! Replaces the legacy per-capture `MemoryDistill` foundry job. Once per
//! day the bootstrap scheduler invokes [`run_daily_batch_distill`], which:
//!
//! 1. Scans the project DB for unprocessed source memories (those whose
//!    id does not appear in any existing distill memory's
//!    `metadata.source_memory_ids` array).
//! 2. Groups them by (path_prefix, coherence_key) using the same
//!    `coherent_distill_buckets` logic as the legacy scheduler.
//! 3. Builds one mega-prompt per configured batch and dispatches it through
//!    the configured backend.
//! 4. Parses the JSON array response, persists one distill `MemoryEntry`
//!    per group (with full provenance metadata), and writes a
//!    `source_manifest.json` audit file under
//!    `~/.tachi/foundry-runs/distill/<project>/<batch_run_id>/`.
//! 5. On error or unparseable response, splits the batch (API path) or
//!    falls back per-group to single-group [`LlmClient::call_distill_llm`].

mod candidates;
mod config;
mod consolidate_prepass;
mod parser;
mod persist;
mod prompt;
mod runner;
mod types;

pub use config::scrub_agent_noise;
pub use runner::{run_daily_batch_distill, run_daily_batch_distill_with_options};

#[cfg(test)]
use candidates::collect_candidate_groups;
#[cfg(test)]
use config::{
    resolve_batch_size, resolve_candidate_scan_limit, resolve_distill_backend,
    resolve_processed_scan_limit, DistillBackend, DEFAULT_CANDIDATE_SCAN_LIMIT,
    DEFAULT_GROUPS_PER_BATCH, DEFAULT_PROCESSED_SCAN_LIMIT,
};
#[cfg(test)]
use memcore::MemoryEntry;
// Unit tests in `daily_distill/tests` call this via `super::*`.
#[cfg(test)]
pub(crate) use parser::parse_distill_response;
#[cfg(test)]
use persist::{
    normalized_source_memory_ids, persist_distill_memory, serialized_source_set_identity_bytes,
    stable_distill_memory_id, validate_existing_distill_winner,
};
// #1087: unit test asserts the flag-off default is unchanged.
#[cfg(test)]
pub(crate) use runner::call_claude_batch;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use types::{CandidateGroup, GroupPayload};

#[cfg(test)]
mod tests;
