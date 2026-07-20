//! `tachi_shell` — skill-gated flow orchestration facade.
//!
//! Implements the MVP described in `wiki/agent/tachi/Tachi-Shell-重构计划-2026-05-04.md`.
//!
//! Each `tachi_shell(action=...)` call is a thin coordinator that:
//!   1. Resolves / creates a `flow_id` and its run directory.
//!   2. Injects the meta skill SOP file required for the stage.
//!   3. Writes / updates `instruction.md`, `status.json`, `events.jsonl`.
//!   4. For `status` it delegates to the existing read-only handler.
//!   5. For `dispatch` it can optionally hand off to the existing
//!      `dispatch_ops::handle_tachi_dispatch` (Phase 4 hook).
//!
//! This module deliberately does **not** re-implement clanker dispatch,
//! or skill discovery — it composes existing infra.

use crate::{MemoryServer, TachiDispatchParams, TachiShellDispatchSliceParams, TachiShellParams};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

mod actions;
mod flow;
mod injection;
mod instruction;
mod shell_github;

#[cfg(test)]
mod tests;

#[cfg(test)]
use self::flow::new_flow_id;
use self::flow::{
    advance_stage, cached_git_root, injection_to_json, read_status, read_status_async,
    resolve_or_create_flow, slugify, validate_slice_id,
};
#[cfg(test)]
use self::injection::meta_skill_for_stage;
use self::injection::{inject_meta_skill, InjectionResult, STAGE_ACTIONS};
use self::instruction::build_instruction_md;

// Re-exported for `builtins::helpers` (capability-seed time) and
// `bootstrap::skill_surface_cli` (manifest-read time) — see the `pub(crate)`
// doc comment on `injection::resolve_meta_skill` for why.
pub(crate) use self::injection::resolve_meta_skill;

pub(crate) use self::actions::handle_tachi_shell;
#[cfg(test)]
use self::actions::{handle_status_action, resolve_slice_id};
#[cfg(test)]
pub(crate) use self::flow::tachi_run_root_env_lock;
pub(crate) use self::flow::{
    run_dir_for_flow_id, scan_open_loops, shell_runs_root, validate_flow_id,
};
pub(crate) use shell_github::*;
