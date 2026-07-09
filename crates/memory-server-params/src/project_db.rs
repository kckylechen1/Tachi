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
