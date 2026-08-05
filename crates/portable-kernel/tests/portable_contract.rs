use portable_kernel::{
    outbox_payload_digest, DanglingSupersession, LocalStoreStatus, MemoryEntry, MemoryError,
    MemoryStore, OutboxEventMeta, OutboxState, PortableImportEntry, PortableImportReceipt,
    RemoteSyncStatus, ADMIN_SURFACE_ENABLED, IS_PORTABLE_BUILD,
};

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

fn snapshot_row(id: &str, text: &str) -> PortableImportEntry {
    let mut entry = smoke_entry(id);
    entry.text = text.into();
    PortableImportEntry {
        entry,
        created_at: "2025-03-04T05:06:07.000Z".into(),
        updated_at: "2025-09-10T11:12:13.000Z".into(),
        superseded_by: None,
    }
}

/// tachi#1607: `import_snapshot_batch` must be callable — and resolve — in
/// the genuinely no-admin shape, and it must carry the lifecycle columns
/// `MemoryEntry` cannot. The six row shapes from the issue's acceptance are
/// all here; the destination-side checksums in the receipt are reconciled
/// against source-side values computed without touching the database, which
/// is the parity check a consumer with no raw destination SQL has to rely on.
#[test]
fn portable_build_import_snapshot_batch_preserves_lifecycle() {
    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");

    // 1: active, unsuperseded.
    let active = snapshot_row("portable-snap-active", "portable snapshot active row");
    // 2: archived (and superseded, the ordinary shape).
    let mut archived = snapshot_row("portable-snap-archived", "portable snapshot archived row");
    archived.entry.archived = true;
    archived.entry.revision = 5;
    archived.entry.valid_until = Some("2025-05-05T00:00:00.000Z".into());
    archived.superseded_by = Some("portable-snap-active".into());
    // 3: active AND superseded — the shape a replay through supersede_memory
    // cannot reproduce.
    let mut active_superseded = snapshot_row(
        "portable-snap-active-superseded",
        "portable snapshot active superseded row",
    );
    active_superseded.entry.revision = 4;
    active_superseded.entry.valid_until = Some("2025-06-06T00:00:00.000Z".into());
    active_superseded.superseded_by = Some("portable-snap-active".into());
    // 4: superseded with a null valid_until.
    let mut null_valid_until = snapshot_row(
        "portable-snap-null-valid-until",
        "portable snapshot null valid until row",
    );
    null_valid_until.entry.valid_until = None;
    null_valid_until.superseded_by = Some("portable-snap-active".into());
    // 5: vector present. 6: vector missing.
    let mut with_vector = snapshot_row("portable-snap-vector", "portable snapshot vector row");
    with_vector.entry.vector = Some(vec![0.375_f32; 1024]);
    let without_vector = snapshot_row("portable-snap-no-vector", "portable snapshot no vector row");

    let batch = vec![
        active,
        archived,
        active_superseded,
        null_valid_until,
        with_vector,
        without_vector,
    ];
    let receipt = store
        .import_snapshot_batch(&batch)
        .expect("import_snapshot_batch must succeed on a portable-build store");

    assert_eq!(receipt.rows_imported, 6);
    assert_eq!(receipt.vectors_imported, 1);
    assert_eq!(receipt.vectors_absent, 5);
    assert!(
        receipt.dangling_supersessions.is_empty(),
        "every edge in this batch resolves: {:?}",
        receipt.dangling_supersessions
    );
    assert_eq!(
        receipt.lifecycle_checksum,
        PortableImportReceipt::expected_lifecycle_checksum(&batch).expect("expected lifecycle"),
        "the destination's stored lifecycle must reconcile with the source side"
    );
    assert_eq!(
        receipt.vector_checksum,
        PortableImportReceipt::expected_vector_checksum(&batch).expect("expected vector"),
        "the destination's stored vectors must reconcile with the source side"
    );

    // Portable readback of the fields `MemoryEntry` does carry. Archive
    // visibility must be on: `get` alone hides the archived row, and hiding it
    // is exactly the state this import has to have preserved.
    for import in &batch {
        let stored = store
            .get_with_options(&import.entry.id, true)
            .expect("get_with_options")
            .unwrap_or_else(|| panic!("row {} must exist", import.entry.id));
        assert_eq!(stored.archived, import.entry.archived);
        assert_eq!(stored.revision, import.entry.revision);
        assert_eq!(stored.valid_until, import.entry.valid_until);
    }
}

#[test]
fn portable_build_import_snapshot_batch_reports_dangling_and_refuses_existing_ids() {
    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");

    let mut dangling = snapshot_row("portable-snap-dangling", "portable snapshot dangling row");
    dangling.superseded_by = Some("portable-snap-absent".into());
    let receipt = store
        .import_snapshot_batch(&[dangling])
        .expect("a dangling edge is preserved, not rejected");
    assert_eq!(
        receipt.dangling_supersessions,
        vec![DanglingSupersession {
            id: "portable-snap-dangling".into(),
            superseded_by: "portable-snap-absent".into(),
        }]
    );

    let error = store
        .import_snapshot_batch(&[
            snapshot_row("portable-snap-second", "portable snapshot second row"),
            snapshot_row("portable-snap-dangling", "portable snapshot colliding row"),
        ])
        .expect_err("an id already in the destination must be refused");
    assert!(
        matches!(error, MemoryError::Duplicate(_)),
        "unexpected error variant: {error:?}"
    );
    assert!(
        store.get("portable-snap-second").expect("get").is_none(),
        "the refused batch must not leave its earlier row behind"
    );

    let empty = store
        .import_snapshot_batch(&[])
        .expect("empty import_snapshot_batch is a successful no-op");
    assert_eq!(empty.rows_imported, 0);
    assert_eq!(
        empty.lifecycle_checksum,
        PortableImportReceipt::expected_lifecycle_checksum(&[]).expect("expected lifecycle")
    );
}

fn outbox_meta(event_id: &str) -> OutboxEventMeta {
    OutboxEventMeta {
        event_id: event_id.into(),
        object_class: "memory".into(),
        authority_class: "host".into(),
        source_store: "portable".into(),
        source_partition: "default".into(),
    }
}

/// tachi#1643: the durable outbox is PORTABLE surface, so the whole leaf —
/// commit boundary, state machine, health read model — must be callable and
/// resolve in a build that has genuinely disabled the admin feature. #1630's
/// premise is a host-owned sync loop with no Tachi daemon, so an outbox that
/// only worked in the product build would miss its only consumer. Compiled in
/// isolation via `required-features = ["portable-contract-test"]` so workspace
/// feature unification cannot mask an accidental admin dependency.
///
/// Note what this test cannot use: `MemoryStore::connection()` is absent
/// outside admin/test builds, so every assertion here goes through the same
/// public API an external portable consumer has. That is the point.
#[test]
fn portable_build_commit_with_outbox_event_is_atomic() {
    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
    assert_eq!(
        store.outbox_health().expect("health").remote_sync_status,
        RemoteSyncStatus::Idle,
        "an untouched outbox has never held an event"
    );

    let receipt = store
        .commit_with_outbox_event(
            &smoke_entry("portable-outbox-1"),
            &outbox_meta("portable-evt-1"),
        )
        .expect("commit_with_outbox_event must succeed on a portable-build store");
    assert_eq!(receipt.event.state, OutboxState::Pending);
    assert_eq!(receipt.event.object_id, "portable-outbox-1");
    assert_eq!(receipt.event.source_revision, receipt.object_revision);

    let stored = store
        .get("portable-outbox-1")
        .expect("get")
        .expect("the committed row must be visible");
    assert_eq!(
        receipt.event.payload_digest,
        outbox_payload_digest(&stored).expect("digest"),
        "the event records the digest of the payload as stored"
    );

    // A duplicate event_id fails between the memory write and the event
    // insert, so neither half may survive.
    let error = store
        .commit_with_outbox_event(
            &smoke_entry("portable-outbox-2"),
            &outbox_meta("portable-evt-1"),
        )
        .expect_err("a duplicate event_id must fail the commit");
    assert!(
        matches!(error, MemoryError::Duplicate(_)),
        "unexpected error variant: {error:?}"
    );
    assert!(
        store.get("portable-outbox-2").expect("get").is_none(),
        "the memory write must roll back with its failed event"
    );
    assert_eq!(
        store
            .list_outbox_events(OutboxState::Pending, 16)
            .expect("list pending")
            .len(),
        1,
        "only the first commit's event may exist"
    );
}

/// The typed state machine and the six health fields, over the portable API.
#[test]
fn portable_build_outbox_state_machine_and_health() {
    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
    store
        .commit_with_outbox_event(
            &smoke_entry("portable-outbox-fsm"),
            &outbox_meta("portable-evt-fsm"),
        )
        .expect("commit");

    let error = store
        .transition_outbox_event("portable-evt-fsm", OutboxState::Acknowledged, None)
        .expect_err("pending -> acknowledged skips in_flight and must be refused");
    assert!(
        matches!(error, MemoryError::OutboxIllegalTransition { .. }),
        "unexpected error variant: {error:?}"
    );

    let backlog = store.outbox_health().expect("health");
    assert_eq!(backlog.pending_count, 1);
    assert_eq!(
        backlog.remote_sync_status,
        RemoteSyncStatus::Backlogged { pending_count: 1 },
        "no remote exists in this leaf, so a committed mutation rests as backlog"
    );
    assert!(
        backlog.oldest_pending_at.is_some(),
        "a pending event must carry an oldest-pending stamp"
    );
    assert_eq!(backlog.last_successful_sync, None);
    assert_eq!(backlog.last_error_class, None);
    assert_eq!(backlog.local_store_status, LocalStoreStatus::Healthy);

    store
        .transition_outbox_event("portable-evt-fsm", OutboxState::InFlight, None)
        .expect("pending -> in_flight");
    let acknowledged = store
        .transition_outbox_event("portable-evt-fsm", OutboxState::Acknowledged, None)
        .expect("in_flight -> acknowledged");
    assert_eq!(acknowledged.state, OutboxState::Acknowledged);
    assert_eq!(acknowledged.last_error_class, None);

    let drained = store.outbox_health().expect("health");
    assert_eq!(drained.remote_sync_status, RemoteSyncStatus::Drained);
    assert_eq!(drained.pending_count, 0);
    assert_eq!(drained.oldest_pending_at, None);
    assert_eq!(
        drained.last_successful_sync.as_deref(),
        Some(acknowledged.state_changed_at.as_str())
    );

    // Quarantine is reachable from any state and requires its class; it is
    // reported as a local-store condition, not a remote-sync one.
    assert!(
        store
            .transition_outbox_event("portable-evt-fsm", OutboxState::Quarantined, None)
            .is_err(),
        "a failure state without its class must be refused"
    );
    store
        .transition_outbox_event(
            "portable-evt-fsm",
            OutboxState::Quarantined,
            Some("operator_hold"),
        )
        .expect("acknowledged -> quarantined");
    let held = store.outbox_health().expect("health");
    assert_eq!(
        held.local_store_status,
        LocalStoreStatus::Quarantined {
            quarantined_count: 1
        }
    );
    assert_eq!(held.last_error_class.as_deref(), Some("operator_hold"));
}
