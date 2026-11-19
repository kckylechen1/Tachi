use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::pipeline_ops::{handle_get_pipeline_status, handle_ingest_event, handle_sync_memories};
use crate::tool_params::{IngestEventParams, SyncMemoriesParams};
use crate::MemoryServer;

#[tool_router(router = pipeline_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(description = "Ingest a conversation event and extract facts from messages.")]
    pub(crate) async fn ingest_event(
        &self,
        Parameters(params): Parameters<IngestEventParams>,
    ) -> Result<String, String> {
        handle_ingest_event(self, params).await
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
