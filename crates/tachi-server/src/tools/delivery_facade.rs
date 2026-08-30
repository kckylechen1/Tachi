use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::delivery_ops::handle_tachi_delivery;
use crate::tool_params::TachiDeliveryParams;
use crate::MemoryServer;

#[tool_router(router = delivery_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Durable result delivery seam (#1679): claim_ready_delivery / ack_delivered / reject_or_block / resume_requester_operation / dismiss / get. Requester-side receipts only — no destination parameter exists and delivery never rewrites execution or adjudication truth."
    )]
    pub(crate) async fn tachi_delivery(
        &self,
        Parameters(params): Parameters<TachiDeliveryParams>,
    ) -> Result<String, String> {
        handle_tachi_delivery(self, params)
    }
}
