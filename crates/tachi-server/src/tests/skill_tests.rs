use super::{make_entry, make_server, make_skill_capability};
use crate::tool_params::{HubRegisterParams, TachiSkillParams};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

mod builtin_ingest;
mod compact_context;
mod compact_session;
mod discover;
mod retired_actions;
mod run_recommend;
