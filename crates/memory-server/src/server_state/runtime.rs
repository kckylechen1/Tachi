use super::read_pool::ReadStorePool;
use crate::enrichment::EnrichmentItem;
use crate::foundry_runtime_ops::{FoundryMaintenanceItem, FoundryWorkerStats};
use crate::profiles::ToolProfile;
use memory_core::MemoryStore;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DbScope {
    Global,
    Project,
}

impl DbScope {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            DbScope::Global => "global",
            DbScope::Project => "project",
        }
    }
}

/// Default store routing for event reads/writes: an explicit project name
/// routes to that named project DB, otherwise the bound project DB when one
/// exists, otherwise the global DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EventDbRoute {
    NamedProject(String),
    Project,
    Global,
}

pub(crate) struct CachedVaultKey {
    bytes: [u8; 32],
}

impl CachedVaultKey {
    pub(crate) fn copy_from(source: &[u8; 32]) -> Self {
        Self { bytes: *source }
    }

    pub(crate) fn bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}

impl Drop for CachedVaultKey {
    fn drop(&mut self) {
        crate::vault_crypto::zero_key(&mut self.bytes);
    }
}

pub(crate) struct VaultState {
    pub(crate) key: Option<CachedVaultKey>,
    pub(crate) unlock_time: Option<Instant>,
    pub(crate) failed_attempts: (u32, Option<Instant>),
    pub(crate) auto_lock_after_secs: u64,
}

pub(crate) struct RateLimiter {
    pub(crate) windows: HashMap<String, VecDeque<Instant>>,
    pub(crate) bursts: HashMap<String, VecDeque<Instant>>,
    pub(crate) rpm: u64,
    pub(crate) burst: u64,
}

#[derive(Debug)]
pub(crate) struct AgentRuntime {
    pub(crate) agent_profile: Option<AgentProfile>,
    pub(crate) tool_profile: Option<ToolProfile>,
    pub(crate) handoff_memos: Vec<HandoffMemo>,
}

/// Default requests-per-minute limit per session (0 = unlimited)
pub(super) const DEFAULT_RATE_LIMIT_RPM: u64 = 0;
/// Default max identical (tool+args) calls within the burst window (0 = unlimited)
pub(super) const DEFAULT_RATE_LIMIT_BURST: u64 = 8;
/// Burst detection window
pub(crate) const RATE_LIMIT_BURST_WINDOW: Duration = Duration::from_secs(60);
/// Maximum tracked sessions in rate limiter before stale eviction
pub(crate) const RATE_LIMIT_MAX_SESSIONS: usize = 1024;
/// Maximum tracked burst keys in rate limiter before stale eviction
pub(crate) const RATE_LIMIT_MAX_BURST_KEYS: usize = 4096;
/// Soft warning threshold before the hard duplicate-call block kicks in.
pub(crate) const STUCK_SOFT_WARN_THRESHOLD: u64 = 3;

/// Bounded channel capacity for enrichment batcher
pub(super) const ENRICH_CHANNEL_CAPACITY: usize = 512;
/// Bounded channel capacity for foundry maintenance worker
pub(super) const FOUNDRY_CHANNEL_CAPACITY: usize = 256;

#[derive(Clone)]
pub(crate) struct ProjectDbState {
    pub(crate) store: Arc<StdMutex<MemoryStore>>,
    pub(crate) read_pool: ReadStorePool,
    pub(crate) rw_gate: Arc<StdRwLock<()>>,
    pub(crate) db_path: Arc<PathBuf>,
}

/// Agent profile registered via `agent_register`. Stored per-session (in-memory).
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct AgentProfile {
    pub(crate) agent_id: String,
    pub(crate) display_name: String,
    pub(crate) capabilities: Vec<String>,
    pub(crate) tool_filter: Option<Vec<String>>,
    pub(crate) rate_limit_rpm: Option<u64>,
    pub(crate) rate_limit_burst: Option<u64>,
    pub(crate) registered_at: String,
}

/// Cross-agent handoff memo — left by one agent for the next.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct HandoffMemo {
    pub(crate) id: String,
    pub(crate) from_agent: String,
    pub(crate) target_agent: Option<String>,
    pub(crate) summary: String,
    pub(crate) next_steps: Vec<String>,
    pub(crate) context: Option<serde_json::Value>,
    pub(crate) created_at: String,
    pub(crate) acknowledged: bool,
}

#[derive(Clone)]
pub(crate) struct EnrichmentRuntime {
    pub(crate) enrich_tx: mpsc::Sender<EnrichmentItem>,
}

#[derive(Clone)]
pub(crate) struct FoundryRuntime {
    pub(crate) foundry_tx: mpsc::Sender<FoundryMaintenanceItem>,
    pub(crate) foundry_stats: Arc<FoundryWorkerStats>,
}
