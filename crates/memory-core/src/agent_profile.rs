use serde::{Deserialize, Serialize};

pub const AGENT_PROFILE_PACK_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentProfileIdentity {
    pub name: Option<String>,
    pub emoji: Option<String>,
    pub vibe: Option<String>,
    pub avatar: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProfileSource {
    pub kind: String,
    pub path: Option<String>,
    pub section: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProfileRule {
    pub id: String,
    pub text: String,
    pub tags: Vec<String>,
    pub source: Option<AgentProfileSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProfilePack {
    pub schema_version: u32,
    pub agent_id: String,
    pub display_name: Option<String>,
    pub identity: AgentProfileIdentity,
    pub voice: Vec<AgentProfileRule>,
    pub operating_contract: Vec<AgentProfileRule>,
    pub quality_bar: Vec<AgentProfileRule>,
    pub tool_policy: Vec<AgentProfileRule>,
    pub memory_policy: Vec<AgentProfileRule>,
    pub user_model: Vec<AgentProfileRule>,
    pub role_hats: Vec<AgentProfileRule>,
    pub project_overlays: Vec<AgentProfileRule>,
    pub runtime_bindings: Vec<AgentProfileRule>,
    pub provenance: Vec<AgentProfileSource>,
}

impl AgentProfilePack {
    pub fn new(agent_id: impl Into<String>, display_name: Option<String>) -> Self {
        Self {
            schema_version: AGENT_PROFILE_PACK_SCHEMA_VERSION,
            agent_id: agent_id.into(),
            display_name,
            identity: AgentProfileIdentity::default(),
            voice: Vec::new(),
            operating_contract: Vec::new(),
            quality_bar: Vec::new(),
            tool_policy: Vec::new(),
            memory_policy: Vec::new(),
            user_model: Vec::new(),
            role_hats: Vec::new(),
            project_overlays: Vec::new(),
            runtime_bindings: Vec::new(),
            provenance: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderedAgentProfile {
    pub target: String,
    pub filename: String,
    pub content: String,
    pub dry_run: bool,
}
