use super::make_server;
use crate::tool_params::{ExportSkillsParams, HubFeedbackParams, HubRegisterParams};
use memcore::HubCapability;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

mod export_skills;
mod feedback_stats;
mod quick_add;
mod register_review;
mod skill_quality_refresh;
