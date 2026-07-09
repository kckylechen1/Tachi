// hub.rs — Hub capability types for Sigil Hub registry
//
// Defines the data model for Skills, Plugins, and MCP server configs
// that are registered, discovered, and tracked through the Hub.

use serde::{Deserialize, Serialize};
use serde_json::Value;

fn default_review_status() -> String {
    "approved".to_string()
}

fn default_health_status() -> String {
    "healthy".to_string()
}

fn default_exposure_mode() -> String {
    "direct".to_string()
}

fn default_virtual_binding_priority() -> i32 {
    100
}

fn default_enabled() -> bool {
    true
}

fn default_virtual_binding_metadata() -> Value {
    Value::Object(Default::default())
}

/// A registered capability in the Sigil Hub.
///
/// Capabilities come in three types:
/// - **Skill**: A prompt template + orchestration logic
/// - **Plugin**: A code module (WASM, dylib, or JS)
/// - **MCP Server**: An external tool server connection config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubCapability {
    /// Unique identifier, e.g. "skill:code-review", "mcp:github"
    pub id: String,

    /// Type: "skill" | "plugin" | "mcp"
    pub cap_type: String,

    /// Human-readable name
    pub name: String,

    /// Version number (monotonically increasing)
    pub version: u32,

    /// Short description of what this capability does
    pub description: String,

    /// JSON string containing the capability definition:
    /// - Skill: prompt template, tools_required, trigger patterns
    /// - Plugin: module path, entry point, dependencies
    /// - MCP: server command, args, env, transport config
    pub definition: String,

    /// Whether this capability is currently active
    pub enabled: bool,

    /// Review status for governance gate: pending | approved | rejected
    #[serde(default = "default_review_status")]
    pub review_status: String,

    /// Runtime health status: unknown | healthy | degraded | open
    #[serde(default = "default_health_status")]
    pub health_status: String,

    /// Last failure reason surfaced by governance / execution path
    #[serde(default)]
    pub last_error: Option<String>,

    /// ISO 8601 timestamp of last successful invocation
    #[serde(default)]
    pub last_success_at: Option<String>,

    /// ISO 8601 timestamp of last failed invocation
    #[serde(default)]
    pub last_failure_at: Option<String>,

    /// Consecutive failure streak for circuit governance
    #[serde(default)]
    pub fail_streak: u32,

    /// Active version pointer for alias routes (optional)
    #[serde(default)]
    pub active_version: Option<String>,

    /// Exposure mode metadata (e.g. direct | gateway)
    #[serde(default = "default_exposure_mode")]
    pub exposure_mode: String,

    /// Total number of invocations
    pub uses: u64,

    /// Number of successful invocations
    pub successes: u64,

    /// Number of failed invocations
    pub failures: u64,

    /// Running average user rating (0.0 - 5.0)
    pub avg_rating: f64,

    /// ISO 8601 timestamp of last use
    pub last_used: Option<String>,

    /// ISO 8601 timestamp of creation
    pub created_at: String,

    /// ISO 8601 timestamp of last update
    pub updated_at: String,
}

/// One binding from a virtual capability to a concrete backend capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualCapabilityBinding {
    /// Virtual capability id, e.g. "vc:web_search"
    pub vc_id: String,

    /// Concrete target capability id, e.g. "mcp:exa"
    pub capability_id: String,

    /// Lower numbers win during deterministic resolution.
    #[serde(default = "default_virtual_binding_priority")]
    pub priority: i32,

    /// Optional version pin. If set, target capability.version must match.
    #[serde(default)]
    pub version_pin: Option<u32>,

    /// Whether this binding is eligible for routing.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Free-form binding metadata for agents / app layer.
    #[serde(default = "default_virtual_binding_metadata")]
    pub metadata: Value,

    /// ISO 8601 timestamp of creation
    #[serde(default)]
    pub created_at: String,

    /// ISO 8601 timestamp of last update
    #[serde(default)]
    pub updated_at: String,
}
