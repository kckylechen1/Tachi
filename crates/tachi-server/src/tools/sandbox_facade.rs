use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::sandbox_ops::{
    handle_sandbox_check, handle_sandbox_exec_audit, handle_sandbox_get_policy,
    handle_sandbox_list_policies, handle_sandbox_set_policy, handle_sandbox_set_rule,
};
use crate::tool_params::{
    SandboxCheckParams, SandboxExecAuditParams, SandboxGetPolicyParams, SandboxListPoliciesParams,
    SandboxSetPolicyParams, SandboxSetRuleParams, TachiSandboxParams,
};
use crate::MemoryServer;

/// Dispatch a `tachi_sandbox` verb call to the matching pre-fold handler.
///
/// Pure routing: it reconstructs the per-action parameter struct from the flat
/// facade params and calls the exact same `handle_sandbox_*` function the
/// legacy alias tool calls, so a folded call and its legacy alias produce
/// byte-identical output. Required fields absent for the chosen action fail
/// with a precise message (rather than a serde deserialization error).
async fn handle_tachi_sandbox_facade(
    server: &MemoryServer,
    params: TachiSandboxParams,
) -> Result<String, String> {
    let action = params.action.trim().to_ascii_lowercase();
    match action.as_str() {
        "set_rule" => {
            handle_sandbox_set_rule(
                server,
                SandboxSetRuleParams {
                    agent_role: require_field(params.agent_role, "agent_role", "set_rule")?,
                    path_pattern: require_field(params.path_pattern, "path_pattern", "set_rule")?,
                    access_level: params.access_level,
                },
            )
            .await
        }
        "check" => {
            handle_sandbox_check(
                server,
                SandboxCheckParams {
                    agent_role: require_field(params.agent_role, "agent_role", "check")?,
                    path: require_field(params.path, "path", "check")?,
                    operation: params.operation,
                },
            )
            .await
        }
        "set_policy" => {
            handle_sandbox_set_policy(
                server,
                SandboxSetPolicyParams {
                    capability_id: require_field(
                        params.capability_id,
                        "capability_id",
                        "set_policy",
                    )?,
                    runtime_type: params.runtime_type,
                    env_allowlist: params.env_allowlist,
                    fs_read_roots: params.fs_read_roots,
                    fs_write_roots: params.fs_write_roots,
                    cwd_roots: params.cwd_roots,
                    max_startup_ms: params.max_startup_ms,
                    max_tool_ms: params.max_tool_ms,
                    max_concurrency: params.max_concurrency,
                    enabled: params.enabled,
                },
            )
            .await
        }
        "get_policy" => {
            handle_sandbox_get_policy(
                server,
                SandboxGetPolicyParams {
                    capability_id: require_field(
                        params.capability_id,
                        "capability_id",
                        "get_policy",
                    )?,
                },
            )
            .await
        }
        "list_policies" => {
            handle_sandbox_list_policies(
                server,
                SandboxListPoliciesParams {
                    enabled_only: params.enabled_only,
                    limit: params.limit,
                },
            )
            .await
        }
        "exec_audit" => {
            handle_sandbox_exec_audit(
                server,
                SandboxExecAuditParams {
                    capability_id: params.capability_id,
                    stage: params.stage,
                    decision: params.decision,
                    limit: params.limit,
                },
            )
            .await
        }
        other => Err(format!(
            "Invalid action '{other}'. Use 'set_rule', 'check', 'set_policy', 'get_policy', \
             'list_policies', or 'exec_audit'."
        )),
    }
}

/// Unwrap a per-action required field, or return a precise missing-field error.
fn require_field<T>(value: Option<T>, field: &str, action: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("'{field}' is required when action='{action}'"))
}

#[tool_router(router = sandbox_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    /// Canonical folded sandbox verb (#757 Cut3-S1). Admin-only, like every
    /// action it subsumes: `tachi_sandbox` is absent from every profile bundle,
    /// so only the admin profile can see or call it.
    #[tool(
        description = "Sandbox governance (admin only). action='set_rule': set an agent-role + path-pattern memory access rule (read/write/deny); action='check': check whether a role can read/write a path; action='set_policy': set a capability's runtime sandbox policy (timeouts, concurrency, env allowlist, fs/cwd roots); action='get_policy': read a capability's runtime policy; action='list_policies': list runtime policies; action='exec_audit': list sandbox execution audit rows."
    )]
    pub(crate) async fn tachi_sandbox(
        &self,
        Parameters(params): Parameters<TachiSandboxParams>,
    ) -> Result<String, String> {
        handle_tachi_sandbox_facade(self, params).await
    }

    #[tool(
        description = "DEPRECATED: use tachi_sandbox(action='set_rule'); removed in 1.10.0. Set a sandbox access rule for an agent role + path pattern. Controls which memories a role can access. Access levels: read, write, deny."
    )]
    pub(crate) async fn sandbox_set_rule(
        &self,
        Parameters(params): Parameters<SandboxSetRuleParams>,
    ) -> Result<String, String> {
        handle_sandbox_set_rule(self, params).await
    }

    #[tool(
        description = "DEPRECATED: use tachi_sandbox(action='check'); removed in 1.10.0. Check if an agent role can access a given memory path for a read/write operation. The same global sandbox rules are enforced by role-aware memory/wiki search surfaces when agent_role is supplied."
    )]
    pub(crate) async fn sandbox_check(
        &self,
        Parameters(params): Parameters<SandboxCheckParams>,
    ) -> Result<String, String> {
        handle_sandbox_check(self, params).await
    }

    #[tool(
        description = "DEPRECATED: use tachi_sandbox(action='set_policy'); removed in 1.10.0. Set runtime sandbox policy for a capability (timeouts, concurrency, env allowlist, fs/cwd roots)."
    )]
    pub(crate) async fn sandbox_set_policy(
        &self,
        Parameters(params): Parameters<SandboxSetPolicyParams>,
    ) -> Result<String, String> {
        handle_sandbox_set_policy(self, params).await
    }

    #[tool(
        description = "DEPRECATED: use tachi_sandbox(action='get_policy'); removed in 1.10.0. Get runtime sandbox policy for a capability."
    )]
    pub(crate) async fn sandbox_get_policy(
        &self,
        Parameters(params): Parameters<SandboxGetPolicyParams>,
    ) -> Result<String, String> {
        handle_sandbox_get_policy(self, params).await
    }

    #[tool(
        description = "DEPRECATED: use tachi_sandbox(action='list_policies'); removed in 1.10.0. List runtime sandbox policies."
    )]
    pub(crate) async fn sandbox_list_policies(
        &self,
        Parameters(params): Parameters<SandboxListPoliciesParams>,
    ) -> Result<String, String> {
        handle_sandbox_list_policies(self, params).await
    }

    #[tool(
        description = "DEPRECATED: use tachi_sandbox(action='exec_audit'); removed in 1.10.0. List sandbox execution audit rows (policy decisions, startup, runtime outcomes)."
    )]
    pub(crate) async fn sandbox_exec_audit(
        &self,
        Parameters(params): Parameters<SandboxExecAuditParams>,
    ) -> Result<String, String> {
        handle_sandbox_exec_audit(self, params).await
    }
}
