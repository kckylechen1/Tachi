use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::vault_ops::connections::{
    current_node, handle_connections, ConnectionContext, ConnectionQuery,
};
#[cfg(test)]
use crate::vault_ops::{
    handle_vault_init, handle_vault_lock, handle_vault_set, VaultInitParams, VaultSetParams,
};
use crate::MemoryServer;

#[tool_router(router = vault_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Discover saved API and SSH connections. Actions: list, describe, prepare. Returns scoped metadata and a host CLI launch recipe; never returns secret values, changes the parent environment, executes SSH, or grants command execution. Use the host's existing shell permission to run a prepared command.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) async fn vault_connections(
        &self,
        Parameters(params): Parameters<ConnectionQuery>,
    ) -> Result<String, String> {
        let home = self.tachi_home_dir();
        let project = match self.session_project() {
            Some(name) => Self::resolve_existing_named_project_db_path_in_home(&name, &home)
                .map_err(|_| "Cannot establish the current connection project scope".to_string())?
                .and_then(|db| db.canonicalize().ok())
                .and_then(|db| {
                    crate::path_utils::plan_c_project_root_from_local_db_in_home(&db, &home)
                })
                .and_then(|root| crate::path_utils::plan_c_dir_name_from_root(&root)),
            None => None,
        };
        handle_connections(
            self,
            params,
            ConnectionContext {
                agent_id: self.bound_agent_id(),
                project,
                node: current_node(),
            },
        )
    }
}

// Internal fixtures share the core handlers without registering a tool route.
#[cfg(test)]
impl MemoryServer {
    pub(crate) async fn vault_init(
        &self,
        Parameters(params): Parameters<VaultInitParams>,
    ) -> Result<String, String> {
        handle_vault_init(self, params).await
    }
    pub(crate) async fn vault_set(
        &self,
        Parameters(params): Parameters<VaultSetParams>,
    ) -> Result<String, String> {
        handle_vault_set(self, params).await
    }
    pub(crate) async fn vault_lock(&self) -> Result<String, String> {
        handle_vault_lock(self).await
    }
}
