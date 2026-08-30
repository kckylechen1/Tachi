//! Consumer-neutral delivery seam (#1679): the typed wire surface a host
//! (ZeroClaw or another General-Agent requester) implements to claim,
//! acknowledge, refuse, and resume durable result deliveries.
//!
//! Structural law this surface enforces by construction:
//! - **No destination exists.** No action accepts a user, channel, session
//!   target, or "send to" field. The requester binding on an intent is
//!   recorded from ADMITTED identity at mint time (server planes); the seam
//!   caller can only ever be the requester itself. `deny_unknown_fields`
//!   turns any worker-injected destination key into a hard deserialization
//!   refusal (discrimination 7's first gate).
//! - **No mint.** Delivery intents are minted only by Tachi's own terminal
//!   planes (managed `tachi_complete` / attached `ingest_session_event`).
//!   There is no seam action that creates one, so a worker cannot mint
//!   delivery of anything — including a channel choice.
//! - **No execution/adjudication writes.** The seam's responses carry
//!   delivery state only; success or failure here is never an execution
//!   verdict and never an adjudication (tachi#1636 three-plane law).
//!
//! Actions: `claim_ready_delivery`, `ack_delivered`, `reject_or_block`,
//! `resume_requester_operation`, `dismiss`, `get`. The first four are the
//! frozen host-integration contract from the issue body; `dismiss` and
//! `get` round out the retriable/dismissible requirement and the #1693
//! projection observation.

use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

/// Closed seam action set (#1679). Also the wire enum: an unknown or
/// mint-shaped action fails at deserialization, before any handler runs.
pub const TACHI_DELIVERY_ACTIONS: &[&str] = &[
    "claim_ready_delivery",
    "ack_delivered",
    "reject_or_block",
    "resume_requester_operation",
    "dismiss",
    "get",
];

/// The frozen seam actions as a closed wire type (#1679). There is no
/// `mint` variant: delivery intents are minted only by Tachi's own
/// terminal planes, so a worker-shaped "create a delivery" call is a
/// deserialization refusal, structurally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TachiDeliveryAction {
    ClaimReadyDelivery,
    AckDelivered,
    RejectOrBlock,
    ResumeRequesterOperation,
    Dismiss,
    Get,
}

impl TachiDeliveryAction {
    /// The frozen wire token (matches [`TACHI_DELIVERY_ACTIONS`]).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaimReadyDelivery => "claim_ready_delivery",
            Self::AckDelivered => "ack_delivered",
            Self::RejectOrBlock => "reject_or_block",
            Self::ResumeRequesterOperation => "resume_requester_operation",
            Self::Dismiss => "dismiss",
            Self::Get => "get",
        }
    }
}

/// Parameters for `tachi_delivery`. Unknown fields are refused so a
/// caller-supplied destination, policy, or identity field cannot silently
/// become authority.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TachiDeliveryParams {
    pub action: TachiDeliveryAction,

    /// Admitted requester AgentIdentity id. Verified against the
    /// `agent_identities` registry server-side before any seam operation.
    #[serde(default)]
    #[schemars(description = "[all actions|required] Admitted requester AgentIdentity id.")]
    pub agent_identity_id: Option<String>,

    /// Admitted host identity of the requester (verified server-side; this
    /// is the claim holder an ack must match).
    #[serde(default)]
    #[schemars(description = "[all actions|required] Admitted host identity of the requester.")]
    pub host_identity: Option<String>,

    /// Claim idempotency key. Replaying the same key for the same intent
    /// returns the same receipt without a duplicate attempt.
    #[serde(default)]
    #[schemars(
        description = "[action=claim_ready_delivery|required] Claim idempotency key (replay-safe)."
    )]
    pub claim_key: Option<String>,

    /// Claim lease TTL in seconds (requester crash window; default 300).
    #[serde(default)]
    #[schemars(description = "[action=claim_ready_delivery] Claim lease seconds (default 300).")]
    pub lease_seconds: Option<i64>,

    /// Ack idempotency key.
    #[serde(default)]
    #[schemars(description = "[action=ack_delivered|required] Ack idempotency key.")]
    pub ack_key: Option<String>,

    /// Delivery intent id (required for ack/reject/dismiss/get).
    #[serde(default)]
    #[schemars(
        description = "[action=ack_delivered|reject_or_block|dismiss|get|required] Delivery intent id."
    )]
    pub delivery_id: Option<String>,

    /// Typed blocker class for reject_or_block. `ambiguous_send_outcome`
    /// parks the intent in blocked (never auto-redispatched); retryable
    /// classes take retrying.
    #[serde(default)]
    #[schemars(description = "[action=reject_or_block|required] Typed blocker class.")]
    pub blocker_class: Option<String>,

    /// Bounded public-safe blocker detail (max 1000 chars); never a
    /// transcript, never a destination.
    #[serde(default)]
    #[schemars(description = "[action=reject_or_block] Bounded detail (max 1000 chars).")]
    pub detail: Option<String>,

    /// Retry backoff seconds for a retryable blocker class.
    #[serde(default)]
    #[schemars(
        description = "[action=reject_or_block] Retry backoff seconds for retryable classes."
    )]
    pub retry_in_seconds: Option<i64>,

    /// Expected intent revision (compare-and-swap). A stale expectation is
    /// a typed conflict, never a lost update.
    #[serde(default)]
    #[schemars(
        description = "[action=ack_delivered|reject_or_block|dismiss] Expected intent revision (CAS)."
    )]
    pub expected_revision: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Discrimination 7 (wire gate): no destination-shaped field can enter
    /// through the seam; unknown fields fail closed at deserialization.
    #[test]
    fn seam_wire_refuses_worker_chosen_destinations() {
        let valid: TachiDeliveryParams = serde_json::from_value(serde_json::json!({
            "action": "claim_ready_delivery",
            "agent_identity_id": "agent.requester",
            "host_identity": "host-1",
            "claim_key": "ck-1"
        }))
        .expect("frozen claim wire");
        assert_eq!(valid.action, TachiDeliveryAction::ClaimReadyDelivery);

        for forged in [
            serde_json::json!({
                "action": "claim_ready_delivery",
                "agent_identity_id": "agent.requester",
                "host_identity": "host-1",
                "claim_key": "ck-1",
                "deliver_to_user": "wechat:bob"
            }),
            serde_json::json!({
                "action": "claim_ready_delivery",
                "agent_identity_id": "agent.requester",
                "host_identity": "host-1",
                "claim_key": "ck-1",
                "channel": "wecom"
            }),
            serde_json::json!({
                "action": "mint",
                "agent_identity_id": "agent.requester",
                "host_identity": "host-1"
            }),
            serde_json::json!({
                "action": "claim_ready_delivery",
                "agent_identity_id": "agent.requester",
                "host_identity": "host-1",
                "claim_key": "ck-1",
                "delivery_policy": "announce_requester_session"
            }),
        ] {
            assert!(
                serde_json::from_value::<TachiDeliveryParams>(forged).is_err(),
                "destination/mint/policy fields must fail at deserialization"
            );
        }
    }
}
