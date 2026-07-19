use super::super::{make_entry, make_server, make_server_with_temp_home};
use super::{
    dispatch_params, save_grep_evidence_feedback_rule, wait_for_dispatch_result, EnvVarGuard,
};
use crate::tool_params::{DispatchMcpAccessParams, TachiBoardParams, TachiDispatchParams};
use crate::vault_ops::{VaultInitParams, VaultSetParams};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

mod board_ledger;
mod capability_dispatch;
mod credential_profiles;
mod prompt_context;

// tachi#1288 (Fix C, CLI-era fixture cleanup): `v2_smoke` (an `#[ignore]`d
// `v2_two_stage_smoke` test) was deleted. Its doc claimed "the pool invokes
// claude -p --output-format json --dangerously-skip-permissions" driven by a
// `CLAUDE_BIN`-pointed fake binary -- that mechanism was removed by #1274
// (ClaudePool decommission step 2/3); `dispatch_v2::call_plan_llm` now calls
// the real reasoning-LLM provider directly and never reads `CLAUDE_BIN` (see
// `dispatch_v2.rs`'s `call_plan_llm_never_reaches_cli_binary_resolver`
// negative control, and `board_first.rs`'s module doc for the live-test
// counterpart of this same cleanup). The deleted test was already inert --
// `#[ignore]`d, and its premise no longer exists -- so removing it costs zero
// test-suite coverage.
