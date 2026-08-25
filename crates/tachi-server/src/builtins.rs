use crate::server_state::MemoryServer;
use chrono::Utc;
use memcore::HubCapability;
use serde_json::{json, Value};
use tachi_hub::should_expose_skill_tool;

mod coding;
mod helpers;
mod mcp;
mod seed;
mod superpowers;
mod trading;
mod waza;

#[cfg(test)]
mod tests;

pub(crate) use seed::seed_builtin_capabilities;

pub(crate) const RETIRED_TRAJECTORY_DISTILLER_ID: &str = "skill:trajectory-distiller";

pub(crate) fn is_retired_builtin_capability_id(id: &str) -> bool {
    id == RETIRED_TRAJECTORY_DISTILLER_ID
}
