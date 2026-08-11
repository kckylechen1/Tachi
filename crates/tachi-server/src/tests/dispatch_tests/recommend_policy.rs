use super::super::{make_server, make_server_with_temp_home};
use super::completion_eval::seed_dispatch_run;
use super::{run_tune, tune_params};
use crate::tool_params::{TachiAgentsParams, TachiCompleteParams, TachiSkillParams};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::json;

mod proposal_evolution;
mod route_policy;
