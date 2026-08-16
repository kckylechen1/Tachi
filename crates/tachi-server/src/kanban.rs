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

#[cfg(test)]
pub(super) const KANBAN_DISPATCH_PATH_PREFIX: &str = "/kanban/tasks/";
