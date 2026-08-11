use crate::server_state::{DbScope, MemoryServer};
use chrono::Utc;
use memcore::{MemoryEntry, MemoryStore};
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;
use serde_json::json;

mod gc;
mod handlers;
mod inbox;
mod metadata;
mod normalize;
mod types;

#[cfg(test)]
mod tests;

pub(super) use self::gc::gc_expired_kanban_cards;
pub(crate) use self::handlers::{handle_check_inbox, handle_post_card, handle_update_card};
pub(super) use self::types::{CheckInboxParams, PostCardParams, UpdateCardParams};

pub(super) const KANBAN_CATEGORY: &str = "kanban";
pub(super) const KANBAN_PATH_PREFIX: &str = "/kanban/";
pub(super) const DEFAULT_KANBAN_GC_MAX_AGE_DAYS: u64 = 30;

/// Path prefix for dispatch ("board") cards. These are written as
/// `category=fact`, `retention_policy=Pinned` rows with the run lifecycle in
/// `metadata.a2a_state`, so they never match the `resolved`/`expired` kanban
/// reaper and would otherwise accumulate forever.
pub(super) const KANBAN_DISPATCH_PATH_PREFIX: &str = "/kanban/tasks/";

/// Non-terminal `a2a_state` values for dispatch cards. A card stuck in one of
/// these states for longer than the GC max age is from a long-dead run (the
/// run-ledger has already been GC'd), so it is safe to purge without any
/// status reconciliation.
pub(super) const KANBAN_DISPATCH_NON_TERMINAL_STATES: &[&str] = &[
    "TASK_STATE_WORKING",
    "TASK_STATE_PENDING",
    "TASK_STATE_INPUT_REQUIRED",
];
