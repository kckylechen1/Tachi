use portable_kernel::{MemoryEntry, MemoryStore, ADMIN_SURFACE_ENABLED, IS_PORTABLE_BUILD};

#[test]
fn portable_build_disables_admin_surface() {
    assert!(
        !ADMIN_SURFACE_ENABLED,
        "portable-kernel must resolve memcore without the product admin surface"
    );
    assert!(
        IS_PORTABLE_BUILD,
        "portable-kernel must report its portable build"
    );
}

fn smoke_entry(id: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.into(),
        path: "/scratch/portable/batch".into(),
        summary: "portable batch smoke".into(),
        text: "portable kernel batch upsert smoke fact".into(),
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
    }
}

/// tachi#1599: `upsert_batch` must be callable — and resolve, in a build
/// that has genuinely disabled the admin feature (this test file is only
/// compiled in isolation, via `required-features = ["portable-contract-test"]`,
/// specifically so workspace feature unification cannot mask an accidental
/// admin dependency the way it would inside a normal `cargo test` run).
#[test]
fn portable_build_upsert_batch_atomic() {
    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");

    store
        .upsert_batch(&[
            smoke_entry("portable-batch-1"),
            smoke_entry("portable-batch-2"),
        ])
        .expect("upsert_batch must succeed on a portable-build store");
    assert!(store.get("portable-batch-1").expect("get").is_some());
    assert!(store.get("portable-batch-2").expect("get").is_some());

    store
        .upsert_batch(&[])
        .expect("empty upsert_batch is a successful no-op");
}
