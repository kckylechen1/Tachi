use clap::Subcommand;
use std::path::PathBuf;

/// Operator-only A2A administration.
#[derive(Subcommand, Debug, Clone)]
pub enum A2aAction {
    /// Run or inspect the separately authenticated remote admission gateway.
    Remote {
        #[command(subcommand)]
        action: RemoteA2aAction,
    },
    /// Inventory and retire legacy sticky rows into the A2A envelope model.
    StickyCutover {
        #[command(subcommand)]
        action: StickyCutoverAction,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum RemoteA2aAction {
    /// Freeze an authorized origin job, then make at most three durable attempts.
    Submit {
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
        #[arg(long, value_name = "PATH")]
        request: PathBuf,
        /// Persist without network I/O; resume later by the returned submission id.
        #[arg(long)]
        prepare_only: bool,
    },
    /// Resume the same frozen submission within its original budget and window.
    Resume {
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
        #[arg(long)]
        submission: String,
    },
    /// Inspect body-free origin state; unknown is not remote refusal or completion.
    Status {
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
        #[arg(long)]
        submission: String,
        /// Query the enrolled destination over mTLS `GetTask` and reconcile monotonically.
        #[arg(long)]
        reconcile: bool,
    },
    /// Serve mutual-TLS admission only; this does not expose MCP or start jobs.
    Serve {
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },
    /// Verify an enrolled destination before sending any application content.
    Probe {
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
        #[arg(long)]
        peer: String,
    },
    /// Append a revocation for an mTLS admission, retiring its peer key.
    Revoke {
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
        #[arg(long)]
        admission: String,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum StickyCutoverAction {
    /// Print a read-only, editable cutover plan to stdout.
    Plan,
    /// Apply one frozen plan and durably publish its receipt.
    Apply {
        /// Frozen plan JSON previously emitted by `plan`.
        #[arg(long, value_name = "RECEIPT")]
        plan: PathBuf,
        /// Explicitly confirm the irreversible legacy-row retirement.
        #[arg(long, required = true)]
        confirm: bool,
    },
}
