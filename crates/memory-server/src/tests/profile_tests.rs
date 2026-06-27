use super::{call_tool_via_server, make_server, make_server_with_temp_home};
use crate::server_state::{AgentProfile, RATE_LIMIT_MAX_BURST_KEYS, RATE_LIMIT_MAX_SESSIONS};
use crate::tool_params::{AgentRegisterParams, AgentWhoamiParams};
use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::json;

mod agent_identity;
mod rate_limit;
mod runtime;
mod tool_profile;
