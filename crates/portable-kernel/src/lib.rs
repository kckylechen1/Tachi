//! Portable memory kernel: the lean, embeddable memory core ZeroClaw
//! links directly as its native memory module (#1195).
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
//! Consumer rule of thumb:
//! - ZeroClaw native memory module (owner-ratified direction, #1195): link
//!   this crate directly as an embedded Rust backend (equivalently depend on
//!   `memcore` with `default-features = false`).
//! - Full Tachi monorepo product: depend on `memcore` with default
//!   features (admin on).
//!
//! Historical note (#1195, 2026-07-17): the prior "downstream forks /
//! HyperTachi / HyperMemory convergence" framing is stale — that fork
//! divorced 2026-07-14 (Hyperion-HyperTachi 9da2015). This crate stays;
//! its future is the ZeroClaw-native memory module direction.
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
            last_access: None,
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
