use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::MemoryServer;
use tachi_params::PeerQueryParams;

#[tool_router(router = peer_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Read an advisory, structurally read-only publication from a same-host peer session (#1016 observe-only v1). Params: noun (exhaustively whitelisted — 'presence' reads the claims heartbeat board, optionally narrowed by target_session_client; 'run' resolves target_session_client [required] to its active claim's dispatch_id and returns that dispatch's status.json + a recent progress.jsonl tail — any other noun is denied). Identity is self-asserted-local (same_host_loopback_v1); this never writes and never blocks a peer's turn — 'run' answers include a turn_boundary_callback pointer to tachi_memory(action='sticky_leave') for an async reply since MCP cannot interrupt a live agent. Returns a peer-publication/v1 envelope."
    )]
    pub(crate) async fn peer_query(
        &self,
        Parameters(params): Parameters<PeerQueryParams>,
    ) -> Result<String, String> {
        crate::peer_ops::handle_peer_query(self, params)
    }
}
