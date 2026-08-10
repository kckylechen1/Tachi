use clap::Parser;
use std::path::PathBuf;

// ─── CLI Arguments ────────────────────────────────────────────────────────────

// clap 4.x: when `long_version` is set, `--version` emits the long form
// (semver + "+git-" + full SHA + build time). There is no separate
// short-form output on `--version`. Scripts extracting the bare semver
// must split on the `+` boundary.
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

    /// Explicitly opt in to migrating an EXISTING older-schema DB forward in
    /// place (#1119). Default is refuse-and-report: opening a live DB stamped
    /// below this binary's schema version without this flag is a hard error
    /// naming both versions and how to opt in, instead of silently upgrading
    /// it (which would brick any other deployed daemon still on the old
    /// schema). Only the deploy ritual's launchd/brew invocation should pass
    /// this — `tachi_server::bootstrap::serve` turns it into a typed
    /// `memcore::MigrationAuthority::Allow` threaded down every DB-open call
    /// (NOT a process env var; the reverted first attempt used
    /// `TACHI_ALLOW_SCHEMA_MIGRATION`, which serve now defensively clears
    /// once at startup). A dev/test/agent-lane binary that never passes this
    /// flag carries `Deny` by construction and hits the refusal by default.
    #[arg(long)]
    pub allow_schema_migration: bool,

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

pub use commands::{Commands, RecallCoverageArgs};
pub use maintenance_actions::{
    BuildAction, CardAction, CardsAction, CleanAction, DaemonAction, DedupeAction, DistillAction,
    EvalAction, FoundryAction, HarnessAction, HostAction, HubAction, InjectionSurfaceAction,
    LifecycleConsistencyAction, ManifestAction, McpAction, PokeAction, QuarantineAction,
    RepairAction, RescueAction, SkillSurfaceAction, WatcherAction, WikiAction, WorktreeAction,
    WorktreeOpenArgs, DEFAULT_ORPHAN_REAP_MAX_AGE_DAYS, DEFAULT_WORKTREE_SWEEP_MAX_AGE_DAYS,
};
pub use vault_actions::{EnvAction, VaultAction, VaultIntakeAction, VaultReconcileAction};

#[cfg(test)]
mod tests;
