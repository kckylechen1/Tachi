use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

/// Closed local advisory-mailbox action set (#1751).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TachiA2aAction {
    Respond,
    Send,
    Status,
    Receive,
}

/// Parameters for the same-host mailbox. Send/receive require a bound project.
///
/// Unknown fields are refused so caller-controlled identity, message kind,
/// seat, model, or session aliases cannot silently become authority.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TachiA2aParams {
    pub action: TachiA2aAction,

    #[serde(default)]
    #[schemars(description = "[action=respond/send|required] Stable recipient AgentIdentity id.")]
    pub recipient_agent_identity_id: Option<String>,

    #[serde(default)]
    #[schemars(
        description = "[action=respond/send|required] Respond: peer_publication:<id> or a2a_envelope:<id>. Send: coordination:<opaque-id>."
    )]
    pub subject_ref: Option<String>,

    #[serde(default)]
    #[schemars(
        description = "[action=respond/send|required] Scrubbed advisory text, at most 4096 bytes."
    )]
    pub text: Option<String>,

    #[serde(default)]
    #[schemars(description = "[action=respond/send|required] Issuer-scoped idempotency key.")]
    pub idempotency_key: Option<String>,

    #[serde(default)]
    #[schemars(
        description = "[action=respond/send] Expiry in days, 1..=30. Required for send; respond defaults to 7."
    )]
    pub ttl_days: Option<u32>,

    #[serde(default)]
    #[schemars(
        description = "[action=status/receive] Row limit, 1..=100; defaults to 20. Status never consumes; receive deliberately consumes."
    )]
    pub limit: Option<usize>,
}
