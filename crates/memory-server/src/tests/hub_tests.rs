use super::{acquire_real_home_lock, make_server};
use crate::tool_params::{
    ExportSkillsParams, HubCallParams, HubDisconnectParams, HubFeedbackParams, HubRegisterParams,
    TachiTaskParams,
};
use crate::utils::lock_or_recover;
use memory_core::HubCapability;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

mod export_skills;
mod feedback_stats;
mod quick_add;
mod register_review;
mod schema;
