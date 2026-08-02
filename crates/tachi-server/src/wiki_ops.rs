use crate::memory_search_ops::{
    annotate_wiki_exact_token_matches, search_wiki_store_candidates, WikiStoreSearchCandidate,
};
use crate::network_safety::is_private_or_local_ip;
use crate::server_state::{DbScope, MemoryServer};
use crate::shared_defs::slim_search_result;
use crate::tool_params::{
    build_candidate_knowledge_artifact_fields, build_evidence_refs_v1,
    derive_effective_knowledge_artifact, derive_wiki_lifecycle, derive_wiki_review_receipt,
    HybridWeightsParam, SearchMemoryParams, StoreRef, TachiWikiIngestParams, WikiBrowseParams,
    WikiLifecycleV1, WikiLintParams, WikiReadPlan, WikiSearchParams, LOGICAL_SHARED_WIKI_PROJECT,
};
use crate::utils::sanitize_safe_path_name;
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use memcore::{scorer::local_pagerank, HubCapability, MemoryEntry, MemoryStore};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration as StdDuration;
use tachi_hub::{capability_callable, should_expose_skill_tool};
use tokio::net::lookup_host;

const WIKI_LOG_MAX_BYTES: usize = 256 * 1024;
const WIKI_LOG_MAX_ENTRIES: usize = 200;
const WIKI_LOG_ENTRY_MAX_BYTES: usize = 4096;
const WIKI_INGEST_HTTP_MAX_BYTES: usize = 2 * 1024 * 1024;
static WIKI_INGEST_HTTP_CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();

mod export;
mod handoff_lookup;
mod ingest;
mod lint;
mod log;
mod provenance;
mod references;
mod search;
mod similarity;
mod skill_quality;
mod store;

#[cfg(test)]
mod tests;

use self::provenance::{
    attach_wiki_provenance, preferred_wiki_references, wiki_entry_matches_lifecycle_scope,
};
use self::similarity::{contradiction_score, parse_rfc3339_utc, token_cosine_similarity};
use self::store::{
    find_related_by_entities, is_user_facing_wiki_entry, stores_for_wiki_plan, with_wiki_store,
    with_wiki_store_read, StoredWikiEntry,
};

pub(crate) use self::export::export_wiki_obsidian;
pub(crate) use self::handoff_lookup::list_handoff_mirrors_for_repo;
pub(crate) use self::ingest::handle_wiki_ingest;
pub(crate) use self::lint::{handle_wiki_lint, wiki_hygiene_counts};
pub(crate) use self::log::append_wiki_log;
#[cfg(test)]
pub(crate) use self::provenance::apply_wiki_lifecycle_gate;
pub(crate) use self::references::validate_references;
pub(crate) use self::search::{
    collect_wiki_browse_value, collect_wiki_read_value_for_plan, collect_wiki_search_value,
    handle_wiki_browse, handle_wiki_read_for_plan, handle_wiki_search, search_wiki_rows_for_plan,
};
#[cfg(test)]
pub(crate) use self::search::{collect_wiki_read_value, handle_wiki_read};
pub(crate) use self::skill_quality::refresh_skill_quality_guards;
pub(crate) use self::store::list_wiki_entries_for_plan;
