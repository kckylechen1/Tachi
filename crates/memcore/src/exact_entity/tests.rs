use super::*;
use std::sync::{atomic::AtomicUsize, Arc};

fn budget() -> ExactEntityReadBudget {
    ExactEntityReadBudget {
        max_results: 8,
        max_vm_steps: 2_000_000,
        max_request_bytes: 4096,
        max_admission_rows: 256,
        max_row_bytes: 16384,
        max_admission_bytes: 262144,
        max_hydrated_bytes: 65536,
    }
}

static UNKNOWN_STORE: std::sync::LazyLock<db::StoreIdentity> =
    std::sync::LazyLock::new(|| db::StoreIdentity {
        db_label: "unknown".into(),
        profile: db::StoreProfile::default(),
    });

fn request(aliases: &[String]) -> ExactEntityReadRequest<'_> {
    ExactEntityReadRequest {
        expected_store: &UNKNOWN_STORE,
        expected_physical_db_identity: None,
        expected_private_partition: None,
        exact_aliases: aliases,
        path_prefix: Some("/facts"),
        domain: Some("fixture"),
        surface: None,
        agent_role: None,
        sandbox_rules: &[],
        as_of: None,
        include_archived: false,
        validity_tolerance: ExactEntityValidityTolerance::Reject,
        budget: budget(),
    }
}

fn entry(id: &str, entity: &str) -> MemoryEntry {
    serde_json::from_value(serde_json::json!({"id":id,"path":"/facts/public",
        "text":format!("distinct fixture body {id}; 600519 2026-06-01 123.45"),
        "timestamp":"2026-01-01T00:00:00Z","domain":"fixture",
        "entities":[entity],"keywords":["exact"]}))
    .unwrap()
}

fn ids(result: &ExactEntityReadResult) -> Vec<&str> {
    result.entries.iter().map(|e| e.id.as_str()).collect()
}

#[test]
fn structural_identity_precedes_result_cap_and_full_hydration() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    let mut lexical = entry("a-lexical", "other");
    lexical.text = "exact".repeat(10000); // Must never be fully hydrated.
    store.upsert(&lexical).unwrap();
    store.upsert(&entry("z-exact", "exact")).unwrap();
    let aliases = vec!["exact".into()];
    let mut req = request(&aliases);
    req.budget.max_results = 1;
    let result = store.read_exact_entities(req).unwrap();
    assert_eq!(result.status, ExactEntityReadStatus::Complete);
    assert_eq!(ids(&result), ["z-exact"]);
    assert!(result.hydrated_bytes < 16384);
}

#[test]
fn frozen_june_august_supersession_is_half_open_and_archive_does_not_revive() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    let a = entry("a", "exact");
    let mut b = entry("b", "exact");
    b.timestamp = "2026-08-01T00:00:00Z".into();
    store.upsert(&a).unwrap();
    store.upsert(&b).unwrap();
    store
        .mark_superseded_closing_validity("a", "b", "2026-08-01T00:00:00Z")
        .unwrap();
    let aliases = vec!["exact".into()];
    for (at, expected) in [
        (Some("2026-06-01T00:00:00Z"), "a"),
        (Some("2026-08-01T00:00:00Z"), "b"),
        (Some("2026-09-01T00:00:00Z"), "b"),
        (None, "b"),
    ] {
        let mut req = request(&aliases);
        req.as_of = at;
        req.include_archived = true;
        let result = store.read_exact_entities(req).unwrap();
        assert_eq!(result.status, ExactEntityReadStatus::Complete);
        assert_eq!(ids(&result), [expected]);
    }
}

#[test]
fn missing_or_inverted_supersession_windows_are_excluded_now_but_refused_in_history() {
    for end in [None, Some("2025-12-31T00:00:00Z")] {
        let mut store = MemoryStore::open_in_memory().unwrap();
        let mut old_entry = entry("old", "exact");
        old_entry.valid_until = end.map(str::to_owned);
        let old = crate::PortableImportEntry {
            entry: old_entry,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-08-01T00:00:00Z".into(),
            superseded_by: Some("current".into()),
        };
        let current = crate::PortableImportEntry {
            entry: entry("current", "exact"),
            superseded_by: None,
            ..old.clone()
        };
        store.import_snapshot_batch(&[old, current]).unwrap();
        let aliases = vec!["exact".into()];
        for include_archived in [false, true] {
            let mut req = request(&aliases);
            req.include_archived = include_archived;
            let result = store.read_exact_entities(req).unwrap();
            assert_eq!(result.status, ExactEntityReadStatus::Complete);
            assert_eq!(ids(&result), ["current"]);
        }
        let mut req = request(&aliases);
        req.as_of = Some("2026-06-01T00:00:00Z");
        let error = store.read_exact_entities(req).unwrap_err();
        assert!(error.to_string().contains(if end.is_none() {
            "lacks historical validity boundary"
        } else {
            "invalid half-open validity window"
        }));
        assert_eq!(
            store.get("old").unwrap().unwrap().valid_until.as_deref(),
            end
        );
    }
}

#[test]
fn superseded_empty_windows_are_never_visible() {
    // The downstream legacy copy contains rows created and superseded within
    // the same millisecond. [t, t) contains no instant, including t itself.
    for end in ["2026-08-05T04:27:15.656Z", "2026-08-05T12:27:15.656+08:00"] {
        let mut store = MemoryStore::open_in_memory().unwrap();
        let mut old = entry("empty", "exact");
        old.timestamp = "2026-08-05T04:27:15.656Z".into();
        old.valid_until = Some(end.into());
        let rows = [
            crate::PortableImportEntry {
                entry: old,
                created_at: "2026-08-05T04:27:15.656Z".into(),
                updated_at: "2026-08-05T04:27:15.656Z".into(),
                superseded_by: Some("current".into()),
            },
            crate::PortableImportEntry {
                entry: entry("current", "exact"),
                created_at: "2026-01-01T00:00:00Z".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
                superseded_by: None,
            },
        ];
        store.import_snapshot_batch(&rows).unwrap();
        let before = store.get("empty").unwrap().unwrap();
        let aliases = vec!["exact".into()];
        for include_archived in [false, true] {
            for at in [
                None,
                Some("2026-06-01T00:00:00Z"),
                Some("2026-08-05T04:27:15.655Z"),
                Some("2026-08-05T04:27:15.656Z"),
                Some("2026-08-05T04:27:15.657Z"),
            ] {
                let mut req = request(&aliases);
                req.as_of = at;
                req.include_archived = include_archived;
                let result = store.read_exact_entities(req).unwrap();
                assert_eq!(result.status, ExactEntityReadStatus::Complete);
                assert_eq!(ids(&result), ["current"]);
            }
        }
        let after = store.get("empty").unwrap().unwrap();
        assert_eq!(after.valid_until, before.valid_until);
        assert_eq!(
            after.revision, before.revision,
            "reads must not repair data"
        );
    }
}

#[test]
fn active_empty_windows_remain_invalid() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    let mut active = entry("active-empty", "exact");
    active.valid_until = Some(active.timestamp.clone());
    store
        .import_snapshot_batch(&[crate::PortableImportEntry {
            entry: active,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            superseded_by: None,
        }])
        .unwrap();
    let aliases = vec!["exact".into()];
    for at in [
        None,
        Some("2025-12-31T00:00:00Z"),
        Some("2026-01-01T00:00:00Z"),
    ] {
        let mut req = request(&aliases);
        req.as_of = at;
        assert!(store
            .read_exact_entities(req)
            .unwrap_err()
            .to_string()
            .contains("invalid half-open validity window"));
    }
}

#[test]
fn canonical_idless_merge_closes_past_rows_and_keeps_future_rows_independent() {
    for (from, explicit_end, should_merge) in [
        ("2026-01-01T00:00:00Z", None, true),
        ("2026-01-01T00:00:00Z", Some("2026-08-01T00:00:00Z"), true),
        ("2099-01-01T00:00:00Z", None, false),
    ] {
        let mut store = MemoryStore::open_in_memory().unwrap();
        let shared = "alpha bravo charlie delta echo foxtrot golf hotel india juliet \
                      kilo lima mike november oscar papa quebec romeo sierra";
        let mut winner = entry("winner", "exact");
        winner.text = format!("{shared} tango");
        store.upsert(&winner).unwrap();
        let mut incoming = entry("incoming", "exact");
        incoming.text = format!("{shared} uniform");
        incoming.valid_from = from.into();
        incoming.valid_until = explicit_end.map(str::to_owned);
        let (result, _) = store
            .upsert_with_validated_reference_mutations_and_metadata_removals(
                &incoming,
                Some("canonical-near-duplicate-identity"),
                &serde_json::Map::new(),
                &[],
                &[],
                db::NearDuplicatePolicy::AllowNearDuplicateMerge,
            )
            .unwrap();
        assert_eq!(result, db::IdlessUpsertResult::Saved);
        assert_eq!(
            store.supersession_target("incoming").unwrap(),
            Some(should_merge.then(|| "winner".into()))
        );
        let saved = store.get("incoming").unwrap().unwrap();
        if should_merge {
            assert!(saved.valid_until.as_deref().unwrap() > saved.valid_from.as_str());
            if let Some(end) = explicit_end {
                assert_eq!(saved.valid_until, Some(db::normalize_utc_iso(end).unwrap()));
            }
            assert_eq!(saved.revision, 2);
        } else {
            assert!(saved.valid_until.is_none());
            assert_eq!(saved.revision, 1);
        }
        let identity: Option<String> = store
            .conn
            .query_row(
                "SELECT idless_identity FROM memories WHERE id='incoming'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            identity.as_deref(),
            (!should_merge).then_some("canonical-near-duplicate-identity")
        );
        let aliases = vec!["exact".into()];
        assert_eq!(
            ids(&store.read_exact_entities(request(&aliases)).unwrap()),
            ["winner"]
        );
        let mut historical = request(&aliases);
        historical.as_of = Some(if should_merge {
            "2026-06-01T00:00:00Z"
        } else {
            "2099-06-01T00:00:00Z"
        });
        assert_eq!(
            ids(&store.read_exact_entities(historical).unwrap()),
            ["incoming", "winner"]
        );
    }
}

#[test]
fn governed_role_path_domain_archive_and_namespace_admit_before_cap() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    for (id, path, domain, archived) in [
        ("a-denied", "/facts/private", "fixture", false),
        ("b-prefix", "/facts-other", "fixture", false),
        ("c-domain", "/facts/public", "other", false),
        ("d-archive", "/facts/public", "fixture", true),
        ("z-visible", "/facts/public", "fixture", false),
    ] {
        let mut e = entry(id, "exact");
        e.path = path.into();
        e.domain = Some(domain.into());
        e.archived = archived;
        if id == "a-denied" {
            e.text = "x".repeat(100000);
        }
        store.upsert(&e).unwrap();
    }
    let mut wiki = entry("e-inactive-wiki", "exact");
    wiki.category = "guide".into();
    wiki.metadata = serde_json::json!({"lifecycle":"pending_review"});
    store.upsert(&wiki).unwrap();
    let aliases = vec!["exact".into()];
    let rules = vec![
        ("/facts/*".into(), "read".into()),
        ("/facts/private".into(), "deny".into()),
    ];
    let mut req = request(&aliases);
    req.agent_role = Some("reader");
    req.sandbox_rules = &rules;
    req.budget.max_results = 1;
    let result = store.read_exact_entities(req).unwrap();
    assert_eq!(result.status, ExactEntityReadStatus::Complete);
    assert_eq!(ids(&result), ["z-visible"]);
    let mut req = request(&aliases);
    req.agent_role = Some("reader");
    req.sandbox_rules = &rules;
    req.include_archived = true;
    assert_eq!(
        ids(&store.read_exact_entities(req).unwrap()),
        ["d-archive", "z-visible"]
    );
}

#[test]
fn projection_preparation_waits_for_path_and_role_admission() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    let aliases = vec!["exact".into()];
    // The probe and id/path scan fit, but the guarded entity projection does
    // not. Empty or denied reads must not prepare that unused projection.
    let previous = store
        .conn
        .set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 1024)
        .unwrap();
    let empty = store.read_exact_entities(request(&aliases)).unwrap();
    assert_eq!(empty.status, ExactEntityReadStatus::Complete);
    assert!(empty.entries.is_empty());
    store
        .conn
        .set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, previous)
        .unwrap();
    store.upsert(&entry("candidate", "exact")).unwrap();
    assert_eq!(
        ids(&store.read_exact_entities(request(&aliases)).unwrap()),
        ["candidate"]
    );
    store
        .conn
        .set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 1024)
        .unwrap();
    let rules = vec![("/facts/*".into(), "deny".into())];
    for denied_by_role in [false, true] {
        let mut req = request(&aliases);
        if denied_by_role {
            req.agent_role = Some("reader");
            req.sandbox_rules = &rules;
        } else {
            req.path_prefix = Some("/elsewhere");
        }
        let result = store.read_exact_entities(req).unwrap();
        assert_eq!(result.status, ExactEntityReadStatus::Complete);
        assert_eq!(result.admission_rows, 1);
        assert!(result.entries.is_empty());
    }
    let limited = store.read_exact_entities(request(&aliases)).unwrap();
    assert_eq!(limited.status, ExactEntityReadStatus::RowBytes);
    assert!(limited.entries.is_empty());
    assert!(store.conn.is_autocommit());
    store
        .conn
        .set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, previous)
        .unwrap();
    assert_eq!(
        ids(&store.read_exact_entities(request(&aliases)).unwrap()),
        ["candidate"]
    );
}

#[test]
fn hostile_population_and_json_have_explicit_nonempty_work_exhaustion() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    for index in 0..12 {
        store
            .upsert(&entry(&format!("miss-{index}"), "other"))
            .unwrap();
    }
    store.upsert(&entry("target", "exact")).unwrap();
    let aliases = vec!["exact".into()];
    let mut req = request(&aliases);
    req.budget.max_admission_rows = 3;
    let result = store.read_exact_entities(req).unwrap();
    assert_eq!(result.status, ExactEntityReadStatus::AdmissionRows);
    assert!(result.entries.is_empty());
    assert_eq!(result.admission_rows, 3);
    // Malicious entities are measured before serde sees them.
    store
        .conn
        .execute(
            "UPDATE memories SET entities=?1 WHERE id='miss-0'",
            [format!("[\"{}\"]", "x".repeat(100000))],
        )
        .unwrap();
    let result = store.read_exact_entities(request(&aliases)).unwrap();
    assert_eq!(result.status, ExactEntityReadStatus::RowBytes);
    assert!(result.entries.is_empty());
}

#[test]
fn malformed_metadata_and_time_are_errors_and_cleanup_restores_connection() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    store.upsert(&entry("bad", "exact")).unwrap();
    let aliases = vec!["exact".into()];
    for (column, bad, good) in [
        ("entities", "not-json", "[\"exact\"]"),
        ("metadata", "[]", "{}"),
        ("valid_from", "tomorrow", "2026-01-01T00:00:00Z"),
    ] {
        {
            // Seed/restore malformed legacy data through the existing typed
            // fixture authority; disarm it before the reader is exercised.
            let _authorization =
                db::authorize_reserved_reference_write(&store.reserved_reference_write).unwrap();
            store
                .conn
                .execute(
                    &format!("UPDATE memories SET {column}=?1 WHERE id='bad'"),
                    [bad],
                )
                .unwrap();
        }
        assert!(store.read_exact_entities(request(&aliases)).is_err());
        assert!(store.conn.is_autocommit());
        assert!(!store
            .reserved_reference_write
            .exact_active
            .load(Ordering::SeqCst));
        {
            // Seed/restore malformed legacy data through the existing typed
            // fixture authority; disarm it before the reader is exercised.
            let _authorization =
                db::authorize_reserved_reference_write(&store.reserved_reference_write).unwrap();
            store
                .conn
                .execute(
                    &format!("UPDATE memories SET {column}=?1 WHERE id='bad'"),
                    [good],
                )
                .unwrap();
        }
        assert_eq!(
            ids(&store.read_exact_entities(request(&aliases)).unwrap()),
            ["bad"]
        );
    }
}

#[test]
fn foreign_callback_is_witnessed_without_overwrite_and_interrupt_is_not_budget() {
    let store = MemoryStore::open_in_memory().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    store
        .conn
        .progress_handler(
            1,
            Some(move || {
                observed.fetch_add(1, Ordering::SeqCst);
                false
            }),
        )
        .unwrap();
    let aliases = vec!["exact".into()];
    assert_eq!(
        store.read_exact_entities(request(&aliases)).unwrap().status,
        ExactEntityReadStatus::CallbackUnavailable
    );
    let before = calls.load(Ordering::SeqCst);
    store
        .conn
        .query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
        .unwrap();
    assert!(calls.load(Ordering::SeqCst) > before);
    store.conn.progress_handler(1, Some(|| true)).unwrap();
    assert!(
        matches!(store.read_exact_entities(request(&aliases)), Err(MemoryError::Sqlite(e)) if e.sqlite_error_code()==Some(rusqlite::ErrorCode::OperationInterrupted))
    );
}

#[test]
fn vm_probe_is_charged_and_error_panic_nested_state_cleans_up() {
    let store = MemoryStore::open_in_memory().unwrap();
    let aliases = vec!["exact".into()];
    let mut req = request(&aliases);
    req.budget.max_vm_steps = 1;
    let result = store.read_exact_entities(req).unwrap();
    assert_eq!(result.status, ExactEntityReadStatus::VmBudget);
    assert!(result.charged_vm_steps >= db::EXACT_PROGRESS_INTERVAL);
    assert!(store.conn.is_autocommit());
    let state = &store.reserved_reference_write;
    state.exact_active.store(true, Ordering::SeqCst);
    assert_eq!(
        store.read_exact_entities(request(&aliases)).unwrap().status,
        ExactEntityReadStatus::NestedRead
    );
    assert!(state.exact_active.load(Ordering::SeqCst));
    let length = store.conn.limit(Limit::SQLITE_LIMIT_LENGTH).unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = QueryGuard {
            conn: &store.conn,
            state,
            previous_length: length,
        };
        let _snapshot = ReadSnapshot {
            transaction: Some(store.conn.unchecked_transaction().unwrap()),
            state,
        };
        panic!("controlled reader guard unwind");
    }));
    assert!(panic.is_err());
    assert!(store.conn.is_autocommit());
    assert!(!state.exact_active.load(Ordering::SeqCst));
    assert_eq!(
        store.conn.limit(Limit::SQLITE_LIMIT_LENGTH).unwrap(),
        length
    );
    let result = store.read_exact_entities(request(&aliases)).unwrap();
    assert_eq!(result.status, ExactEntityReadStatus::Complete);
    assert!(result.charged_vm_steps > 0);
}

#[test]
fn request_empty_zero_overflow_and_deterministic_cap_are_discriminated() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    let aliases = vec!["exact".into()];
    assert!(store.read_exact_entities(request(&[])).is_err());
    let empty = vec![String::new()];
    assert!(store.read_exact_entities(request(&empty)).is_err());
    for field in 0..7 {
        let mut req = request(&aliases);
        match field {
            0 => req.budget.max_results = 0,
            1 => req.budget.max_vm_steps = 0,
            2 => req.budget.max_request_bytes = 0,
            3 => req.budget.max_admission_rows = 0,
            4 => req.budget.max_row_bytes = 0,
            5 => req.budget.max_admission_bytes = 0,
            _ => req.budget.max_hydrated_bytes = 0,
        }
        assert!(store.read_exact_entities(req).is_err());
    }
    let mut req = request(&aliases);
    req.budget.max_vm_steps = u64::MAX;
    assert!(store.read_exact_entities(req).is_err());
    for id in ["z", "b", "a"] {
        store.upsert(&entry(id, "exact")).unwrap();
    }
    let mut req = request(&aliases);
    req.budget.max_results = 2;
    let result = store.read_exact_entities(req).unwrap();
    assert_eq!(result.status, ExactEntityReadStatus::ResultLimit);
    assert_eq!(ids(&result), ["a", "b"]);
}

#[test]
fn fresh_reopened_readonly_maintenance_and_private_connections_have_owned_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.db");
    let path = path.to_str().unwrap();
    let aliases = vec!["exact".into()];
    let fresh = MemoryStore::open(path).unwrap();
    assert_eq!(
        fresh.read_exact_entities(request(&aliases)).unwrap().status,
        ExactEntityReadStatus::Complete
    );
    drop(fresh);
    for store in [
        MemoryStore::open(path).unwrap(),
        MemoryStore::open_read_only(path).unwrap(),
        MemoryStore::open_existing_read_write(path).unwrap(),
        MemoryStore::open_in_memory().unwrap(),
        MemoryStore::open_private_image(
            None,
            crate::private_partition::AdmittedPartition {
                partition_id: "exact-reader-private-fixture".into(),
            },
        )
        .unwrap(),
    ] {
        let expected = db::StoreIdentity {
            db_label: store.db_label().into(),
            profile: store.store_profile(),
        };
        let mut req = request(&aliases);
        req.expected_store = &expected;
        req.expected_private_partition = store.admitted_partition.as_ref();
        let result = store.read_exact_entities(req).unwrap();
        assert_eq!(result.status, ExactEntityReadStatus::Complete);
        assert!(result.charged_vm_steps > 0);
    }
}

#[test]
fn admitted_physical_store_and_private_image_never_cross_request_selectors() {
    let dir = tempfile::tempdir().unwrap();
    let mut public = MemoryStore::open(dir.path().join("public.db").to_str().unwrap()).unwrap();
    let mut other = MemoryStore::open(dir.path().join("other.db").to_str().unwrap()).unwrap();
    let mut private = MemoryStore::open_private_image(
        None,
        crate::private_partition::AdmittedPartition {
            partition_id: "isolated-reader-subject".into(),
        },
    )
    .unwrap();
    public.upsert(&entry("public", "exact")).unwrap();
    other.upsert(&entry("other-project", "exact")).unwrap();
    private.upsert(&entry("private-subject", "exact")).unwrap();
    let aliases = vec!["exact".into()];
    for (store, expected) in [
        (&public, "public"),
        (&other, "other-project"),
        (&private, "private-subject"),
    ] {
        let mut req = request(&aliases);
        let expected_identity = db::StoreIdentity {
            db_label: store.db_label().into(),
            profile: store.store_profile(),
        };
        req.expected_store = &expected_identity;
        req.expected_private_partition = store.admitted_partition.as_ref();
        req.expected_physical_db_identity = store.opened_physical_db_identity();
        req.path_prefix = None;
        req.domain = None;
        req.include_archived = true;
        let result = store.read_exact_entities(req).unwrap();
        assert_eq!(result.status, ExactEntityReadStatus::Complete);
        assert_eq!(ids(&result), [expected]);
    }
    assert_ne!(
        public.opened_physical_db_identity(),
        other.opened_physical_db_identity()
    );
}

#[test]
fn aggregate_admission_and_hydration_ceiling_never_publish_partial_rows() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    store.upsert(&entry("a", "exact")).unwrap();
    store.upsert(&entry("b", "exact")).unwrap();
    let aliases = vec!["exact".into()];
    let mut req = request(&aliases);
    req.budget.max_admission_bytes = 1;
    let result = store.read_exact_entities(req).unwrap();
    assert_eq!(result.status, ExactEntityReadStatus::AdmissionBytes);
    assert!(result.entries.is_empty());
    let mut req = request(&aliases);
    req.budget.max_hydrated_bytes = 1;
    let result = store.read_exact_entities(req).unwrap();
    assert_eq!(result.status, ExactEntityReadStatus::HydratedBytes);
    assert!(result.entries.is_empty());
    // A caller's transaction stays open across a successful reader call.
    let tx = store.conn.unchecked_transaction().unwrap();
    assert_eq!(
        ids(&store.read_exact_entities(request(&aliases)).unwrap()),
        ["a", "b"]
    );
    assert!(!store.conn.is_autocommit());
    drop(tx);
    assert!(store.conn.is_autocommit());
}

#[test]
fn mismatched_role_profile_file_and_private_identity_refuse_before_sql() {
    let store = MemoryStore::open_in_memory().unwrap();
    let aliases = vec!["exact".into()];
    let other_label = db::StoreIdentity {
        db_label: "named-project:other".into(),
        profile: store.store_profile(),
    };
    let other_profile = db::StoreIdentity {
        db_label: store.db_label().into(),
        profile: match store.store_profile() {
            db::StoreProfile::PortableKernel => db::StoreProfile::TachiFull,
            db::StoreProfile::TachiFull => db::StoreProfile::PortableKernel,
        },
    };
    let private = crate::private_partition::AdmittedPartition {
        partition_id: "different-subject".into(),
    };
    let witness = store
        .reserved_reference_write
        .exact_witness
        .load(Ordering::SeqCst);
    for choice in 0..4 {
        let mut req = request(&aliases);
        match choice {
            0 => req.expected_store = &other_label,
            1 => req.expected_store = &other_profile,
            2 => req.expected_physical_db_identity = Some("other-file"),
            _ => req.expected_private_partition = Some(&private),
        }
        assert!(store.read_exact_entities(req).is_err());
        assert_eq!(
            store
                .reserved_reference_write
                .exact_witness
                .load(Ordering::SeqCst),
            witness
        );
    }
    let private_store = MemoryStore::open_private_image(None, private).unwrap();
    let identity = db::StoreIdentity {
        db_label: private_store.db_label().into(),
        profile: private_store.store_profile(),
    };
    let before = private_store
        .reserved_reference_write
        .exact_witness
        .load(Ordering::SeqCst);
    let mut req = request(&aliases);
    req.expected_store = &identity;
    assert!(private_store.read_exact_entities(req).is_err()); // None means public.
    assert_eq!(
        private_store
            .reserved_reference_write
            .exact_witness
            .load(Ordering::SeqCst),
        before
    );
}

#[test]
fn exhaustion_after_partial_admission_or_hydration_discards_every_row() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    let aliases = vec!["exact".into()];
    store.upsert(&entry("a", "exact")).unwrap();
    store.upsert(&entry("b", "exact")).unwrap();
    let full = store.read_exact_entities(request(&aliases)).unwrap();
    let mut req = request(&aliases);
    req.budget.max_admission_bytes = full.admission_bytes - 1;
    let result = store.read_exact_entities(req).unwrap();
    assert_eq!(result.status, ExactEntityReadStatus::AdmissionBytes);
    assert!(result.entries.is_empty());
    let mut req = request(&aliases);
    req.budget.max_hydrated_bytes = full.hydrated_bytes - 1;
    let result = store.read_exact_entities(req).unwrap();
    assert_eq!(result.status, ExactEntityReadStatus::HydratedBytes);
    assert!(result.entries.is_empty());
    assert!(
        result.hydrated_bytes > 0,
        "at least the first row was charged before refusal"
    );
}

#[test]
fn removed_callback_and_replaced_connection_refuse_without_reinstallation() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    let aliases = vec!["exact".into()];
    store
        .conn
        .progress_handler(0, None::<fn() -> bool>)
        .unwrap();
    assert_eq!(
        store.read_exact_entities(request(&aliases)).unwrap().status,
        ExactEntityReadStatus::CallbackUnavailable
    );
    // A fresh store supplies the owned connection for the extraction case.
    store = MemoryStore::open_in_memory().unwrap();
    let extracted = std::mem::replace(&mut store.conn, Connection::open_in_memory().unwrap());
    assert_eq!(
        store.read_exact_entities(request(&aliases)).unwrap().status,
        ExactEntityReadStatus::CallbackUnavailable
    );
    let state = Arc::clone(&store.reserved_reference_write);
    drop(store);
    // The original connection owns callback/authorizer Arc lifetimes even
    // after its MemoryStore is gone. Ordinary SQL does not advance the witness.
    let before = state.exact_witness.load(Ordering::SeqCst);
    assert_eq!(extracted.query_row("WITH RECURSIVE p(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM p WHERE n<128) SELECT max(n) FROM p",[],|r|r.get::<_,i64>(0)).unwrap(),128);
    assert_eq!(state.exact_witness.load(Ordering::SeqCst), before);
}

fn tolerant_request<'a>(
    aliases: &'a [String],
    max_diagnostic_ids: usize,
    max_diagnostic_bytes: usize,
) -> ExactEntityReadRequest<'a> {
    let mut req = request(aliases);
    req.validity_tolerance = ExactEntityValidityTolerance::SkipClassified {
        max_diagnostic_ids,
        max_diagnostic_bytes,
    };
    req
}

fn legacy_row(
    id: &str,
    valid_until: Option<&str>,
    superseded_by: Option<&str>,
) -> crate::PortableImportEntry {
    let mut e = entry(id, "exact");
    e.valid_until = valid_until.map(str::to_owned);
    crate::PortableImportEntry {
        entry: e,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-08-01T00:00:00Z".into(),
        superseded_by: superseded_by.map(str::to_owned),
    }
}

#[test]
fn tolerant_history_returns_entries_with_classified_skip_receipt() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    store
        .import_snapshot_batch(&[
            legacy_row("old-missing", None, Some("current")),
            legacy_row(
                "old-inverted",
                Some("2025-12-31T00:00:00Z"),
                Some("current"),
            ),
            legacy_row("good-old", Some("2026-07-01T00:00:00Z"), Some("current")),
            legacy_row("current", None, None),
        ])
        .unwrap();
    let aliases = vec!["exact".into()];
    // Strict history still refuses the whole read (frozen #2169 semantics).
    let mut strict = request(&aliases);
    strict.as_of = Some("2026-06-01T00:00:00Z");
    assert!(store
        .read_exact_entities(strict)
        .unwrap_err()
        .to_string()
        .contains("lacks historical validity boundary"));
    // The opted-in degraded read returns the two visible rows.
    let mut req = tolerant_request(&aliases, 16, 1024);
    req.as_of = Some("2026-06-01T00:00:00Z");
    let result = store.read_exact_entities(req).unwrap();
    assert_eq!(result.status, ExactEntityReadStatus::Complete);
    assert_eq!(ids(&result), ["current", "good-old"]);
    let skips = &result.validity_skips;
    assert_eq!(skips.invalid_windows, 1);
    assert_eq!(skips.missing_boundaries, 1);
    assert_eq!(skips.invalid_window_ids, ["old-inverted"]);
    assert_eq!(skips.missing_boundary_ids, ["old-missing"]);
    assert!(!skips.diagnostics_truncated);
    assert_eq!(skips.eligible_rows, 2);
    assert_eq!(skips.filtered_rows, 0);
    // Skipped rows still consume admission: the mutually exclusive classes
    // close exactly over the stepped candidate rows.
    assert_eq!(
        result.admission_rows,
        skips.filtered_rows
            + skips.invalid_windows
            + skips.missing_boundaries
            + skips.eligible_rows
    );
    // Skipping never repairs the row.
    assert!(store
        .get("old-missing")
        .unwrap()
        .unwrap()
        .valid_until
        .is_none());
}

#[test]
fn tolerant_current_read_skips_active_inverted_window() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    store
        .import_snapshot_batch(&[
            legacy_row("active-inverted", Some("2025-12-31T00:00:00Z"), None),
            legacy_row("current", None, None),
        ])
        .unwrap();
    let aliases = vec!["exact".into()];
    let strict = request(&aliases);
    assert!(store
        .read_exact_entities(strict)
        .unwrap_err()
        .to_string()
        .contains("invalid half-open validity window"));
    let result = store
        .read_exact_entities(tolerant_request(&aliases, 16, 1024))
        .unwrap();
    assert_eq!(result.status, ExactEntityReadStatus::Complete);
    assert_eq!(ids(&result), ["current"]);
    assert_eq!(result.validity_skips.invalid_windows, 1);
    assert_eq!(
        result.validity_skips.invalid_window_ids,
        ["active-inverted"]
    );
}

#[test]
fn tolerance_never_degrades_unclassified_errors() {
    // Only the two classified validity classes degrade: malformed payloads
    // and timestamps still fail the whole tolerant read.
    let mut store = MemoryStore::open_in_memory().unwrap();
    store
        .import_snapshot_batch(&[legacy_row("bad", None, None)])
        .unwrap();
    let aliases = vec!["exact".into()];
    for bad in ["not-json-entities", "[]-metadata", "tomorrow-valid-from"] {
        let (column, value) = match bad {
            "not-json-entities" => ("entities", "not-json"),
            "[]-metadata" => ("metadata", "[]"),
            _ => ("valid_from", "tomorrow"),
        };
        {
            // Seed malformed legacy data through the existing typed fixture
            // authority; disarm it before the reader is exercised.
            let _authorization =
                db::authorize_reserved_reference_write(&store.reserved_reference_write).unwrap();
            store
                .conn
                .execute(
                    &format!("UPDATE memories SET {column}=?1 WHERE id='bad'"),
                    [value],
                )
                .unwrap();
        }
        let mut req = tolerant_request(&aliases, 16, 1024);
        req.as_of = Some("2026-06-01T00:00:00Z");
        assert!(
            store.read_exact_entities(req).is_err(),
            "{bad} must still fail the whole tolerant read"
        );
        {
            let _authorization =
                db::authorize_reserved_reference_write(&store.reserved_reference_write).unwrap();
            let good = match column {
                "entities" => "[\"exact\"]",
                "metadata" => "{}",
                _ => "2026-01-01T00:00:00Z",
            };
            store
                .conn
                .execute(
                    &format!("UPDATE memories SET {column}=?1 WHERE id='bad'"),
                    [good],
                )
                .unwrap();
        }
    }
    let mut req = tolerant_request(&aliases, 16, 1024);
    req.as_of = Some("2026-06-01T00:00:00Z");
    assert_eq!(ids(&store.read_exact_entities(req).unwrap()), ["bad"]);
    // Zero diagnostic budgets are refused before any store access.
    let mut zero = tolerant_request(&aliases, 0, 1024);
    zero.as_of = Some("2026-06-01T00:00:00Z");
    assert!(store
        .read_exact_entities(zero)
        .unwrap_err()
        .to_string()
        .contains("zero diagnostic budget"));
    let mut zero = tolerant_request(&aliases, 16, 0);
    zero.as_of = Some("2026-06-01T00:00:00Z");
    assert!(store
        .read_exact_entities(zero)
        .unwrap_err()
        .to_string()
        .contains("zero diagnostic budget"));
}

#[test]
fn tolerance_does_not_name_namespace_scoped_rows() {
    // A row namespace scoping would have excluded is filtered, never a named
    // skip, even when its window is also broken: diagnostics do not widen
    // visibility beyond what the caller could have retrieved.
    let mut store = MemoryStore::open_in_memory().unwrap();
    let mut wiki = legacy_row(
        "wiki-inverted",
        Some("2025-12-31T00:00:00Z"),
        Some("current"),
    );
    wiki.entry.category = "guide".into();
    wiki.entry.metadata = serde_json::json!({"lifecycle":"pending_review"});
    store
        .import_snapshot_batch(&[
            wiki,
            legacy_row(
                "old-inverted",
                Some("2025-12-31T00:00:00Z"),
                Some("current"),
            ),
            legacy_row("current", None, None),
        ])
        .unwrap();
    let aliases = vec!["exact".into()];
    let mut req = tolerant_request(&aliases, 16, 1024);
    req.as_of = Some("2026-06-01T00:00:00Z");
    let result = store.read_exact_entities(req).unwrap();
    assert_eq!(ids(&result), ["current"]);
    let skips = &result.validity_skips;
    assert_eq!(skips.invalid_windows, 1);
    assert_eq!(skips.invalid_window_ids, ["old-inverted"]);
    assert_eq!(skips.missing_boundaries, 0);
    assert!(skips.missing_boundary_ids.is_empty());
    assert!(skips.filtered_rows >= 1);
}

#[test]
fn superseded_empty_window_stays_filtered_not_skipped() {
    // Frozen [t, t) invisibility is normal semantics, not corruption: the
    // tolerant read must not count or name such rows.
    let mut store = MemoryStore::open_in_memory().unwrap();
    let mut old = entry("empty", "exact");
    old.timestamp = "2026-08-05T04:27:15.656Z".into();
    old.valid_until = Some("2026-08-05T04:27:15.656Z".into());
    store
        .import_snapshot_batch(&[
            crate::PortableImportEntry {
                entry: old,
                created_at: "2026-08-05T04:27:15.656Z".into(),
                updated_at: "2026-08-05T04:27:15.656Z".into(),
                superseded_by: Some("current".into()),
            },
            crate::PortableImportEntry {
                entry: entry("current", "exact"),
                created_at: "2026-01-01T00:00:00Z".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
                superseded_by: None,
            },
        ])
        .unwrap();
    let aliases = vec!["exact".into()];
    for at in [
        None,
        Some("2026-08-05T04:27:15.656Z"),
        Some("2026-08-05T04:27:15.657Z"),
    ] {
        let mut req = tolerant_request(&aliases, 16, 1024);
        req.as_of = at;
        let result = store.read_exact_entities(req).unwrap();
        assert_eq!(result.status, ExactEntityReadStatus::Complete);
        assert_eq!(ids(&result), ["current"]);
        let skips = &result.validity_skips;
        assert_eq!((skips.invalid_windows, skips.missing_boundaries), (0, 0));
        assert!(skips.invalid_window_ids.is_empty());
        assert!(skips.missing_boundary_ids.is_empty());
        assert!(!skips.diagnostics_truncated);
    }
}

#[test]
fn skip_diagnostics_are_capped_but_counts_stay_exact() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    let mut rows: Vec<_> = ["a", "b", "c"]
        .iter()
        .map(|id| legacy_row(id, Some("2025-12-31T00:00:00Z"), Some("current")))
        .collect();
    rows.push(legacy_row("current", None, None));
    store.import_snapshot_batch(&rows).unwrap();
    let aliases = vec!["exact".into()];
    // Count cap: two ids retained, the third is only counted.
    let mut req = tolerant_request(&aliases, 2, 1024);
    req.as_of = Some("2026-06-01T00:00:00Z");
    let result = store.read_exact_entities(req).unwrap();
    assert_eq!(ids(&result), ["current"]);
    let skips = &result.validity_skips;
    assert_eq!(skips.invalid_windows, 3);
    assert_eq!(skips.invalid_window_ids.len(), 2);
    assert!(skips.diagnostics_truncated);
    // Byte cap: single-char ids, budget two bytes, no count cap.
    let mut req = tolerant_request(&aliases, 16, 2);
    req.as_of = Some("2026-06-01T00:00:00Z");
    let result = store.read_exact_entities(req).unwrap();
    let skips = &result.validity_skips;
    assert_eq!(skips.invalid_windows, 3);
    assert_eq!(skips.invalid_window_ids.len(), 2);
    assert!(skips.diagnostics_truncated);
    assert_eq!(
        result.admission_rows,
        skips.filtered_rows
            + skips.invalid_windows
            + skips.missing_boundaries
            + skips.eligible_rows
    );
}

#[test]
fn strict_time_invisible_rows_are_never_decoded() {
    // Regression (cold-review BUG): a row filtered by time visibility must not
    // fail the read because a classification column holds a hostile storage
    // class. Baseline filtered such rows before any id/path/category decode.
    let mut store = MemoryStore::open_in_memory().unwrap();
    store
        .import_snapshot_batch(&[
            legacy_row("future", None, None),
            legacy_row("current", None, None),
        ])
        .unwrap();
    {
        let _authorization =
            db::authorize_reserved_reference_write(&store.reserved_reference_write).unwrap();
        store
            .conn
            .execute(
                "UPDATE memories SET topic=X'61', valid_from='2099-01-01T00:00:00Z' WHERE id='future'",
                [],
            )
            .unwrap();
    }
    let aliases = vec!["exact".into()];
    for tolerance in [
        ExactEntityValidityTolerance::Reject,
        ExactEntityValidityTolerance::SkipClassified {
            max_diagnostic_ids: 8,
            max_diagnostic_bytes: 256,
        },
    ] {
        let mut req = request(&aliases);
        req.validity_tolerance = tolerance;
        req.as_of = Some("2026-06-01T00:00:00Z");
        let result = store.read_exact_entities(req).unwrap();
        assert_eq!(result.status, ExactEntityReadStatus::Complete);
        assert_eq!(ids(&result), ["current"]);
        assert_eq!(result.validity_skips.invalid_windows, 0);
        assert_eq!(result.validity_skips.missing_boundaries, 0);
    }
}

#[test]
fn diagnostic_byte_pool_is_shared_across_classes() {
    // The byte cap is one pool across both classes: an implementation that
    // budgets each class separately would retain both single-char ids without
    // reporting truncation.
    let mut store = MemoryStore::open_in_memory().unwrap();
    store
        .import_snapshot_batch(&[
            legacy_row("a", Some("2025-12-31T00:00:00Z"), Some("current")),
            legacy_row("b", None, Some("current")),
            legacy_row("current", None, None),
        ])
        .unwrap();
    let aliases = vec!["exact".into()];
    let mut req = tolerant_request(&aliases, 16, 1);
    req.as_of = Some("2026-06-01T00:00:00Z");
    let result = store.read_exact_entities(req).unwrap();
    assert_eq!(ids(&result), ["current"]);
    let skips = &result.validity_skips;
    assert_eq!(skips.invalid_windows, 1);
    assert_eq!(skips.missing_boundaries, 1);
    assert_eq!(skips.invalid_window_ids, ["a"]);
    assert!(skips.missing_boundary_ids.is_empty());
    assert!(skips.diagnostics_truncated);
}

#[test]
fn tolerant_reads_charge_skipped_rows_against_admission_budget() {
    // Interruption mid-scan still discards every row (admission-before-output)
    // and the skip already charged its admission row: the identity closes over
    // the scanned prefix exactly.
    let mut store = MemoryStore::open_in_memory().unwrap();
    store
        .import_snapshot_batch(&[
            legacy_row("a", Some("2025-12-31T00:00:00Z"), Some("current")),
            legacy_row("y", None, None),
            legacy_row("z", None, None),
        ])
        .unwrap();
    let aliases = vec!["exact".into()];
    let mut req = tolerant_request(&aliases, 16, 1024);
    req.as_of = Some("2026-06-01T00:00:00Z");
    req.budget.max_admission_rows = 2;
    let result = store.read_exact_entities(req).unwrap();
    assert_eq!(result.status, ExactEntityReadStatus::AdmissionRows);
    assert!(result.entries.is_empty());
    assert_eq!(result.admission_rows, 2);
    let skips = &result.validity_skips;
    assert_eq!(skips.invalid_windows, 1);
    assert_eq!(skips.invalid_window_ids, ["a"]);
    assert_eq!(skips.eligible_rows, 1);
    assert_eq!(
        result.admission_rows,
        skips.filtered_rows
            + skips.invalid_windows
            + skips.missing_boundaries
            + skips.eligible_rows
    );
}
