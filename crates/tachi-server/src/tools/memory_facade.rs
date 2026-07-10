use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde_json::json;

use crate::memory_ops::{
    handle_archive_memory, handle_get_memory, handle_list_memories, handle_memory_stats,
    handle_runtime_info,
};
use crate::memory_search_ops::{handle_find_similar_memory, handle_remember, handle_save_memory};
use crate::tool_params::{
    ArchiveMemoryParams, FindSimilarMemoryParams, GetMemoryParams, ListMemoriesParams,
    RememberParams, SaveMemoryParams, SearchMemoryParams,
};
use crate::MemoryServer;

#[tool_router(router = memory_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Save a memory entry to the store. Creates a new entry or updates an existing one if id is provided."
    )]
    pub(crate) async fn save_memory(
        &self,
        Parameters(params): Parameters<SaveMemoryParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_write(self, "save_memory", &params).await?
        {
            return Ok(body);
        }
        handle_save_memory(self, params).await
    }

    #[tool(
        description = "Low-friction shortcut to save a note. Only `text` is required; path defaults to /notes/{YYYY-MM-DD}, category to \"fact\", importance to 0.6, scope to \"project\". Use save_memory directly when you need full control over path, importance, retention, vector, or auto-link."
    )]
    pub(crate) async fn remember(
        &self,
        Parameters(params): Parameters<RememberParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_write(self, "remember", &params).await?
        {
            return Ok(body);
        }
        handle_remember(self, params).await
    }

    #[tool(
        description = "Search memory entries using hybrid search (vector + FTS + symbolic). Returns ranked results with scores."
    )]
    pub(crate) async fn search_memory(
        &self,
        Parameters(params): Parameters<SearchMemoryParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_read(self, "search_memory", &params).await?
        {
            return Ok(body);
        }
        crate::memory_search_ops::handle_search_memory_with_access(self, params, false, true).await
    }

    #[tool(
        description = "Find memory entries similar to a provided vector. Uses vector similarity only (no FTS/symbolic/decay weighting)."
    )]
    pub(crate) async fn find_similar_memory(
        &self,
        Parameters(params): Parameters<FindSimilarMemoryParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_read(self, "find_similar_memory", &params)
                .await?
        {
            return Ok(body);
        }
        handle_find_similar_memory(self, params).await
    }

    #[tool(description = "Get a single memory entry by ID.")]
    pub(crate) async fn get_memory(
        &self,
        Parameters(params): Parameters<GetMemoryParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_read(self, "get_memory", &params).await?
        {
            return Ok(body);
        }
        handle_get_memory(self, params).await
    }

    #[tool(description = "List memory entries under a path prefix.")]
    pub(crate) async fn list_memories(
        &self,
        Parameters(params): Parameters<ListMemoriesParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_read(self, "list_memories", &params).await?
        {
            return Ok(body);
        }
        handle_list_memories(self, params).await
    }

    #[tool(description = "Get aggregate statistics about the memory store.")]
    pub(crate) async fn memory_stats(&self) -> Result<String, String> {
        let params = json!({});
        if let Some(body) =
            crate::cli_client::maybe_forward_server_read(self, "memory_stats", &params).await?
        {
            return Ok(body);
        }
        handle_memory_stats(self).await
    }

    #[tool(
        description = "Return Tachi runtime identity and DB routing metadata. Clients should verify this before writing through embedded or derivative Tachi deployments."
    )]
    pub(crate) async fn runtime_info(&self) -> Result<String, String> {
        handle_runtime_info(self).await
    }

    #[tool(
        description = "Cheap health check: daemon status, vector coverage, foundry queue depth, provider key drift/auth-failure inference, model lane config, and agent readiness warnings. Call at session start; use `tachi status --probe-keys` or `tachi doctor --probe-keys` for live provider calls."
    )]
    pub(crate) async fn tachi_status(&self) -> Result<String, String> {
        crate::status_ops::handle_tachi_status_agent(self).await
    }

    #[tool(
        description = "List the native Tachi tools visible to the current profile. Use before calling unfamiliar tools instead of guessing tool names."
    )]
    pub(crate) async fn tachi_tools(&self) -> Result<String, String> {
        let tools = self.native_tool_visibility();
        let visible_tools = tools
            .iter()
            .filter(|(_, _, visible)| *visible)
            .collect::<Vec<_>>();
        let rows = visible_tools
            .iter()
            .map(|(name, description, _)| format!("- `{name}` — {description}"))
            .collect::<Vec<_>>();
        Ok(format!(
            "## Tachi tools\nprofile: `{}`\ncount: {}\n\n{}\n\nUse exact names from this list; unknown tool names are treated as not connected/unsupported by some MCP hosts.",
            self.active_tool_profile()
                .map(|p| p.as_str())
                .unwrap_or_else(|| tachi_hub::default_tool_profile().as_str()),
            visible_tools.len(),
            rows.join("\n")
        ))
    }

    #[tool(
        description = "Archive a memory entry (soft-delete, set archived=1). Entry is hidden from default searches but can be retrieved with include_archived=true."
    )]
    pub(crate) async fn archive_memory(
        &self,
        Parameters(params): Parameters<ArchiveMemoryParams>,
    ) -> Result<String, String> {
        handle_archive_memory(self, params).await
    }
}
