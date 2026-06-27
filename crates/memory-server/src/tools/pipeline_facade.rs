use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::pipeline_ops::{
    handle_extract_facts, handle_get_pipeline_status, handle_ingest, handle_ingest_event,
    handle_ingest_source, handle_sync_memories,
};
use crate::tool_params::{
    ExtractFactsParams, IngestEventParams, IngestParams, IngestSourceParams, SyncMemoriesParams,
};
use crate::MemoryServer;

#[tool_router(router = pipeline_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(description = "Extract structured facts from text using LLM and save to memory.")]
    pub(crate) async fn extract_facts(
        &self,
        Parameters(params): Parameters<ExtractFactsParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_write(self, "extract_facts", &params).await?
        {
            return Ok(body);
        }
        handle_extract_facts(self, params).await
    }

    #[tool(description = "Ingest a conversation event and extract facts from messages.")]
    pub(crate) async fn ingest_event(
        &self,
        Parameters(params): Parameters<IngestEventParams>,
    ) -> Result<String, String> {
        handle_ingest_event(self, params).await
    }

    #[tool(
        description = "Unified ingest entrypoint for conversation events and source documents. Use ingest_type to select event vs source behavior."
    )]
    pub(crate) async fn ingest(
        &self,
        Parameters(params): Parameters<IngestParams>,
    ) -> Result<String, String> {
        handle_ingest(self, params).await
    }

    #[tool(
        description = "Batch ingest source content with optional chunking, enrichment, and graph edge building."
    )]
    pub(crate) async fn ingest_source(
        &self,
        Parameters(params): Parameters<IngestSourceParams>,
    ) -> Result<String, String> {
        handle_ingest_source(self, params).await
    }

    #[tool(description = "Get pipeline status and statistics.")]
    pub(crate) async fn get_pipeline_status(&self) -> Result<String, String> {
        handle_get_pipeline_status(self).await
    }

    #[tool(
        description = "Get only new or changed memories since last sync for this agent. Returns incremental diff to save tokens. Use agent_id to identify your agent uniquely."
    )]
    pub(crate) async fn sync_memories(
        &self,
        Parameters(params): Parameters<SyncMemoriesParams>,
    ) -> Result<String, String> {
        handle_sync_memories(self, params).await
    }
}
