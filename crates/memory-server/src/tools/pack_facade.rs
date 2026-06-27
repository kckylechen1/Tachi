use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::pack_ops::{
    handle_pack_get, handle_pack_list, handle_pack_project, handle_pack_register,
    handle_pack_remove, handle_projection_list,
};
use crate::tool_params::{
    PackGetParams, PackListParams, PackProjectParams, PackRegisterParams, PackRemoveParams,
    ProjectionListParams,
};
use crate::MemoryServer;

#[tool_router(router = pack_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(description = "List installed skill packs. Optionally filter by enabled_only.")]
    pub(crate) async fn pack_list(
        &self,
        Parameters(params): Parameters<PackListParams>,
    ) -> Result<String, String> {
        handle_pack_list(self, params).await
    }

    #[tool(description = "Get details of a single installed skill pack by ID.")]
    pub(crate) async fn pack_get(
        &self,
        Parameters(params): Parameters<PackGetParams>,
    ) -> Result<String, String> {
        handle_pack_get(self, params).await
    }

    #[tool(
        description = "Register a skill pack after git clone / download. Records the pack in the registry with its metadata, source, and skill count."
    )]
    pub(crate) async fn pack_register(
        &self,
        Parameters(params): Parameters<PackRegisterParams>,
    ) -> Result<String, String> {
        handle_pack_register(self, params).await
    }

    #[tool(
        description = "Remove a skill pack from the registry. Also cleans up projected files in agent directories unless clean_files=false."
    )]
    pub(crate) async fn pack_remove(
        &self,
        Parameters(params): Parameters<PackRemoveParams>,
    ) -> Result<String, String> {
        handle_pack_remove(self, params).await
    }

    #[tool(
        description = "Project a pack's skills, workflows, and host overlays to one or more agents. Converts SKILL.md files to each agent's native format (e.g. .mdc rules for Cursor) and emits a tachi-projection manifest for adapters such as OpenClaw."
    )]
    pub(crate) async fn pack_project(
        &self,
        Parameters(params): Parameters<PackProjectParams>,
    ) -> Result<String, String> {
        handle_pack_project(self, params).await
    }

    #[tool(description = "List agent projections. Filter by agent and/or pack_id.")]
    pub(crate) async fn projection_list(
        &self,
        Parameters(params): Parameters<ProjectionListParams>,
    ) -> Result<String, String> {
        handle_projection_list(self, params).await
    }
}
