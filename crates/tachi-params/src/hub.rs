use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

fn default_hub_version() -> u32 {
    1
}

fn default_scope() -> String {
    "project".to_string()
}

fn default_hub_review_status() -> String {
    "approved".to_string()
}

fn default_true() -> bool {
    true
}

fn default_export_agent() -> String {
    "claude".to_string()
}

fn default_export_visibility() -> String {
    "listed".to_string()
}

fn default_audit_limit() -> usize {
    50
}

// ─── Registration / Discovery ───────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct HubRegisterParams {
    /// Unique capability ID, e.g. "skill:code-review"
    pub id: String,
    /// Type: "skill" | "plugin"
    pub cap_type: String,
    /// Human-readable name
    pub name: String,
    /// Short description
    #[serde(default)]
    pub description: String,
    /// JSON string of capability definition (prompt template, manifest, config)
    #[serde(default)]
    pub definition: String,
    /// Version number
    #[serde(default = "default_hub_version")]
    pub version: u32,
    /// Target database scope: "global" or "project" (default)
    #[serde(default = "default_scope")]
    pub scope: String,
}

/// PR6: composite "register + (optional) auto-approve" params.
///
/// `auto_approve = true` requests a follow-up `hub_review` step for a
/// registration that came back disabled; registrations that are already
/// enabled at register time (skills, plugins) report "already_enabled".
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct HubQuickAddParams {
    /// Unique capability ID, e.g. "skill:code-review"
    pub id: String,
    /// Type: "skill" | "plugin"
    pub cap_type: String,
    /// Human-readable name
    pub name: String,
    /// Short description
    #[serde(default)]
    pub description: String,
    /// JSON string of capability definition (prompt template, manifest, config)
    #[serde(default)]
    pub definition: String,
    /// Version number
    #[serde(default = "default_hub_version")]
    pub version: u32,
    /// Target database scope: "global" or "project" (default)
    #[serde(default = "default_scope")]
    pub scope: String,
    /// Request immediate approve+enable after register.
    #[serde(default)]
    pub auto_approve: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct HubDiscoverParams {
    /// Optional search query (searches name + description)
    #[serde(default)]
    pub query: Option<String>,
    /// Optional type filter: "skill" | "plugin"
    #[serde(default)]
    pub cap_type: Option<String>,
    /// Only return enabled capabilities (default: true)
    #[serde(default = "default_true")]
    pub enabled_only: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct HubGetParams {
    /// Capability ID to retrieve
    pub id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct HubFeedbackParams {
    /// Capability ID
    pub id: String,
    /// Whether the invocation was successful
    pub success: bool,
    /// Optional user rating (0.0 - 5.0)
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    #[schemars(schema_with = "super::coerce::opt_number_from_string_or_number_schema")]
    pub rating: Option<f64>,
}

// ─── Review / Governance ────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct HubSetEnabledParams {
    /// Capability ID
    pub id: String,
    /// Whether to enable (true) or disable (false)
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct HubReviewParams {
    /// Capability ID
    pub id: String,
    /// Governance review status: pending | approved | rejected
    #[serde(default = "default_hub_review_status")]
    pub review_status: String,
    /// Optional explicit enabled state override
    #[serde(default)]
    pub enabled: Option<bool>,
}

// ─── Export / Evolve / Chain ────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ExportSkillsParams {
    /// Target agent format: "claude", "openclaw", "cursor", "generic"
    /// Defaults to "claude" if omitted.
    #[serde(default = "default_export_agent")]
    pub agent: String,

    /// Only export skills matching these IDs (e.g. ["skill:code-review"]).
    /// If omitted, exports all enabled skills matching the visibility filter.
    #[serde(default)]
    pub skill_ids: Option<Vec<String>>,

    /// Visibility filter: "listed", "discoverable", "all" (default: "listed")
    #[serde(default = "default_export_visibility")]
    pub visibility: String,

    /// Override output directory. If omitted, uses agent-specific defaults:
    /// - claude: ~/.tachi/skills/ (with symlinks to ~/.claude/skills/)
    /// - openclaw: ~/.openclaw/plugins/
    /// - cursor: .cursor/rules/ (relative to cwd)
    /// - generic: ./exported-skills/
    #[serde(default)]
    pub output_dir: Option<String>,

    /// Request cleanup (default: false). Claude exports preserve existing entries; cleanup is not done.
    #[serde(default)]
    pub clean: bool,
}

// ─── Audit Log ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AuditLogParams {
    /// Maximum entries to return (default: 50)
    #[serde(default = "default_audit_limit")]
    pub limit: usize,
    /// Optional server_id filter
    #[serde(default)]
    pub server_filter: Option<String>,
}
