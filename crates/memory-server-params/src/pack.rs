use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

fn default_project_db_relpath() -> String {
    ".tachi/memory.db".to_string()
}

// ─── Project DB ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct InitProjectDbParams {
    /// Optional target repository root. Defaults to current git root.
    #[serde(default)]
    pub project_root: Option<String>,
    /// Relative DB path under the repo root.
    #[serde(default = "default_project_db_relpath")]
    pub db_relpath: String,
}

// ─── Pack System ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PackListParams {
    /// If true, only return enabled packs (default: false)
    #[serde(default)]
    pub enabled_only: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PackGetParams {
    /// Pack identifier, e.g. "garrytan/gstack" or "obra/superpowers"
    pub id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PackRegisterParams {
    /// Pack identifier, e.g. "garrytan/gstack"
    pub id: String,

    /// Display name
    #[serde(default)]
    pub name: Option<String>,

    /// Source URI: "github:owner/repo", "local:/path/to/pack"
    #[serde(default)]
    pub source: Option<String>,

    /// Version string (git tag, commit sha, or semver)
    #[serde(default)]
    pub version: Option<String>,

    /// Short description
    #[serde(default)]
    pub description: Option<String>,

    /// Local filesystem path where the pack is stored
    #[serde(default)]
    pub local_path: Option<String>,

    /// Extra metadata (JSON object)
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PackRemoveParams {
    /// Pack identifier to remove
    pub id: String,

    /// If true (default), also delete projected files from agent directories
    #[serde(default)]
    pub clean_files: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PackProjectParams {
    /// Pack identifier to project
    pub pack_id: String,

    /// List of agent kinds to project to: "claude", "codex", "cursor", "gemini", "openclaw", "antigravity", "trae", "kiro", "generic"
    pub agents: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProjectionListParams {
    /// Filter by agent kind (optional)
    #[serde(default)]
    pub agent: Option<String>,

    /// Filter by pack ID (optional)
    #[serde(default)]
    pub pack_id: Option<String>,
}
