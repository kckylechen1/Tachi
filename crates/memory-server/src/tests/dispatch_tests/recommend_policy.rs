use super::super::make_server;
use super::task_params;
use crate::tool_params::{
    TachiAgentsParams, TachiCompleteParams, TachiSkillParams, TachiSubagentEvalParams,
};
use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::json;

mod proposal_evolution;
mod recommendation;
mod route_policy;
