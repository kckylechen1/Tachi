// provider_health.rs — provider secret pools, key health, and lane configuration

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use memcore::vault::health::{EvidenceKind, TypedOutcome};
use memcore::vault::VaultKeyHealth;

mod config;
mod persistence;
mod pools;
mod selection;
mod state;
mod types;

pub use self::config::{LaneFallbackConfig, ProviderRuntimeConfig};
pub(super) use self::state::{
    ProviderHealthPersistState, ProviderHealthReloadState, ProviderHealthSnapshot, ProviderState,
};
pub use self::types::ProviderSecret;
pub(super) use self::types::{
    ChatLane, ClaudeCliFailure, ClaudeCliFailureKind, ClaudeCliSkip, KeyAvailability,
    KeyRetryStatus, SelectedProviderSecret,
};
pub use self::types::{
    ChatLaneConfig, CompletionStatusV1, Generated, ModelEngineKindV1, ModelInvocationLaneV1,
    PersistedModelInvocationReceiptV1, ProviderAuthProbeClass, ProviderAuthProbeFamily,
    ProviderAuthProbeResult, ProviderInvocationFailure, ProviderInvocationFailureClass,
    ProviderInvocationOutcome, ProviderInvocationReceipt, LLM_OUTPUT_TRUNCATED,
    MODEL_INVOCATION_SCHEMA_V1,
};
pub use self::types::{
    LaneOutageStatus, ProviderHealthStatus, ProviderKeyCooldownStatus, ProviderPoolStatus,
};

// The persisted `vault_key_health.status` vocabulary belongs to the single
// writer (#1680 D6); these are that crate's constants under this module's
// historical names, so a status string cannot drift between the writer and
// the availability mapping that reads it back.
pub(super) const HEALTH_OK: &str = memcore::vault::health::HEALTH_STATUS_OK;
pub(super) const HEALTH_RATE_LIMITED: &str = memcore::vault::health::HEALTH_STATUS_RATE_LIMITED;
pub(super) const HEALTH_AUTH_FAILED: &str = memcore::vault::health::HEALTH_STATUS_AUTH_FAILED;
pub(super) const HEALTH_EXHAUSTED: &str = memcore::vault::health::HEALTH_STATUS_EXHAUSTED;
// Read-side only: no writer emits these two, but rows written elsewhere
// (operator disable, legacy "cooldown") still map onto availability.
pub(super) const HEALTH_COOLDOWN: &str = "cooldown";
pub(super) const HEALTH_DISABLED: &str = "disabled";
pub(super) const AUTH_FAILED_RETRY_TTL_SECS: i64 = 24 * 60 * 60;
pub(super) const CLAUDE_CLI_FAILURE_COOLDOWN: Duration = Duration::from_secs(600);
