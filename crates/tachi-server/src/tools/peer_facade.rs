use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::MemoryServer;
use tachi_params::PeerQueryParams;

#[tool_router(router = peer_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Read a read-only same-host peer publication (#1016). Nouns: 'presence' reads the claims board; 'run' requires target_session_client, resolves exactly one active locally admitted AgentIdentity plus dispatch, and returns status.json plus a safe progress tail; other nouns are denied. Missing, conflicting, stale, or unavailable identity returns recipient_unresolved without fallback. Successful run includes a stable peer_publication_id and teaches only tachi_a2a(action='respond', recipient_agent_identity_id=..., subject_ref='peer_publication:...'). Current admitted recipient identity is required before a callback is exposed. Advisory only: no peer interruption, work authority, or execution grant. Returns peer-publication/v1."
    )]
    pub(crate) async fn peer_query(
        &self,
        Parameters(params): Parameters<PeerQueryParams>,
    ) -> Result<String, String> {
        crate::peer_ops::handle_peer_query(self, params)
    }
}
