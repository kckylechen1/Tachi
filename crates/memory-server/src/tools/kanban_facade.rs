use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::kanban::{
    handle_check_inbox, handle_post_card, handle_update_card, CheckInboxParams, PostCardParams,
    UpdateCardParams,
};
use crate::MemoryServer;

#[tool_router(router = kanban_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(description = "Post a kanban card from one agent to another.")]
    pub(crate) async fn post_card(
        &self,
        Parameters(params): Parameters<PostCardParams>,
    ) -> Result<String, String> {
        handle_post_card(self, params).await
    }

    #[tool(description = "Check kanban inbox for a target agent.")]
    pub(crate) async fn check_inbox(
        &self,
        Parameters(params): Parameters<CheckInboxParams>,
    ) -> Result<String, String> {
        handle_check_inbox(self, params).await
    }

    #[tool(description = "Update status of a kanban card.")]
    pub(crate) async fn update_card(
        &self,
        Parameters(params): Parameters<UpdateCardParams>,
    ) -> Result<String, String> {
        handle_update_card(self, params).await
    }
}
