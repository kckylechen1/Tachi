use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentProfileDocumentKind {
    Identity,
    Agents,
    LatestTruths,
    RoutingPolicy,
    ToolPolicy,
    MemoryPolicy,
    Other,
}

impl AgentProfileDocumentKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "identity" | "identity_md" => Some(Self::Identity),
            "agents" | "agents_md" => Some(Self::Agents),
            "latest_truths" | "latest_truths_md" => Some(Self::LatestTruths),
            "routing_policy" | "routing" => Some(Self::RoutingPolicy),
            "tool_policy" | "tooling_policy" => Some(Self::ToolPolicy),
            "memory_policy" => Some(Self::MemoryPolicy),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentProfileDocument {
    pub kind: AgentProfileDocumentKind,
    pub path: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FoundryEvidenceKind {
    Memory,
    Reflection,
    Tooluse,
    Eval,
    Ghost,
    SessionOutcome,
    SkillTelemetry,
    ProfileSnapshot,
    Proposal,
    Other,
}

impl FoundryEvidenceKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "memory" => Some(Self::Memory),
            "reflection" | "reflections" => Some(Self::Reflection),
            "tooluse" | "tool_use" | "tool" => Some(Self::Tooluse),
            "eval" | "evaluation" => Some(Self::Eval),
            "ghost" => Some(Self::Ghost),
            "session_outcome" | "session" => Some(Self::SessionOutcome),
            "skill_telemetry" | "skill" => Some(Self::SkillTelemetry),
            "profile_snapshot" | "profile" => Some(Self::ProfileSnapshot),
            "proposal" | "proposals" => Some(Self::Proposal),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FoundryEvidence {
    pub kind: FoundryEvidenceKind,
    pub title: Option<String>,
    pub content: String,
    pub source_ref: Option<String>,
    pub path: Option<String>,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FoundryModelLane {
    Embedding,
    Extraction,
    Maintenance,
    Rerank,
    Distill,
    Reasoning,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FoundryJobKind {
    SessionIngest,
    MemoryEnrichment,
    #[serde(alias = "memory_rerank")]
    MemoryNeighborhood,
    RecallRerankCache,
    MemoryDistill,
    ForgetSweep,
    SkillEvolution,
    AgentEvolution,
    ProfileProjection,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FoundryJobStatus {
    Planned,
    Queued,
    Running,
    Completed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FoundryJobSpec {
    pub id: String,
    pub kind: FoundryJobKind,
    pub lane: FoundryModelLane,
    pub status: FoundryJobStatus,
    pub target_agent_id: Option<String>,
    pub requested_by: Option<String>,
    pub created_at: String,
    pub evidence_count: usize,
    pub goal_count: usize,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentEvolutionProposal {
    pub title: String,
    pub target: String,
    pub target_section: Option<String>,
    pub current_value: Option<String>,
    pub suggested_value: String,
    pub rationale: String,
    pub risk: String,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentEvolutionSynthesis {
    pub summary: String,
    pub stable_signals: Vec<String>,
    pub drift_signals: Vec<String>,
    pub proposals: Vec<AgentEvolutionProposal>,
    pub no_change_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_memory_rerank_deserializes_as_neighborhood_job() {
        let kind: FoundryJobKind = serde_json::from_str("\"memory_rerank\"").unwrap();
        assert_eq!(kind, FoundryJobKind::MemoryNeighborhood);
    }

    #[test]
    fn split_background_jobs_have_distinct_wire_names() {
        assert_eq!(
            serde_json::to_string(&FoundryJobKind::MemoryNeighborhood).unwrap(),
            "\"memory_neighborhood\""
        );
        assert_eq!(
            serde_json::to_string(&FoundryJobKind::RecallRerankCache).unwrap(),
            "\"recall_rerank_cache\""
        );
    }
}
