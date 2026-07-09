use memcore::MemoryStore;
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

/// Default store routing for event reads/writes: an explicit project name
/// routes to that named project DB, otherwise the bound project DB when one
/// exists, otherwise the global DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventDbRoute {
    NamedProject(String),
    Project,
    Global,
}

pub fn event_db_route(project: Option<&str>, has_project_db: bool) -> EventDbRoute {
    if let Some(project) = project.map(str::trim).filter(|value| !value.is_empty()) {
        EventDbRoute::NamedProject(project.to_string())
    } else if has_project_db {
        EventDbRoute::Project
    } else {
        EventDbRoute::Global
    }
}

pub fn trim_opt(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn query_limit(limit: usize) -> usize {
    limit.clamp(1, 500)
}

#[derive(Clone)]
pub struct ReadStorePool {
    stores: Arc<Vec<StdMutex<MemoryStore>>>,
    next: Arc<AtomicUsize>,
}

impl ReadStorePool {
    pub fn open_read_only(db_path: &str, size: usize) -> Result<Self, memcore::MemoryError> {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitRejection {
    pub message: String,
}

impl RateLimiter {
    pub fn check_tool_call(
        &mut self,
        tool_name: &str,
        args_hash: &str,
        session_id: &str,
        rpm_override: Option<u64>,
        burst_override: Option<u64>,
    ) -> Result<Option<String>, RateLimitRejection> {
        let now = Instant::now();
        let effective_rpm = rpm_override.unwrap_or(self.rpm);
        let effective_burst = burst_override.unwrap_or(self.burst);

        if effective_rpm > 0 {
            Self::reserve_entry_capacity(
                &mut self.windows,
                RATE_LIMIT_MAX_SESSIONS,
                now - Duration::from_secs(120),
                session_id,
            );

            let window = self.windows.entry(session_id.to_string()).or_default();
            let cutoff = now - Duration::from_secs(60);
            while let Some(&front) = window.front() {
                if front < cutoff {
                    window.pop_front();
                } else {
                    break;
                }
            }

            if window.len() as u64 >= effective_rpm {
                let oldest = window.front().copied().unwrap_or(now);
                let retry_after = Duration::from_secs(60)
                    .checked_sub(now.duration_since(oldest))
                    .unwrap_or(Duration::from_secs(1));
                return Err(RateLimitRejection {
                    message: format!(
                        "Rate limited: {} calls/min exceeded (limit={}). Retry in {:.0}s.",
                        window.len(),
                        effective_rpm,
                        retry_after.as_secs_f64()
                    ),
                });
            }

            window.push_back(now);
        }

        let mut soft_warning: Option<String> = None;
        if effective_burst > 0 {
            let burst_key = format!("{session_id}:{tool_name}:{args_hash}");
            Self::reserve_entry_capacity(
                &mut self.bursts,
                RATE_LIMIT_MAX_BURST_KEYS,
                now - RATE_LIMIT_BURST_WINDOW,
                &burst_key,
            );

            let stamps = self.bursts.entry(burst_key).or_default();
            let cutoff = now - RATE_LIMIT_BURST_WINDOW;
            while let Some(&front) = stamps.front() {
                if front < cutoff {
                    stamps.pop_front();
                } else {
                    break;
                }
            }

            if stamps.len() as u64 >= effective_burst {
                return Err(RateLimitRejection {
                    message: format!(
                        "Loop detected: tool '{}' called {} times with identical arguments within {}s (burst_limit={}). \
                         Stop before retrying the same path. Call tachi_unstick with the current task, attempts, and latest error to get a debug checklist and ask_codex_prompt; search prior lessons with tachi_wiki_search or tachi_task_brief; if still blocked, ask another agent using that prompt.",
                        tool_name,
                        stamps.len() + 1,
                        RATE_LIMIT_BURST_WINDOW.as_secs(),
                        effective_burst
                    ),
                });
            }

            let upcoming_count = stamps.len() as u64 + 1;
            if upcoming_count >= STUCK_SOFT_WARN_THRESHOLD && upcoming_count < effective_burst {
                soft_warning = Some(format!(
                    "⚠️ stuck-detection: tool '{}' has been called {} times with identical arguments within {}s. \
                     Hard block triggers at {} repeats. Consider calling tachi_unstick with the current task / attempts / latest error, \
                     or searching prior solutions via tachi_wiki_search / tachi_task_brief before retrying the same path.",
                    tool_name,
                    upcoming_count,
                    RATE_LIMIT_BURST_WINDOW.as_secs(),
                    effective_burst
                ));
            }

            stamps.push_back(now);
        }

        Ok(soft_warning)
    }

    pub fn entry_counts(&self) -> (usize, usize) {
        (self.windows.len(), self.bursts.len())
    }

    fn reserve_entry_capacity(
        map: &mut HashMap<String, VecDeque<Instant>>,
        max_entries: usize,
        stale_cutoff: Instant,
        new_key: &str,
    ) {
        if max_entries == 0 || map.contains_key(new_key) {
            return;
        }

        if map.len() >= max_entries {
            map.retain(|_, deque| deque.back().is_some_and(|&t| t >= stale_cutoff));
        }

        while map.len() >= max_entries {
            let Some(oldest_key) = map
                .iter()
                .min_by_key(|(_, deque)| deque.back().copied().unwrap_or(stale_cutoff))
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            map.remove(&oldest_key);
        }
    }
}

#[derive(Clone)]
pub struct DbRuntime {
    pub global_store: Arc<StdMutex<MemoryStore>>,
    pub global_read_pool: ReadStorePool,
    pub global_rw_gate: Arc<StdRwLock<()>>,
    pub global_db_path: Arc<PathBuf>,
    pub global_vec_available: bool,
    pub project_db: Arc<StdRwLock<Option<ProjectDbState>>>,
    pub attached_project_dbs: Arc<StdRwLock<HashMap<PathBuf, ProjectDbState>>>,
    pub project_attach_init_gate: Arc<StdMutex<()>>,
}

impl DbRuntime {
    pub fn has_project_db(&self) -> bool {
        self.project_db
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    pub fn activate_project_db(&self, db_path: PathBuf) -> Result<bool, String> {
        let state = ProjectDbState::open(db_path, configured_memory_read_pool_size())
            .map_err(|e| format!("open project db: {e}"))?;

        let mut guard = self.project_db.write().unwrap_or_else(|e| e.into_inner());
        let was_none = guard.is_none();
        *guard = Some(state);
        Ok(was_none)
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
        let state = self.attached_project_state(db_path)?;
        let _gate = write_or_recover(&state.rw_gate, "path_db_rw_gate");
        let mut store = lock_or_recover(&state.store, label);
        f(&mut store)
    }

    pub fn with_path_store_read_with_label<T>(
        &self,
        db_path: &Path,
        label: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let key = project_db_read_cache_key(db_path)?;
        if let Some(state) = self
            .attached_project_dbs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
        {
            let _gate = read_or_recover(&state.rw_gate, "path_db_rw_gate");
            return state.read_pool.with_store(label, f);
        }

        let _gate = read_or_recover(&self.global_rw_gate, "path_db_read_gate");
        let mut store = open_read_store(&key, label)?;
        f(&mut store)
    }

    fn attached_project_state(&self, db_path: &Path) -> Result<ProjectDbState, String> {
        let key = project_db_cache_key(db_path)?;
        if let Some(state) = self
            .attached_project_dbs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
        {
            return Ok(state);
        }

        let _init_gate =
            lock_or_recover(&self.project_attach_init_gate, "project_attach_init_gate");
        if let Some(state) = self
            .attached_project_dbs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
        {
            return Ok(state);
        }

        let state = ProjectDbState::open(key.clone(), configured_memory_read_pool_size())?;
        let mut guard = self
            .attached_project_dbs
            .write()
            .unwrap_or_else(|e| e.into_inner());
        Ok(guard.entry(key).or_insert(state).clone())
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
        let guard = self.project_db.read().unwrap_or_else(|e| e.into_inner());
        let state = guard
            .as_ref()
            .ok_or_else(|| "No project database available".to_string())?;
        let _gate = write_or_recover(&state.rw_gate, "project_rw_gate");
        let mut store = lock_or_recover(&state.store, "project_store");
        f(&mut store)
    }

    pub fn with_project_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let guard = self.project_db.read().unwrap_or_else(|e| e.into_inner());
        let state = guard
            .as_ref()
            .ok_or_else(|| "No project database available".to_string())?;
        let _gate = read_or_recover(&state.rw_gate, "project_rw_gate");
        state.read_pool.with_store("project_read_pool", f)
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
        self.project_db
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|state| state.db_path.as_ref().clone())
    }

    pub fn global_read_pool_size(&self) -> usize {
        self.global_read_pool.len()
    }

    pub fn project_vec_available(&self) -> bool {
        self.project_db
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|state| state.vec_available)
    }
}

fn project_db_cache_key(db_path: &Path) -> Result<PathBuf, String> {
    if db_path.exists() {
        return std::fs::canonicalize(db_path)
            .map_err(|e| format!("canonicalize project db {}: {e}", db_path.display()));
    }
    let parent = db_path
        .parent()
        .ok_or_else(|| format!("project db path has no parent: {}", db_path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("create project db parent {}: {e}", parent.display()))?;
    let parent = std::fs::canonicalize(parent)
        .map_err(|e| format!("canonicalize project db parent {}: {e}", parent.display()))?;
    let file_name = db_path
        .file_name()
        .ok_or_else(|| format!("project db path has no file name: {}", db_path.display()))?;
    Ok(parent.join(file_name))
}

fn project_db_read_cache_key(db_path: &Path) -> Result<PathBuf, String> {
    if !db_path.exists() {
        return Err(format!("project db does not exist: {}", db_path.display()));
    }
    std::fs::canonicalize(db_path)
        .map_err(|e| format!("canonicalize project db {}: {e}", db_path.display()))
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
    pub vec_available: bool,
}

impl ProjectDbState {
    pub fn open(db_path: PathBuf, read_pool_size: usize) -> Result<Self, String> {
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
        let vec_available = store.vec_available;
        let read_pool = ReadStorePool::open_read_only(db_str, read_pool_size)
            .map_err(|e| format!("open project read db: {e}"))?;
        Ok(Self {
            store: Arc::new(StdMutex::new(store)),
            read_pool,
            rw_gate: Arc::new(StdRwLock::new(())),
            db_path: Arc::new(db_path),
            vec_available,
        })
    }
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
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "tachi-runtime-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        path
    }

    fn test_runtime(global_db: PathBuf) -> DbRuntime {
        let global_db_str = global_db.to_str().expect("global db utf8");
        DbRuntime {
            global_store: Arc::new(StdMutex::new(
                MemoryStore::open_with_label(global_db_str, "global").expect("global store"),
            )),
            global_read_pool: ReadStorePool::open_read_only(global_db_str, 1)
                .expect("global read pool"),
            global_rw_gate: Arc::new(StdRwLock::new(())),
            global_db_path: Arc::new(global_db),
            global_vec_available: false,
            project_db: Arc::new(StdRwLock::new(None)),
            attached_project_dbs: Arc::new(StdRwLock::new(HashMap::new())),
            project_attach_init_gate: Arc::new(StdMutex::new(())),
        }
    }

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

    #[test]
    fn path_store_attaches_project_db_once_and_reuses_it() {
        let temp = unique_temp_dir("path-store-cache");
        let global_db = temp.join("global/memory.db");
        let project_db = temp.join("project/.tachi/memory.db");
        std::fs::create_dir_all(global_db.parent().expect("global parent")).expect("global dir");
        let runtime = test_runtime(global_db);

        runtime
            .with_path_store(&project_db, |store| {
                let _ = store.vec_available;
                Ok(())
            })
            .expect("first attach");
        assert_eq!(
            runtime
                .attached_project_dbs
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .len(),
            1
        );

        runtime
            .with_path_store_read(&project_db, |store| {
                let _ = store.vec_available;
                Ok(())
            })
            .expect("second attach reuses cached state");
        assert_eq!(
            runtime
                .attached_project_dbs
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .len(),
            1
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn first_path_store_read_does_not_attach_writer_state() {
        let temp = unique_temp_dir("path-store-read-only");
        let global_db = temp.join("global/memory.db");
        let project_db = temp.join("project/.tachi/memory.db");
        std::fs::create_dir_all(global_db.parent().expect("global parent")).expect("global dir");
        std::fs::create_dir_all(project_db.parent().expect("project parent")).expect("project dir");
        let project_db_str = project_db.to_str().expect("project db utf8");
        drop(MemoryStore::open_with_label(project_db_str, "seed").expect("seed project db"));
        let runtime = test_runtime(global_db);

        runtime
            .with_path_store_read(&project_db, |store| {
                let _ = store.vec_available;
                Ok(())
            })
            .expect("first read-only attach");
        assert_eq!(
            runtime
                .attached_project_dbs
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .len(),
            0,
            "read-only first use must not create cached writer state"
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn missing_path_store_read_does_not_create_parent_dir() {
        let temp = unique_temp_dir("path-store-missing-read");
        let global_db = temp.join("global/memory.db");
        let project_db = temp.join("missing/.tachi/memory.db");
        std::fs::create_dir_all(global_db.parent().expect("global parent")).expect("global dir");
        let runtime = test_runtime(global_db);

        let err = runtime
            .with_path_store_read(&project_db, |_| Ok(()))
            .expect_err("missing read should fail");
        assert!(
            err.contains("project db does not exist"),
            "unexpected error: {err}"
        );
        assert!(
            !project_db.parent().expect("project parent").exists(),
            "read-only missing DB lookup must not create parent dirs"
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn concurrent_first_attach_uses_one_cached_project_state() {
        let temp = unique_temp_dir("path-store-concurrent-cache");
        let global_db = temp.join("global/memory.db");
        let project_db = temp.join("project/.tachi/memory.db");
        std::fs::create_dir_all(global_db.parent().expect("global parent")).expect("global dir");
        let runtime = Arc::new(test_runtime(global_db));

        let left_runtime = runtime.clone();
        let left_db = project_db.clone();
        let left = std::thread::spawn(move || {
            left_runtime
                .with_path_store(&left_db, |_| Ok(()))
                .expect("left attach");
        });
        let right_runtime = runtime.clone();
        let right_db = project_db.clone();
        let right = std::thread::spawn(move || {
            right_runtime
                .with_path_store(&right_db, |_| Ok(()))
                .expect("right attach");
        });

        left.join().expect("left thread");
        right.join().expect("right thread");
        assert_eq!(
            runtime
                .attached_project_dbs
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .len(),
            1
        );

        let _ = std::fs::remove_dir_all(temp);
    }
}
