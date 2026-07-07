use clap::Parser;
use std::path::PathBuf;

// ─── CLI Arguments ────────────────────────────────────────────────────────────

// `--version` prints the short form (just the semver); `--version` on its own
// uses `version`. A long-form (`tachi --version` already covers the short
// value; clap shows `long_version` when invoked as `--version` ONLY if no
// separate short/long distinction is made). We expose the git sha via the
// `long_version` so `tachi --version` (the common case) shows it; the bare
// semver is retained as the short form for scripts that grep version output.
//
// `concat!` + `env!` evaluate at compile time, so this is a `&'static str`
// as clap's attribute requires. The `GIT_SHA` / `BUILD_TIME` env values are
// injected by `build.rs`.
const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "+git-",
    env!("GIT_SHA"),
    " (built ",
    env!("BUILD_TIME"),
    ")"
);

#[derive(Parser, Debug)]
#[command(
    name = "tachi",
    version,
    long_version = LONG_VERSION,
    about = "Tachi — memory + Hub MCP server"
)]
pub struct Cli {
    /// Run as HTTP daemon instead of stdio transport
    #[arg(long)]
    pub daemon: bool,

    /// Port for HTTP daemon (default: 6919)
    #[arg(long, default_value_t = 6919)]
    pub port: u16,

    /// Override global memory DB path (equivalent to MEMORY_DB_PATH)
    #[arg(long, value_name = "PATH")]
    pub global_db: Option<PathBuf>,

    /// Override project memory DB path
    #[arg(long, value_name = "PATH")]
    pub project_db: Option<PathBuf>,

    /// Disable project DB entirely (force single-DB mode)
    #[arg(long)]
    pub no_project_db: bool,

    /// Built-in tool surface bundles or host alias, e.g. remember, observe+coordinate, openclaw, admin
    #[arg(long)]
    pub profile: Option<String>,

    /// Enable/disable background database GC (overrides MEMORY_GC_ENABLED)
    #[arg(long)]
    pub gc_enabled: Option<bool>,

    /// Delay before first background GC run in seconds (overrides MEMORY_GC_INITIAL_DELAY_SECS)
    #[arg(long)]
    pub gc_initial_delay_secs: Option<u64>,

    /// Interval between background GC runs in seconds (overrides MEMORY_GC_INTERVAL_SECS)
    #[arg(long)]
    pub gc_interval_secs: Option<u64>,

    /// CLI command (defaults to `serve` when omitted)
    #[command(subcommand)]
    pub command: Option<Commands>,
}

mod commands;
mod maintenance_actions;
mod vault_actions;

pub use commands::Commands;
pub use maintenance_actions::{
    CardAction, CleanAction, DaemonAction, DistillAction, FoundryAction, HarnessAction, HubAction,
    ManifestAction, McpAction, PokeAction, QuarantineAction, RepairAction, RescueAction,
    SkillSurfaceAction, WatcherAction, WikiAction, DEFAULT_WORKTREE_SWEEP_MAX_AGE_DAYS,
};
pub use vault_actions::{EnvAction, VaultAction, VaultIntakeAction};

#[cfg(test)]
mod tests;
