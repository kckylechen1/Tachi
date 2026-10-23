use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

fn default_copilot_top_k() -> usize {
    6
}

// ─── Copilot / Task Guidance ────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProgressCheckParams {
    /// Natural-language task currently being attempted.
    pub task: String,

    /// Attempts already made, in chronological order.
    #[serde(default)]
    pub attempts: Vec<String>,

    /// Latest error, symptom, or failed observation.
    #[serde(default)]
    pub latest_error: Option<String>,

    /// Optional canonical agent id for sandbox filtering and context scoping.
    #[serde(default)]
    pub agent_id: Option<String>,

    /// Optional named project DB.
    #[serde(default)]
    pub project: Option<String>,

    /// Optional domain filter.
    #[serde(default)]
    pub domain: Option<String>,

    /// Number of relevant wiki hits to return.
    #[serde(default = "default_copilot_top_k")]
    pub top_k: usize,

    /// Optional flow id for append-only progress.jsonl logging under .tachi/runs/<flow_id>.
    #[serde(default)]
    pub flow_id: Option<String>,
}
