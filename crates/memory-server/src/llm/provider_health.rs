// provider_health.rs — provider secret pools, key health, and lane configuration

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use memory_core::vault::VaultKeyHealth;

mod config;
mod persistence;
mod pools;
mod selection;
mod state;
mod types;

pub(super) use self::state::{
    ProviderHealthPersistState, ProviderHealthReloadState, ProviderHealthSnapshot, ProviderState,
};
pub(crate) use self::types::ProviderSecret;
pub(super) use self::types::{
    ChatLane, ChatLaneConfig, ClaudeCliFailure, ClaudeCliFailureKind, ClaudeCliSkip,
    KeyAvailability, KeyRetryStatus, SelectedProviderSecret,
};
pub(crate) use self::types::{ProviderHealthStatus, ProviderKeyCooldownStatus, ProviderPoolStatus};

pub(super) const HEALTH_OK: &str = "ok";
pub(super) const HEALTH_COOLDOWN: &str = "cooldown";
pub(super) const HEALTH_RATE_LIMITED: &str = "rate_limited";
pub(super) const HEALTH_AUTH_FAILED: &str = "auth_failed";
pub(super) const HEALTH_DISABLED: &str = "disabled";
pub(super) const HEALTH_EXHAUSTED: &str = "exhausted";
pub(super) const AUTH_FAILED_RETRY_TTL_SECS: i64 = 24 * 60 * 60;
pub(super) const CLAUDE_CLI_FAILURE_COOLDOWN: Duration = Duration::from_secs(600);
