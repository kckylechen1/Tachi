//! The provider persistence writer (audit H1).
//!
//! Every provider outcome this client persists (the `vault_key_health`
//! snapshot, the `model_deployment_health` event and the `llm_usage` row) used
//! to pay a full `MemoryStore` open of the vault database per write: process
//! startup lock, schema-init `BEGIN IMMEDIATE`, trigger-inventory validation,
//! migration-marker rewrite and sqlite-vec load, around a write that costs a
//! fraction of a millisecond. This writer keeps one opened handle per client
//! (shared across clones) and reuses it while it provably still addresses the
//! database a fresh open would reach.
//!
//! # When the retained handle is reused
//!
//! A retained handle is used for a write only when all of these hold, checked
//! immediately before the write:
//!
//! * it was admitted less than [`RETAINED_STORE_TTL`] ago, so a handle is
//!   re-opened (and every open-time validation re-run) at least that often;
//! * [`memcore::MemoryStore::verify_opened_physical_db_identity`] still
//!   passes for the client's vault path. Targets without a stable file
//!   identity (Windows) never pass, so there every write keeps its own open;
//! * `PRAGMA schema_version` is unchanged since admission, so any DDL by any
//!   connection (a migration by another binary, an added or replaced trigger)
//!   forces a full open that re-validates the schema and trigger inventory.
//!
//! The identity is checked again after the write. If the path was replaced
//! meanwhile, the handle is dropped — but the committed write is never
//! replayed against whatever the path names then. The deployment and usage
//! rows this writer carries are append-only events with no idempotency key,
//! and a replacement that already includes the commit (a snapshot or backup
//! taken after it) would record the row twice. The write's value is
//! returned as committed, a diagnostic notes that the path identity changed
//! under it, and the next write opens the new identity afresh.
//!
//! Any failed write drops the handle; the next write opens afresh. Opens use
//! the caller's own context and busy budget, exactly as before, and a handle is
//! only admitted after its first write succeeded.
//!
//! Key-health writes on a retained handle hold memcore's process startup
//! ownership for the write
//! ([`memcore::MemoryStore::vault_upsert_key_health_with_startup_ownership`]),
//! and a fresh key-health open keeps open and upsert under one hold, so the
//! #1680 D6 open-then-write guarantee is the same on both paths.
//!
//! # Coalescing
//!
//! Key-health persistence is a full-row snapshot upsert whose counters
//! (`error_count`) are accumulated in memory by
//! [`memcore::vault::health::record_key_outcome_for_generation`] before the
//! snapshot is taken, so the latest snapshot of a key always carries the
//! accumulated counts. Pending snapshots are queued per key and written in
//! enqueue order. A pending snapshot is replaced by a newer one only when both
//! are plain success snapshots with the same evidence and credential
//! generation ([`success_snapshots_merge`]). An auth failure, a rate limit, a
//! cooldown, an error, an unknown outcome or a generation change is never
//! replaced before it has been written.
//!
//! Deployment-health outcomes and usage rows are events, not snapshots, so
//! they are never coalesced: each one is still applied on its own.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use memcore::vault::VaultKeyHealth;

/// Upper bound on how long one opened handle is reused. After this the next
/// write re-opens, which re-runs every open-time validation.
pub(in crate::llm) const RETAINED_STORE_TTL: Duration = Duration::from_secs(30);

struct RetainedStore {
    store: memcore::MemoryStore,
    schema_version: i64,
    admitted_at: Instant,
}

impl RetainedStore {
    /// Admit a freshly opened handle for reuse, or `None` when it cannot be
    /// verified later (no stable physical identity, unreadable schema version)
    /// or is not idle (an open transaction).
    fn admit(store: memcore::MemoryStore, db_path: &Path) -> Option<Self> {
        if !store.connection().is_autocommit() {
            return None;
        }
        store.verify_opened_physical_db_identity(db_path).ok()?;
        let schema_version = schema_version(&store)?;
        Some(Self {
            store,
            schema_version,
            admitted_at: Instant::now(),
        })
    }

    fn still_addresses(&self, db_path: &Path, now: Instant) -> bool {
        now.saturating_duration_since(self.admitted_at) < RETAINED_STORE_TTL
            && self.store.connection().is_autocommit()
            && self
                .store
                .verify_opened_physical_db_identity(db_path)
                .is_ok()
            && schema_version(&self.store) == Some(self.schema_version)
    }
}

fn schema_version(store: &memcore::MemoryStore) -> Option<i64> {
    store
        .connection()
        .query_row("PRAGMA schema_version", [], |row| row.get(0))
        .ok()
}

#[cfg(test)]
struct RetainedPostCommitHook {
    db_path: std::path::PathBuf,
    hook: Box<dyn FnOnce() + Send + 'static>,
}

#[cfg(test)]
static RETAINED_POST_COMMIT_HOOK: Mutex<Option<RetainedPostCommitHook>> = Mutex::new(None);

/// Test seam: run `hook` between the commit of the next retained write on
/// `db_path` and the post-write identity check that follows it, so a test
/// can replace the path in exactly that window. Fires once; dropping the
/// guard uninstalls a hook that never fired. The hook runs while the
/// writer's handle lock is held, so it must not write through this writer.
#[cfg(test)]
#[doc(hidden)]
pub(in crate::llm) struct RetainedPostCommitHookGuard;

#[cfg(test)]
impl Drop for RetainedPostCommitHookGuard {
    fn drop(&mut self) {
        *RETAINED_POST_COMMIT_HOOK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

#[cfg(test)]
pub(in crate::llm) fn install_retained_post_commit_hook_for_tests(
    db_path: &Path,
    hook: impl FnOnce() + Send + 'static,
) -> RetainedPostCommitHookGuard {
    let mut slot = RETAINED_POST_COMMIT_HOOK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(
        slot.is_none(),
        "retained post-commit hook already installed"
    );
    *slot = Some(RetainedPostCommitHook {
        db_path: db_path.to_path_buf(),
        hook: Box::new(hook),
    });
    RetainedPostCommitHookGuard
}

#[cfg(test)]
fn take_retained_post_commit_hook_for(
    db_path: &Path,
) -> Option<Box<dyn FnOnce() + Send + 'static>> {
    let mut slot = RETAINED_POST_COMMIT_HOOK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match slot.as_ref() {
        Some(candidate) if candidate.db_path == db_path => slot.take().map(|taken| taken.hook),
        _ => None,
    }
}

/// How often each path was taken. Test and measurement surface only; nothing
/// reads these to decide anything.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::llm) struct ProviderPersistWriterCounts {
    /// Writes that paid a full `MemoryStore` open.
    pub full_opens: u64,
    /// Writes that reused the retained handle.
    pub retained_writes: u64,
    /// Key-health snapshots replaced by a newer success snapshot of the same
    /// key before they were written.
    pub coalesced_key_health: u64,
}

#[derive(Default)]
pub(in crate::llm) struct ProviderPersistWriter {
    retained: Mutex<Option<RetainedStore>>,
    pending_key_health: Mutex<HashMap<(String, String), VecDeque<VaultKeyHealth>>>,
    full_opens: AtomicU64,
    retained_writes: AtomicU64,
    coalesced_key_health: AtomicU64,
    /// Every key-health snapshot a write landed, in write order.
    #[cfg(test)]
    key_health_writes: Mutex<Vec<VaultKeyHealth>>,
}

impl ProviderPersistWriter {
    #[cfg(test)]
    pub(in crate::llm) fn counts(&self) -> ProviderPersistWriterCounts {
        ProviderPersistWriterCounts {
            full_opens: self.full_opens.load(Ordering::Relaxed),
            retained_writes: self.retained_writes.load(Ordering::Relaxed),
            coalesced_key_health: self.coalesced_key_health.load(Ordering::Relaxed),
        }
    }

    #[cfg(test)]
    pub(in crate::llm) fn note_key_health_written(&self, health: &VaultKeyHealth) {
        self.key_health_writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(health.clone());
    }

    #[cfg(test)]
    pub(in crate::llm) fn key_health_writes(&self) -> Vec<VaultKeyHealth> {
        self.key_health_writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    #[cfg(test)]
    pub(in crate::llm) fn pending_key_health(
        &self,
        logical_name: &str,
        key_id: &str,
    ) -> Vec<VaultKeyHealth> {
        self.pending_key_health
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&(logical_name.to_string(), key_id.to_string()))
            .map(|queue| queue.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Make the retained handle look `by` older, to cross the TTL in a test.
    #[cfg(test)]
    pub(in crate::llm) fn age_retained_for_tests(&self, by: Duration) {
        if let Some(retained) = self
            .retained
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_mut()
        {
            retained.admitted_at = retained
                .admitted_at
                .checked_sub(by)
                .expect("test clock offset fits");
        }
    }

    #[cfg(test)]
    pub(in crate::llm) fn has_retained_store(&self) -> bool {
        self.retained
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }

    /// Run one write against `db_path`, on the retained handle when it is
    /// still valid (see the module header), otherwise through `open_and_write`,
    /// whose handle is retained when its write succeeded.
    ///
    /// The retained-handle lock is held for the whole write, so writes through
    /// one writer never overlap. Blocking; call it off the async runtime.
    pub(in crate::llm) fn write<T, E>(
        &self,
        db_path: &Path,
        write_retained: impl Fn(&memcore::MemoryStore) -> Result<T, E>,
        open_and_write: impl Fn() -> Result<(memcore::MemoryStore, T), E>,
    ) -> Result<T, E> {
        let mut slot = self
            .retained
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(retained) = slot.take() {
            if retained.still_addresses(db_path, Instant::now()) {
                // A failed write returns here and drops the handle.
                let value = write_retained(&retained.store)?;
                #[cfg(test)]
                if let Some(hook) = take_retained_post_commit_hook_for(db_path) {
                    hook();
                }
                if retained
                    .store
                    .verify_opened_physical_db_identity(db_path)
                    .is_ok()
                {
                    self.retained_writes.fetch_add(1, Ordering::Relaxed);
                    // A handle the write left inside a transaction is not
                    // retained. Dropping it is what a per-write open did.
                    if retained.store.connection().is_autocommit() {
                        *slot = Some(retained);
                    }
                    return Ok(value);
                }
                // The path was replaced during the write, so this handle
                // addresses a detached file. The write itself did commit
                // (`write_retained` returned Ok), and the deployment and
                // usage rows this writer carries are append-only events
                // with no idempotency key: replaying against whatever the
                // path names now would record the row twice whenever the
                // replacement already includes this commit (a snapshot or
                // backup taken after it). Keep the committed value, drop
                // the handle, and let the next write open the new identity
                // afresh. Which file readers of the path see is uncertain,
                // never duplicated.
                self.retained_writes.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    "[provider] retained write committed, but the vault path was replaced during it ({}); not replaying against the replacement: the active target is uncertain",
                    db_path.display()
                );
                return Ok(value);
            }
        }
        self.full_opens.fetch_add(1, Ordering::Relaxed);
        let (store, value) = open_and_write()?;
        *slot = RetainedStore::admit(store, db_path);
        Ok(value)
    }

    /// Queue one key-health snapshot for the next background write of its key.
    pub(in crate::llm) fn enqueue_key_health(&self, health: VaultKeyHealth) {
        let mut pending = self
            .pending_key_health
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let queue = pending
            .entry((health.logical_name.clone(), health.key_id.clone()))
            .or_default();
        match queue.back_mut() {
            Some(last) if success_snapshots_merge(last, &health) => {
                *last = health;
                self.coalesced_key_health.fetch_add(1, Ordering::Relaxed);
            }
            _ => queue.push_back(health),
        }
    }

    /// The oldest pending snapshot of this key, or `None` when an earlier
    /// background write already took it (its event was coalesced).
    pub(in crate::llm) fn take_key_health(
        &self,
        logical_name: &str,
        key_id: &str,
    ) -> Option<VaultKeyHealth> {
        let mut pending = self
            .pending_key_health
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let key = (logical_name.to_string(), key_id.to_string());
        let queue = pending.get_mut(&key)?;
        let next = queue.pop_front();
        if queue.is_empty() {
            pending.remove(&key);
        }
        next
    }
}

/// Whether the pending snapshot `pending` may be replaced by `newer` without
/// being written first: both must be plain success snapshots (status ok, no
/// auth failure, not disabled, no cooldown, no error, zero error count) whose
/// evidence (kind, outcome, credential generation and any other metadata) is
/// identical apart from its timestamp. Anything else keeps its own write.
pub(in crate::llm) fn success_snapshots_merge(
    pending: &VaultKeyHealth,
    newer: &VaultKeyHealth,
) -> bool {
    fn plain_success(health: &VaultKeyHealth) -> bool {
        health.status == memcore::vault::health::HEALTH_STATUS_OK
            && !health.auth_failed
            && !health.disabled
            && health.cooldown_until.is_none()
            && health.last_error.is_none()
            && health.error_count == 0
            && evidence(health).is_some_and(|evidence| {
                evidence.get(memcore::vault::health::EVIDENCE_OUTCOME_FIELD)
                    == Some(&serde_json::Value::String(
                        memcore::vault::health::TypedOutcome::Success
                            .as_str()
                            .to_string(),
                    ))
            })
    }
    fn evidence(health: &VaultKeyHealth) -> Option<serde_json::Map<String, serde_json::Value>> {
        match serde_json::from_str::<serde_json::Value>(&health.metadata).ok()? {
            serde_json::Value::Object(mut object) => {
                object.remove(memcore::vault::health::EVIDENCE_AT_FIELD);
                Some(object)
            }
            _ => None,
        }
    }
    pending.logical_name == newer.logical_name
        && pending.key_id == newer.key_id
        && plain_success(pending)
        && plain_success(newer)
        && evidence(pending) == evidence(newer)
}
