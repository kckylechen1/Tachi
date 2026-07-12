use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::tool_params::PeerQueryParams;
use crate::MemoryServer;

#[tool_router(router = peer_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Read an advisory, structurally read-only publication from a same-host peer session (#1016 S1). Params: noun (exhaustively whitelisted — S1 supports only 'presence'; any other noun is denied), optional target_session_client to narrow presence to a single seat. Identity is self-asserted-local (same_host_loopback_v1); this never writes and never blocks. Returns a peer-publication/v1 envelope."
    )]
    pub(crate) async fn peer_query(
        &self,
        Parameters(params): Parameters<PeerQueryParams>,
    ) -> Result<String, String> {
        crate::peer_ops::handle_peer_query(self, params)
    }
}
