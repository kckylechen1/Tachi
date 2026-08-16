use clap::Subcommand;
use std::path::PathBuf;

/// Operator-only migration of the retired legacy sticky surface.
#[derive(Subcommand, Debug, Clone)]
pub enum A2aAction {
    /// Inventory and retire legacy sticky rows into the A2A envelope model.
    StickyCutover {
        #[command(subcommand)]
        action: StickyCutoverAction,
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
