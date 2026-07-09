use crate::foundry_runtime_ops::FoundryMaintenanceItem;
use crate::memory_search_ops::search_memory_rows;
use crate::server_state::MemoryServer;
use crate::shared_defs::slim_search_result;
use crate::tool_params::SearchMemoryParams;

pub(in crate::foundry_runtime_ops::recall_cache) async fn search_rows_for_recall_cache(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
    mut params: SearchMemoryParams,
) -> Result<Vec<serde_json::Value>, String> {
    if let Some(db_path) = item.db_path.as_ref() {
        if params.query_vec.is_none() {
            let (scrubbed_query, _) = crate::memory_search_ops::scrub_secrets(&params.query);
            match server.llm.embed_voyage(&scrubbed_query, "query").await {
                Ok(query_vec) => params.query_vec = Some(query_vec),
                Err(e) => {
                    tracing::warn!(
                        "[recall-rerank-cache] path-db query embedding failed, falling back to lexical-only search: {e}"
                    );
                }
            }
        }
        return server.with_path_store_read(db_path, |store| {
            let opts = params.to_search_options(store.vec_available);
            let rows = store
                .search(&params.query, Some(opts))
                .map_err(|e| format!("Search failed in path DB {}: {e}", db_path.display()))?;
            Ok(rows
                .into_iter()
                .map(|row| slim_search_result(&row, item.target_db, params.include_metadata))
                .collect())
        });
    }

    search_memory_rows(server, params, false).await
}
