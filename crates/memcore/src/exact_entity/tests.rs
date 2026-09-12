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
