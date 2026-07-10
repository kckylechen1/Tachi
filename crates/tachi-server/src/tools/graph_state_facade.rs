//! Internal graph/state helpers (not MCP-registered after #757).
//!
//! Low-level primitives remain callable by in-crate code and unit tests via
//! these `MemoryServer` methods. Store logic lives in `graph_state_ops` /
//! memcore — only the MCP surface registration was removed.

use rmcp::handler::server::wrapper::Parameters;

use crate::graph_state_ops::{
    handle_add_edge, handle_get_edges, handle_get_state, handle_memory_graph, handle_set_state,
};
use crate::tool_params::{
    AddEdgeParams, GetEdgesParams, GetStateParams, MemoryGraphParams, SetStateParams,
};
use crate::MemoryServer;

impl MemoryServer {
    /// Add or update an edge in the memory graph (internal; not MCP-registered).
    pub(crate) async fn add_edge(
        &self,
        Parameters(params): Parameters<AddEdgeParams>,
    ) -> Result<String, String> {
        handle_add_edge(self, params).await
    }

    /// Get edges connected to a memory entry (internal; not MCP-registered).
    pub(crate) async fn get_edges(
        &self,
        Parameters(params): Parameters<GetEdgesParams>,
    ) -> Result<String, String> {
        handle_get_edges(self, params).await
    }

    /// Inspect a read-only graph neighborhood (internal; not MCP-registered).
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

    /// Set hard_state key/value (internal; not MCP-registered).
    pub(crate) async fn set_state(
        &self,
        Parameters(params): Parameters<SetStateParams>,
    ) -> Result<String, String> {
        handle_set_state(self, params).await
    }

    /// Get hard_state value by key (internal; not MCP-registered).
    pub(crate) async fn get_state(
        &self,
        Parameters(params): Parameters<GetStateParams>,
    ) -> Result<String, String> {
        handle_get_state(self, params).await
    }
}
