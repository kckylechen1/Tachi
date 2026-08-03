//! [`crate::kernel_policy::KernelPolicy`] attachment on [`MemoryStore`]
//! (tachi#1585 D5).
//!
//! Every `MemoryStore` constructor initializes `policy` to
//! [`crate::KernelPolicy::default()`] (pure, no env). This module is the
//! seam a caller uses to attach a non-default, host-injected policy — e.g.
//! the tachi-server adapter, after resolving `TACHI_*`/`config.env` once —
//! without threading a new parameter through every `open*` constructor.

use crate::{KernelPolicy, MemoryStore};

impl MemoryStore {
    /// Attach a host-injected [`KernelPolicy`], replacing the pure default
    /// this store opened with. Consuming-`self` builder so it composes at
    /// the open call site: `MemoryStore::open(path)?.with_kernel_policy(p)`.
    pub fn with_kernel_policy(mut self, policy: KernelPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Read the store's current [`KernelPolicy`].
    pub fn kernel_policy(&self) -> &KernelPolicy {
        &self.policy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A portable, in-memory store carrying an explicit, non-default
    /// `KernelPolicy` — no env involved anywhere in this construction.
    fn store_with_policy(rrf_k: f64, raw_tier_enabled: bool) -> MemoryStore {
        let mut policy = KernelPolicy::default();
        policy.recall.rrf_k = rrf_k;
        policy.embed.raw_tier_enabled = raw_tier_enabled;
        MemoryStore::open_in_memory()
            .expect("open in-memory store")
            .with_kernel_policy(policy)
    }

    /// tachi#1585 D5 / D7 pin: two portable stores in one process, built
    /// with different explicit recall/backfill configs and zero env
    /// mutation, must carry those configs independently — this is what a
    /// process-wide `OnceLock` (the pre-#1585 `RecallConfig::get()` shape)
    /// structurally cannot do. Construction order must not matter, so this
    /// is pinned in both orders.
    #[test]
    fn two_portable_stores_carry_independent_kernel_policy_forward_order() {
        let store_a = store_with_policy(11.0, true);
        let store_b = store_with_policy(99.0, false);

        assert_eq!(store_a.kernel_policy().recall.rrf_k, 11.0);
        assert!(store_a.kernel_policy().embed.raw_tier_enabled);
        assert_eq!(store_b.kernel_policy().recall.rrf_k, 99.0);
        assert!(!store_b.kernel_policy().embed.raw_tier_enabled);

        // Re-check `store_a` after `store_b` exists: building the second
        // store must not retroactively change the first's already-carried
        // policy (the failure mode a shared global would produce).
        assert_eq!(store_a.kernel_policy().recall.rrf_k, 11.0);
        assert!(store_a.kernel_policy().embed.raw_tier_enabled);
    }

    #[test]
    fn two_portable_stores_carry_independent_kernel_policy_reverse_order() {
        let store_b = store_with_policy(99.0, false);
        let store_a = store_with_policy(11.0, true);

        assert_eq!(store_b.kernel_policy().recall.rrf_k, 99.0);
        assert!(!store_b.kernel_policy().embed.raw_tier_enabled);
        assert_eq!(store_a.kernel_policy().recall.rrf_k, 11.0);
        assert!(store_a.kernel_policy().embed.raw_tier_enabled);

        assert_eq!(store_b.kernel_policy().recall.rrf_k, 99.0);
        assert!(!store_b.kernel_policy().embed.raw_tier_enabled);
    }

    /// A store that never receives `with_kernel_policy` carries the pure
    /// default — `RecallConfig::default()`, never `RecallConfig::get()`
    /// (tachi#1585 D5 point 1).
    #[test]
    fn store_without_injected_policy_is_pure_default() {
        let store = MemoryStore::open_in_memory().expect("open in-memory store");
        assert_eq!(store.kernel_policy().recall, crate::RecallConfig::default());
        assert!(store.kernel_policy().embed.raw_tier_enabled);
        assert!(!store.kernel_policy().path_validation_escape_hatch);
    }
}
