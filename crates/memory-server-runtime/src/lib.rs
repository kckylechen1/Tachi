use memcore::MemoryStore;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex as StdMutex, RwLock as StdRwLock};
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

/// Per-checkout diagnostics for a [`ReadStorePool::with_store_recording`]
/// call: how long the caller waited for the pool to hand back an available
/// slot, and how long the caller's closure then ran with that slot checked
/// out. This is timing data only (no DB path, query text, memory content, or
/// credential) — see `with_store_recording`'s doc comment for what it does
/// and does not observe.
#[derive(Debug, Clone, Copy)]
pub struct ReadPoolCheckoutReceipt {
    pub pool_checkout_wait: Duration,
    pub operation_wall_time: Duration,
}

/// Shared state behind every clone of a [`ReadStorePool`]: the fixed slots
/// plus a release signal used to wake checkout attempts that found every
/// slot busy (see `with_store_recording`).
struct ReadPoolInner {
    stores: Vec<StdMutex<MemoryStore>>,
    /// Monotonically increasing generation, bumped once per slot release.
    /// Checkouts that find every slot busy snapshot this value, then block
    /// on `release_cv` until it changes, then rescan — this is how a
    /// checkout waits for "the next slot to free up" without polling/spinning
    /// and without being tied to any particular (possibly still-busy) slot.
    release_signal: StdMutex<u64>,
    release_cv: Condvar,
}

/// RAII notifier: bumps the release generation and wakes every waiter when
/// **dropped** — on both the normal-return path and a panicking `f` (`Drop`
/// runs during unwind too), so a panicking checkout closure can never leave
/// another thread parked forever in [`ReadStorePool::with_store_recording`]'s
/// `wait_while` (codex-m56e0 BUG-1: the pre-fix code only bumped/notified
/// after `f` returned normally — a panic inside `f` skipped it entirely,
/// since the slot's `MutexGuard` unwinds and releases the slot but nothing
/// downstream of the skipped call ever ran). This is the single source of
/// notification for the pool; nothing else calls `notify_all` on
/// `release_cv`.
///
/// Ordering note: on the normal-return path, `with_store_recording`
/// explicitly drops the slot's `MutexGuard` before this guard's scope ends,
/// so a woken waiter finds the slot already free. On a panicking `f`, Rust's
/// unwind drops locals in reverse declaration order, so this guard (declared
/// after the slot guard) fires its notify *before* the slot guard actually
/// unlocks a few instructions later — a woken waiter can rescan into a
/// still-locked slot and simply loop back to waiting, an ordinary spurious
/// wakeup the loop already handles, not a lost one. The property this guard
/// restores is that a wakeup happens at all.
struct ReleaseNotifyGuard<'a> {
    inner: &'a ReadPoolInner,
}

impl Drop for ReleaseNotifyGuard<'_> {
    fn drop(&mut self) {
        let mut generation = lock_or_recover(&self.inner.release_signal, "read_pool_release");
        *generation = generation.wrapping_add(1);
        drop(generation);
        self.inner.release_cv.notify_all();
    }
}

#[derive(Clone)]
pub struct ReadStorePool {
    inner: Arc<ReadPoolInner>,
}

impl ReadStorePool {
    pub fn open_read_only(db_path: &str, size: usize) -> Result<Self, memcore::MemoryError> {
        let size = size.clamp(1, MAX_MEMORY_READ_POOL_SIZE);
        let mut stores = Vec::with_capacity(size);
        for _ in 0..size {
            stores.push(StdMutex::new(MemoryStore::open_read_only(db_path)?));
        }
        Ok(Self {
            inner: Arc::new(ReadPoolInner {
                stores,
                release_signal: StdMutex::new(0),
                release_cv: Condvar::new(),
            }),
        })
    }

    /// Check out an available store and run `f` against it.
    ///
    /// Availability-aware: this tries every slot (`try_lock`) before waiting
    /// on anything, so a checkout never waits behind a busy slot while
    /// another slot sits idle (kckylechen1/tachi#1093 — the prior
    /// `next.fetch_add(1) % len()` round robin could route a checkout onto a
    /// specific busy slot even with other slots free). If every slot is
    /// busy, this blocks on a release signal — notified once, by whichever
    /// checkout releases a slot next — and rescans; no busy-spin, no
    /// unbounded connections, pool size unchanged.
    pub fn with_store<T>(
        &self,
        label: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.with_store_recording(label, f).0
    }

    /// Same checkout behavior as [`Self::with_store`], plus a
    /// [`ReadPoolCheckoutReceipt`] timing how long this call waited for a
    /// slot (`pool_checkout_wait`) and how long `f` then ran
    /// (`operation_wall_time`). Exists for the before/after benchmark suite
    /// (see the `bench` test module below); production call sites use the
    /// plain `with_store`, which pays only the cost of two `Instant::now()`
    /// calls beyond this.
    pub fn with_store_recording<T>(
        &self,
        label: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> (Result<T, String>, ReadPoolCheckoutReceipt) {
        let checkout_started = Instant::now();
        loop {
            // Hold `release_signal` while scanning: any other in-flight
            // checkout whose `ReleaseNotifyGuard` wants to announce a
            // release takes the same lock, so a release can never happen
            // silently in the gap between "we saw every slot busy" and "we
            // start waiting" — that gap is exactly what would otherwise
            // cause a missed wakeup.
            let gen_guard = lock_or_recover(&self.inner.release_signal, label);
            for slot in self.inner.stores.iter() {
                if let Some(mut store) = try_lock_or_recover(slot, label) {
                    drop(gen_guard);
                    let pool_checkout_wait = checkout_started.elapsed();
                    // Constructed before `f` runs so its `Drop` fires the
                    // release notification unconditionally — including when
                    // `f` panics (see the type's doc comment, codex-m56e0
                    // BUG-1). Single source of notification: don't also call
                    // anything else that bumps `release_signal` here.
                    let _release_notify = ReleaseNotifyGuard { inner: &self.inner };
                    let op_started = Instant::now();
                    let result = f(&mut store);
                    let operation_wall_time = op_started.elapsed();
                    drop(store);
                    return (
                        result,
                        ReadPoolCheckoutReceipt {
                            pool_checkout_wait,
                            operation_wall_time,
                        },
                    );
                }
            }
            // No idle slot: block until the next release (never spin), then
            // rescan. We deliberately don't keep the guard `wait_while` hands
            // back — the next iteration's `lock_or_recover` reacquires it.
            let generation_before_wait = *gen_guard;
            drop(
                self.inner
                    .release_cv
                    .wait_while(gen_guard, |generation| {
                        *generation == generation_before_wait
                    })
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            );
        }
    }

    pub fn len(&self) -> usize {
        self.inner.stores.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.stores.is_empty()
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

/// Per-session agent identity (rate-limit overrides, tool filter, provenance
/// attribution). Stored in-memory; populated directly onto `agent_runtime`
/// state by callers (the `agent_register`/`agent_whoami` MCP tools were
/// retired under #757 — no facade currently sets this at runtime).
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

/// Non-blocking availability probe: `Some(guard)` if the slot was free (or
/// recovered from poisoning — poisoning is not "busy", it must not make a
/// slot look permanently occupied), `None` only when another holder
/// genuinely has it locked right now.
fn try_lock_or_recover<'a, T>(
    mutex: &'a StdMutex<T>,
    label: &str,
) -> Option<std::sync::MutexGuard<'a, T>> {
    match mutex.try_lock() {
        Ok(guard) => Some(guard),
        Err(std::sync::TryLockError::Poisoned(poisoned)) => {
            eprintln!("WARNING: mutex poisoned: {label}; recovering with inner state");
            Some(poisoned.into_inner())
        }
        Err(std::sync::TryLockError::WouldBlock) => None,
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

    pub(crate) fn unique_temp_dir(name: &str) -> PathBuf {
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

    /// kckylechen1/tachi#1093: an availability-aware checkout must not wait
    /// behind a busy slot while other slots are idle.
    ///
    /// Pool size 4, matching the production default. The old selector
    /// (`next.fetch_add(1) % stores.len()`) assigns slots in strict call
    /// order: 0, 1, 2, 3, 0, 1, ... A long-held checkout on slot 0 (held open
    /// via a release gate, not by real work) followed by three quick
    /// checkouts cycles the counter through slots 1..3 and back to 0 — the
    /// fifth checkout is then routed by the old selector onto the
    /// still-occupied slot 0, even though slots 1-3 are idle again. This is
    /// exactly the reported convoy.
    ///
    /// Synchronization is entirely channel-based (an entrance signal proves
    /// the occupier holds slot 0 before we proceed; the checkout thread
    /// signals `started` before it ever calls `with_store`, so the 500ms
    /// bound below is timed from "thread definitely running", not from
    /// "thread spawned" — otherwise scheduler latency alone could eat the
    /// bound and false-RED the fixed code; a completion signal on a third
    /// channel is what the final assertion is built on). The 500ms bound is
    /// a generous bound to avoid hanging forever, not a narrow timing race:
    /// the occupier holds its slot until this test explicitly releases it,
    /// so under the unfixed round-robin selector the fifth checkout provably
    /// blocks (on a real `Mutex::lock()`) until that release fires, which
    /// does not happen inside the bound (codex-m56e0 BUG-2).
    #[test]
    fn read_pool_checkout_does_not_wait_behind_the_round_robin_occupied_slot() {
        let temp = unique_temp_dir("read-pool-convoy");
        let db_path = temp.join("memory.db");
        drop(
            MemoryStore::open_with_label(db_path.to_str().expect("db path utf8"), "seed")
                .expect("seed db"),
        );
        let db_str = db_path.to_str().expect("db path utf8").to_string();
        let pool = ReadStorePool::open_read_only(&db_str, 4).expect("open pool");

        let release = Arc::new((StdMutex::new(false), std::sync::Condvar::new()));
        let (occupier_entered_tx, occupier_entered_rx) = std::sync::mpsc::channel();
        let occupier_pool = pool.clone();
        let occupier_release = Arc::clone(&release);
        let occupier = std::thread::spawn(move || {
            occupier_pool.with_store("occupier", |_store| {
                occupier_entered_tx
                    .send(())
                    .expect("occupier entrance signal");
                let (released, wake) = &*occupier_release;
                let mut released = released.lock().expect("lock release gate");
                while !*released {
                    released = wake.wait(released).expect("wait release gate");
                }
                Ok(())
            })
        });
        occupier_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("occupier should enter slot 0 before we proceed");

        // Three quick checkouts cycle the round-robin cursor through slots
        // 1..3, each returning immediately (they never touch slot 0).
        for i in 1..=3 {
            pool.with_store("filler", |_store| Ok(()))
                .unwrap_or_else(|e| panic!("filler checkout {i} failed: {e}"));
        }

        // The fifth checkout: round-robin would route it back onto slot 0
        // (still held by `occupier`), even though slots 1-3 are idle again.
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let checkout_pool = pool.clone();
        let checkout = std::thread::spawn(move || {
            // Signal BEFORE calling with_store, so the main thread's 500ms
            // bound (below) times from "this thread is definitely running",
            // not from "this thread was spawned" — scheduler delay between
            // spawn and first instruction must not be counted against the
            // checkout, or it could false-RED the fixed code under load.
            started_tx.send(()).expect("checkout thread started signal");
            let result = checkout_pool.with_store("checkout", |_store| Ok(()));
            let _ = done_tx.send(result.is_ok());
        });
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("checkout thread should signal started before we start timing it");

        let completed_promptly = done_rx.recv_timeout(Duration::from_millis(500));

        // Always release the occupier before inspecting the result, so a
        // failing assertion below never leaves either thread parked.
        {
            let (released, wake) = &*release;
            *released.lock().expect("lock release gate") = true;
            wake.notify_all();
        }
        occupier
            .join()
            .expect("occupier thread should join")
            .expect("occupier checkout should succeed");
        checkout.join().expect("checkout thread should join");

        assert!(
            completed_promptly.is_ok(),
            "checkout into an idle slot (1-3) must not wait behind the round-robin-selected, \
             still-occupied slot 0"
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    /// codex-m56e0 BUG-1 regression: a panicking checkout closure must not
    /// leave another already-in-flight checkout parked forever. Pre-fix,
    /// `with_store_recording` only bumped the release generation and
    /// notified waiters *after* `f` returned normally — a panic inside `f`
    /// unwound straight past that call, so a waiter blocked in `wait_while`
    /// on the same slot was never woken.
    ///
    /// Pool size 1 (deliberately): the only slot is held by the panicking
    /// checkout, so the waiter has nowhere else to go — it can only proceed
    /// once the panicking checkout's release notification fires (via
    /// `ReleaseNotifyGuard`'s `Drop`, which must run during unwind). This
    /// isolates the panic-path notification specifically, as opposed to the
    /// multi-slot convoy covered by the test above.
    ///
    /// Ordering is pinned by two entrance signals, not by sleeping: the
    /// panicker signals `entered` before it parks on its own release gate
    /// (so the sole slot is provably held before the waiter is even
    /// spawned), and the waiter signals `started` immediately before calling
    /// `with_store` (so the main thread doesn't flip the panicker's gate —
    /// triggering the panic — until the waiter's checkout is already in
    /// flight against a slot that is, at that moment, still held). The panic
    /// is caught with `catch_unwind` inside the panicker thread so it is
    /// contained and inspectable rather than just failing that thread
    /// silently.
    #[test]
    fn panicking_checkout_still_wakes_a_waiter_parked_on_the_same_slot() {
        let temp = unique_temp_dir("read-pool-panic-wakeup");
        let db_path = temp.join("memory.db");
        drop(
            MemoryStore::open_with_label(db_path.to_str().expect("db path utf8"), "seed")
                .expect("seed db"),
        );
        let db_str = db_path.to_str().expect("db path utf8").to_string();
        let pool = ReadStorePool::open_read_only(&db_str, 1).expect("open pool");

        let panicker_gate = Arc::new((StdMutex::new(false), std::sync::Condvar::new()));
        let (panicker_entered_tx, panicker_entered_rx) = std::sync::mpsc::channel();
        let panicker_pool = pool.clone();
        let panicker_gate_clone = Arc::clone(&panicker_gate);
        let panicker = std::thread::spawn(move || {
            let checkout = |_store: &mut MemoryStore| -> Result<(), String> {
                panicker_entered_tx
                    .send(())
                    .expect("panicker entrance signal");
                let (proceed, wake) = &*panicker_gate_clone;
                let mut proceed_guard = proceed.lock().expect("lock panicker gate");
                while !*proceed_guard {
                    proceed_guard = wake.wait(proceed_guard).expect("wait panicker gate");
                }
                panic!(
                    "intentional test panic: checkout closure failure \
                     (codex-m56e0 BUG-1 regression)"
                );
            };
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                panicker_pool.with_store("panicker", checkout)
            }))
            .is_err()
        });
        panicker_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("panicker should hold the sole slot before we proceed");

        let (waiter_started_tx, waiter_started_rx) = std::sync::mpsc::channel();
        let (waiter_done_tx, waiter_done_rx) = std::sync::mpsc::channel();
        let waiter_pool = pool.clone();
        let waiter = std::thread::spawn(move || {
            waiter_started_tx.send(()).expect("waiter started signal");
            let result = waiter_pool.with_store("waiter", |_store| Ok(()));
            let _ = waiter_done_tx.send(result.is_ok());
        });
        waiter_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("waiter should signal started before we trigger the panic");

        // Trigger the panic: the sole slot's only holder panics instead of
        // returning normally.
        {
            let (proceed, wake) = &*panicker_gate;
            *proceed.lock().expect("lock panicker gate") = true;
            wake.notify_all();
        }

        assert!(
            panicker.join().expect("panicker thread should join"),
            "panicker checkout should have panicked as intended"
        );

        let waiter_completed = waiter_done_rx.recv_timeout(Duration::from_millis(500));
        waiter.join().expect("waiter thread should join");

        assert!(
            waiter_completed.is_ok(),
            "a panicking checkout closure must still wake a waiter parked on the \
             same (sole) slot — otherwise the waiter is left blocked forever"
        );

        let _ = std::fs::remove_dir_all(temp);
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

/// Before/after receipt benchmarks for kckylechen1/tachi#1093 (the
/// `ReadStorePool` convoy fix). These are `#[ignore]`d — they measure
/// wall-clock behavior, not correctness, and are meant to be run explicitly
/// on a given SHA (base vs. fixed) to produce a before/after comparison:
///
/// ```text
/// cargo test -p memory-server-runtime --lib bench:: -- --ignored --nocapture
/// ```
///
/// No tracing subscriber is installed here, so results are printed directly;
/// `memcore::db::lock_retry_backoff_count()` (a plain counter, not a tracing
/// consumer) supplies the SQLite-retry side of the receipt where relevant.
/// Every microbenchmark's "operation" closure is a trivial field read
/// (matching this file's existing test convention), so `operation_wall_time`
/// here isolates pool/lock overhead rather than real query cost — that
/// isolation is deliberate: it is exactly what the convoy fix changes.
#[cfg(test)]
mod bench {
    use super::tests::unique_temp_dir;
    use super::*;

    /// `None` means "no samples", distinct from a genuinely-measured `0`.
    fn percentile(sorted_micros: &[u128], pct: f64) -> Option<u128> {
        if sorted_micros.is_empty() {
            return None;
        }
        let rank = (((sorted_micros.len() - 1) as f64) * pct).round() as usize;
        Some(sorted_micros[rank.min(sorted_micros.len() - 1)])
    }

    /// codex-m56e0 BUG-3 (last item): an empty sample set must print as
    /// "N/A", not silently as `0` — printing `0` would misreport "measured
    /// zero microseconds" when the truth is "nothing was measured at all".
    fn fmt_percentile(value: Option<u128>) -> String {
        match value {
            Some(v) => v.to_string(),
            None => "N/A".to_string(),
        }
    }

    fn report(name: &str, mut checkout_wait_us: Vec<u128>, mut op_wall_us: Vec<u128>) {
        checkout_wait_us.sort_unstable();
        op_wall_us.sort_unstable();
        println!(
            "[bench:{name}] n={} checkout_wait_us(p50={} p95={}) op_wall_us(p50={} p95={})",
            checkout_wait_us.len().max(op_wall_us.len()),
            fmt_percentile(percentile(&checkout_wait_us, 0.50)),
            fmt_percentile(percentile(&checkout_wait_us, 0.95)),
            fmt_percentile(percentile(&op_wall_us, 0.50)),
            fmt_percentile(percentile(&op_wall_us, 0.95)),
        );
    }

    #[test]
    #[ignore = "manual before/after receipt; run with --ignored --nocapture"]
    fn uncontended_checkout_overhead() {
        let temp = unique_temp_dir("bench-uncontended");
        let db_path = temp.join("memory.db");
        drop(
            MemoryStore::open_with_label(db_path.to_str().expect("db path utf8"), "seed")
                .expect("seed db"),
        );
        let pool = ReadStorePool::open_read_only(db_path.to_str().expect("db path utf8"), 4)
            .expect("open pool");

        const ITERATIONS: usize = 500;
        let mut checkout_wait_us = Vec::with_capacity(ITERATIONS);
        let mut op_wall_us = Vec::with_capacity(ITERATIONS);
        for _ in 0..ITERATIONS {
            let (result, receipt) = pool.with_store_recording("bench", |_store| Ok(()));
            result.expect("uncontended checkout should succeed");
            checkout_wait_us.push(receipt.pool_checkout_wait.as_micros());
            op_wall_us.push(receipt.operation_wall_time.as_micros());
        }
        report(
            "uncontended_checkout_overhead",
            checkout_wait_us,
            op_wall_us,
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    #[ignore = "manual before/after receipt; run with --ignored --nocapture"]
    fn one_long_read_with_concurrent_short_reads() {
        // One reader holds a slot for a fixed duration (simulating a slow
        // scan); N short concurrent readers exercise the remaining slots
        // while it does. This is the exact shape of the reported convoy: on
        // the unfixed round-robin selector, some fraction of the short
        // readers' checkout_wait shows large tail latency (whichever ones
        // the round-robin cursor happens to route onto the long reader's
        // slot); on the fix, checkout_wait should stay low throughout since
        // idle slots are always found before waiting. This benchmark's
        // printed p50/p95 IS the before/after receipt — no separate
        // assertion.
        //
        // codex-m56e0 BUG-3: the long reader releases on its OWN timer
        // (`HOLD_DURATION`), not by waiting for the short readers to finish
        // first. An earlier revision released the long reader only after
        // joining all 20 short readers — on the pre-fix round-robin
        // selector, some of those readers land on the long reader's occupied
        // slot and block until it releases, so that join could never
        // complete and the base-SHA run would never produce a receipt at
        // all. Releasing on a timer means the long reader always eventually
        // lets go regardless of which readers are blocked on it.
        //
        // Collecting the short readers' receipts is ALSO bounded (a
        // generous per-item timeout), not an unconditional `.join()`: a
        // receipt legitimately arriving up to ~`HOLD_DURATION` late (pre-fix,
        // for a reader routed onto the occupied slot) is expected, but
        // anything past the bound is reported as an explicit observed
        // timeout rather than hung on — on unfixed code, that count is
        // itself the headline before/after number.
        let temp = unique_temp_dir("bench-convoy");
        let db_path = temp.join("memory.db");
        drop(
            MemoryStore::open_with_label(db_path.to_str().expect("db path utf8"), "seed")
                .expect("seed db"),
        );
        let pool = ReadStorePool::open_read_only(db_path.to_str().expect("db path utf8"), 4)
            .expect("open pool");

        const HOLD_DURATION: Duration = Duration::from_millis(200);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let long_pool = pool.clone();
        let long_reader = std::thread::spawn(move || {
            long_pool.with_store("long_read", |_store| {
                entered_tx.send(()).expect("long reader entrance signal");
                // Simulated workload duration, reported implicitly via the
                // short readers' checkout_wait — not itself a
                // synchronization mechanism (nothing waits on this value).
                std::thread::sleep(HOLD_DURATION);
                Ok(())
            })
        });
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("long reader should enter before short readers start");

        const SHORT_READERS: usize = 20;
        let (receipt_tx, receipt_rx) = std::sync::mpsc::channel();
        for _ in 0..SHORT_READERS {
            let pool = pool.clone();
            let receipt_tx = receipt_tx.clone();
            std::thread::spawn(move || {
                let (result, receipt) =
                    pool.with_store_recording("short_read", |_store| Ok(()));
                result.expect("short read should succeed");
                let _ = receipt_tx.send(receipt);
            });
        }
        drop(receipt_tx);

        const PER_ITEM_TIMEOUT: Duration = Duration::from_secs(2);
        let mut receipts = Vec::with_capacity(SHORT_READERS);
        let mut observed_timeouts = 0usize;
        for _ in 0..SHORT_READERS {
            match receipt_rx.recv_timeout(PER_ITEM_TIMEOUT) {
                Ok(receipt) => receipts.push(receipt),
                Err(_) => observed_timeouts += 1,
            }
        }

        long_reader
            .join()
            .expect("long reader thread should join")
            .expect("long reader checkout should succeed");

        let checkout_wait_us = receipts
            .iter()
            .map(|r| r.pool_checkout_wait.as_micros())
            .collect();
        let op_wall_us = receipts
            .iter()
            .map(|r| r.operation_wall_time.as_micros())
            .collect();
        report(
            "one_long_read_with_concurrent_short_reads",
            checkout_wait_us,
            op_wall_us,
        );
        println!(
            "[bench:one_long_read_with_concurrent_short_reads] \
             observed_timeouts={observed_timeouts}/{SHORT_READERS} (a nonzero count here \
             IS convoy evidence on the pre-fix selector, not a test failure)"
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    #[ignore = "manual before/after receipt; run with --ignored --nocapture"]
    fn controlled_write_contention() {
        // Best-effort, per the contract: this DB runs in WAL mode
        // (schema/ddl.rs), where readers never block writers and vice versa,
        // and only concurrent *writers* can contend for the single write
        // lock. This opens several independent writer connections to the
        // same DB file and hammers them concurrently. If BUSY/LOCKED never
        // actually triggers at this contention level, an honest zero retry
        // count is the correct receipt — the contract forbids inflating
        // busy_timeout/retry counts/pool size to force a positive number.
        let temp = unique_temp_dir("bench-write-contention");
        let db_path = temp.join("memory.db");
        let db_str = db_path.to_str().expect("db path utf8").to_string();
        drop(MemoryStore::open_with_label(&db_str, "seed").expect("seed db"));

        let retries_before = memcore::db::lock_retry_backoff_count();

        const WRITERS: usize = 6;
        const WRITES_PER_WRITER: usize = 20;
        let handles: Vec<_> = (0..WRITERS)
            .map(|writer_idx| {
                let db_str = db_str.clone();
                std::thread::spawn(move || {
                    // `open` (not `open_with_label`) disables path-routing
                    // validation entirely — this benchmark exercises lock
                    // contention on the write path, not path routing.
                    let mut store = MemoryStore::open(&db_str).expect("open writer");
                    let mut local_wall_us = Vec::with_capacity(WRITES_PER_WRITER);
                    for i in 0..WRITES_PER_WRITER {
                        let entry = memcore::MemoryEntry {
                            id: format!("bench-write-contention-{writer_idx}-{i}"),
                            path: "/bench".to_string(),
                            summary: "bench write contention".to_string(),
                            text: "bench write contention".to_string(),
                            importance: 0.3,
                            // Fixed valid RFC3339 literal — this benchmark
                            // exercises write-path lock contention, not
                            // wall-clock timestamps, and adding a `chrono`
                            // dependency to this crate just to stamp "now"
                            // would be an unjustified new dependency.
                            timestamp: "2026-01-01T00:00:00.000Z".to_string(),
                            valid_from: String::new(),
                            valid_until: None,
                            category: "other".to_string(),
                            topic: String::new(),
                            keywords: Vec::new(),
                            persons: Vec::new(),
                            entities: Vec::new(),
                            location: String::new(),
                            source: "bench".to_string(),
                            scope: "general".to_string(),
                            archived: false,
                            access_count: 0,
                            last_access: None,
                            revision: 1,
                            metadata: serde_json::json!({}),
                            vector: None,
                            retention_policy: None,
                            domain: None,
                            recall_count: 0,
                            query_diversity: 0,
                            tier: "raw".to_string(),
                        };
                        let started = Instant::now();
                        store
                            .upsert(&entry)
                            .expect("bench upsert should eventually succeed within retry budget");
                        local_wall_us.push(started.elapsed().as_micros());
                    }
                    local_wall_us
                })
            })
            .collect();

        let mut op_wall_us = Vec::new();
        for handle in handles {
            op_wall_us.extend(handle.join().expect("writer thread should join"));
        }

        let retries_after = memcore::db::lock_retry_backoff_count();
        report("controlled_write_contention", Vec::new(), op_wall_us);
        println!(
            "[bench:controlled_write_contention] explicit_lock_retry_backoffs={} \
             (application-level thread::sleep backoffs only — SQLite's own opaque \
             busy_timeout wait is not separately observable, see retry_memory_locked)",
            retries_after.saturating_sub(retries_before)
        );

        let _ = std::fs::remove_dir_all(temp);
    }
}
