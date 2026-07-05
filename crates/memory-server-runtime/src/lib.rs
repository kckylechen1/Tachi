use memory_core::MemoryStore;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};
use std::time::{Duration, Instant};

const DEFAULT_MEMORY_READ_POOL_SIZE: usize = 4;
const MAX_MEMORY_READ_POOL_SIZE: usize = 32;

/// Logical memory database scope used by server handlers and background jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbScope {
    Global,
    Project,
}

impl DbScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            DbScope::Global => "global",
            DbScope::Project => "project",
        }
    }
}

#[derive(Clone)]
pub struct ReadStorePool {
    stores: Arc<Vec<StdMutex<MemoryStore>>>,
    next: Arc<AtomicUsize>,
}

impl ReadStorePool {
    pub fn open_read_only(db_path: &str, size: usize) -> Result<Self, memory_core::MemoryError> {
        let size = size.clamp(1, MAX_MEMORY_READ_POOL_SIZE);
        let mut stores = Vec::with_capacity(size);
        for _ in 0..size {
            stores.push(StdMutex::new(MemoryStore::open_read_only(db_path)?));
        }
        Ok(Self {
            stores: Arc::new(stores),
            next: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn with_store<T>(
        &self,
        label: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let index = self.next.fetch_add(1, Ordering::Relaxed) % self.stores.len();
        let mut store = lock_or_recover(&self.stores[index], label);
        f(&mut store)
    }

    pub fn len(&self) -> usize {
        self.stores.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stores.is_empty()
    }
}

pub fn configured_memory_read_pool_size() -> usize {
    parse_env_u64("TACHI_MEMORY_READ_POOL_SIZE")
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(DEFAULT_MEMORY_READ_POOL_SIZE)
        .clamp(1, MAX_MEMORY_READ_POOL_SIZE)
}

pub struct CachedVaultKey {
    bytes: [u8; 32],
}

impl CachedVaultKey {
    pub fn copy_from(source: &[u8; 32]) -> Self {
        Self { bytes: *source }
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}

impl Drop for CachedVaultKey {
    fn drop(&mut self) {
        zero_key(&mut self.bytes);
    }
}

pub struct VaultState {
    pub key: Option<CachedVaultKey>,
    pub unlock_time: Option<Instant>,
    pub failed_attempts: (u32, Option<Instant>),
    pub auto_lock_after_secs: u64,
}

pub struct RateLimiter {
    pub windows: HashMap<String, VecDeque<Instant>>,
    pub bursts: HashMap<String, VecDeque<Instant>>,
    pub rpm: u64,
    pub burst: u64,
}

/// Default requests-per-minute limit per session (0 = unlimited)
pub const DEFAULT_RATE_LIMIT_RPM: u64 = 0;
/// Default max identical (tool+args) calls within the burst window (0 = unlimited)
pub const DEFAULT_RATE_LIMIT_BURST: u64 = 8;
/// Burst detection window
pub const RATE_LIMIT_BURST_WINDOW: Duration = Duration::from_secs(60);
/// Maximum tracked sessions in rate limiter before stale eviction
pub const RATE_LIMIT_MAX_SESSIONS: usize = 1024;
/// Maximum tracked burst keys in rate limiter before stale eviction
pub const RATE_LIMIT_MAX_BURST_KEYS: usize = 4096;
/// Soft warning threshold before the hard duplicate-call block kicks in.
pub const STUCK_SOFT_WARN_THRESHOLD: u64 = 3;

#[derive(Clone)]
pub struct ProjectDbState {
    pub store: Arc<StdMutex<MemoryStore>>,
    pub read_pool: ReadStorePool,
    pub rw_gate: Arc<StdRwLock<()>>,
    pub db_path: Arc<PathBuf>,
}

/// Agent profile registered via `agent_register`. Stored per-session (in-memory).
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentProfile {
    pub agent_id: String,
    pub display_name: String,
    pub capabilities: Vec<String>,
    pub tool_filter: Option<Vec<String>>,
    pub rate_limit_rpm: Option<u64>,
    pub rate_limit_burst: Option<u64>,
    pub registered_at: String,
}

/// Cross-agent handoff memo — left by one agent for the next.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HandoffMemo {
    pub id: String,
    pub from_agent: String,
    pub target_agent: Option<String>,
    pub summary: String,
    pub next_steps: Vec<String>,
    pub context: Option<serde_json::Value>,
    pub created_at: String,
    pub acknowledged: bool,
}

fn parse_env_u64(name: &str) -> Option<u64> {
    let raw = std::env::var(name).ok()?;
    match raw.trim().parse::<u64>() {
        Ok(value) => Some(value),
        Err(_) => {
            eprintln!("Ignoring invalid {name} value '{raw}' (expected non-negative integer)");
            None
        }
    }
}

fn lock_or_recover<'a, T>(mutex: &'a StdMutex<T>, label: &str) -> std::sync::MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            eprintln!("WARNING: mutex poisoned: {label}; recovering with inner state");
            poisoned.into_inner()
        }
    }
}

fn zero_key(key: &mut [u8; 32]) {
    for byte in key.iter_mut() {
        unsafe {
            std::ptr::write_volatile(byte, 0);
        }
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_scope_strings_match_persisted_labels() {
        assert_eq!(DbScope::Global.as_str(), "global");
        assert_eq!(DbScope::Project.as_str(), "project");
    }

    #[test]
    fn cached_vault_key_copies_source_buffer() {
        let mut source = [0u8; 32];
        source[0] = 7;
        source[31] = 9;

        let cached = CachedVaultKey::copy_from(&source);
        source.fill(0);

        assert_eq!(cached.bytes()[0], 7);
        assert_eq!(cached.bytes()[31], 9);
    }
}
