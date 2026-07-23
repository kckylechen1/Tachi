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

/// Completion now refuses to invent a durable receipt for an arbitrary
/// dispatch id. Fixtures that exercise a legitimate completion must therefore
/// seed the run authority the dispatch launcher would have created.
pub(crate) fn seed_dispatch_run(server: &crate::MemoryServer, dispatch_id: &str) {
    let run_dir = server.tachi_home_dir().join("runs").join(dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("create trusted dispatch run directory");
    std::fs::write(
        run_dir.join("status.json"),
        json!({ "dispatch_id": dispatch_id }).to_string(),
    )
    .expect("seed trusted dispatch status");
}
