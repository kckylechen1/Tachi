use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use tachi_params::TachiA2aParams;

use crate::MemoryServer;

#[tool_router(router = a2a_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Same-host advisory turn-response mailbox. action='respond' writes one scrubbed turn_response/v1 to an explicit historically local self_asserted AgentIdentity; action='status' returns only issuer/recipient-scoped headers and receipts and never consumes. Sender identity always comes from the current admitted connection."
    )]
    pub(crate) async fn tachi_a2a(
        &self,
        Parameters(params): Parameters<TachiA2aParams>,
    ) -> Result<String, String> {
        crate::a2a_ops::handle_tachi_a2a(self, params)
    }
}
