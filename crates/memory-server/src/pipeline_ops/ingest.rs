use memory_core::{MemoryEntry, MemoryStore};
use serde_json::json;

use crate::server_state::{DbScope, MemoryServer};
use crate::tool_params::{
    fact_to_entry, ExtractFactsParams, IngestEventParams, IngestParams, IngestSourceParams,
};
use crate::utils::{stable_hash, value_to_template_text};

use super::audit::{
    claim_ingest_event, enqueue_dead_letter, insert_ingest_audit, insert_ingest_skip_audit,
    release_ingest_claim,
};
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
pub(crate) use source::handle_ingest_source;
