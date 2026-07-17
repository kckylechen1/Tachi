use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

/// Parameters for the raw `tachi_status` MCP tool (tachi#1201 k3).
///
/// Prior to k3 this tool took zero parameters and always returned JSON; this
/// struct is net-new API surface, not a default-value flip on an existing
/// field.
#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct TachiStatusParams {
    /// Response shape. Omitted, empty, or anything other than "json" renders
    /// a compact human-readable markdown digest; pass "json" explicitly to
    /// get the full JSON payload, byte-identical to the pre-k3 unconditional
    /// shape. NOTE the default polarity here is the OPPOSITE of the
    /// `tachi_memory`/`tachi_search` facade's `format` field (that one
    /// defaults an omitted value to JSON) — this field only governs the raw
    /// `tachi_status` tool, not the higher-level facades.
    #[serde(default)]
    pub format: Option<String>,
}
