use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

fn default_access_level() -> String {
    "read".to_string()
}

fn default_sandbox_operation() -> String {
    "read".to_string()
}

fn default_sandbox_policy_limit() -> usize {
    100
}

// ─── Per-action handler arguments ───────────────────────────────────────────
//
// Internal argument structs for the `sandbox_ops` handlers. They are built
// field by field by the `tachi_sandbox` facade dispatcher
// (`tools::sandbox_facade`) from [`TachiSandboxParams`] and are never
// deserialized or published as a schema; wire defaults live on the facade.

// ─── Sandbox Access Rules ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SandboxSetRuleParams {
    /// Agent role (e.g. "code-review", "domain-pack", "admin")
    pub agent_role: String,
    /// Path pattern to match (e.g. "/domain-pack/*", "/project/secrets")
    pub path_pattern: String,
    /// Access level: "read", "write", or "deny"
    pub access_level: String,
}

#[derive(Debug, Clone)]
pub struct SandboxCheckParams {
    /// Agent role to check access for
    pub agent_role: String,
    /// Memory path to check
    pub path: String,
    /// Operation type: "read" or "write"
    pub operation: String,
}

// ─── Sandbox Execution Audit ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SandboxExecAuditParams {
    /// Optional capability filter (e.g. "mcp:exa")
    pub capability_id: Option<String>,
    /// Optional stage filter (e.g. "preflight", "startup", "tool_call")
    pub stage: Option<String>,
    /// Optional decision filter (e.g. "allowed", "denied", "timeout", "failed")
    pub decision: Option<String>,
    /// Max rows returned.
    pub limit: usize,
}

// ─── Unified sandbox verb facade (#757 Cut3-S1) ─────────────────────────────

/// Parameters for the folded `tachi_sandbox` verb. The six pre-fold sandbox
/// tools (`sandbox_set_rule`/`check`/`set_policy`/`get_policy`/`list_policies`/
/// `exec_audit`) became `action=` selectors on this one struct; the six legacy
/// tool names were retired in v2.0.0 after their 1.10.0 removal deadline. The
/// runtime-policy actions (`set_policy`/`get_policy`/`list_policies`) were
/// later retired with the Hub MCP proxy, their only consumer. Per-action required fields are
/// validated in the facade dispatcher (`tools::sandbox_facade`), not by serde,
/// so a caller supplying the wrong action gets a precise error rather than a
/// deserialization failure. Defaults preserve the pre-fold per-action tool
/// defaults so a folded call preserves the pre-fold handler output.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiSandboxParams {
    /// Which sandbox operation to run: "set_rule" | "check" | "exec_audit".
    pub action: String,

    // ── set_rule / check ──
    /// Agent role (set_rule, check). e.g. "code-review", "domain-pack", "admin".
    #[serde(default)]
    pub agent_role: Option<String>,

    // ── set_rule ──
    /// Path pattern to match (set_rule). e.g. "/domain-pack/*".
    #[serde(default)]
    pub path_pattern: Option<String>,
    /// Access level (set_rule): "read", "write", or "deny".
    #[serde(default = "default_access_level")]
    pub access_level: String,

    // ── check ──
    /// Memory path to check (check).
    #[serde(default)]
    pub path: Option<String>,
    /// Operation type (check): "read" or "write".
    #[serde(default = "default_sandbox_operation")]
    pub operation: String,

    // ── exec_audit ──
    /// Optional capability filter (exec_audit).
    #[serde(default)]
    pub capability_id: Option<String>,
    /// Optional stage filter (exec_audit): "preflight", "startup", "tool_call".
    #[serde(default)]
    pub stage: Option<String>,
    /// Optional decision filter (exec_audit): "allowed", "denied", "timeout".
    #[serde(default)]
    pub decision: Option<String>,

    /// Max rows returned (exec_audit).
    #[serde(default = "default_sandbox_policy_limit")]
    pub limit: usize,
}
