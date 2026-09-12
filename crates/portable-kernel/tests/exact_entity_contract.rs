//! #1882: the public reader is usable without admin or a raw connection.
//! This isolated contract is not downstream Hypermem adapter execution proof.
use portable_kernel::{
    ExactEntityReadBudget, ExactEntityReadRequest, ExactEntityReadStatus, MemoryEntry, MemoryStore,
    ADMIN_SURFACE_ENABLED, IS_PORTABLE_BUILD,
};

#[test]
fn exact_entity_portable_no_admin_temporal_and_store_isolation() {
    assert!(!ADMIN_SURFACE_ENABLED);
    assert!(IS_PORTABLE_BUILD);
    let directory = tempfile::tempdir().unwrap();
    let context = portable_kernel::db::DbOpenContext::create_fresh()
        .with_profile(portable_kernel::db::StoreProfile::PortableKernel);
    let mut a_store = MemoryStore::open_with_context(
        directory.path().join("project-a.db").to_str().unwrap(),
        &context,
    )
    .unwrap();
    let mut b_store = MemoryStore::open_with_context(
        directory.path().join("project-b.db").to_str().unwrap(),
        &context,
    )
    .unwrap();
    assert_eq!(
        a_store.store_profile(),
        portable_kernel::db::StoreProfile::PortableKernel
    );
    assert_eq!(
        b_store.store_profile(),
        portable_kernel::db::StoreProfile::PortableKernel
    );
    for (id, from, until) in [
        ("a", "2026-01-01T00:00:00Z", Some("2026-08-01T00:00:00Z")),
        ("b", "2026-08-01T00:00:00Z", None),
    ] {
        let entry: MemoryEntry = serde_json::from_value(serde_json::json!({
            "id":id,"path":"/facts","text":format!("portable temporal fixture {id}"),
            "timestamp":from,"valid_from":from,"valid_until":until,"entities":["identity"]
        }))
        .unwrap();
        a_store.upsert(&entry).unwrap();
    }
    let other: MemoryEntry = serde_json::from_value(serde_json::json!({
        "id":"other-store","path":"/facts","text":"independent store fixture",
        "timestamp":"2026-01-01T00:00:00Z","entities":["identity"]
    }))
    .unwrap();
    b_store.upsert(&other).unwrap();
    let identity = portable_kernel::db::StoreIdentity {
        db_label: "unknown".into(),
        profile: a_store.store_profile(),
    };
    let aliases = vec!["identity".into()];
    let read = |store: &MemoryStore, at| {
        store
            .read_exact_entities(ExactEntityReadRequest {
                expected_store: &identity,
                expected_physical_db_identity: None,
                expected_private_partition: None,
                exact_aliases: &aliases,
                path_prefix: Some("/facts"),
                domain: None,
                surface: None,
                agent_role: None,
                sandbox_rules: &[],
                as_of: at,
                include_archived: false,
                budget: ExactEntityReadBudget {
                    max_results: 4,
                    max_vm_steps: 1_000_000,
                    max_request_bytes: 4096,
                    max_admission_rows: 32,
                    max_row_bytes: 16384,
                    max_admission_bytes: 65536,
                    max_hydrated_bytes: 65536,
                },
            })
            .unwrap()
    };
    for (at, expected) in [
        (Some("2026-06-01T00:00:00Z"), "a"),
        (Some("2026-08-01T00:00:00Z"), "b"),
        (Some("2026-09-01T00:00:00Z"), "b"),
        (None, "b"),
    ] {
        let result = read(&a_store, at);
        assert_eq!(result.status, ExactEntityReadStatus::Complete);
        assert_eq!(
            result
                .entries
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>(),
            [expected]
        );
    }
    let result = read(&b_store, None);
    assert_eq!(
        result
            .entries
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        ["other-store"]
    );
}
