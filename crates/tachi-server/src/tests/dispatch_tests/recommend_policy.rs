use super::super::{make_server, make_server_with_temp_home};
use super::completion_eval::seed_dispatch_run;
use super::{run_tune, task_params, tune_params};
use crate::tool_params::{
    TachiAgentsParams, TachiCompleteParams, TachiSkillParams, TachiSubagentEvalParams,
};
use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::json;

mod proposal_evolution;
mod recommendation;
mod route_policy;
