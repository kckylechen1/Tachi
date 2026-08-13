use memcore::{MemoryEntry, MemoryStore};
use serde_json::json;

use crate::server_state::{DbScope, MemoryServer};
use crate::tool_params::{
    fact_to_entry, fact_to_entry_candidate_with_reason, ExtractFactsParams, IngestEventParams,
    IngestParams, IngestSourceParams, WikiArtifactKindV1, WikiAuthorityV1, WikiLifecycleV1,
};
use crate::utils::{stable_hash, value_to_template_text};

use super::auto_ingest::build_similarity_edges;
use super::helpers::{
    build_ingest_entry, chunk_text, default_event_path_prefix, default_source_path_prefix,
    is_lazy_source, merge_optional_metadata, resolve_domain, serialize_json,
    should_enqueue_enrichment,
};

mod event;
mod extract;
mod router;
mod source;
mod structured_event;

pub(crate) use event::handle_ingest_event;
pub(crate) use extract::handle_extract_facts;
pub(crate) use router::handle_ingest;
#[cfg(test)]
pub(crate) use source::force_next_admitted_enrichment_ownership_loss_for_test;
pub(in crate::pipeline_ops) use source::handle_admitted_ingest_source;
pub(crate) use source::handle_ingest_source;
