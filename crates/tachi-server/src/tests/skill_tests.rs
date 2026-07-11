use super::{make_entry, make_server, make_skill_capability};
use crate::tool_params::{
    DistillTrajectoryParams, HubRegisterParams, IngestSourceParams, PrepareCapabilityBundleParams,
    RecommendCapabilityParams, RecommendSkillParams, RecommendToolchainParams, RunSkillParams,
    TachiCompleteParams, TachiSkillParams,
};
use chrono::Utc;
use memcore::MemoryEntry;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};
use std::time::Duration;

mod builtin_ingest;
mod bundle;
mod discover;
mod from_pattern;
mod loadout;
mod run_recommend;
