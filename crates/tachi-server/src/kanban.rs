use crate::server_state::{DbScope, MemoryServer};
use chrono::Utc;
use memcore::{MemoryEntry, MemoryStore};
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;
use serde_json::json;

mod gc;
#[cfg_attr(not(test), allow(dead_code, unused_imports))]
mod handlers;
#[cfg_attr(not(test), allow(dead_code))]
mod inbox;
#[cfg_attr(not(test), allow(dead_code))]
mod metadata;
#[cfg_attr(not(test), allow(dead_code))]
mod normalize;
#[cfg_attr(not(test), allow(dead_code, unused_imports))]
mod types;

#[cfg(test)]
mod tests;

pub(crate) use self::gc::gc_expired_kanban_cards;
#[cfg(test)]
pub(crate) use self::handlers::{handle_check_inbox, handle_post_card, handle_update_card};
#[cfg(test)]
pub(crate) use self::types::{CheckInboxParams, PostCardParams, UpdateCardParams};

#[cfg_attr(not(test), allow(dead_code))]
pub(super) const KANBAN_CATEGORY: &str = "kanban";
pub(super) const KANBAN_PATH_PREFIX: &str = "/kanban/";
pub(crate) const DEFAULT_KANBAN_GC_MAX_AGE_DAYS: u64 = 30;

#[cfg(test)]
pub(super) const KANBAN_DISPATCH_PATH_PREFIX: &str = "/kanban/tasks/";
