use clap::Subcommand;

/// Operator-authorized ACP capability administration on admitted
/// AgentIdentity rows (#1960). Reachable only from the host CLI: an agent
/// session cannot invoke these commands, and the acting operator is
/// recorded on every audit receipt.
#[derive(Subcommand, Debug, Clone)]
pub enum AgentAcpAction {
    /// Grant the #1733 closed ACP envelope to an admitted identity.
    Grant {
        /// The AgentIdentity id (for example the zeroclaw sub-agent id).
        #[arg(long, value_name = "IDENTITY")]
        identity: String,
        /// Canonical tool profiles: observe, delegate (comma-separated).
        #[arg(long, value_delimiter = ',', value_name = "PROFILES")]
        tool_profiles: Vec<String>,
        /// Canonical capability classes: tachi (comma-separated).
        #[arg(long, value_delimiter = ',', value_name = "CLASSES")]
        capability_classes: Vec<String>,
        /// The acting operator, recorded on the audit receipt.
        #[arg(long, value_name = "ACTOR")]
        actor: String,
    },
    /// Revoke the ACP capability from an identity (new attachments refuse).
    Revoke {
        /// The AgentIdentity id.
        #[arg(long, value_name = "IDENTITY")]
        identity: String,
        /// The acting operator, recorded on the audit receipt.
        #[arg(long, value_name = "ACTOR")]
        actor: String,
    },
}
