use clap::Subcommand;
use std::path::PathBuf;

/// `tachi broker …` — model-broker alias governance (tachi#1681 D2).
///
/// Three verbs and no fourth. There is deliberately **no** `bind`/`unbind`
/// convenience verb: an alias is the name live traffic routes by, and a
/// one-shot mutation would land a routing change with no artifact anybody
/// reviewed. Everything that writes goes through plan → read → apply.
#[derive(Subcommand, Debug, Clone)]
pub enum BrokerAction {
    /// Show the recorded alias set and its policy revision. Read-only.
    Aliases {
        /// Emit JSON instead of the human table.
        #[arg(long)]
        json: bool,
    },
    /// Build a bound alias plan from the catalog. Read-only: writes nothing to
    /// the database, and writes a file only with --out.
    Plan {
        /// Emit the plan artifact as JSON on stdout.
        #[arg(long)]
        json: bool,
        /// Also write the plan artifact to this path, for review and later
        /// `broker apply --plan`.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
    },
    /// Apply a previously written plan. Every binding the plan recorded is
    /// re-verified inside one write transaction; any drift writes nothing.
    Apply {
        /// Plan artifact written by `broker plan --out`.
        #[arg(long, value_name = "PATH")]
        plan: PathBuf,
        /// Emit the apply report as JSON.
        #[arg(long)]
        json: bool,
    },
}
