use super::*;

fn default_access_level() -> String {
    "read".to_string()
}

fn default_sandbox_operation() -> String {
    "read".to_string()
}

#[allow(dead_code)]
fn default_runtime_type() -> String {
    "process".to_string()
}

#[allow(dead_code)]
fn default_sandbox_startup_ms() -> u64 {
    30_000
}

#[allow(dead_code)]
fn default_sandbox_tool_ms() -> u64 {
    30_000
}

#[allow(dead_code)]
fn default_sandbox_max_concurrency() -> u32 {
    1
}

#[allow(dead_code)]
fn default_true_bool() -> bool {
    true
}

#[allow(dead_code)]
fn default_sandbox_policy_limit() -> usize {
    100
}

#[allow(dead_code)]
fn default_sandbox_exec_audit_limit() -> usize {
    100
}

// ─── Sandbox Access Rules ───────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct SandboxSetRuleParams {
    /// Agent role (e.g. "code-review", "finance", "admin")
    pub agent_role: String,
    /// Path pattern to match (e.g. "/finance/*", "/project/secrets")
    pub path_pattern: String,
    /// Access level: "read", "write", or "deny"
    #[serde(default = "default_access_level")]
    pub access_level: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct SandboxCheckParams {
    /// Agent role to check access for
    pub agent_role: String,
    /// Memory path to check
    pub path: String,
    /// Operation type: "read" or "write"
    #[serde(default = "default_sandbox_operation")]
    pub operation: String,
}

// ─── Sandbox Execution Policies ─────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct SandboxSetPolicyParams {
    /// Capability ID (typically MCP capability id, e.g. "mcp:exa")
    pub capability_id: String,
    /// Runtime type: "process" | "wasm"
    #[serde(default = "default_runtime_type")]
    pub runtime_type: String,
    /// Environment variable allowlist. Empty means keep existing behavior.
    #[serde(default)]
    pub env_allowlist: Vec<String>,
    /// Allowed read roots for filesystem access (advisory + cwd guard).
    #[serde(default)]
    pub fs_read_roots: Vec<String>,
    /// Allowed write roots for filesystem access (reserved for executors).
    #[serde(default)]
    pub fs_write_roots: Vec<String>,
    /// Allowed working-directory roots for process startup.
    #[serde(default)]
    pub cwd_roots: Vec<String>,
    /// Startup timeout cap in milliseconds.
    #[serde(default = "default_sandbox_startup_ms")]
    pub max_startup_ms: u64,
    /// Tool call timeout cap in milliseconds.
    #[serde(default = "default_sandbox_tool_ms")]
    pub max_tool_ms: u64,
    /// Max concurrency cap for the capability.
    #[serde(default = "default_sandbox_max_concurrency")]
    pub max_concurrency: u32,
    /// Whether this policy is enabled.
    #[serde(default = "default_true_bool")]
    pub enabled: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct SandboxGetPolicyParams {
    /// Capability ID to query
    pub capability_id: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct SandboxListPoliciesParams {
    /// Only return enabled policies
    #[serde(default)]
    pub enabled_only: bool,
    /// Max rows returned
    #[serde(default = "default_sandbox_policy_limit")]
    pub limit: usize,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct SandboxExecAuditParams {
    /// Optional capability filter (e.g. "mcp:exa")
    #[serde(default)]
    pub capability_id: Option<String>,
    /// Optional stage filter (e.g. "preflight", "startup", "tool_call")
    #[serde(default)]
    pub stage: Option<String>,
    /// Optional decision filter (e.g. "allowed", "denied", "timeout", "failed")
    #[serde(default)]
    pub decision: Option<String>,
    /// Max rows returned.
    #[serde(default = "default_sandbox_exec_audit_limit")]
    pub limit: usize,
}
