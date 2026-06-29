use crate::hub_helpers::CapabilityVisibility;
use memory_core::HubCapability;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone)]
pub(super) struct CapabilityRecord {
    pub(super) cap: HubCapability,
    pub(super) db: &'static str,
    pub(super) visibility: CapabilityVisibility,
    pub(super) callable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct CapabilityRecommendation {
    pub(super) id: String,
    pub(super) cap_type: String,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) db: String,
    pub(super) visibility: String,
    pub(super) callable: bool,
    pub(super) score: f64,
    pub(super) reasons: Vec<String>,
    pub(super) uses: u64,
    pub(super) avg_rating: f64,
    pub(super) suggested_tool_name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) pattern_refs: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PackRecommendation {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) version: String,
    pub(super) projected_to_host: bool,
    pub(super) projected_path: Option<String>,
    pub(super) score: f64,
    pub(super) reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct CapabilityBundleSection {
    pub(super) title: String,
    pub(super) estimated_tokens: usize,
    pub(super) block: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct CapabilityBundle {
    pub(super) primary_skill: Option<CapabilityRecommendation>,
    pub(super) supporting_capabilities: Vec<CapabilityRecommendation>,
    pub(super) packs: Vec<PackRecommendation>,
    pub(super) host_tools: Vec<String>,
    pub(super) activation_steps: Vec<String>,
    pub(super) rationale: Vec<String>,
    pub(super) section: Option<CapabilityBundleSection>,
}
