use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

/// Closed local advisory-mailbox action set (#1751).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TachiA2aAction {
    Respond,
    Status,
}

/// Parameters for the same-host `turn_response/v1` mailbox.
///
/// Unknown fields are refused so caller-controlled identity, message kind,
/// seat, model, or session aliases cannot silently become authority.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TachiA2aParams {
    pub action: TachiA2aAction,

    #[serde(default)]
    #[schemars(description = "[action=respond|required] Stable recipient AgentIdentity id.")]
    pub recipient_agent_identity_id: Option<String>,

    #[serde(default)]
    #[schemars(
        description = "[action=respond|required] peer_publication:<id> or a2a_envelope:<id>."
    )]
    pub subject_ref: Option<String>,

    #[serde(default)]
    #[schemars(
        description = "[action=respond|required] Scrubbed advisory text, at most 4096 bytes."
    )]
    pub text: Option<String>,

    #[serde(default)]
    #[schemars(description = "[action=respond|required] Issuer-scoped idempotency key.")]
    pub idempotency_key: Option<String>,

    #[serde(default)]
    #[schemars(description = "[action=respond] Expiry in days, 1..=30; defaults to 7.")]
    pub ttl_days: Option<u32>,

    #[serde(default)]
    #[schemars(description = "[action=status] Header/receipt row limit, 1..=100; defaults to 20.")]
    pub limit: Option<usize>,
}
