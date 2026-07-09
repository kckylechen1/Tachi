//! `tachi status` / `tachi daemon` / `tachi foundry config` handlers.
//!
//! These are lightweight diagnostic commands that read state without
//! holding any DB write locks. They open each manifest DB read-only, query
//! the per-DB foundry histogram, and render a single combined view of:
//!
//! - daemon singleton state (PID file + flock peer)
//! - manifest size + freshness
//! - per-DB foundry job counts + GC-eligible terminal jobs
//! - orphan warnings: manifest entries the running daemon's scheduler
//!   cannot route to (agents/, hub/, vault/, dark DBs)
//! - stuck running warnings: jobs older than [`STUCK_THRESHOLD_SECS`]
//!   that the safety-net poll should have re-injected by now
//!
//! All output uses ASCII severity icons (`[OK]`, `[!]`, `[X]`) — no
//! emojis, since the terminal rendering target is heterogeneous.

pub(crate) mod daemon;
pub(crate) mod db_probe;
pub(crate) mod ledger;
pub(crate) mod recall_eval;
pub(crate) mod status_cli;
pub(crate) mod status_health;
pub(crate) mod warnings;

pub(crate) use daemon::*;
pub(crate) use db_probe::*;
pub(crate) use ledger::*;
pub(crate) use warnings::*;

use std::path::{Path, PathBuf};
use std::time::Duration;

use memory_core::MemoryEntry;
use serde_json::json;

use memory_core::MemoryStore;

use crate::daemon_lock::{process_alive, read_pid_file};
use crate::manifest::Manifest;

pub(crate) const STUCK_THRESHOLD_SECS: i64 = 600;
const DISPATCH_STALE_THRESHOLD_SECS: i64 = 6 * 60 * 60;

pub(crate) const EXPECTED_EMBEDDING_DIM: usize = 1024;
const FOUNDRY_RECALL_CACHE_SOURCE: &str = "foundry_recall_rerank_cache";

const DISTILL_STALE_THRESHOLD_SECS: i64 = 36 * 3600;

pub(crate) const WATCH_INTERVAL: Duration = Duration::from_secs(2);
mod runtime;
mod snapshot;
mod types;

#[cfg(test)]
mod tests;

pub(crate) use runtime::{
    handle_tachi_status_agent, handle_tachi_status_full, resolve_app_home,
    runtime_observability_json, truncate,
};
use snapshot::paths_equal;
pub(crate) use snapshot::{
    collect_snapshot, collect_snapshot_with_provider_value_compare, list_recent_checkpoint_entries,
    list_recent_checkpoint_entries_for_project, list_recent_kanban_entries,
};
pub(crate) use types::*;
