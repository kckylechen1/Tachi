//! Portable facade over `memcore`, built without Tachi's admin feature.
//!
//! Re-exports the `memcore` public API built with
//! `default-features = false` (no `admin` feature). That means:
//!
//! | Included (portable) | Excluded (Tachi admin) |
//! |---|---|
//! | `MemoryStore` open/CRUD | Vault secrets |
//! | Hybrid search / scorer | Hub capability catalog |
//! | Schema + migrations | Foundry job queue types |
//! | Graph / events / sandbox | agent_profile APIs |
//!
//! Current consumer rule of thumb:
//! - Embedders that need only the memory kernel can depend on this crate, or
//!   equivalently on `memcore` with `default-features = false`.
//! - Full Tachi monorepo product: depend on `memcore` with default
//!   features (admin on).
//!
//! Future direction (#1195, owner-ratified 2026-07-17): retain this package
//! as a candidate boundary for a ZeroClaw-native memory module.
//! No direct ZeroClaw Cargo integration has landed. ZeroClaw does not
//! currently depend on this crate.
//!
//! Owner ruling 2026-08-03 (#1585): Hypermem is the supported external
//! consumer of this boundary. Tachi owns the reusable memory kernel; Hypermem
//! is an independent component (standalone or embedded in Hyperion) that
//! depends on this crate only, opening explicitly supplied Hyperion-owned
//! databases under the `PortableKernel` schema profile, with policy injected
//! via `KernelPolicy` rather than any `TACHI_*` environment. Product adapters
//! remain downstream.
//!
//! Historical provenance: this facade was originally cut for downstream
//! HyperTachi and HyperMemory convergence; that fork separated on 2026-07-14
//! (Hyperion-HyperTachi `9da2015`), and the #1585 ruling above is the current
//! consumer contract.
//!
//! See `docs/engineering/architecture/portable-kernel-split.md` and
//! `docs/engineering/architecture/downstream-sync-surface.md`.

pub use memcore::*;

/// True when this facade was built without the admin surface.
/// Always true for `portable-kernel` (by construction).
pub const IS_PORTABLE_BUILD: bool = !memcore::ADMIN_SURFACE_ENABLED;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_store_open_upsert_and_stats() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");

        let entry = MemoryEntry {
            id: "portable-smoke-1".into(),
            path: "/scratch/portable/smoke".into(),
            summary: "portable smoke".into(),
            text: "portable kernel smoke fact".into(),
            importance: 0.7,
            timestamp: "2026-07-09T00:00:00Z".into(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".into(),
            topic: String::new(),
            keywords: vec!["portable".into()],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            source: "manual".into(),
            scope: "general".into(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: serde_json::Value::Object(Default::default()),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".into(),
        };
        store.upsert(&entry).expect("upsert");

        let stats = store.stats(true).expect("stats");
        assert!(stats.total >= 1, "expected at least one row after upsert");
    }
}
