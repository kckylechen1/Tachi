//! #1882: the public reader is usable without admin or a raw connection.
//! This isolated contract is not downstream Hypermem adapter execution proof.
use portable_kernel::{
    ExactEntityReadBudget, ExactEntityReadRequest, ExactEntityReadStatus,
    ExactEntityValidityTolerance, MemoryEntry, MemoryStore, ADMIN_SURFACE_ENABLED,
    IS_PORTABLE_BUILD,
};

/// Explicit release report of the public, no-admin reader on synthetic files.
/// Run with --ignored --exact --nocapture --test-threads=1. Opening/importing
/// and result checks are outside each read timer; no wall-time gate is imposed.
#[test]
#[ignore = "explicit exact-entity read scaling report"]
fn exact_entity_read_scaling_report() {
    assert!(!ADMIN_SURFACE_ENABLED);
    for count in [1_000, 4_000, 16_000, 32_265] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("exact-entity-report.db");
        let context = portable_kernel::DbOpenContext::create_fresh()
            .with_profile(portable_kernel::StoreProfile::PortableKernel);
        let mut store = MemoryStore::open_with_context(path.to_str().unwrap(), &context).unwrap();
        let rows: Vec<_> = (0..count)
            .map(|index| {
                let mut entities = vec![format!("entity-{index}")];
                if index == 0 || index == count / 2 || index + 1 == count {
                    entities.push("cold".into());
                }
                if index % 512 == 0 {
                    entities.push("hot".into());
                }
                portable_kernel::PortableImportEntry {
                    entry: serde_json::from_value(serde_json::json!({
                        "id": format!("record-{index:08}"), "path": "/facts/public",
                        "text": format!("deterministic entity fixture {index}"),
                        "timestamp": "2026-01-01T00:00:00Z",
                        "valid_from": "2026-01-01T00:00:00Z",
                        "domain": "fixture", "entities": entities,
                        "metadata": {"ordinal": index}
                    }))
                    .unwrap(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                    updated_at: "2026-01-01T00:00:00Z".into(),
                    superseded_by: None,
                }
            })
            .collect();
        let checksum =
            portable_kernel::PortableImportReceipt::expected_lifecycle_checksum(&rows).unwrap();
        let receipt = store.import_snapshot_batch(&rows).unwrap();
        assert_eq!(receipt.rows_imported, count);
        assert_eq!(receipt.lifecycle_checksum, checksum);
        drop(store);
        let store = MemoryStore::open_read_only(path.to_str().unwrap()).unwrap();
        let identity = portable_kernel::db::StoreIdentity {
            db_label: store.db_label().into(),
            profile: store.store_profile(),
        };
        for alias in ["cold", "hot"] {
            let aliases = vec![alias.to_owned()];
            let expected: Vec<_> = rows
                .iter()
                .filter(|row| row.entry.entities.contains(&aliases[0]))
                .map(|row| row.entry.id.as_str())
                .collect();
            // Record the first read separately from two warm observations.
            for repetition in 0..3 {
                let started = std::time::Instant::now();
                let result = store
                    .read_exact_entities(ExactEntityReadRequest {
                        expected_store: &identity,
                        expected_physical_db_identity: store.opened_physical_db_identity(),
                        expected_private_partition: None,
                        exact_aliases: &aliases,
                        path_prefix: Some("/facts"),
                        domain: None,
                        surface: None,
                        agent_role: None,
                        sandbox_rules: &[],
                        as_of: None,
                        include_archived: false,
                        validity_tolerance: ExactEntityValidityTolerance::Reject,
                        budget: ExactEntityReadBudget {
                            max_results: count,
                            max_vm_steps: 1_000_000_000,
                            max_request_bytes: 4096,
                            max_admission_rows: count + 1,
                            max_row_bytes: 1_048_576,
                            max_admission_bytes: 268_435_456,
                            max_hydrated_bytes: 268_435_456,
                        },
                    })
                    .unwrap();
                let elapsed_ns = started.elapsed().as_nanos();
                assert_eq!(result.status, ExactEntityReadStatus::Complete);
                assert_eq!(result.admission_rows, count);
                assert_eq!(
                    result
                        .entries
                        .iter()
                        .map(|entry| entry.id.as_str())
                        .collect::<Vec<_>>(),
                    expected
                );
                println!(
                    "{}",
                    serde_json::json!({
                        "report": "exact_entity_read_scaling", "schema_version": 1,
                        "rows": count, "alias": alias, "repetition": repetition,
                        "first_read_for_alias": repetition == 0,
                        "elapsed_ns": elapsed_ns, "input_lifecycle_checksum": checksum,
                        "store_profile": "PortableKernel", "admin_feature": ADMIN_SURFACE_ENABLED,
                        "debug_assertions": cfg!(debug_assertions),
                        "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
                        "admission_rows": result.admission_rows,
                        "admission_bytes": result.admission_bytes,
                        "hydrated_bytes": result.hydrated_bytes,
                        "charged_vm_steps": result.charged_vm_steps,
                        "entries": result.entries
                    })
                );
            }
        }
    }
}

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
        validity_tolerance: ExactEntityValidityTolerance::Reject,
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
fn same_millisecond_supersession_is_empty_after_explicit_repair_and_reopen() {
    assert!(!ADMIN_SURFACE_ENABLED);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("same-millisecond.db");
    let from = "2026-08-05T04:27:15.656Z";
    let rows: Vec<_> = [("old", Some("new")), ("new", None)]
        .into_iter()
        .map(|(id, superseded)| portable_kernel::PortableImportEntry {
            entry: serde_json::from_value(serde_json::json!({
                "id":id, "path":"/facts", "text":format!("same millisecond {id}"),
                "timestamp":from, "valid_from":from, "entities":["identity"]
            }))
            .unwrap(),
            created_at: from.into(),
            updated_at: from.into(),
            superseded_by: superseded.map(str::to_owned),
        })
        .collect();
    let mut store = MemoryStore::open_with_context(
        path.to_str().unwrap(),
        &portable_kernel::DbOpenContext::create_fresh()
            .with_profile(portable_kernel::StoreProfile::PortableKernel),
    )
    .unwrap();
    let receipt = store.import_snapshot_batch(&rows).unwrap();
    assert_eq!(
        receipt.lifecycle_checksum,
        portable_kernel::PortableImportReceipt::expected_lifecycle_checksum(&rows).unwrap()
    );
    drop(store);

    let mut store = MemoryStore::open_with_context(
        path.to_str().unwrap(),
        &portable_kernel::DbOpenContext::open_existing_deny()
            .with_profile(portable_kernel::StoreProfile::PortableKernel),
    )
    .unwrap();
    assert!(store.get("old").unwrap().unwrap().valid_until.is_none());
    assert_eq!(read_at(&store, None).unwrap(), ["new"]);
    assert!(read_at(&store, Some("2026-06-01T00:00:00Z")).is_err());
    let report = store.repair_superseded_validity_from_updated_at().unwrap();
    assert_eq!(
        (report.examined, report.repaired, report.unresolved),
        (1, 1, 0)
    );
    assert_eq!(
        report.inference_policy,
        "superseded-validity-from-updated-at-v2"
    );
    assert_eq!(
        store.get("old").unwrap().unwrap().valid_until.as_deref(),
        Some(from)
    );
    assert_eq!(
        store
            .repair_superseded_validity_from_updated_at()
            .unwrap()
            .examined,
        0
    );
    drop(store);

    let store = MemoryStore::open_read_only(path.to_str().unwrap()).unwrap();
    for at in [
        "2026-01-01T00:00:00Z",
        "2026-06-01T00:00:00Z",
        "2026-08-05T04:27:15.655Z",
    ] {
        assert!(read_at(&store, Some(at)).unwrap().is_empty());
    }
    for at in [None, Some(from), Some("2026-08-05T04:27:15.657Z")] {
        assert_eq!(read_at(&store, at).unwrap(), ["new"]);
    }
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
                validity_tolerance: ExactEntityValidityTolerance::Reject,
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

#[test]
fn tolerant_history_reads_degrade_with_classified_receipt() {
    assert!(!ADMIN_SURFACE_ENABLED);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tolerant.db");
    let context = portable_kernel::DbOpenContext::create_fresh()
        .with_profile(portable_kernel::StoreProfile::PortableKernel);
    let mut store = MemoryStore::open_with_context(path.to_str().unwrap(), &context).unwrap();
    let rows: Vec<_> = [
        ("old-missing", serde_json::Value::Null, Some("new")),
        (
            "old-inverted",
            serde_json::json!("2025-12-31T00:00:00Z"),
            Some("new"),
        ),
        ("new", serde_json::Value::Null, None),
    ]
    .into_iter()
    .map(
        |(id, until, superseded)| portable_kernel::PortableImportEntry {
            entry: serde_json::from_value(serde_json::json!({
                "id":id, "path":"/facts", "text":format!("tolerant fixture {id}"),
                "timestamp":"2026-01-01T00:00:00Z", "valid_from":"2026-01-01T00:00:00Z",
                "valid_until":until, "entities":["identity"]
            }))
            .unwrap(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-08-01T00:00:00Z".into(),
            superseded_by: superseded.map(str::to_owned),
        },
    )
    .collect();
    store.import_snapshot_batch(&rows).unwrap();
    drop(store);

    let store = MemoryStore::open_read_only(path.to_str().unwrap()).unwrap();
    let identity = portable_kernel::db::StoreIdentity {
        db_label: store.db_label().into(),
        profile: store.store_profile(),
    };
    let aliases = vec!["identity".into()];
    let tolerant_read = |as_of: Option<&str>| {
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
                as_of,
                include_archived: false,
                validity_tolerance: ExactEntityValidityTolerance::SkipClassified {
                    max_diagnostic_ids: 8,
                    max_diagnostic_bytes: 256,
                },
                budget: ExactEntityReadBudget {
                    max_results: 10,
                    max_vm_steps: 1_000_000,
                    max_request_bytes: 4096,
                    max_admission_rows: 100,
                    max_row_bytes: 16384,
                    max_admission_bytes: 65536,
                    max_hydrated_bytes: 65536,
                },
            })
            .unwrap()
    };
    // Current reads are unaffected: superseded rows are excluded before any
    // window classification, so nothing is skipped.
    let current = tolerant_read(None);
    assert_eq!(current.status, ExactEntityReadStatus::Complete);
    assert_eq!(
        current
            .entries
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        ["new"]
    );
    assert_eq!(current.validity_skips.invalid_windows, 0);
    assert_eq!(current.validity_skips.missing_boundaries, 0);
    // The degraded historical read returns the visible row plus an exact,
    // named receipt for both classified classes.
    let history = tolerant_read(Some("2026-06-01T00:00:00Z"));
    assert_eq!(history.status, ExactEntityReadStatus::Complete);
    assert_eq!(
        history
            .entries
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        ["new"]
    );
    assert_eq!(history.validity_skips.invalid_windows, 1);
    assert_eq!(history.validity_skips.missing_boundaries, 1);
    assert_eq!(history.validity_skips.invalid_window_ids, ["old-inverted"]);
    assert_eq!(history.validity_skips.missing_boundary_ids, ["old-missing"]);
    assert!(!history.validity_skips.diagnostics_truncated);
}
