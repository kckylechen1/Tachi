use memcore::MemoryStore;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
    pub attached_project_dbs: Arc<StdRwLock<HashMap<PathBuf, AttachedProjectEntry>>>,
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
        let cached = self
            .attached_project_dbs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .map(|entry| {
                entry.touch();
                entry.state.clone()
            });
        if let Some(state) = cached {
            let _gate = read_or_recover(&state.rw_gate, "path_db_rw_gate");
            return state.read_pool.with_store(label, f);
        }

        let _gate = read_or_recover(&self.global_rw_gate, "path_db_read_gate");
        let mut store = open_read_store(&key, label)?;
        f(&mut store)
    }

    fn attached_project_state(&self, db_path: &Path) -> Result<ProjectDbState, String> {
        let key = project_db_cache_key(db_path)?;
        if let Some(state) = Self::touch_and_clone(&self.attached_project_dbs, &key) {
            return Ok(state);
        }

        let _init_gate =
            lock_or_recover(&self.project_attach_init_gate, "project_attach_init_gate");
        if let Some(state) = Self::touch_and_clone(&self.attached_project_dbs, &key) {
            return Ok(state);
        }

        let state = ProjectDbState::open(key.clone(), configured_memory_read_pool_size())?;
        let mut guard = self
            .attached_project_dbs
            .write()
            .unwrap_or_else(|e| e.into_inner());
        Self::evict_lru_if_needed(&mut guard, ATTACHED_PROJECT_DBS_MAX_ENTRIES, &key);
        Ok(guard
            .entry(key)
            .or_insert_with(|| AttachedProjectEntry::new(state))
            .state
            .clone())
    }

    /// Read-then-touch a cached entry's last-used timestamp and clone its
    /// state out, without holding the lock across the caller's DB work.
    fn touch_and_clone(
        map: &Arc<StdRwLock<HashMap<PathBuf, AttachedProjectEntry>>>,
        key: &Path,
    ) -> Option<ProjectDbState> {
        map.read()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .map(|entry| {
                entry.touch();
                entry.state.clone()
            })
    }

    /// Evict the least-recently-used *evictable* entry/entries when
    /// inserting `new_key` would push the map past `max_entries`.
    ///
    /// An entry is evictable only if it has no outstanding clone: the map's
    /// own slot holds exactly one `Arc` reference per resource-owning field,
    /// so `Arc::strong_count(&entry.state.rw_gate) == 1` means nobody else
    /// currently holds a `ProjectDbState` clone from `attached_project_state`
    /// or `touch_and_clone`. `rw_gate` stands in for the whole struct here
    /// because every clone of `ProjectDbState` is a whole-struct clone (see
    /// `AttachedProjectEntry`'s doc comment) — all its Arcs are cloned
    /// together, so any one of them is representative.
    ///
    /// This check is race-free: clones are only ever handed out while
    /// holding the map's lock (`touch_and_clone`, `attached_project_state`),
    /// and this function only runs while the caller holds the map's *write*
    /// lock, which excludes concurrent readers/cloners for the duration of
    /// eviction. So the strong count observed here cannot change underneath
    /// us mid-eviction.
    ///
    /// If evicting the least-recently-used entry would split an in-flight
    /// caller off from a fresh `rw_gate` (see kckylechen1/tachi#969 review),
    /// skip it and consider the next-least-recently-used entry instead. If
    /// every entry is currently in use, the map is allowed to temporarily
    /// exceed `max_entries` — real concurrent callers bound how far over the
    /// cap it can go, and the alternative (evicting an in-use entry) is a
    /// live per-path read/write exclusion violation, not a bookkeeping nit.
    fn evict_lru_if_needed(
        map: &mut HashMap<PathBuf, AttachedProjectEntry>,
        max_entries: usize,
        new_key: &Path,
    ) {
        if max_entries == 0 || map.contains_key(new_key) {
            return;
        }
        while map.len() >= max_entries {
            let Some(oldest_evictable_key) = map
                .iter()
                .filter(|(_, entry)| Arc::strong_count(&entry.state.rw_gate) == 1)
                .min_by_key(|(_, entry)| entry.recency_tick())
                .map(|(key, _)| key.clone())
            else {
                // No evictable entry (everything currently in flight) or an
                // empty map — stop rather than evict an in-use entry.
                break;
            };
            map.remove(&oldest_evictable_key);
        }
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

/// Maximum number of distinct project DBs the daemon keeps warm
/// (live SQLite read pool + rw_gate) in `attached_project_dbs` before
/// evicting the least-recently-used entry. Bounds the daemon's fd/memory
/// footprint against unbounded distinct project-DB paths (kckylechen1/tachi#969);
/// 32 mirrors `MAX_MEMORY_READ_POOL_SIZE`'s ceiling and comfortably covers a
/// single agent's realistic number of concurrently-active repos/worktrees
/// without keeping every project a daemon has ever touched attached forever.
///
/// This cap is a leak bound, not a working-set guarantee: the background WAL
/// checkpoint task (`spawn_wal_checkpoint` in
/// `tachi-server/src/bootstrap/serve/background.rs`) iterates and reopens
/// *every* named project on each checkpoint pass, independent of this cache.
/// On an installation with more than `ATTACHED_PROJECT_DBS_MAX_ENTRIES`
/// distinct named project DBs, that periodic sweep will itself churn this
/// cache (attach → evict → reattach) every pass. If that usage profile
/// materializes, raise the cap or decouple the checkpoint sweep from the
/// attach cache rather than growing this constant blindly.
pub const ATTACHED_PROJECT_DBS_MAX_ENTRIES: usize = 32;

/// Cache entry for a per-project DB attachment: the shared, `Arc`-backed
/// `ProjectDbState` plus a last-used timestamp used for LRU eviction.
/// Cloning `state` out of the map (see `attached_project_state`) is safe to
/// evict later — all resource-owning fields of `ProjectDbState` are
/// `Arc`-backed (a whole-struct clone shares them: `store` and `rw_gate` are
/// themselves `Arc`s, and `read_pool` is internally `Arc`-backed), so the
/// map only ever holds one of potentially many references; dropping the
/// map's entry drops one refcount, not the underlying pool/mutex. See
/// `evict_lru_if_needed`, which uses that shared refcount to refuse to evict
/// an entry that is still in use.
///
/// `last_used` is an `Arc<AtomicU64>` recency ordinal (see
/// `next_recency_tick`) rather than a plain `Instant` so cache-hit reads can
/// bump recency under a shared `RwLock::read()` instead of forcing every
/// hot-path read to take the map's write lock just to update a timestamp.
#[derive(Clone)]
pub struct AttachedProjectEntry {
    pub state: ProjectDbState,
    pub last_used: Arc<AtomicU64>,
}

impl AttachedProjectEntry {
    fn new(state: ProjectDbState) -> Self {
        Self {
            state,
            last_used: Arc::new(AtomicU64::new(next_recency_tick())),
        }
    }

    fn touch(&self) {
        self.last_used.store(next_recency_tick(), Ordering::Relaxed);
    }

    fn recency_tick(&self) -> u64 {
        self.last_used.load(Ordering::Relaxed)
    }
}

/// Monotonically increasing recency counter for LRU ordering.
///
/// Deliberately not wall-clock time: two touches within the same
/// millisecond (or, on some platforms, sub-microsecond scheduling) would
/// otherwise tie and make "least recently used" ambiguous. A global atomic
/// counter gives every touch a strictly distinct, strictly increasing
/// ordinal, which is all LRU comparison needs.
static RECENCY_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_recency_tick() -> u64 {
    RECENCY_COUNTER.fetch_add(1, Ordering::Relaxed)
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

    #[test]
    fn attached_project_dbs_evicts_least_recently_used_not_first_inserted() {
        // Discriminates true LRU from FIFO: entry 0 is the insertion-oldest
        // AND gets re-touched, so FIFO would evict it (oldest by insertion
        // order) while LRU keeps it (most-recently-used). Entry 1 is
        // insertion-second-oldest but is never re-touched, so it becomes the
        // strict least-recently-used entry — only real LRU evicts it.
        let temp = unique_temp_dir("path-store-lru-not-fifo");
        let global_db = temp.join("global/memory.db");
        std::fs::create_dir_all(global_db.parent().expect("global parent")).expect("global dir");
        let runtime = test_runtime(global_db);

        // Fill the cache to the cap: entries 0..cap, in order.
        let mut project_dbs = Vec::new();
        for i in 0..ATTACHED_PROJECT_DBS_MAX_ENTRIES {
            let project_db = temp.join(format!("project-{i}/.tachi/memory.db"));
            runtime
                .with_path_store(&project_db, |_| Ok(()))
                .unwrap_or_else(|e| panic!("attach project {i}: {e}"));
            project_dbs.push(project_db);
        }
        assert_eq!(
            runtime
                .attached_project_dbs
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .len(),
            ATTACHED_PROJECT_DBS_MAX_ENTRIES,
            "cache should be exactly at the cap"
        );

        // Re-touch only entry 0 (the insertion-oldest). Entry 1 is left
        // untouched, so it — not entry 0 — becomes the true LRU victim.
        runtime
            .with_path_store(&project_dbs[0], |_| Ok(()))
            .expect("re-touch entry 0");

        // Insert one more distinct project DB, pushing past the cap.
        let fresh_db = temp.join("project-fresh/.tachi/memory.db");
        runtime
            .with_path_store(&fresh_db, |_| Ok(()))
            .expect("attach fresh project");

        let guard = runtime
            .attached_project_dbs
            .read()
            .unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            guard.len(),
            ATTACHED_PROJECT_DBS_MAX_ENTRIES,
            "map size must stay bounded at the cap after eviction"
        );

        let entry0_key = project_db_cache_key(&project_dbs[0]).expect("entry 0 key");
        assert!(
            guard.contains_key(&entry0_key),
            "entry 0 was just re-touched (most-recently-used among the old \
             entries) and must survive under LRU — FIFO would wrongly evict \
             it as the insertion-oldest"
        );

        let entry1_key = project_db_cache_key(&project_dbs[1]).expect("entry 1 key");
        assert!(
            !guard.contains_key(&entry1_key),
            "entry 1 was never re-touched and is the strict least-recently- \
             used entry; it must be the one evicted"
        );

        let fresh_key = project_db_cache_key(&fresh_db).expect("fresh key");
        assert!(
            guard.contains_key(&fresh_key),
            "freshly inserted entry must remain"
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn attached_project_dbs_never_evicts_an_in_use_entry() {
        // Pins the in-use guard added for kckylechen1/tachi#969's review
        // fix: an entry with an outstanding clone must never be evicted,
        // even when it is the strict LRU victim and the map is at cap —
        // because evicting it would hand the next caller for the same path
        // a *new* rw_gate, defeating per-path read/write exclusion for the
        // caller still holding the old clone (a split-gate bug).
        let temp = unique_temp_dir("path-store-lru-in-use-guard");
        let global_db = temp.join("global/memory.db");
        std::fs::create_dir_all(global_db.parent().expect("global parent")).expect("global dir");
        let runtime = test_runtime(global_db);

        // Fill the cache to the cap: entries 0..cap, in order.
        let mut project_dbs = Vec::new();
        for i in 0..ATTACHED_PROJECT_DBS_MAX_ENTRIES {
            let project_db = temp.join(format!("project-{i}/.tachi/memory.db"));
            runtime
                .with_path_store(&project_db, |_| Ok(()))
                .unwrap_or_else(|e| panic!("attach project {i}: {e}"));
            project_dbs.push(project_db);
        }

        // Make entry 0 the strict LRU victim: re-touch every other entry,
        // leave entry 0 untouched.
        for project_db in project_dbs.iter().skip(1) {
            runtime
                .with_path_store(project_db, |_| Ok(()))
                .expect("re-touch project");
        }

        // Take an outstanding clone of entry 0's state, simulating an
        // in-flight caller that has attached but not yet finished its work
        // (mirrors what `attached_project_state`/`touch_and_clone` hand
        // back). This clone is held for the rest of the test.
        let entry0_key = project_db_cache_key(&project_dbs[0]).expect("entry 0 key");
        let entry0_state_before = runtime
            .attached_project_dbs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&entry0_key)
            .expect("entry 0 present before eviction attempt")
            .state
            .clone();
        assert_eq!(
            Arc::strong_count(&entry0_state_before.rw_gate),
            2,
            "map's own ref plus this held clone"
        );

        // Insert one more distinct project DB, pushing past the cap. Entry 0
        // is the LRU victim by recency but must be skipped because it's
        // in use; some other (unused) entry may be evicted instead, or the
        // map may temporarily exceed the cap if nothing else is evictable.
        let fresh_db = temp.join("project-fresh/.tachi/memory.db");
        runtime
            .with_path_store(&fresh_db, |_| Ok(()))
            .expect("attach fresh project");

        let guard = runtime
            .attached_project_dbs
            .read()
            .unwrap_or_else(|e| e.into_inner());
        assert!(
            guard.contains_key(&entry0_key),
            "in-use entry must never be evicted, even as strict LRU victim"
        );
        drop(guard);

        // No split-gate: a fresh attach for the same path must hand back
        // the SAME rw_gate Arc as the clone taken before the eviction
        // attempt, proving the map still points at the same ProjectDbState
        // rather than having recreated it under a new gate.
        let entry0_state_after = runtime
            .attached_project_state(&project_dbs[0])
            .expect("re-attach entry 0 after eviction attempt");
        assert!(
            Arc::ptr_eq(&entry0_state_before.rw_gate, &entry0_state_after.rw_gate),
            "in-use entry must keep the same rw_gate Arc — a different Arc \
             here means eviction split the per-path lock (split-gate bug)"
        );

        drop(entry0_state_before);
        let _ = std::fs::remove_dir_all(temp);
    }
}
