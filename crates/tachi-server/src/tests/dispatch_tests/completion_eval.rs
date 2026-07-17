use super::super::{make_entry, make_server, make_server_with_temp_home};
use super::{save_grep_evidence_feedback_rule, task_params};
use crate::tool_params::{
    GetMemoryParams, SearchMemoryParams, TachiAgentEvalParams, TachiCompleteParams,
    TachiSubagentEvalParams,
};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

mod aggregate;
mod completion_record;
mod flow_dispatch;
mod mirror_eval;
