use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::sandbox_ops::{
    handle_sandbox_check, handle_sandbox_exec_audit, handle_sandbox_set_rule,
};
use crate::tool_params::{
    SandboxCheckParams, SandboxExecAuditParams, SandboxSetRuleParams, TachiSandboxParams,
};
use crate::MemoryServer;

/// Runtime-policy actions retired together with the Hub MCP proxy, the only
/// runtime that enforced them.
pub(crate) const TACHI_SANDBOX_RETIRED_POLICY_ACTIONS: &[&str] =
    &["set_policy", "get_policy", "list_policies"];

/// Dispatch a `tachi_sandbox` verb call to the matching pre-fold handler.
///
/// Pure routing: it reconstructs the per-action parameter struct from the flat
/// facade params and calls the exact pre-fold `handle_sandbox_*` handler the
/// retired `sandbox_*` alias tools used to forward to (the handlers themselves
/// are unchanged since the fold), so a folded call produces byte-identical
/// output to pre-fold behavior — pinned in `tests/sandbox_fold.rs` against
/// these handlers as the oracle. Required fields absent for the chosen action
/// fail with a precise message (rather than a serde deserialization error).
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
        retired if TACHI_SANDBOX_RETIRED_POLICY_ACTIONS.contains(&retired) => Err(format!(
            "action='{retired}' is retired: capability runtime policies were only enforced by the \
             removed Hub MCP proxy. Use 'set_rule', 'check', or 'exec_audit'."
        )),
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
            "Invalid action '{other}'. Use 'set_rule', 'check', or 'exec_audit'."
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
    ///
    /// v2.0 retirement: the six pre-fold `sandbox_*` forwarding aliases expired
    /// (their `remove_in_release` was 1.10.0) and their `#[tool]` wrappers were
    /// deleted. Their manifest entries survive as tombstones in
    /// `tools/alias_manifest.rs` (deadlines unchanged) and the
    /// `tests/sandbox_fold.rs` tripwires pin that the names stay unrouted and
    /// rejected even for the admin profile. The dispatcher below is unchanged:
    /// it still forwards every action to the exact pre-fold `sandbox_ops`
    /// handler, which `tests/sandbox_fold.rs` uses as its output oracle.
    #[tool(
        description = "Sandbox governance (admin only). action='set_rule': set an agent-role + path-pattern memory access rule (read/write/deny); action='check': check whether a role can read/write a path; action='exec_audit': list sandbox execution audit rows."
    )]
    pub(crate) async fn tachi_sandbox(
        &self,
        Parameters(params): Parameters<TachiSandboxParams>,
    ) -> Result<String, String> {
        handle_tachi_sandbox_facade(self, params).await
    }
}
