//! Host-injected kernel policy (tachi#1585 D5).
//!
//! `RecallConfig::get()` and its sibling `TACHI_*` env readers are a
//! process-wide `OnceLock`/ambient-env design: correct for a single Tachi
//! daemon, wrong for a portable kernel embedded as a library, where two
//! stores in the same process (e.g. two HyperMemory forks under test) may
//! legitimately want different recall weights with no shared global to fight
//! over.
//!
//! [`KernelPolicy`] is the replacement seam: a small, `Clone`, plain-data
//! bundle a [`crate::MemoryStore`] carries per-instance instead of reaching
//! into ambient state at point of use. `KernelPolicy::default()` is PURE —
//! [`crate::RecallConfig::default()`], never [`crate::RecallConfig::get()`] —
//! so constructing one never touches the environment. Only an adapter layer
//! outside this crate (the tachi-server adapter, for the shipped product)
//! resolves `TACHI_*`/`config.env` once and builds a non-default
//! `KernelPolicy` from that resolution; portable callers construct
//! `KernelPolicy` explicitly with zero env involvement.
//!
//! Attach a non-default policy to an already-open store with
//! [`crate::MemoryStore::with_kernel_policy`].

use std::sync::{Arc, OnceLock};

use crate::recall_config::RecallConfig;
use crate::scorer::{DecayPolicy, DefaultDecayPolicy};

/// Embedding-backfill selection tuning carried on [`KernelPolicy`].
///
/// Replaces the free-standing `TACHI_EMBED_RAW_TIER` env read
/// (`embed_config::embed_raw_tier_enabled`) as the value gated call sites
/// consult. `embed_config::embed_raw_tier_enabled` itself is unchanged and
/// remains the env-resolution helper an adapter may call to populate this
/// field; it is no longer called directly from the gated call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbedPolicy {
    /// Whether raw-tier memories are eligible for gated embedding backfill
    /// paths (`MemoryStore::entries_missing_vectors`,
    /// `db::list_memory_ids_needing_embedding`). Default matches the
    /// historical `TACHI_EMBED_RAW_TIER`-unset behavior: on.
    pub raw_tier_enabled: bool,
}

impl Default for EmbedPolicy {
    fn default() -> Self {
        Self {
            raw_tier_enabled: true,
        }
    }
}

/// Host-injected configuration a [`crate::MemoryStore`] carries per-instance.
///
/// `Default` is the pure default (see module docs): no env, no process-wide
/// state. Two stores built with `KernelPolicy::default()` (or two explicit,
/// differing `KernelPolicy` values) in the same process are fully
/// independent — this is what makes the D7 two-stores-two-configs pin test
/// meaningful.
#[derive(Clone)]
pub struct KernelPolicy {
    /// Recall/ranking weights and thresholds. Was `RecallConfig::get()` at
    /// four hardcoded call sites; those now read this field (directly, or
    /// via `MemoryStore::search`'s `SearchOptions.recall_config` seam, which
    /// still lets a per-call override win — tachi#1585 D5 point 6).
    pub recall: RecallConfig,
    /// Library-specific decay policy hook. Mirrors
    /// `SearchOptions::decay_policy`'s `Arc<dyn DecayPolicy>` shape/default
    /// (`DEFAULT_DECAY_POLICY`) so the same trait object can be shared
    /// between the store-level default and a per-call override.
    pub decay: Arc<dyn DecayPolicy>,
    /// Embedding-backfill raw-tier gate. See [`EmbedPolicy`].
    pub embed: EmbedPolicy,
    /// Test/operator escape hatch that bypasses path-routing validation even
    /// when the store's `path_validation` gate is on.
    ///
    /// Was two independently `TACHI_DISABLE_PATH_VALIDATION`-reading free
    /// functions (`store/open.rs::path_validation_disabled`,
    /// `db/memory_crud.rs::atomic_evidence_path_validation_disabled`) called
    /// at every write. Deduplicated to this single injected bool; default
    /// `false` (no escape — the pure default never disables validation).
    /// The tachi-server adapter mirrors `TACHI_DISABLE_PATH_VALIDATION` into
    /// this field to preserve production behavior.
    pub path_validation_escape_hatch: bool,
}

impl Default for KernelPolicy {
    fn default() -> Self {
        Self {
            recall: RecallConfig::default(),
            decay: Arc::new(DefaultDecayPolicy),
            embed: EmbedPolicy::default(),
            path_validation_escape_hatch: false,
        }
    }
}

/// Resolve `TACHI_DISABLE_PATH_VALIDATION` from the environment.
///
/// tachi#1585 D5: this is the single surviving env read, kept for the
/// tachi-server adapter to populate
/// [`KernelPolicy::path_validation_escape_hatch`] with. It replaces two
/// byte-identical free functions that used to read this same env var
/// independently at every write call
/// (`store/open.rs::path_validation_disabled`,
/// `db/memory_crud.rs::atomic_evidence_path_validation_disabled`) — a
/// portable caller that never calls this now gets the pure default
/// (`false`, validation stays on) with zero env involvement.
///
/// `admin`-gated (kckylechen1/Sigil#1585 review): `pub mod kernel_policy` is
/// unconditional, so this `pub fn` is directly reachable as
/// `memcore::kernel_policy::path_validation_escape_hatch_from_env()` by any
/// portable-kernel embedder via that facade's `pub use memcore::*;` — being
/// "kept for the tachi-server adapter" was a naming convention, not a
/// structural guarantee. Under `not(admin)` this returns the pure default
/// (`false`, validation stays on) with zero env reads.
#[cfg(feature = "admin")]
pub fn path_validation_escape_hatch_from_env() -> bool {
    matches!(
        std::env::var("TACHI_DISABLE_PATH_VALIDATION")
            .ok()
            .as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes")
    )
}

/// See the `admin`-gated `path_validation_escape_hatch_from_env` above: under
/// `not(admin)` this is the entire implementation — no env read, the pure
/// default.
#[cfg(not(feature = "admin"))]
pub fn path_validation_escape_hatch_from_env() -> bool {
    false
}

// ─── Migration-backup retention (TACHI_MIGRATION_BACKUP_RETAIN) ────────────
//
// Not part of `KernelPolicy` above: its one call site
// (`db::schema::retain_recent_migration_backups`) fires mid schema-init,
// before any `MemoryStore` exists to carry a per-instance policy on — there
// is no `self` to thread it through, and the collision boundary with the
// parallel #1585 lane restricts this lane to the config *read* at
// `db/schema.rs:1484-1489`, not the open-funnel call chain that would be
// needed to pass an explicit parameter down from a store. So this knob stays
// process-wide, but changes shape from "read env at every call" to
// "injected once, defaults pure": a `OnceLock<usize>` that defaults to the
// historical literal (3) with zero env reads, settable exactly once by an
// adapter before the first store opens.
static MIGRATION_BACKUP_RETAIN: OnceLock<usize> = OnceLock::new();

/// Inject the migration-backup retention count once, before any store opens.
/// Second and later calls are no-ops (matching `OnceLock` semantics) — this
/// is an adapter-startup knob, not a per-call override.
pub fn set_migration_backup_retain(count: usize) {
    let _ = MIGRATION_BACKUP_RETAIN.set(count.max(1));
}

/// Resolve the migration-backup retention count. Pure default (3, matching
/// the historical `TACHI_MIGRATION_BACKUP_RETAIN`-unset behavior) unless
/// [`set_migration_backup_retain`] was called first.
pub(crate) fn migration_backup_retain_count() -> usize {
    *MIGRATION_BACKUP_RETAIN.get_or_init(|| 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_policy_default_is_pure_recall_default() {
        let policy = KernelPolicy::default();
        assert_eq!(policy.recall, RecallConfig::default());
        assert!(policy.embed.raw_tier_enabled);
        assert!(!policy.path_validation_escape_hatch);
    }

    #[test]
    fn migration_backup_retain_defaults_to_three_without_injection() {
        // NOTE: shares process state with any other test that calls
        // `set_migration_backup_retain`; this crate's test binary runs
        // single-threaded per `cfg(test)` conventions used elsewhere in this
        // module tree, but if a sibling test in this file ever calls the
        // setter first, this assertion would observe that value instead of
        // the pure default. No such sibling exists today.
        assert_eq!(migration_backup_retain_count(), 3);
    }
}

/// Portable-kernel pin (kckylechen1/Sigil#1585 review): under `not(admin)`,
/// `path_validation_escape_hatch_from_env()` must ignore
/// `TACHI_DISABLE_PATH_VALIDATION` entirely and return the pure default. Only
/// runs under `--no-default-features`, which the build seat runs.
#[cfg(all(test, not(feature = "admin")))]
mod portable_tests {
    use super::path_validation_escape_hatch_from_env;

    #[test]
    fn path_validation_escape_hatch_from_env_ignores_env_under_not_admin() {
        std::env::set_var("TACHI_DISABLE_PATH_VALIDATION", "true");
        assert!(
            !path_validation_escape_hatch_from_env(),
            "not(admin) must return the pure default (false) even with the env var set"
        );
        std::env::remove_var("TACHI_DISABLE_PATH_VALIDATION");
    }
}
