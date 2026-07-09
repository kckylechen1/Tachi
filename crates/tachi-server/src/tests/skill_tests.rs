use super::{
    make_entry, make_server, make_server_with_temp_home, make_skill_capability, TempHomeGuard,
};
use crate::tool_params::{
    AgentEvolutionDocumentPathParams, AgentEvolutionEvidencePathParams,
    AgentEvolutionMemoryQueryParams, CompactSessionMemoryParams, DistillTrajectoryParams,
    HubRegisterParams, IngestSourceParams, ListAgentEvolutionProposalsParams,
    PrepareCapabilityBundleParams, RecommendCapabilityParams, RecommendSkillParams,
    RecommendToolchainParams, ReviewAgentEvolutionProposalParams, RunSkillParams,
    SynthesizeAgentEvolutionParams, TachiCompleteParams, TachiSkillParams,
};
use chrono::Utc;
use memcore::MemoryEntry;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};
use std::time::Duration;

mod agent_evolution;
mod builtin_ingest;
mod bundle;
mod discover;
mod from_pattern;
mod loadout;
mod run_recommend;
