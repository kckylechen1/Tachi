use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

// ─── Peer publication broker (#1016) ────────────────────────────────────────

/// Parameters for `peer_query` — the local peer-publication broker (#1016).
/// Advisory read-only surface: one exhaustively-whitelisted `noun` routed to
/// a structurally read-only projection of THIS host's own daemon state, over
/// a self-asserted-local trust boundary (`same_host_loopback_v1`).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PeerQueryParams {
    /// Target session_client. For `noun="presence"` this is optional — when
    /// present it narrows the board to that session's live claim(s), when
    /// absent the full board is returned. For `noun="run"` it is REQUIRED —
    /// `run` addresses one specific peer (resolved to its active claim's
    /// dispatch_id), it is never a board scan; omitting it is denied with
    /// `code="target_required"`.
    #[serde(default)]
    pub target_session_client: Option<String>,
    /// The peer-publication noun to read. Exhaustive whitelist: `"presence"`
    /// (claims heartbeat board) and `"run"` (target's active dispatch's
    /// status.json + a recent progress.jsonl tail). Any other noun
    /// (including future `checkpoint`/`outcomes`/`sticky`/`handoff`) is
    /// denied — never silently routed.
    pub noun: String,
}
