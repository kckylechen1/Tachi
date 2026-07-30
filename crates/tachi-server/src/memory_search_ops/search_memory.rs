mod cache;
mod cross_library;
mod exact;
mod filters;
mod handlers;
mod rows;
mod similar;
mod store;
#[cfg(test)]
mod tests;

pub(crate) use cache::invalidate_recall_cache_after_write;
#[cfg(test)]
pub(crate) use cache::{RecallCacheRaceHook, RecallCacheRacePoint, RecallCacheTestOverride};
pub(crate) use exact::has_high_confidence_exact_token_top;
pub(crate) use handlers::{handle_search_memory, handle_search_memory_with_access};
pub(crate) use rows::{
    annotate_wiki_exact_token_matches, search_memory_rows, search_memory_rows_with_access,
    search_memory_rows_with_recall_config, search_wiki_store_candidates, WikiStoreSearchCandidate,
};
pub(crate) use similar::handle_find_similar_memory;
