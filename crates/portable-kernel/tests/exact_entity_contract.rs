//! #1882: the public reader is usable without admin or a raw connection.
//! This isolated contract is not downstream Hypermem adapter execution proof.
use portable_kernel::{
    ExactEntityReadBudget, ExactEntityReadRequest, ExactEntityReadStatus, MemoryEntry, MemoryStore,
    ADMIN_SURFACE_ENABLED, IS_PORTABLE_BUILD,
};

fn read_at(
    store: &MemoryStore,
    at: Option<&str>,
) -> Result<Vec<String>, portable_kernel::MemoryError> {
    let identity = portable_kernel::db::StoreIdentity {
        db_label: store.db_label().into(),
        profile: store.store_profile(),
    };
    let aliases = vec!["identity".into()];
    let result = store.read_exact_entities(ExactEntityReadRequest {
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
            max_results: 10,
            max_vm_steps: 1_000_000,
            max_request_bytes: 4096,
            max_admission_rows: 100,
            max_row_bytes: 16384,
            max_admission_bytes: 65536,
            max_hydrated_bytes: 65536,
        },
    })?;
    assert_eq!(result.status, ExactEntityReadStatus::Complete);
    Ok(result.entries.into_iter().map(|entry| entry.id).collect())
}

#[test]
fn imported_legacy_boundaries_require_explicit_portable_repair_after_reopen() {
    assert!(!ADMIN_SURFACE_ENABLED);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("imported.db");
    let context = portable_kernel::DbOpenContext::create_fresh()
        .with_profile(portable_kernel::StoreProfile::PortableKernel);
    let mut store = MemoryStore::open_with_context(path.to_str().unwrap(), &context).unwrap();
    let rows: Vec<_> = [
        ("old", "2026-01-01T00:00:00Z", Some("new")),
        ("new", "2026-08-01T00:00:00Z", None),
    ]
    .into_iter()
    .map(
        |(id, from, superseded)| portable_kernel::PortableImportEntry {
            entry: serde_json::from_value(serde_json::json!({
                "id":id, "path":"/facts", "text":format!("snapshot {id}"),
                "timestamp":from, "valid_from":from, "entities":["identity"]
            }))
            .unwrap(),
            created_at: from.into(),
            updated_at: "2026-08-01T00:00:00Z".into(),
            superseded_by: superseded.map(str::to_owned),
        },
    )
    .collect();
    let source_checksum =
        portable_kernel::PortableImportReceipt::expected_lifecycle_checksum(&rows).unwrap();
    let import = store.import_snapshot_batch(&rows).unwrap();
    assert_eq!(import.rows_imported, 2);
    assert_eq!(import.lifecycle_checksum, source_checksum);
    assert!(store.get("old").unwrap().unwrap().valid_until.is_none());
    drop(store);

    let mut readonly = MemoryStore::open_read_only(path.to_str().unwrap()).unwrap();
    assert_eq!(read_at(&readonly, None).unwrap(), ["new"]);
    assert!(read_at(&readonly, Some("2026-06-01T00:00:00Z")).is_err());
    assert!(readonly
        .repair_superseded_validity_from_updated_at()
        .is_err());
    drop(readonly);
    let mut store = MemoryStore::open_with_context(
        path.to_str().unwrap(),
        &portable_kernel::DbOpenContext::open_existing_deny()
            .with_profile(portable_kernel::StoreProfile::PortableKernel),
    )
    .unwrap();
    assert!(
        store.get("old").unwrap().unwrap().valid_until.is_none(),
        "reopen must not silently repair import data"
    );
    let report = store.repair_superseded_validity_from_updated_at().unwrap();
    assert_eq!(
        (report.examined, report.repaired, report.unresolved),
        (1, 1, 0)
    );
    assert_eq!(
        report.physical_db_identity.as_deref(),
        store.opened_physical_db_identity()
    );
    assert_eq!(
        read_at(&store, Some("2026-06-01T00:00:00Z")).unwrap(),
        ["old"]
    );
    assert_eq!(
        read_at(&store, Some("2026-08-01T00:00:00Z")).unwrap(),
        ["new"]
    );
    assert_eq!(read_at(&store, None).unwrap(), ["new"]);
    assert_eq!(
        store
            .repair_superseded_validity_from_updated_at()
            .unwrap()
            .examined,
        0
    );
    let mut repaired_rows = rows;
    repaired_rows[0].entry = store.get("old").unwrap().unwrap();
    let repaired_checksum =
        portable_kernel::PortableImportReceipt::expected_lifecycle_checksum(&repaired_rows)
            .unwrap();
    assert_ne!(repaired_checksum, import.lifecycle_checksum);
}

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
