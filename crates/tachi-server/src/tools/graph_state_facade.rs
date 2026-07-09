use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::graph_state_ops::{
    handle_add_edge, handle_get_edges, handle_get_state, handle_memory_graph, handle_set_state,
};
use crate::tool_params::{
    AddEdgeParams, GetEdgesParams, GetStateParams, MemoryGraphParams, SetStateParams,
};
use crate::MemoryServer;

#[tool_router(router = graph_state_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Add or update an edge in the memory graph. Edges represent causal, temporal, or entity relationships between memories."
    )]
    pub(crate) async fn add_edge(
        &self,
        Parameters(params): Parameters<AddEdgeParams>,
    ) -> Result<String, String> {
        handle_add_edge(self, params).await
    }

    #[tool(
        description = "Get edges connected to a memory entry. Returns causal, temporal, and entity relationship edges."
    )]
    pub(crate) async fn get_edges(
        &self,
        Parameters(params): Parameters<GetEdgesParams>,
    ) -> Result<String, String> {
        handle_get_edges(self, params).await
    }

    #[tool(
        description = "Inspect a read-only neighborhood from the memory graph, seeded by memory id or a search query. Returns seed nodes, neighboring nodes, and connecting edges."
    )]
    pub(crate) async fn memory_graph(
        &self,
        Parameters(params): Parameters<MemoryGraphParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_read(self, "memory_graph", &params).await?
        {
            return Ok(body);
        }
        handle_memory_graph(self, params).await
    }

    #[tool(description = "Set a key-value pair in server state (stored in hard_state table).")]
    pub(crate) async fn set_state(
        &self,
        Parameters(params): Parameters<SetStateParams>,
    ) -> Result<String, String> {
        handle_set_state(self, params).await
    }

    #[tool(description = "Get a value from server state by key.")]
    pub(crate) async fn get_state(
        &self,
        Parameters(params): Parameters<GetStateParams>,
    ) -> Result<String, String> {
        handle_get_state(self, params).await
    }
}
