use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::memory_ops::{
    handle_delete_domain, handle_get_domain, handle_list_domains, handle_register_domain,
};
use crate::tool_params::{
    DeleteDomainParams, GetDomainParams, ListDomainsParams, RegisterDomainParams,
};
use crate::MemoryServer;

#[tool_router(router = domain_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Register a domain configuration for memory routing, GC thresholds, and default retention policies."
    )]
    pub(crate) async fn register_domain(
        &self,
        Parameters(params): Parameters<RegisterDomainParams>,
    ) -> Result<String, String> {
        handle_register_domain(self, params).await
    }

    #[tool(description = "Get a domain configuration by name.")]
    pub(crate) async fn get_domain(
        &self,
        Parameters(params): Parameters<GetDomainParams>,
    ) -> Result<String, String> {
        handle_get_domain(self, params).await
    }

    #[tool(description = "List all registered domain configurations.")]
    pub(crate) async fn list_domains(
        &self,
        Parameters(_params): Parameters<ListDomainsParams>,
    ) -> Result<String, String> {
        handle_list_domains(self).await
    }

    #[tool(description = "Delete a domain configuration by name.")]
    pub(crate) async fn delete_domain(
        &self,
        Parameters(params): Parameters<DeleteDomainParams>,
    ) -> Result<String, String> {
        handle_delete_domain(self, params).await
    }
}
