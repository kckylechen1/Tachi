use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::sandbox_ops::{
    handle_sandbox_check, handle_sandbox_exec_audit, handle_sandbox_get_policy,
    handle_sandbox_list_policies, handle_sandbox_set_policy, handle_sandbox_set_rule,
};
use crate::tool_params::{
    SandboxCheckParams, SandboxExecAuditParams, SandboxGetPolicyParams, SandboxListPoliciesParams,
    SandboxSetPolicyParams, SandboxSetRuleParams,
};
use crate::MemoryServer;

#[tool_router(router = sandbox_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Set a sandbox access rule for an agent role + path pattern. Controls which memories a role can access. Access levels: read, write, deny."
    )]
    pub(crate) async fn sandbox_set_rule(
        &self,
        Parameters(params): Parameters<SandboxSetRuleParams>,
    ) -> Result<String, String> {
        handle_sandbox_set_rule(self, params).await
    }

    #[tool(
        description = "Check if an agent role can access a given path for a specific operation. Advisory mode — not enforced in search_memory yet (TODO: future enforcement integration)."
    )]
    pub(crate) async fn sandbox_check(
        &self,
        Parameters(params): Parameters<SandboxCheckParams>,
    ) -> Result<String, String> {
        handle_sandbox_check(self, params).await
    }

    #[tool(
        description = "Set runtime sandbox policy for a capability (timeouts, concurrency, env allowlist, fs/cwd roots)."
    )]
    pub(crate) async fn sandbox_set_policy(
        &self,
        Parameters(params): Parameters<SandboxSetPolicyParams>,
    ) -> Result<String, String> {
        handle_sandbox_set_policy(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for sandbox_set_policy. Configure shell execution policy."
    )]
    pub(crate) async fn shell_set_policy(
        &self,
        Parameters(params): Parameters<SandboxSetPolicyParams>,
    ) -> Result<String, String> {
        handle_sandbox_set_policy(self, params).await
    }

    #[tool(description = "Get runtime sandbox policy for a capability.")]
    pub(crate) async fn sandbox_get_policy(
        &self,
        Parameters(params): Parameters<SandboxGetPolicyParams>,
    ) -> Result<String, String> {
        handle_sandbox_get_policy(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for sandbox_get_policy. Read shell execution policy."
    )]
    pub(crate) async fn shell_get_policy(
        &self,
        Parameters(params): Parameters<SandboxGetPolicyParams>,
    ) -> Result<String, String> {
        handle_sandbox_get_policy(self, params).await
    }

    #[tool(description = "List runtime sandbox policies.")]
    pub(crate) async fn sandbox_list_policies(
        &self,
        Parameters(params): Parameters<SandboxListPoliciesParams>,
    ) -> Result<String, String> {
        handle_sandbox_list_policies(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for sandbox_list_policies. List shell policies."
    )]
    pub(crate) async fn shell_list_policies(
        &self,
        Parameters(params): Parameters<SandboxListPoliciesParams>,
    ) -> Result<String, String> {
        handle_sandbox_list_policies(self, params).await
    }

    #[tool(
        description = "List sandbox execution audit rows (policy decisions, startup, runtime outcomes)."
    )]
    pub(crate) async fn sandbox_exec_audit(
        &self,
        Parameters(params): Parameters<SandboxExecAuditParams>,
    ) -> Result<String, String> {
        handle_sandbox_exec_audit(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for sandbox_exec_audit. Inspect shell execution audit."
    )]
    pub(crate) async fn shell_exec_audit(
        &self,
        Parameters(params): Parameters<SandboxExecAuditParams>,
    ) -> Result<String, String> {
        handle_sandbox_exec_audit(self, params).await
    }
}
