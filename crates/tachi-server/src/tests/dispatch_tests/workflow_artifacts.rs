use super::super::{
    make_entry, make_server, make_server_with_temp_home, plant_leftover_shared_wiki,
};
use super::{dispatch_params, task_params, EnvVarGuard};
use crate::tool_params::{GetMemoryParams, TaskBriefParams};
use chrono::Utc;
use memcore::MemoryEntry;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

fn cycle_view(raw: &str) -> Value {
    let response: Value = serde_json::from_str(raw).expect("status response JSON");
    response
        .get("cycle")
        .cloned()
        .expect("status lifecycle response must nest its cycle view")
}

mod briefing_doc_index;
mod closure_dispatch_markers;
mod cycle_status;
mod intake_flow;
mod pr_release_handoff;
mod ux_dispatch_gates;
