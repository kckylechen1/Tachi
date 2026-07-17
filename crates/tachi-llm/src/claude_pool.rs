//! Bounded Claude Code CLI pool used by Foundry batch distill, dispatch V2
//! plan stage, Hub skill evolve, and Hub security scan.
//!
//! Each call:
//!   * acquires a semaphore permit (cap concurrent CLI invocations);
//!   * writes `prompt.md` / `result.md` / `status.json` under
//!     `<tachi_home>/foundry-runs/<label>-<UTCts>/`;
//!   * spawns `claude -p --output-format json` by default; adds
//!     `--dangerously-skip-permissions` only when
//!     `TACHI_CLAUDE_SKIP_PERMISSIONS=true` (or `1`) is set. Daemon-driven
//!     callers (Foundry distill, Hub evolve) should export that variable
//!     explicitly. Leave it unset for interactive use so Claude's permission
//!     prompts are preserved;
//!   * enforces a wall-clock timeout (default 180s, env
//!     `CLAUDE_POOL_TIMEOUT_SECS`) and kills timed-out children on drop.
//!
//! The pool also exposes `cleanup_expired` which removes run directories
//! older than the policy (7d success / 30d failed) so the foundry runs
//! folder doesn't grow without bound.
//!
//! Callers use `pool_call_with_fallback` for a Claude CLI → raw API
//! degradation path. Each caller is expected to handle its own LLM-fallback
//! policy when `call()` returns Err.
//!
//! ## Provider-path rollout (#1087)
//!
//! `claude_pool` is being retired in favor of provider-based `tachi-llm`
//! calls (no CLI subprocess, no bounded-CLI-binary dependency). The
//! rollout is staged: [`call_via_provider`](ClaudePool::call_via_provider)
//! preserves the exact run-directory artifact contract
//! (`prompt.md`/`result.md`/`status.json` under
//! `<tachi_home>/foundry-runs/<label>-<ts>/`) while swapping the executor
//! underneath — the caller supplies a provider closure instead of spawning
//! `claude`. [`provider_rollout_enabled`] gates whether callers route
//! through the provider path first (CLI pool as fallback) or keep today's
//! CLI-first behavior; it defaults to `false` so this lands inert. Once the
//! provider path has run a full cycle in production, a follow-up change
//! flips the default and later deletes the CLI pool + its env surface
//! (`CLAUDE_POOL_MAX_CONCURRENT` / `CLAUDE_BIN` /
//! `TACHI_CLAUDE_SKIP_PERMISSIONS`) — out of scope here.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::runtime_files::{write_owner_only_file, write_owner_only_file_atomic};
use chrono::Utc;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::time::timeout;

mod binary;
mod cleanup;
mod envelope;
mod fallback;
mod files;
mod lifecycle;
mod rollout;

#[cfg(test)]
mod tests;

pub use self::fallback::{pool_call_with_fallback, PoolCallSource};
pub use self::rollout::provider_rollout_enabled;

/// Default bounded concurrency for Claude CLI invocations.
pub const DEFAULT_MAX_CONCURRENT: usize = 2;
/// Default per-call wall-clock budget.
pub const DEFAULT_TIMEOUT_SECS: u64 = 180;
/// Cleanup retention for successful runs.
pub const SUCCESS_RETENTION_DAYS: u64 = 7;
/// Cleanup retention for failed runs (kept longer for post-mortem).
pub const FAILED_RETENTION_DAYS: u64 = 30;

pub struct ClaudePool {
    sem: Arc<Semaphore>,
    runs_dir: PathBuf,
    timeout: Duration,
    binary: Result<String, String>,
}

#[derive(Debug)]
pub struct ClaudeCallOutcome {
    pub text: String,
}
