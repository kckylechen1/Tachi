use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

// ─── Peer publication broker (#1016) ────────────────────────────────────────

/// Parameters for `peer_query` — the S1 local peer-publication broker
/// (#1016). Advisory read-only surface: one exhaustively-whitelisted `noun`
/// routed to a structurally read-only projection of THIS host's own daemon
/// state, over a self-asserted-local trust boundary (`same_host_loopback_v1`).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PeerQueryParams {
    /// Optional target session_client. When present, the presence answer is
    /// narrowed to that session's live claim(s) (a single-seat row); when
    /// absent, the full local presence board is returned.
    #[serde(default)]
    pub target_session_client: Option<String>,
    /// The peer-publication noun to read. S1 exhaustively whitelists exactly
    /// one value: `"presence"`. Any other noun (including future
    /// `outcomes`/`sticky`/`handoff`) is denied — never silently routed.
    pub noun: String,
}
