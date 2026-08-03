use memcore::MemoryStore;
use memcore::{DbOpenContext, MigrationAuthority, OpenIntent};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
#[cfg(feature = "test-support")]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex as StdMutex, OnceLock, RwLock as StdRwLock};
use std::time::{Duration, Instant};

const DEFAULT_MEMORY_READ_POOL_SIZE: usize = 4;
const MAX_MEMORY_READ_POOL_SIZE: usize = 32;
const DEFAULT_DB_CONTENTION_RECEIPT_CAPACITY: usize = 4096;

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

/// One opt-in timing sample from a real global [`DbRuntime`] operation.
///
/// The labels name the two resources involved in every global operation:
/// `gate` is always `global_rw_gate`, while `resource` is either the write
/// `global_store` mutex or the read `global_read_pool`. Timings deliberately
/// contain no database path, query, memory content, or credential.
#[derive(Debug, Clone)]
pub struct DbContentionReceipt {
    /// Stable instrumentation boundary, currently `db_runtime`.
    pub phase: &'static str,
    /// `write` for [`DbRuntime::with_global_store`], `read` for its read twins.
    pub action: &'static str,
    /// The primary operation resource: `global_store` or `global_read_pool`.
    pub resource: &'static str,
    /// The `global_rw_gate` mode: `exclusive` for writes, `shared` for reads.
    pub mode: &'static str,
    /// The shared gate that serialized or admitted the operation.
    pub gate: &'static str,
    pub gate_wait: Duration,
    pub resource_wait: Duration,
    pub resource_hold: Duration,
    pub gate_hold: Duration,
    /// `false` when the closure returned an error or panicked.
    pub completed: bool,
}

/// One atomic drain of a [`DbContentionRecorder`].
///
/// `dropped_samples` covers only the same interval as `receipts`; draining
/// resets both together so a harness cannot silently report a complete phase
/// after collector overflow.
#[derive(Debug)]
pub struct DbContentionBatch {
    pub receipts: Vec<DbContentionReceipt>,
    pub dropped_samples: u64,
}

#[derive(Default)]
struct DbContentionRecorderState {
    receipts: VecDeque<DbContentionReceipt>,
    dropped_samples: u64,
}

/// Shared, opt-in sink for [`DbContentionReceipt`]s.
///
/// A runtime allocates this only when a caller asks to observe it. Normal
/// global reads and writes retain their existing no-timer path.
pub struct DbContentionRecorder {
    state: StdMutex<DbContentionRecorderState>,
    capacity: usize,
}

impl Default for DbContentionRecorder {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_DB_CONTENTION_RECEIPT_CAPACITY)
    }
}

impl DbContentionRecorder {
    /// Construct a recorder with a fixed maximum number of retained samples.
    /// Once full, new samples are counted as dropped until the next drain.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            state: StdMutex::new(DbContentionRecorderState {
                receipts: VecDeque::with_capacity(capacity),
                dropped_samples: 0,
            }),
            capacity,
        }
    }

    fn record(&self, receipt: DbContentionReceipt) {
        let mut state = lock_or_recover(&self.state, "db_contention_receipts");
        if state.receipts.len() < self.capacity {
            state.receipts.push_back(receipt);
        } else {
            state.dropped_samples = state.dropped_samples.saturating_add(1);
        }
    }

    /// Drain every retained sample and the overflow count observed so far.
    /// The queue and counter reset atomically for phase-isolated collection.
    pub fn drain(&self) -> DbContentionBatch {
        let mut state = lock_or_recover(&self.state, "db_contention_receipts");
        DbContentionBatch {
            receipts: state.receipts.drain(..).collect(),
            dropped_samples: std::mem::take(&mut state.dropped_samples),
        }
    }
}

struct ElapsedOnDrop<'a> {
    started: Instant,
    elapsed: &'a mut Option<Duration>,
}

impl Drop for ElapsedOnDrop<'_> {
    fn drop(&mut self) {
        *self.elapsed = Some(self.started.elapsed());
    }
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
    /// Test-only causal observation point (codex-qa111 R4-1): senders
    /// registered here are notified exactly when a checkout, having scanned
    /// every slot and found none free, is about to block in `wait_while` —
    /// see `ReadStorePool::observe_next_parked_checkout_for_test`. `#[cfg(test)]`-gated
    /// so it adds no field, lock, or branch to non-test builds. Never read
    /// or written from any production path — test-only, for proving a
    /// specific waiter has actually reached the wait before a test triggers
    /// whatever event it expects to wake it.
    #[cfg(test)]
    parked_observers: StdMutex<Vec<std::sync::mpsc::Sender<()>>>,
}

/// Owns both the checked-out slot's `MutexGuard` and the release
/// notification for that checkout. The unlock-then-notify order is encoded
/// in **one** `Drop` impl, as a fixed sequence of two statements in a single
/// function body — it can no longer be gotten wrong by rearranging local
/// `let`-bindings at a call site.
///
/// # Load-bearing ordering invariant (codex-o964f R3-1, structuralized codex-s305b R5)
///
/// An earlier version of this fix used two separate locals — a slot
/// `MutexGuard` and a standalone `ReleaseNotifyGuard` — and relied on
/// binding the notify guard *before* the slot guard in the same scope
/// (Rust's reverse-bind-order drop then unlocked the slot before notifying,
/// on both the normal-return path and a panicking `f`'s unwind). That
/// worked, but codex-s305b's review pointed out the real problem with it:
/// **binding order is not auditable by a black-box test.** A test can prove
/// today's code wakes a waiter correctly; it cannot prove some future
/// refactor won't silently swap the order of two adjacent `let` statements
/// and reopen the exact lost-wakeup window R3-1 fixed (notify firing before
/// the slot actually unlocks, so a waiter's rescan lands on an
/// already-busy slot and re-parks on a generation value that will never
/// change again).
///
/// `SlotCheckout` removes the possibility entirely rather than documenting
/// around it: there is only one object, one `Drop` impl, and the order is
/// two sequential statements in that one function — "unlock the slot" then
/// "bump generation + notify_all". Reordering those two lines is a visible,
/// one-function diff to `SlotCheckout::drop`, not an invisible fact spread
/// across whichever call site happens to construct the checkout.
struct SlotCheckout<'a> {
    /// `Option` so `Drop` can move the guard out and drop it *explicitly*,
    /// as the first statement of `Drop::drop`, rather than depending on
    /// struct field-drop order (which Rust does define, but which is far
    /// less obviously load-bearing to a future reader than an explicit
    /// `drop(self.store.take())` line is).
    store: Option<std::sync::MutexGuard<'a, MemoryStore>>,
    inner: &'a ReadPoolInner,
}

impl<'a> SlotCheckout<'a> {
    fn new(store: std::sync::MutexGuard<'a, MemoryStore>, inner: &'a ReadPoolInner) -> Self {
        Self {
            store: Some(store),
            inner,
        }
    }
}

impl Drop for SlotCheckout<'_> {
    fn drop(&mut self) {
        // 1. The slot unlocks — dropping the `MutexGuard` releases the real
        // lock. This runs on both the normal-return path and during a
        // panicking `f`'s unwind (`Drop` runs during unwind too).
        drop(self.store.take());
        // 2. THEN, and only then, bump the release generation and wake
        // every waiter. A waiter woken by this `notify_all` is therefore
        // guaranteed the slot is already free — see the struct's doc
        // comment for why this fixed order is now structural, not a
        // call-site binding convention.
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
    /// Open `size` read-only handles on `db_path`, every one of them carrying
    /// `db_label`.
    ///
    /// tachi#1569: `db_label` must be the same manifest label the write store
    /// for this path was opened with (`global`, `wiki`, the project name).
    /// Pooled read stores used to be born unlabelled, which is why read-side
    /// gates could not be keyed on store identity at all.
    pub fn open_read_only(
        db_path: &str,
        size: usize,
        db_label: &str,
    ) -> Result<Self, memcore::MemoryError> {
        let size = size.clamp(1, MAX_MEMORY_READ_POOL_SIZE);
        let mut stores = Vec::with_capacity(size);
        for _ in 0..size {
            stores.push(StdMutex::new(MemoryStore::open_read_only_with_label(
                db_path, db_label,
            )?));
        }
        Ok(Self {
            inner: Arc::new(ReadPoolInner {
                stores,
                release_signal: StdMutex::new(0),
                release_cv: Condvar::new(),
                #[cfg(test)]
                parked_observers: StdMutex::new(Vec::new()),
            }),
        })
    }

    /// Test-only: register to be notified the next time a checkout that
    /// finds every slot busy is about to block on `release_cv` (fires once,
    /// for the next such checkout only — register again for a second one).
    /// Never call this from production code (codex-qa111 R4-1): it exists
    /// purely so tests can prove a specific waiter has genuinely reached
    /// `wait_while`, replacing a probabilistic "signal that it started, then
    /// hope it got far enough" with a real causal signal.
    #[cfg(test)]
    fn observe_next_parked_checkout_for_test(&self) -> std::sync::mpsc::Receiver<()> {
        let (tx, rx) = std::sync::mpsc::channel();
        lock_or_recover(&self.inner.parked_observers, "read_pool_parked_observers").push(tx);
        rx
    }

    /// Test-only: fire every observer registered via
    /// `observe_next_parked_checkout_for_test`, then clear them (each
    /// observer is one-shot). Called immediately before `wait_while` in the
    /// shared `ReadStorePool::checkout` loop (both the recording and plain
    /// paths — the observer is about the wait, not the timer).
    #[cfg(test)]
    fn notify_parked_observers_for_test(&self) {
        let mut observers =
            lock_or_recover(&self.inner.parked_observers, "read_pool_parked_observers");
        for observer in observers.drain(..) {
            let _ = observer.send(());
        }
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
    ///
    /// Delegates to [`Self::checkout`] with `record: false` — this path
    /// performs NO `Instant::now()` call and constructs no
    /// [`ReadPoolCheckoutReceipt`] (kckylechen1/tachi#1125 cold review: the
    /// unsampled path must stay free of the timing instrumentation that
    /// `with_store_recording` opts into).
    pub fn with_store<T>(
        &self,
        label: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.checkout(label, false, f).0
    }

    /// Same checkout behavior as [`Self::with_store`], plus a
    /// [`ReadPoolCheckoutReceipt`] timing how long this call waited for a
    /// slot (`pool_checkout_wait`) and how long `f` then ran
    /// (`operation_wall_time`). Exists for the before/after benchmark suite
    /// (see the `bench` test module below); production call sites use the
    /// plain `with_store`, which — via [`Self::checkout`]'s `record: false`
    /// — pays none of this timing cost.
    pub fn with_store_recording<T>(
        &self,
        label: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> (Result<T, String>, ReadPoolCheckoutReceipt) {
        let (result, receipt) = self.checkout(label, true, f);
        (
            result,
            receipt.expect("checkout(record: true) always returns a receipt"),
        )
    }

    /// The single production checkout loop backing both [`Self::with_store`]
    /// and [`Self::with_store_recording`] (kckylechen1/tachi#1125 cold
    /// review: a second, timer-free copy of this loop would be a
    /// pooling/locking divergence bomb — two implementations of the slot
    /// scan, the `wait_while` park, and the release-signal wakeup that must
    /// never drift apart). `record` gates ONLY the timing instrumentation,
    /// using the same `sample.then(Instant::now)` idiom as
    /// `memcore::search` / `auto_link.rs`: when `record` is `false`, neither
    /// `Instant::now()` call below runs and no [`ReadPoolCheckoutReceipt`] is
    /// built, so `with_store`'s production callers pay zero timing cost.
    /// Every pooling/locking behavior — slot-scan order, the `try_lock`
    /// availability check, the `wait_while` park, the release-signal
    /// wakeup, and the `#[cfg(test)]` parked-observer notify — is identical
    /// regardless of `record`; only the two `Instant` reads and the receipt
    /// construction are conditional.
    fn checkout<T>(
        &self,
        label: &str,
        record: bool,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> (Result<T, String>, Option<ReadPoolCheckoutReceipt>) {
        let checkout_started = record.then(Instant::now);
        loop {
            // Hold `release_signal` while scanning: any other in-flight
            // checkout whose `SlotCheckout` is being dropped (and so wants
            // to announce a release) takes the same lock, so a release can
            // never happen silently in the gap between "we saw every slot
            // busy" and "we start waiting" — that gap is exactly what would
            // otherwise cause a missed wakeup.
            let gen_guard = lock_or_recover(&self.inner.release_signal, label);
            for slot in self.inner.stores.iter() {
                if let Some(candidate) = try_lock_or_recover(slot, label) {
                    // The ONLY statement between acquiring the guard and
                    // wrapping it in `SlotCheckout` is this MutexGuard drop,
                    // which cannot panic — so no unwind can release the slot
                    // outside SlotCheckout::drop (codex-tdf83 item b). The
                    // wrap must NOT move before this drop: constructing
                    // SlotCheckout while holding `release_signal` would
                    // self-deadlock on unwind (its Drop takes the same lock).
                    drop(gen_guard);
                    let mut checkout = SlotCheckout::new(candidate, &self.inner);
                    let pool_checkout_wait = checkout_started.map(|started| started.elapsed());
                    let op_started = record.then(Instant::now);
                    let result = f(checkout
                        .store
                        .as_mut()
                        .expect("SlotCheckout store missing before drop"));
                    let receipt = op_started.map(|op_started| ReadPoolCheckoutReceipt {
                        pool_checkout_wait: pool_checkout_wait
                            .expect("record gates checkout_started and op_started together"),
                        operation_wall_time: op_started.elapsed(),
                    });
                    return (result, receipt);
                }
            }
            // No idle slot: block until the next release (never spin), then
            // rescan. We deliberately don't keep the guard `wait_while` hands
            // back — the next iteration's `lock_or_recover` reacquires it.
            let generation_before_wait = *gen_guard;
            // Test-only causal observation point (codex-qa111 R4-1): fires
            // immediately before we actually block, while still holding
            // `gen_guard` — a test that already registered an observer is
            // guaranteed this checkout is about to enter `wait_while` the
            // instant it receives this. No effect and no cost outside test
            // builds. Fires on both recording and non-recording checkouts —
            // this is about the wait, not the timer.
            #[cfg(test)]
            self.notify_parked_observers_for_test();
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

impl VaultState {
    /// Single source of truth for "has the auto-lock timeout expired?".
    ///
    /// `auto_lock_after_secs == 0` means auto-lock is **disabled** (daemon
    /// mode): the session never expires on its own, no matter how long ago it
    /// was unlocked. This sentinel MUST be honored by every expiry check — both
    /// the enforcement path (clearing the key in `maybe_auto_lock_vault` /
    /// `with_vault_key`) and the status-reporting path (`runtime_observability_json`
    /// vault block) — so reported state matches reality. A bare
    /// `elapsed() > auto_lock_after_secs` without this sentinel makes a
    /// `0`-configured daemon report `locked: true` one second after unlock while
    /// the key is still live and usable.
    ///
    /// Returns `false` when there is no `unlock_time` (not unlocked); callers
    /// handle the locked case separately.
    pub fn auto_lock_expired(&self) -> bool {
        let Some(unlock_time) = self.unlock_time else {
            return false;
        };
        self.auto_lock_after_secs > 0
            && unlock_time.elapsed() > Duration::from_secs(self.auto_lock_after_secs)
    }
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
    /// Lazily initialized only by an explicit measurement caller (#1255).
    /// The normal path performs a single `OnceLock::get` and takes no timer.
    pub global_contention_recorder: Arc<OnceLock<Arc<DbContentionRecorder>>>,
    /// Test-only causal signal for a writer that has observed the real global
    /// gate as busy immediately before its measured blocking acquisition.
    #[cfg(test)]
    global_write_gate_contended_observers: Arc<StdMutex<Vec<std::sync::mpsc::Sender<()>>>>,
    pub global_db_path: Arc<PathBuf>,
    pub global_vec_available: bool,
    pub project_db: Arc<StdRwLock<Option<ProjectDbState>>>,
    pub attached_project_dbs: Arc<StdRwLock<HashMap<PathBuf, AttachedProjectEntry>>>,
    pub project_attach_init_gate: Arc<StdMutex<()>>,
    /// #1119: migration authority for *dynamic* project DB opens (activate /
    /// attach). Fail-closed [`MigrationAuthority::Deny`] by default; the
    /// deploy-time daemon threads `Allow` from its `--allow-schema-migration`
    /// flag so opening an older-schema project DB migrates instead of
    /// refusing. The policy lives HERE, at the open site, not only in
    /// bootstrap — every runtime project open must express it.
    pub schema_migration: MigrationAuthority,
}

/// A request-owned read-only store.
///
/// Unlike a [`ReadStorePool`] checkout, this contains no mutex or `RwLock`
/// guard. Callers may retain it across async boundaries, but it is intentionally
/// single-owner and exposes the store only through synchronous closures.
pub struct RequestScopedReadStore {
    store: MemoryStore,
}

impl RequestScopedReadStore {
    pub fn with_store<T>(
        &mut self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        f(&mut self.store)
    }
}

impl DbRuntime {
    /// Enable isolated observation of future global read/write contention.
    ///
    /// This does not change lock order, pool size, or admission semantics. The
    /// returned recorder is shared by every clone of this runtime, including
    /// per-MCP-session server clones used by the in-process receipt harness.
    pub fn enable_global_contention_receipts(&self) -> Arc<DbContentionRecorder> {
        Arc::clone(
            self.global_contention_recorder
                .get_or_init(|| Arc::new(DbContentionRecorder::default())),
        )
    }

    /// Test-only: notify once when a global writer has observed
    /// `global_rw_gate` as contended immediately before its measured acquire.
    /// This is a causal test probe, not a production lock path or policy.
    #[cfg(test)]
    fn observe_next_global_write_gate_contention_for_test(&self) -> std::sync::mpsc::Receiver<()> {
        let (tx, rx) = std::sync::mpsc::channel();
        lock_or_recover(
            &self.global_write_gate_contended_observers,
            "global_write_gate_contended_observers",
        )
        .push(tx);
        rx
    }

    #[cfg(test)]
    fn notify_global_write_gate_contention_for_test(&self) {
        if lock_or_recover(
            &self.global_write_gate_contended_observers,
            "global_write_gate_contended_observers",
        )
        .is_empty()
        {
            return;
        }
        let contended = matches!(
            self.global_rw_gate.try_write(),
            Err(std::sync::TryLockError::WouldBlock)
        );
        if !contended {
            return;
        }
        for observer in lock_or_recover(
            &self.global_write_gate_contended_observers,
            "global_write_gate_contended_observers",
        )
        .drain(..)
        {
            let _ = observer.send(());
        }
    }

    pub fn has_project_db(&self) -> bool {
        self.project_db
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    pub fn activate_project_db(&self, db_path: PathBuf) -> Result<bool, String> {
        let state = ProjectDbState::open(
            db_path,
            configured_memory_read_pool_size(),
            &self.schema_migration,
        )
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
        self.with_path_store_with_label(db_path, path_store_label(db_path), f)
    }

    pub fn with_path_store_read<T>(
        &self,
        db_path: &Path,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        // tachi#1569 (cross-vendor review): this route takes an arbitrary
        // path and has no caller-declared role for it, so the label is
        // *inferred* from the directory name and must stay diagnostic-only.
        // Promoting a guess to identity would make any database that happens
        // to sit in a directory called `wiki` filter its own rows out of
        // reads — a false positive that deletes data from a caller's view
        // instead of merely leaking, and one nobody would think to look for.
        self.with_path_store_read_labelled(db_path, StoreLabel::inferred(db_path), f)
    }

    /// Shared body of [`Self::with_path_store_read`] and
    /// [`Self::with_path_store_read_with_label`]; the two differ only in
    /// whether the label is authoritative enough to become the handle's
    /// `db_label` (see [`StoreLabel`]).
    fn with_path_store_read_labelled<T>(
        &self,
        db_path: &Path,
        label: StoreLabel<'_>,
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
            return state.read_pool.with_store(label.text(), f);
        }

        let _gate = read_or_recover(&self.global_rw_gate, "path_db_read_gate");
        let mut store = open_read_store(&key, label)?;
        f(&mut store)
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

    /// Read a DB whose manifest role the caller has already resolved (the
    /// global store, or a `validate_named_project`-checked project name whose
    /// path was derived *from* that name). Only such a declared label becomes
    /// the handle's `db_label` — see [`StoreLabel`].
    pub fn with_path_store_read_with_label<T>(
        &self,
        db_path: &Path,
        label: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.with_path_store_read_labelled(db_path, StoreLabel::Declared(label), f)
    }

    /// Open one read-only store for a request that targets a currently
    /// unattached path. The returned session owns no runtime lock or pool
    /// checkout, so retaining it across an async await cannot block writers.
    ///
    /// A cached attached path deliberately returns `None`: its established
    /// read-pool routing and LRU touch behavior remain unchanged. The direct
    /// branch only validates an existing DB and opens it read-only; it never
    /// initializes, migrates, or attaches the path.
    ///
    /// `label` is a *declared* role (see [`StoreLabel`]): its one caller
    /// resolved the path from a validated named project.
    pub fn open_unattached_path_store_read_session_with_label(
        &self,
        db_path: &Path,
        label: &str,
    ) -> Result<Option<RequestScopedReadStore>, String> {
        let key = project_db_read_cache_key(db_path)?;
        let attached = self
            .attached_project_dbs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .map(|entry| entry.touch())
            .is_some();
        if attached {
            return Ok(None);
        }

        // This gate protects the open itself just as the existing unattached
        // read closure does. It is dropped before the request session escapes.
        let store = {
            let _gate = read_or_recover(&self.global_rw_gate, "path_db_read_gate");
            open_read_store(&key, StoreLabel::Declared(label))?
        };
        Ok(Some(RequestScopedReadStore { store }))
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

        let migration = self.named_project_write_migration_authority(&key);
        let state =
            ProjectDbState::open(key.clone(), configured_memory_read_pool_size(), &migration)?;
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

    /// Dynamic named-project attachment is a write boundary. Permit exactly
    /// the v22-to-v23 guard migration when the process otherwise carries
    /// `Deny`; no other historical or future schema transition gains ambient
    /// authority. The resulting state is cached, so this authority is used at
    /// most once per attached library and never escapes as a raw connection.
    fn named_project_write_migration_authority(&self, db_path: &Path) -> MigrationAuthority {
        if !matches!(self.schema_migration, MigrationAuthority::Deny) {
            return self.schema_migration.clone();
        }
        if memcore::db::migrations::EXPECTED_SCHEMA_VERSION == 23
            && matches!(
                memcore::db::migrations::read_schema_version_at_path(db_path),
                Ok(22)
            )
        {
            return MigrationAuthority::Allow {
                approved_by: "runtime:named-project-write:v23-evidence-guards".to_string(),
            };
        }
        MigrationAuthority::Deny
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
        let Some(recorder) = self.global_contention_recorder.get().cloned() else {
            let _gate = write_or_recover(&self.global_rw_gate, "global_rw_gate");
            let mut store = lock_or_recover(&self.global_store, "global_store");
            return f(&mut store);
        };

        let gate_wait_started = Instant::now();
        let mut gate_wait = Duration::ZERO;
        let mut gate_hold_started = None;
        let mut gate_hold = Duration::ZERO;
        let mut resource_wait = Duration::ZERO;
        let mut resource_hold_started = None;
        let mut resource_hold = Duration::ZERO;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            #[cfg(test)]
            self.notify_global_write_gate_contention_for_test();
            let _gate = write_or_recover(&self.global_rw_gate, "global_rw_gate");
            gate_wait = gate_wait_started.elapsed();
            gate_hold_started = Some(Instant::now());
            let resource_wait_started = Instant::now();
            let mut store = lock_or_recover(&self.global_store, "global_store");
            resource_wait = resource_wait_started.elapsed();
            resource_hold_started = Some(Instant::now());
            let result = f(&mut store);
            resource_hold = resource_hold_started
                .expect("resource hold timer set before measured write")
                .elapsed();
            drop(store);
            gate_hold = gate_hold_started
                .expect("gate hold timer set after measured write admission")
                .elapsed();
            drop(_gate);
            result
        }));
        if outcome.is_err() {
            resource_hold = resource_hold_started.map_or(Duration::ZERO, |start| start.elapsed());
            gate_hold = gate_hold_started.map_or(Duration::ZERO, |start| start.elapsed());
        }
        let completed = matches!(&outcome, Ok(Ok(_)));
        recorder.record(DbContentionReceipt {
            phase: "db_runtime",
            action: "write",
            resource: "global_store",
            mode: "exclusive",
            gate: "global_rw_gate",
            gate_wait,
            resource_wait,
            resource_hold,
            gate_hold,
            completed,
        });
        match outcome {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    fn with_global_store_read_instrumented<T>(
        &self,
        recorder: Arc<DbContentionRecorder>,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> (Result<T, String>, ReadPoolCheckoutReceipt) {
        let gate_wait_started = Instant::now();
        let mut gate_wait = Duration::ZERO;
        let mut gate_hold_started = None;
        let mut gate_hold = Duration::ZERO;
        let mut pool_wait_on_panic = None;
        let mut resource_hold_on_panic = None;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _gate = read_or_recover(&self.global_rw_gate, "global_rw_gate");
            gate_wait = gate_wait_started.elapsed();
            gate_hold_started = Some(Instant::now());
            let pool_wait_started = Instant::now();
            let result = self
                .global_read_pool
                .with_store_recording("global_read_pool", |store| {
                    pool_wait_on_panic = Some(pool_wait_started.elapsed());
                    let _hold_timer = ElapsedOnDrop {
                        started: Instant::now(),
                        elapsed: &mut resource_hold_on_panic,
                    };
                    f(store)
                });
            gate_hold = gate_hold_started
                .expect("gate hold timer set after measured read admission")
                .elapsed();
            drop(_gate);
            result
        }));
        if outcome.is_err() {
            gate_hold = gate_hold_started.map_or(Duration::ZERO, |start| start.elapsed());
        }
        let (resource_wait, resource_hold, completed) = match &outcome {
            Ok((result, receipt)) => (
                receipt.pool_checkout_wait,
                receipt.operation_wall_time,
                result.is_ok(),
            ),
            Err(_) => (
                pool_wait_on_panic.unwrap_or(Duration::ZERO),
                resource_hold_on_panic.unwrap_or(Duration::ZERO),
                false,
            ),
        };
        recorder.record(DbContentionReceipt {
            phase: "db_runtime",
            action: "read",
            resource: "global_read_pool",
            mode: "shared",
            gate: "global_rw_gate",
            gate_wait,
            resource_wait,
            resource_hold,
            gate_hold,
            completed,
        });
        match outcome {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    pub fn with_global_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let Some(recorder) = self.global_contention_recorder.get().cloned() else {
            let _gate = read_or_recover(&self.global_rw_gate, "global_rw_gate");
            return self.global_read_pool.with_store("global_read_pool", f);
        };

        self.with_global_store_read_instrumented(recorder, f).0
    }

    /// Recording twin of [`Self::with_global_store_read`]: identical
    /// read-gate + pool-checkout semantics, additionally returning the
    /// [`ReadPoolCheckoutReceipt`] so a sampled recall path (#1125) can carry
    /// a MEASURED `pool_checkout_wait` instead of leaving it
    /// `LayerAvailability::Unavailable`. The plain `with_global_store_read`
    /// remains the production path and is unchanged — the receipt is observed
    /// only when a caller opts into this twin (no new cost on the unsampled
    /// path). No pooling/locking semantics change: this delegates to
    /// `ReadStorePool::with_store_recording`, which shares its checkout loop
    /// with `with_store` (`ReadStorePool::checkout`) — the plain variant's
    /// `Instant::now` calls are gated off entirely (`record: false`), so it
    /// pays none of this timing cost.
    pub fn with_global_store_read_recording<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<(T, ReadPoolCheckoutReceipt), String> {
        let Some(recorder) = self.global_contention_recorder.get().cloned() else {
            let _gate = read_or_recover(&self.global_rw_gate, "global_rw_gate");
            let (result, receipt) = self
                .global_read_pool
                .with_store_recording("global_read_pool", f);
            return Ok((result?, receipt));
        };

        let (result, receipt) = self.with_global_store_read_instrumented(recorder, f);
        Ok((result?, receipt))
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
    if project_db_leaf_exists_without_symlink(db_path)? {
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
    if !project_db_leaf_exists_without_symlink(db_path)? {
        return Err(format!("project db does not exist: {}", db_path.display()));
    }
    std::fs::canonicalize(db_path)
        .map_err(|e| format!("canonicalize project db {}: {e}", db_path.display()))
}

fn project_db_leaf_exists_without_symlink(db_path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(db_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "project DB path {} must not be a symlink",
            db_path.display()
        )),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "inspect project DB path {}: {error}",
            db_path.display()
        )),
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
    /// Open a project DB with an explicit #1119 [`MigrationAuthority`]. A
    /// project DB is always opened with [`OpenIntent::OpenExisting`] — it is
    /// operational data, never a fresh-provisioning target here. Authority is
    /// normally the deploy-time value; dynamic named-project write attachment
    /// may instead pass the exact v22-to-v23 guard-migration authority above.
    ///
    /// # Known narrow face: the label here is inferred, not resolved
    ///
    /// `project_label` below is the parent directory's name. That predates
    /// tachi#1569 and is already the identity the **write** path enforces
    /// against (`path_router::validate_path_for_db`), so a DB placed at
    /// `.../wiki/memory.db` has always been allowed to accept `/wiki` writes.
    /// #1569 gives the read pool the *same* label, so reads and writes agree —
    /// which also means a false positive here now over-filters reads instead
    /// of only over-permitting writes.
    ///
    /// It is left inferred deliberately. This function receives a `PathBuf`
    /// and a `MigrationAuthority`; the authoritative mapping (manifest role /
    /// validated named project) lives in `tachi-server` and is not passed in,
    /// and both ways of plumbing it here are worse than the current rule:
    /// taking the label from whichever caller happens to create the cached
    /// state first is order-dependent and fails *open* (a wiki read that lost
    /// the race would silently skip the gate), and defaulting the unlabelled
    /// callers to `unknown` would change write-side path routing for stores
    /// that rely on today's derivation. Under the shipped layout the two
    /// agree anyway: attached paths are `~/.tachi/projects/<validated
    /// name>/memory.db` (`path_utils::alias::plan_c_global_db_path_in_home`),
    /// so parent-directory == project name. Giving a database a real
    /// self-describing identity is its own issue.
    pub fn open(
        db_path: PathBuf,
        read_pool_size: usize,
        migration: &MigrationAuthority,
    ) -> Result<Self, String> {
        project_db_leaf_exists_without_symlink(&db_path)?;
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
        let ctx = DbOpenContext {
            intent: OpenIntent::OpenExisting,
            migration: migration.clone(),
        };
        let store = MemoryStore::open_with_label_and_context(db_str, &project_label, &ctx)
            .map_err(|e| format!("open project db: {e}"))?;
        let vec_available = store.vec_available;
        let read_pool = ReadStorePool::open_read_only(db_str, read_pool_size, &project_label)
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

#[cfg(feature = "test-support")]
#[derive(Clone)]
struct ReadStoreOpenObserver {
    db_path: PathBuf,
    count: Arc<AtomicUsize>,
}

#[cfg(feature = "test-support")]
/// Test instrumentation for physical direct read-store opens at one canonical
/// DB path. Pool construction does not use this path; the observer therefore
/// counts precisely the one-off opens used by unattached path reads.
#[doc(hidden)]
pub struct ReadStoreOpenObservation {
    observer: ReadStoreOpenObserver,
}

#[cfg(feature = "test-support")]
impl ReadStoreOpenObservation {
    pub fn count(&self) -> usize {
        self.observer.count.load(Ordering::SeqCst)
    }
}

#[cfg(feature = "test-support")]
impl Drop for ReadStoreOpenObservation {
    fn drop(&mut self) {
        let mut guard = lock_or_recover(read_store_open_observer(), "read_store_open_observer");
        if guard
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(&active.count, &self.observer.count))
        {
            *guard = None;
        }
    }
}

#[cfg(feature = "test-support")]
/// Install one path-filtered observer for a targeted test. The observation is
/// process-global because direct unattached opens do not carry `DbRuntime`
/// state; callers must serialize tests that observe the same process.
#[doc(hidden)]
pub fn observe_read_store_opens_for_test(
    db_path: &Path,
) -> Result<ReadStoreOpenObservation, String> {
    let db_path = std::fs::canonicalize(db_path).map_err(|error| {
        format!(
            "canonicalize observed read store {}: {error}",
            db_path.display()
        )
    })?;
    let observer = ReadStoreOpenObserver {
        db_path,
        count: Arc::new(AtomicUsize::new(0)),
    };
    let mut guard = lock_or_recover(read_store_open_observer(), "read_store_open_observer");
    if guard.is_some() {
        return Err("a read-store open observation is already active".to_string());
    }
    *guard = Some(observer.clone());
    Ok(ReadStoreOpenObservation { observer })
}

#[cfg(feature = "test-support")]
fn read_store_open_observer() -> &'static StdMutex<Option<ReadStoreOpenObserver>> {
    static OBSERVER: OnceLock<StdMutex<Option<ReadStoreOpenObserver>>> = OnceLock::new();
    OBSERVER.get_or_init(|| StdMutex::new(None))
}

#[cfg(feature = "test-support")]
fn record_read_store_open(db_path: &Path) {
    let observer = lock_or_recover(read_store_open_observer(), "read_store_open_observer").clone();
    if observer
        .as_ref()
        .is_some_and(|observer| observer.db_path == db_path)
    {
        observer
            .expect("observer checked above")
            .count
            .fetch_add(1, Ordering::SeqCst);
    }
}

/// Name for a DB addressed by path: the directory that holds it
/// (`.../wiki/memory.db` -> `wiki`), the same rule `ProjectDbState::open`
/// uses. This is a *guess* about the store's role, fit for diagnostics; see
/// [`StoreLabel`] for why it must not become the store's identity.
fn path_store_label(db_path: &Path) -> &str {
    db_path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|os| os.to_str())
        .unwrap_or("path")
}

/// How much authority a caller has over a store's manifest identity
/// (tachi#1569, cross-vendor review).
///
/// Only a caller that *resolved* the store's role may confer it. A name read
/// off the filesystem is evidence about a directory, not about a database:
/// treating it as identity means an unrelated DB under a directory called
/// `wiki` silently starts filtering wiki-internal rows out of its own reads.
/// A leak is visible to whoever reads the output; over-filtering is not
/// visible to anyone.
#[derive(Clone, Copy)]
enum StoreLabel<'a> {
    /// The caller knows this store's manifest role: the global store, or a
    /// named project whose path was derived from its validated name. Becomes
    /// the handle's `db_label`.
    Declared(&'a str),
    /// Guessed from the path. Used for lock names and error text only; the
    /// handle stays unlabelled, exactly as every read store was before
    /// tachi#1569.
    Inferred(&'a str),
}

impl<'a> StoreLabel<'a> {
    fn inferred(db_path: &'a Path) -> Self {
        Self::Inferred(path_store_label(db_path))
    }

    /// The human-facing name, whatever its authority.
    fn text(self) -> &'a str {
        match self {
            Self::Declared(label) | Self::Inferred(label) => label,
        }
    }

    /// The label that may become `MemoryStore::db_label`.
    fn identity(self) -> &'a str {
        match self {
            Self::Declared(label) => label,
            Self::Inferred(_) => memcore::path_router::UNKNOWN_DB_LABEL,
        }
    }
}

/// Open one read-only store. The label names the store in error text; only
/// its [`StoreLabel::identity`] half becomes the handle's `db_label`, so
/// read-side identity predicates (`is_wiki_corpus_store`) see the same
/// identity the write path uses for the same file — and see *nothing* when
/// the caller was only guessing.
fn open_read_store(db_path: &Path, label: StoreLabel<'_>) -> Result<MemoryStore, String> {
    let name = label.text();
    let db_str = db_path.to_str().ok_or_else(|| {
        format!(
            "{} DB path contains invalid UTF-8: {}",
            name,
            db_path.display()
        )
    })?;
    let store = MemoryStore::open_read_only_with_label(db_str, label.identity())
        .map_err(|e| format!("open {name} read store: {e}"))?;
    #[cfg(feature = "test-support")]
    record_read_store_open(db_path);
    Ok(store)
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
            global_read_pool: ReadStorePool::open_read_only(global_db_str, 1, "global")
                .expect("global read pool"),
            global_rw_gate: Arc::new(StdRwLock::new(())),
            global_contention_recorder: Arc::new(OnceLock::new()),
            global_write_gate_contended_observers: Arc::new(StdMutex::new(Vec::new())),
            global_db_path: Arc::new(global_db),
            global_vec_available: false,
            project_db: Arc::new(StdRwLock::new(None)),
            attached_project_dbs: Arc::new(StdRwLock::new(HashMap::new())),
            project_attach_init_gate: Arc::new(StdMutex::new(())),
            schema_migration: MigrationAuthority::Deny,
        }
    }

    #[cfg(unix)]
    fn test_file_identity(path: &Path) -> (u64, u64) {
        use std::os::unix::fs::MetadataExt;

        let metadata = std::fs::symlink_metadata(path).expect("path metadata");
        (metadata.dev(), metadata.ino())
    }

    #[cfg(unix)]
    fn assert_project_symlink_refused_for_runtime_read_and_write(kind: &str) {
        let temp = unique_temp_dir(&format!("project-symlink-{kind}"));
        let global_db = temp.join("global.db");
        drop(
            MemoryStore::open_with_label(global_db.to_str().expect("global path"), "global")
                .expect("seed global DB"),
        );
        let runtime = test_runtime(global_db);
        let project_link = temp.join("project.db");
        let external_db = temp.join("external.db");
        let external_before = if kind == "wrong" {
            drop(
                MemoryStore::open_with_label(
                    external_db.to_str().expect("external path"),
                    "foreign",
                )
                .expect("seed foreign DB"),
            );
            Some((
                test_file_identity(&external_db),
                std::fs::read(&external_db).expect("foreign bytes"),
            ))
        } else {
            None
        };
        let target = if kind == "loop" {
            project_link.clone()
        } else {
            external_db.clone()
        };
        std::os::unix::fs::symlink(&target, &project_link).expect("project symlink");
        let link_identity = test_file_identity(&project_link);

        let read_result = runtime.with_path_store_read(&project_link, |_store| Ok(()));
        let write_result = runtime.with_path_store(&project_link, |_store| Ok(()));

        for (operation, result) in [("read", read_result), ("write", write_result)] {
            let error = match result {
                Err(error) => error,
                Ok(()) => panic!("project {kind} symlink must refuse runtime {operation}"),
            };
            assert!(
                error.contains("project DB path") && error.contains("must not be a symlink"),
                "expected project leaf refusal for {operation}, got: {error}"
            );
        }
        assert_eq!(test_file_identity(&project_link), link_identity);
        assert_eq!(std::fs::read_link(&project_link).unwrap(), target);
        if let Some((identity, bytes)) = external_before {
            assert_eq!(test_file_identity(&external_db), identity);
            assert_eq!(std::fs::read(&external_db).unwrap(), bytes);
        } else if kind == "dangling" {
            assert!(std::fs::symlink_metadata(&external_db).is_err());
        }
    }

    #[test]
    #[cfg(unix)]
    fn runtime_project_wrong_target_symlink_refuses_read_and_write() {
        assert_project_symlink_refused_for_runtime_read_and_write("wrong");
    }

    #[test]
    #[cfg(unix)]
    fn runtime_project_dangling_symlink_refuses_read_and_write() {
        assert_project_symlink_refused_for_runtime_read_and_write("dangling");
    }

    #[test]
    #[cfg(unix)]
    fn runtime_project_symlink_loop_refuses_read_and_write() {
        assert_project_symlink_refused_for_runtime_read_and_write("loop");
    }

    #[test]
    #[cfg(unix)]
    fn runtime_global_symlink_remains_supported() {
        let temp = unique_temp_dir("global-symlink");
        let external_db = temp.join("external-global.db");
        drop(
            MemoryStore::open_with_label(
                external_db.to_str().expect("external global path"),
                "global",
            )
            .expect("seed external global DB"),
        );
        let global_link = temp.join("global.db");
        std::os::unix::fs::symlink(&external_db, &global_link).expect("global symlink");

        let runtime = test_runtime(global_link);

        runtime
            .with_global_store_read(|_store| Ok(()))
            .expect("global symlink read remains supported");
        runtime
            .with_global_store(|_store| Ok(()))
            .expect("global symlink write remains supported");
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
        let pool = ReadStorePool::open_read_only(&db_str, 4, "test-pool").expect("open pool");

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

    /// codex-m56e0 BUG-1 / codex-o964f R3-1 / codex-s305b R5 / codex-qa111
    /// R4-2: a panicking checkout closure must not leave another
    /// already-parked checkout blocked forever.
    ///
    /// This is a **behavioral-regression safety net, not a discriminator for
    /// the unlock-before-notify ordering** (codex-s305b R5 retired that role
    /// for any black-box test, and this comment previously overclaimed it —
    /// don't reintroduce that claim). The ordering is now structural:
    /// `SlotCheckout::drop` performs "unlock, then notify" as two fixed
    /// statements in one function body (see its doc comment), so there is no
    /// longer any call-site binding order for a dynamic test to catch a
    /// regression in — that property is now auditable by reading one
    /// function, not by timing a race. What this test still usefully proves
    /// is the outward behavior: a panic inside the checked-out closure must
    /// still wake a waiter parked on the same slot. If some future change
    /// reintroduced a second, separately-notifying object, or dropped the
    /// notify call entirely, this test would catch that class of regression
    /// — it just can no longer speak to *ordering* specifically, and must
    /// not claim to.
    ///
    /// The causal chain below (codex-qa111 R4-2 — no sleeping, no "signals
    /// probably far enough along" guessing) still governs how the test
    /// itself is wired, because "is the waiter actually parked before we
    /// panic?" remains something a black-box test must prove causally rather
    /// than assume:
    /// 1. Pool size 1. The panicker's `entered` signal proves it holds the
    ///    sole slot before we do anything else.
    /// 2. We register a parked-observer (`observe_next_parked_checkout_for_test`,
    ///    R4-1) *before* spawning the waiter — so there is no window in
    ///    which the waiter could reach the wait point before we're listening
    ///    for it.
    /// 3. We spawn the waiter. Its `with_store` call is against a pool whose
    ///    sole slot is provably still held (step 1), so it is guaranteed to
    ///    scan, find the slot busy, and call `notify_parked_observers_for_test`
    ///    immediately before `wait_while` — there is no other path.
    /// 4. We block on the parked-observer channel (bounded `recv_timeout`).
    ///    Receiving it is proof the waiter is now *actually inside*
    ///    `wait_while`, not merely "checkout in flight" — a timeout here
    ///    means our own test scaffolding is broken (thread never scheduled,
    ///    channel wiring wrong, etc.), not the bug under test, so it fails
    ///    as a hard test-infrastructure assertion.
    /// 5. Only now do we trigger the panic. The panic is caught with
    ///    `catch_unwind` inside the panicker thread so it is contained and
    ///    inspectable rather than just failing that thread silently.
    /// 6. The waiter's result is collected via a bounded `recv_timeout`
    ///    (never a bare `.join()`, since on unfixed code the waiter thread
    ///    can hang forever and a bare join would hang this test right along
    ///    with it) — *this* is the actual assertion about the bug under
    ///    test. The waiter's `JoinHandle` is intentionally dropped, not
    ///    joined, for the same reason.
    ///
    /// Step 4 makes "parked" a proven fact rather than a probability (unlike
    /// the prior revision's `started`-signal proxy, which only proved the
    /// checkout was in flight, not that it had reached the wait). `TRIALS`
    /// repeats are kept as cheap margin against unrelated scheduling
    /// flakiness in this regression net — not, as an earlier revision of
    /// this comment claimed, to accumulate statistical power against a race:
    /// per the note above, there is no longer a race in the production code
    /// for this test to catch, so there is nothing left to accumulate power
    /// against.
    #[test]
    fn panicking_checkout_still_wakes_a_waiter_parked_on_the_same_slot() {
        const TRIALS: usize = 3;
        for trial in 0..TRIALS {
            run_panicking_checkout_wakeup_trial(trial);
        }
    }

    fn run_panicking_checkout_wakeup_trial(trial: usize) {
        let temp = unique_temp_dir(&format!("read-pool-panic-wakeup-{trial}"));
        let db_path = temp.join("memory.db");
        drop(
            MemoryStore::open_with_label(db_path.to_str().expect("db path utf8"), "seed")
                .expect("seed db"),
        );
        let db_str = db_path.to_str().expect("db path utf8").to_string();
        let pool = ReadStorePool::open_read_only(&db_str, 1, "test-pool").expect("open pool");

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
                     (codex-m56e0 BUG-1 / codex-o964f R3-1 regression, trial {trial})"
                );
            };
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                panicker_pool.with_store("panicker", checkout)
            }))
            .is_err()
        });
        panicker_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_else(|e| {
                panic!("trial {trial}: panicker should hold the sole slot before we proceed: {e}")
            });

        // Register BEFORE spawning the waiter (step 2): closes the window in
        // which the waiter could reach the wait point before we're listening.
        let parked_rx = pool.observe_next_parked_checkout_for_test();

        // Not joined (see doc comment): on a stalled trial this thread may
        // never finish. Its result reaches us only through this channel,
        // collected below with a bounded `recv_timeout`.
        let (waiter_result_tx, waiter_result_rx) = std::sync::mpsc::channel();
        let waiter_pool = pool.clone();
        let _waiter = std::thread::spawn(move || {
            let result = waiter_pool.with_store("waiter", |_store| Ok(()));
            let _ = waiter_result_tx.send(result.is_ok());
        });

        // Step 4: this is a test-infrastructure assertion, not the bug under
        // test — the waiter is guaranteed to reach `wait_while` (the sole
        // slot is provably still held), so a timeout here means our own
        // scaffolding broke, not the fix.
        parked_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap_or_else(|e| {
                panic!(
                    "trial {trial}: test infrastructure failure — waiter never reached \
                 wait_while (parked-observer channel): {e}"
                )
            });

        // Trigger the panic: the sole slot's only holder panics instead of
        // returning normally. The panicker thread itself is short-lived and
        // deterministic from this point (wake -> panic -> caught), so
        // joining it below is safe and never blocks on the bug under test.
        {
            let (proceed, wake) = &*panicker_gate;
            *proceed.lock().expect("lock panicker gate") = true;
            wake.notify_all();
        }

        assert!(
            panicker
                .join()
                .unwrap_or_else(|e| panic!("trial {trial}: panicker thread should join: {e:?}")),
            "trial {trial}: panicker checkout should have panicked as intended"
        );

        // This is the actual assertion about the bug under test: the waiter
        // was proven parked in `wait_while` (step 4) before the panic fired,
        // so it must now complete.
        let waiter_completed = waiter_result_rx.recv_timeout(Duration::from_secs(3));
        assert!(
            waiter_completed.is_ok(),
            "trial {trial}: a panicking checkout closure must still wake a waiter parked on \
             the same (sole) slot — otherwise the waiter is left blocked forever \
             (recv_timeout expired waiting for its result)"
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    /// kckylechen1/tachi#1125 acceptance 1 — the busy-slot test.
    ///
    /// A production recall that goes through the recording checkout must carry
    /// a REAL, measured `pool_checkout_wait`, not an admission of ignorance.
    /// `test_runtime` builds a `DbRuntime` whose `global_read_pool` has
    /// exactly ONE slot. Thread A checks that sole slot out via
    /// `with_global_store_read_recording` (the #1125 wiring the search recall
    /// path uses) and holds it on a release gate; thread B then attempts the
    /// same recording checkout and is provably parked in `wait_while` (via the
    /// `observe_next_parked_checkout_for_test` causal observation point) before
    /// A releases. B's returned [`ReadPoolCheckoutReceipt`] must therefore
    /// show a strictly-positive `pool_checkout_wait`.
    ///
    /// **What this discriminates:** the `Instant::now` that seeds
    /// `checkout_started` inside `ReadStorePool::checkout` (the shared
    /// checkout loop `with_store_recording` — and so
    /// `with_global_store_read_recording` — delegates to with `record: true`).
    /// Stubbing the mapped wait to zero — the type-preserving mutation
    /// `checkout_started.map(|s| s.elapsed())` →
    /// `checkout_started.map(|_| Duration::ZERO)` (a bare `Duration::ZERO`
    /// would not compile: the value stays `Option<Duration>` through the
    /// `.expect()` downstream) — turns the final assertion red even though B
    /// genuinely waited — that is
    /// exactly the mutation the build seat re-checks. `>= ZERO` is
    /// deliberately NOT used (it is a tautology and would not catch the
    /// mutation); the parked-observer handshake is what makes the wait a proven
    /// fact rather than a timing probability, so `> ZERO` is sound here.
    #[test]
    fn recording_checkout_reports_positive_pool_wait_under_contention() {
        let temp = unique_temp_dir("recording-pool-wait");
        let global_db = temp.join("global/memory.db");
        std::fs::create_dir_all(global_db.parent().expect("global parent")).expect("global dir");
        let runtime = Arc::new(test_runtime(global_db));
        let recorder = runtime.enable_global_contention_receipts();

        // Pool size is 1 (test_runtime). Thread A holds the sole slot on a
        // release gate so it deterministically stays checked out until we let
        // it go — B cannot find an idle slot and must park.
        let release = Arc::new((StdMutex::new(false), std::sync::Condvar::new()));
        let (occupier_entered_tx, occupier_entered_rx) = std::sync::mpsc::channel();
        let occupier_runtime = Arc::clone(&runtime);
        let occupier_release = Arc::clone(&release);
        let occupier = std::thread::spawn(move || {
            occupier_runtime
                .with_global_store_read_recording(|_store| {
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
                .expect("occupier recording checkout")
        });
        occupier_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("occupier should hold the sole slot before we proceed");

        // Register the parked observer BEFORE spawning B (closes the window in
        // which B could reach the wait before we are listening — same causal
        // discipline as the panic-wakeup test).
        let parked_rx = runtime
            .global_read_pool
            .observe_next_parked_checkout_for_test();

        let waiter_runtime = Arc::clone(&runtime);
        let (waiter_receipt_tx, waiter_receipt_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let result = waiter_runtime.with_global_store_read_recording(|_store| Ok(()));
            // Send the receipt (or a failure marker) regardless of outcome so
            // the main thread's recv_timeout can never hang on a panicked
            // worker masquerading as a stuck checkout.
            let payload = result.map(|(_, receipt)| receipt.pool_checkout_wait);
            let _ = waiter_receipt_tx.send(payload);
        });

        // Prove B is genuinely parked in wait_while before we release A —
        // without this, a scheduler delay could let the assertion observe a
        // B that has not actually waited yet.
        parked_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("test infrastructure: waiter never reached wait_while");

        // Release A: B's parked checkout wakes, finds the now-free slot, and
        // its `pool_checkout_wait` reflects the time it actually spent
        // blocked.
        {
            let (released, wake) = &*release;
            *released.lock().expect("lock release gate") = true;
            wake.notify_all();
        }
        occupier.join().expect("occupier thread should join");

        let waiter_wait = waiter_receipt_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("waiter should report its receipt within the bound")
            .expect("waiter recording checkout should succeed");
        // The acceptance assertion: a measured wait that is strictly positive
        // because B was PROVEN parked. Mutation target = the
        // `checkout_started`/`elapsed()` timer in the shared
        // `ReadStorePool::checkout` loop (record: true path).
        assert!(
            waiter_wait > Duration::ZERO,
            "contended recording checkout must report a strictly-positive \
             pool_checkout_wait (B was proven parked); got {waiter_wait:?}"
        );

        let contention = recorder.drain();
        assert_eq!(contention.dropped_samples, 0);
        assert!(
            contention.receipts.iter().any(|receipt| {
                receipt.phase == "db_runtime"
                    && receipt.action == "read"
                    && receipt.resource == "global_read_pool"
                    && receipt.mode == "shared"
                    && receipt.gate == "global_rw_gate"
                    && receipt.resource_wait > Duration::ZERO
                    && receipt.resource_hold > Duration::ZERO
                    && receipt.gate_hold >= receipt.resource_hold
                    && receipt.completed
            }),
            "the real pooled reader that was proven parked must emit a complete \
             global_read_pool timing receipt; receipts={contention:?}"
        );

        waiter.join().expect("waiter thread should join");
        let _ = std::fs::remove_dir_all(temp);
    }

    /// #1255: a global write must record the actual `global_rw_gate` wait and
    /// `global_store` hold, rather than inferring contention from an HTTP
    /// handler delay. Writer A holds the real write gate and store mutex on a
    /// condition-variable release; writer B first proves that the same real
    /// gate is busy, then attempts its real DbRuntime write. The assertion is
    /// on the measured DbRuntime receipt, not on a handler delay or scheduler
    /// window.
    ///
    /// Mutation discriminator: replacing the measured `gate_wait` assignment
    /// in `with_global_store` with `Duration::ZERO` makes the strict waiter
    /// assertion below fail while the same writer/reader choreography still
    /// runs. This is intentionally a real gate test, not the transport
    /// harness's server-side `hold_ms` overlap aid.
    #[test]
    fn global_write_contention_receipt_reports_gate_wait_and_store_hold() {
        let temp = unique_temp_dir("global-write-contention-receipt");
        let global_db = temp.join("global/memory.db");
        std::fs::create_dir_all(global_db.parent().expect("global parent")).expect("global dir");
        let runtime = Arc::new(test_runtime(global_db));
        let recorder = runtime.enable_global_contention_receipts();

        let release = Arc::new((StdMutex::new(false), std::sync::Condvar::new()));
        let (holder_entered_tx, holder_entered_rx) = std::sync::mpsc::channel();
        let holder_runtime = Arc::clone(&runtime);
        let holder_release = Arc::clone(&release);
        let holder = std::thread::spawn(move || {
            holder_runtime.with_global_store(|_store| {
                holder_entered_tx
                    .send(())
                    .expect("holder should signal after taking global gate/store");
                let (released, wake) = &*holder_release;
                let mut released = released.lock().expect("lock release gate");
                while !*released {
                    released = wake.wait(released).expect("wait release gate");
                }
                Ok(())
            })
        });
        holder_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("holder must own global_rw_gate before waiter starts");

        let contended_rx = runtime.observe_next_global_write_gate_contention_for_test();
        let (waiter_done_tx, waiter_done_rx) = std::sync::mpsc::channel();
        let waiter_runtime = Arc::clone(&runtime);
        let waiter = std::thread::spawn(move || {
            let _ = waiter_done_tx.send(waiter_runtime.with_global_store(|_store| Ok(())));
        });
        contended_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("waiter must observe the real global_rw_gate held before holder releases");

        {
            let (released, wake) = &*release;
            *released.lock().expect("lock release gate") = true;
            wake.notify_all();
        }
        holder
            .join()
            .expect("holder thread should join")
            .expect("holder DbRuntime write should succeed");
        waiter_done_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("waiter should finish after holder release")
            .expect("waiter DbRuntime write should succeed");
        waiter.join().expect("waiter thread should join");

        let contention = recorder.drain();
        assert_eq!(contention.dropped_samples, 0);
        assert_eq!(
            contention.receipts.len(),
            2,
            "only the holder and waiter writes should be observed; receipts={contention:?}"
        );
        assert!(
            contention.receipts.iter().all(|receipt| {
                receipt.phase == "db_runtime"
                    && receipt.action == "write"
                    && receipt.resource == "global_store"
                    && receipt.mode == "exclusive"
                    && receipt.gate == "global_rw_gate"
                    && receipt.gate_hold >= receipt.resource_hold
                    && receipt.completed
            }),
            "all real global writes must retain their complete labelled timing receipt; \
             receipts={contention:?}"
        );
        assert!(
            contention
                .receipts
                .iter()
                .any(|receipt| receipt.gate_wait > Duration::ZERO),
            "writer B waited behind the holder's real global_rw_gate but receipt lost that wait; \
             receipts={contention:?}"
        );
        assert!(
            contention
                .receipts
                .iter()
                .any(|receipt| receipt.resource_hold > Duration::ZERO),
            "writer A held the real global_store but receipt lost that hold; \
             receipts={contention:?}"
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn measured_global_panics_emit_incomplete_receipts_and_resume_unwind() {
        let temp = unique_temp_dir("global-contention-panic-receipts");
        let global_db = temp.join("global/memory.db");
        std::fs::create_dir_all(global_db.parent().expect("global parent")).expect("global dir");
        let runtime = test_runtime(global_db);
        let recorder = runtime.enable_global_contention_receipts();

        let write_panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.with_global_store::<()>(|_store| panic!("measured write panic"))
        }))
        .expect_err("measured write panic must resume to its caller");
        assert_eq!(
            write_panic.downcast_ref::<&str>(),
            Some(&"measured write panic")
        );
        assert!(
            runtime.global_store.is_poisoned(),
            "instrumented write panic must retain the original mutex-poisoning behavior"
        );

        let read_panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.with_global_store_read::<()>(|_store| panic!("measured read panic"))
        }))
        .expect_err("measured read panic must resume to its caller");
        assert_eq!(
            read_panic.downcast_ref::<&str>(),
            Some(&"measured read panic")
        );
        assert!(
            runtime.global_read_pool.inner.stores[0].is_poisoned(),
            "instrumented read panic must retain the original pool-slot poisoning behavior"
        );

        let recording_read_panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.with_global_store_read_recording::<()>(|_store| {
                panic!("measured recording read panic")
            })
        }))
        .expect_err("measured recording read panic must resume to its caller");
        assert_eq!(
            recording_read_panic.downcast_ref::<&str>(),
            Some(&"measured recording read panic")
        );

        let contention = recorder.drain();
        assert_eq!(contention.dropped_samples, 0);
        assert_eq!(contention.receipts.len(), 3);
        assert!(contention.receipts.iter().all(|receipt| !receipt.completed));
        assert_eq!(
            contention
                .receipts
                .iter()
                .filter(|receipt| receipt.resource == "global_store")
                .count(),
            1
        );
        assert_eq!(
            contention
                .receipts
                .iter()
                .filter(|receipt| receipt.resource == "global_read_pool")
                .count(),
            2
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn contention_recorder_bounds_samples_and_reports_overflow_per_drain() {
        let temp = unique_temp_dir("global-contention-recorder-capacity");
        let global_db = temp.join("global/memory.db");
        std::fs::create_dir_all(global_db.parent().expect("global parent")).expect("global dir");
        let runtime = test_runtime(global_db);
        let recorder = Arc::new(DbContentionRecorder::with_capacity(2));
        assert!(
            runtime
                .global_contention_recorder
                .set(Arc::clone(&recorder))
                .is_ok(),
            "test runtime recorder must be unset"
        );

        for _ in 0..3 {
            runtime
                .with_global_store(|_store| Ok(()))
                .expect("measured write");
        }

        let overflowed = recorder.drain();
        assert_eq!(overflowed.receipts.len(), 2);
        assert_eq!(overflowed.dropped_samples, 1);
        assert!(overflowed.receipts.iter().all(|receipt| receipt.completed));

        let reset = recorder.drain();
        assert!(reset.receipts.is_empty());
        assert_eq!(reset.dropped_samples, 0);

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

    /// tachi#1569 (cross-vendor review, CONCERN 5): a directory name is not a
    /// store identity. An unrelated database that happens to live under a
    /// directory called `wiki` must not start filtering wiki-internal rows out
    /// of its own reads — over-filtering is invisible to the caller, so it is
    /// the worse failure direction. Only a caller that resolved the store's
    /// role (`with_path_store_read_with_label`) confers identity.
    #[test]
    fn an_inferred_directory_name_never_confers_wiki_identity() {
        let temp = unique_temp_dir("inferred-label");
        let global_db = temp.join("global/memory.db");
        // A perfectly ordinary project DB that merely sits in a `wiki` dir.
        let impostor_db = temp.join("wiki/memory.db");
        std::fs::create_dir_all(global_db.parent().expect("global parent")).expect("global dir");
        std::fs::create_dir_all(impostor_db.parent().expect("impostor parent"))
            .expect("impostor dir");
        drop(
            MemoryStore::open_with_label(impostor_db.to_str().expect("impostor utf8"), "seed")
                .expect("seed impostor db"),
        );
        let runtime = test_runtime(global_db);

        runtime
            .with_path_store_read(&impostor_db, |store| {
                assert!(
                    !store.is_wiki_corpus_store(),
                    "a guessed directory name must not make this the wiki corpus"
                );
                Ok(())
            })
            .expect("inferred-label read");

        // The same file, opened by a caller that declares the role, does carry
        // it — that is the half the gate is allowed to trust.
        runtime
            .with_path_store_read_with_label(&impostor_db, "wiki", |store| {
                assert!(
                    store.is_wiki_corpus_store(),
                    "a declared role must reach the store handle"
                );
                Ok(())
            })
            .expect("declared-label read");

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
        let pool =
            ReadStorePool::open_read_only(db_path.to_str().expect("db path utf8"), 4, "test-pool")
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
        //
        // codex-o964f R3-3: a `recv_timeout` failure is either a genuine
        // `Timeout` (the worker is still checking out — convoy evidence) or
        // a `Disconnected` (the worker thread panicked/failed before ever
        // sending, e.g. its `result.expect(...)` below tripped) — these are
        // NOT the same thing and must not share one counter. Only `Timeout`
        // counts as convoy evidence; a `Disconnected` is a worker failure
        // that taints this run's numbers and is reported as such, loudly.
        let temp = unique_temp_dir("bench-convoy");
        let db_path = temp.join("memory.db");
        drop(
            MemoryStore::open_with_label(db_path.to_str().expect("db path utf8"), "seed")
                .expect("seed db"),
        );
        let pool =
            ReadStorePool::open_read_only(db_path.to_str().expect("db path utf8"), 4, "test-pool")
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
                let (result, receipt) = pool.with_store_recording("short_read", |_store| Ok(()));
                result.expect("short read should succeed");
                let _ = receipt_tx.send(receipt);
            });
        }
        drop(receipt_tx);

        const PER_ITEM_TIMEOUT: Duration = Duration::from_secs(2);
        let mut receipts = Vec::with_capacity(SHORT_READERS);
        let mut observed_timeouts = 0usize;
        let mut worker_failures = 0usize;
        for _ in 0..SHORT_READERS {
            match receipt_rx.recv_timeout(PER_ITEM_TIMEOUT) {
                Ok(receipt) => receipts.push(receipt),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => observed_timeouts += 1,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => worker_failures += 1,
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
             observed_timeouts={observed_timeouts}/{SHORT_READERS} \
             worker_failures={worker_failures}/{SHORT_READERS} (only observed_timeouts is \
             convoy evidence on the pre-fix selector, not a test failure; worker_failures \
             means a short-read worker thread failed/panicked before it could send its \
             receipt at all — that is NOT pool-contention evidence)"
        );
        if worker_failures > 0 {
            println!(
                "[bench:one_long_read_with_concurrent_short_reads] WARNING: \
                 {worker_failures} worker failure(s) — this run's p50/p95 above are NOT \
                 reliable evidence; investigate the worker failure before trusting them"
            );
        }

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
                            scored_count: 0,
                            last_access: None,
                            last_use_at: None,
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
