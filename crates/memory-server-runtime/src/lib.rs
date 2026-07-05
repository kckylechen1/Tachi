use memory_core::MemoryStore;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
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

#[derive(Clone)]
pub struct DbRuntime {
    pub global_store: Arc<StdMutex<MemoryStore>>,
    pub global_read_pool: ReadStorePool,
    pub project_store: Option<Arc<StdMutex<MemoryStore>>>,
    pub project_read_pool: Option<ReadStorePool>,
    pub global_rw_gate: Arc<StdRwLock<()>>,
    pub project_rw_gate: Option<Arc<StdRwLock<()>>>,
    pub global_db_path: Arc<PathBuf>,
    pub project_db_path: Option<Arc<PathBuf>>,
    pub global_vec_available: bool,
    pub project_vec_available: bool,
    pub hot_project_db: Arc<StdRwLock<Option<ProjectDbState>>>,
}

impl DbRuntime {
    pub fn has_project_db(&self) -> bool {
        if self.project_db_path.is_some() {
            return true;
        }
        self.hot_project_db
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    pub fn activate_project_db(&self, db_path: PathBuf) -> Result<bool, String> {
        let db_str = db_path.to_str().ok_or_else(|| {
            format!(
                "Project DB path contains invalid UTF-8: {}",
                db_path.display()
            )
        })?;
        let project_label = db_path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|os| os.to_str())
            .unwrap_or("project")
            .to_string();
        let store = MemoryStore::open_with_label(db_str, &project_label)
            .map_err(|e| format!("open project db: {e}"))?;
        let read_pool = ReadStorePool::open_read_only(db_str, configured_memory_read_pool_size())
            .map_err(|e| format!("open project read db: {e}"))?;
        let state = ProjectDbState {
            store: Arc::new(StdMutex::new(store)),
            read_pool,
            rw_gate: Arc::new(StdRwLock::new(())),
            db_path: Arc::new(db_path),
        };

        let mut guard = self
            .hot_project_db
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let was_none = guard.is_none();
        *guard = Some(state);
        Ok(was_none)
    }

    pub fn with_hot_project_store<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let guard = self
            .hot_project_db
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let state = guard
            .as_ref()
            .ok_or_else(|| "No hot-swapped project database available".to_string())?;
        let _gate = write_or_recover(&state.rw_gate, "hot_project_rw_gate");
        let mut store = lock_or_recover(&state.store, "hot_project_store");
        f(&mut store)
    }

    pub fn with_hot_project_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let guard = self
            .hot_project_db
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let state = guard
            .as_ref()
            .ok_or_else(|| "No hot-swapped project database available".to_string())?;
        let _gate = read_or_recover(&state.rw_gate, "hot_project_rw_gate");
        state.read_pool.with_store("hot_project_read_pool", f)
    }

    pub fn with_path_store<T>(
        &self,
        db_path: &Path,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let label = db_path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|os| os.to_str())
            .unwrap_or("path");
        self.with_path_store_with_label(db_path, label, f)
    }

    pub fn with_path_store_read<T>(
        &self,
        db_path: &Path,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.with_path_store_read_with_label(db_path, "path", f)
    }

    pub fn with_path_store_with_label<T>(
        &self,
        db_path: &Path,
        label: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let db_str = db_path
            .to_str()
            .ok_or_else(|| format!("DB path contains invalid UTF-8: {}", db_path.display()))?;
        let _gate = write_or_recover(&self.global_rw_gate, "path_db_rw_gate");
        let mut store = MemoryStore::open_with_label(db_str, label)
            .map_err(|e| format!("open path store {}: {e}", db_path.display()))?;
        f(&mut store)
    }

    pub fn with_path_store_read_with_label<T>(
        &self,
        db_path: &Path,
        label: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let _gate = read_or_recover(&self.global_rw_gate, "path_db_rw_gate");
        let mut store = open_read_store(db_path, label)?;
        f(&mut store)
    }

    pub fn with_global_store<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let _gate = write_or_recover(&self.global_rw_gate, "global_rw_gate");
        let mut store = lock_or_recover(&self.global_store, "global_store");
        f(&mut store)
    }

    pub fn with_global_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let _gate = read_or_recover(&self.global_rw_gate, "global_rw_gate");
        self.global_read_pool.with_store("global_read_pool", f)
    }

    pub fn with_project_store<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        if self
            .hot_project_db
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return self.with_hot_project_store(f);
        }
        if let Some(ref store_arc) = self.project_store {
            let gate = self
                .project_rw_gate
                .as_ref()
                .ok_or_else(|| "No project lock available".to_string())?;
            let _gate = write_or_recover(gate, "project_rw_gate");
            let mut store = lock_or_recover(store_arc, "project_store");
            return f(&mut store);
        }
        Err("No project database available".to_string())
    }

    pub fn with_project_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        if self
            .hot_project_db
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return self.with_hot_project_store_read(f);
        }
        if let Some(ref read_pool) = self.project_read_pool {
            let gate = self
                .project_rw_gate
                .as_ref()
                .ok_or_else(|| "No project lock available".to_string())?;
            let _gate = read_or_recover(gate, "project_rw_gate");
            return read_pool.with_store("project_read_pool", f);
        }
        Err("No project database available".to_string())
    }

    pub fn with_store_for_scope<T>(
        &self,
        scope: DbScope,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        match scope {
            DbScope::Global => self.with_global_store(f),
            DbScope::Project => self.with_project_store(f),
        }
    }

    pub fn with_store_for_scope_read<T>(
        &self,
        scope: DbScope,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        match scope {
            DbScope::Global => self.with_global_store_read(f),
            DbScope::Project => self.with_project_store_read(f),
        }
    }

    pub fn global_db_path_buf(&self) -> PathBuf {
        (*self.global_db_path).clone()
    }

    pub fn project_db_path_buf(&self) -> Option<PathBuf> {
        if let Some(state) = self
            .hot_project_db
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            return Some(state.db_path.as_ref().clone());
        }
        self.project_db_path.as_ref().map(|p| (**p).clone())
    }

    pub fn global_read_pool_size(&self) -> usize {
        self.global_read_pool.len()
    }
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

fn read_or_recover<'a, T>(
    rwlock: &'a StdRwLock<T>,
    label: &str,
) -> std::sync::RwLockReadGuard<'a, T> {
    match rwlock.read() {
        Ok(guard) => guard,
        Err(poisoned) => {
            eprintln!("WARNING: rwlock poisoned on read: {label}; recovering with inner state");
            poisoned.into_inner()
        }
    }
}

fn write_or_recover<'a, T>(
    rwlock: &'a StdRwLock<T>,
    label: &str,
) -> std::sync::RwLockWriteGuard<'a, T> {
    match rwlock.write() {
        Ok(guard) => guard,
        Err(poisoned) => {
            eprintln!("WARNING: rwlock poisoned on write: {label}; recovering with inner state");
            poisoned.into_inner()
        }
    }
}

fn open_read_store(db_path: &Path, label: &str) -> Result<MemoryStore, String> {
    let db_str = db_path.to_str().ok_or_else(|| {
        format!(
            "{} DB path contains invalid UTF-8: {}",
            label,
            db_path.display()
        )
    })?;
    MemoryStore::open_read_only(db_str).map_err(|e| format!("open {label} read store: {e}"))
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
