pub(crate) use tachi_params::*;

use rmcp::schemars;
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Standard Tachi facade parameters for terminal dispatch receipt reads/acks.
/// Identity is deliberately absent: the server resolves the recipient seat.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(crate) struct TerminalInboxParams {
    /// `list` returns this caller's receipts; `ack` persists acknowledgement.
    pub action: String,
    #[serde(default)]
    pub dispatch_id: Option<String>,
    #[serde(default)]
    pub include_acknowledged: bool,
    #[serde(default = "default_terminal_inbox_limit")]
    pub limit: usize,
}

fn default_terminal_inbox_limit() -> usize {
    20
}
