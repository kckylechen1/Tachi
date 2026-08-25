use std::{
    env,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use portable_kernel::{
    outbox_local_wins_successor_id, outbox_payload_digest, DanglingSupersession, LocalStoreStatus,
    MemoryEntry, MemoryError, MemoryStore, OutboxClaimKind, OutboxClaimRequest,
    OutboxConflictResolution, OutboxConflictResolutionReceipt, OutboxEventMeta, OutboxOutcome,
    OutboxOutcomeApplication, OutboxOutcomeEvidence, OutboxOutcomeRefusal, OutboxState,
    PortableImportEntry, PortableImportReceipt, RemoteSyncStatus, ADMIN_SURFACE_ENABLED,
    IS_PORTABLE_BUILD, OUTBOX_LOCAL_WINS_RESOLVED_CLASS,
};
use tempfile::tempdir;

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

/// #1119 remediation text, portable branch: memcore built without `admin`
/// must not prescribe a product-specific command. The feature bit cannot
/// know which shell embeds it or which migration operations that shell
/// supports, so the hint must remain product-neutral.
/// The admin branch is pinned by memcore's own Display test; workspace
/// feature unification prevents asserting both branches in one build, which
/// is why this lives in the isolated contract test.
#[test]
fn schema_migration_refusal_keeps_portable_remediation_product_neutral() {
    // Tripwire on the compile-time constant, deliberately (same shape as the
    // post-#1062 tripwire in exec_env_reaper): every assertion below is only
    // meaningful while this facade resolves memcore without `admin`.
    #[allow(clippy::assertions_on_constants)]
    {
        assert!(
            !ADMIN_SURFACE_ENABLED,
            "the portable-branch pin is only meaningful in a no-admin build"
        );
    }
    let err = MemoryError::SchemaMigrationOptInRequired {
        stored: 21,
        expected: 27,
        db_path: "/data/legacy.db".into(),
        backup_hint: "/data/legacy.db.migration-bak.<ts>".into(),
        marker_hint: "/data/legacy.db.migration-marker".into(),
    };
    let text = err.to_string();
    assert!(
        !text.contains("--allow-schema-migration to tachi-server"),
        "portable builds must not chase a flag their shell does not have: {text}"
    );
    assert!(
        text.contains("portable build")
            && text.contains("embedding product")
            && text.contains("documented schema-migration opt-in")
            && text.contains("documented migration procedure"),
        "portable builds must delegate remediation to the embedding product's docs: {text}"
    );
    assert!(
        !text.contains("portable-server") && !text.contains("hypermem migrate"),
        "portable memcore must not guess which product command can remediate the refusal: {text}"
    );
    assert!(
        text.contains("/data/legacy.db.migration-marker"),
        "the migration-trail hints must survive the remediation split: {text}"
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

const OUTBOX_CHILD_MODE_ENV: &str = "TACHI_PORTABLE_OUTBOX_CHILD_MODE";
const OUTBOX_CHILD_PATH_ENV: &str = "TACHI_PORTABLE_OUTBOX_CHILD_PATH";

fn run_outbox_child(test_name: &str, mode: &str, path: &Path) {
    let status = Command::new(env::current_exe().expect("portable test executable"))
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env(OUTBOX_CHILD_MODE_ENV, mode)
        .env(OUTBOX_CHILD_PATH_ENV, path)
        .status()
        .expect("spawn real outbox crash child");
    assert!(
        status.success(),
        "outbox crash child {mode} exited unsuccessfully: {status}"
    );
}

fn child_outbox_path() -> Option<String> {
    env::var(OUTBOX_CHILD_MODE_ENV)
        .ok()
        .map(|_| env::var(OUTBOX_CHILD_PATH_ENV).expect("child outbox path"))
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
        RemoteSyncStatus::Unconfigured,
        "the portable kernel has no configured remote transport"
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

/// A real child process exits after the production atomic commit returns.
/// The parent opens the same file from scratch and verifies both halves of the
/// commit survived; no dropped-handle shortcut can prove this boundary.
#[test]
fn portable_outbox_subprocess_crash_after_commit_reopens_pending_and_object() {
    if let Some(path) = child_outbox_path() {
        assert_eq!(env::var(OUTBOX_CHILD_MODE_ENV).as_deref(), Ok("commit"));
        let mut store = MemoryStore::open(&path).expect("child open file store");
        store
            .commit_with_outbox_event(
                &smoke_entry("portable-crash-commit-object"),
                &outbox_meta("portable-crash-commit-event"),
            )
            .expect("child local commit");
        std::process::exit(0);
    }

    let directory = tempdir().expect("temporary outbox directory");
    let path = directory.path().join("portable-crash-commit.sqlite");
    run_outbox_child(
        "portable_outbox_subprocess_crash_after_commit_reopens_pending_and_object",
        "commit",
        &path,
    );

    let path_string = path.to_string_lossy();
    let store = MemoryStore::open(&path_string).expect("parent reopen file store");
    let object = store
        .get("portable-crash-commit-object")
        .expect("reopen object")
        .expect("the locally committed object survives the child crash");
    assert_eq!(
        object.text,
        smoke_entry("portable-crash-commit-object").text
    );
    let event = store
        .outbox_event("portable-crash-commit-event")
        .expect("reopen event")
        .expect("the pending event survives the child crash");
    assert_eq!(event.state, OutboxState::Pending);

    let health = store
        .outbox_health_with_stale_after(Duration::from_secs(3600))
        .expect("explicit-bound health after reopen");
    assert_eq!(health.pending_count, 1);
    assert_eq!(health.in_flight_count, 0);
    assert_eq!(health.stale_lease_count, 0);
    assert_eq!(health.remote_sync_status, RemoteSyncStatus::Unconfigured);
}

/// A real child process exits after claiming and before reporting an outcome.
/// Reopen proves the lease is inspectable, a positive bound refuses the fresh
/// lease, expiry permits one takeover, and the immediately repeated claim is
/// refused before the event reaches its terminal outcome.
#[test]
fn portable_outbox_subprocess_claim_crash_reopens_and_reclaims_once() {
    let lease_bound = Duration::from_millis(250);
    if let Some(path) = child_outbox_path() {
        assert_eq!(env::var(OUTBOX_CHILD_MODE_ENV).as_deref(), Ok("claim"));
        let mut store = MemoryStore::open(&path).expect("child open file store");
        store
            .commit_with_outbox_event(
                &smoke_entry("portable-crash-claim-object"),
                &outbox_meta("portable-crash-claim-event"),
            )
            .expect("child local commit");
        let claimed = store
            .claim_outbox_events(&OutboxClaimRequest::first_claims_only(8))
            .expect("child claim");
        assert_eq!(claimed.len(), 1);
        assert!(
            store
                .claim_outbox_events(&OutboxClaimRequest::with_reclaim(8, lease_bound))
                .expect("child pre-expiry bounded reclaim")
                .is_empty(),
            "the child must refuse to reclaim its own fresh lease before simulated process loss"
        );
        std::process::exit(0);
    }

    let directory = tempdir().expect("temporary outbox directory");
    let path = directory.path().join("portable-crash-claim.sqlite");
    run_outbox_child(
        "portable_outbox_subprocess_claim_crash_reopens_and_reclaims_once",
        "claim",
        &path,
    );

    let path_string = path.to_string_lossy();
    let mut store = MemoryStore::open(&path_string).expect("parent reopen file store");
    let event = store
        .outbox_event("portable-crash-claim-event")
        .expect("reopen claimed event")
        .expect("claimed event survives the child crash");
    assert_eq!(event.state, OutboxState::InFlight);
    assert!(
        store
            .get("portable-crash-claim-object")
            .expect("reopen claimed object")
            .is_some(),
        "the source object remains locally readable while its event is leased"
    );

    let health_after_reopen = store
        .outbox_health_with_stale_after(lease_bound)
        .expect("explicit-bound health after reopen");
    assert_eq!(health_after_reopen.pending_count, 0);
    assert_eq!(health_after_reopen.in_flight_count, 1);
    assert_eq!(
        health_after_reopen.oldest_in_flight_at.as_deref(),
        Some(event.state_changed_at.as_str())
    );

    let expiry_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let health = store
            .outbox_health_with_stale_after(lease_bound)
            .expect("poll lease expiry");
        if health.stale_lease_count == 1 {
            break;
        }
        assert!(
            Instant::now() < expiry_deadline,
            "the explicit lease bound did not expire before the test deadline"
        );
        thread::sleep(Duration::from_millis(10));
    }

    let reclaimed = store
        .claim_outbox_events(&OutboxClaimRequest::with_reclaim(8, lease_bound))
        .expect("post-bound reclaim");
    assert_eq!(reclaimed.len(), 1);
    assert!(matches!(
        reclaimed[0].claim,
        OutboxClaimKind::Reclaimed { .. }
    ));
    assert_eq!(reclaimed[0].event.state, OutboxState::InFlight);
    assert!(
        store
            .claim_outbox_events(&OutboxClaimRequest::with_reclaim(8, lease_bound))
            .expect("immediate duplicate bounded reclaim")
            .is_empty(),
        "the takeover renews the lease and must refuse an immediate duplicate claim"
    );

    let object_before_outcome = store
        .get("portable-crash-claim-object")
        .expect("object before outcome")
        .expect("object before outcome present");
    let first = store
        .apply_outbox_outcome(
            "portable-crash-claim-event",
            &OutboxOutcome::Acknowledged,
            &OutboxOutcomeEvidence::from_reporter("portable-child-peer"),
        )
        .expect("terminal outcome after reclaim");
    let first_event = first.event.clone();
    let object_revision = object_before_outcome.revision;
    drop(store);
    let mut restarted = MemoryStore::open(&path_string).expect("restart after terminal outcome");
    let duplicate = restarted
        .apply_outbox_outcome(
            "portable-crash-claim-event",
            &OutboxOutcome::Acknowledged,
            &OutboxOutcomeEvidence::from_reporter("portable-child-peer"),
        )
        .expect("duplicate terminal outcome after reopen");
    assert_eq!(first.application, OutboxOutcomeApplication::Applied);
    assert_eq!(
        duplicate.application,
        OutboxOutcomeApplication::AlreadyApplied
    );
    assert_eq!(duplicate.event, first_event);
    assert_eq!(
        restarted
            .get("portable-crash-claim-object")
            .expect("object after outcome")
            .expect("object after outcome present")
            .revision,
        object_revision,
        "outbox replay must not rewrite the local memory revision"
    );
    assert!(
        restarted
            .claim_outbox_events(&OutboxClaimRequest::with_reclaim(8, lease_bound))
            .expect("terminal event reclaim probe")
            .is_empty(),
        "a terminal event is never returned to pending or reclaimed"
    );
}

/// Every state is counted exactly once through the public portable seams. The
/// fixture also checks the aggregate serialization is content/private-id
/// negative and that an explicit nonzero bound reports no fresh stale lease.
#[test]
fn portable_outbox_health_inventory_is_exact_and_content_free() {
    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
    for index in 0..6 {
        store
            .commit_with_outbox_event(
                &smoke_entry(&format!("portable-health-object-{index}")),
                &outbox_meta(&format!("portable-health-event-{index}")),
            )
            .expect("health fixture commit");
    }

    let claimed = store
        .claim_outbox_events(&OutboxClaimRequest::first_claims_only(5))
        .expect("claim five fixture events");
    assert_eq!(claimed.len(), 5);
    store
        .apply_outbox_outcome(
            "portable-health-event-0",
            &OutboxOutcome::Acknowledged,
            &OutboxOutcomeEvidence::from_reporter("portable-health-peer"),
        )
        .expect("acknowledge fixture event");
    store
        .apply_outbox_outcome(
            "portable-health-event-1",
            &OutboxOutcome::Rejected {
                error_class: "schema_refused".into(),
            },
            &OutboxOutcomeEvidence::from_reporter("portable-health-peer"),
        )
        .expect("reject fixture event");
    store
        .apply_outbox_outcome(
            "portable-health-event-2",
            &OutboxOutcome::Conflicted {
                error_class: "divergent_revision".into(),
            },
            &OutboxOutcomeEvidence::from_reporter("portable-health-peer"),
        )
        .expect("conflict fixture event");
    store
        .transition_outbox_event(
            "portable-health-event-3",
            OutboxState::Quarantined,
            Some("operator_hold"),
        )
        .expect("quarantine fixture event");

    let health = store
        .outbox_health_with_stale_after(Duration::from_secs(3600))
        .expect("explicit-bound health inventory");
    assert_eq!(health.pending_count, 1);
    assert_eq!(health.in_flight_count, 1);
    assert_eq!(health.acknowledged_count, 1);
    assert_eq!(health.rejected_count, 1);
    assert_eq!(health.conflicted_count, 1);
    assert_eq!(health.quarantined_count, 1);
    assert_eq!(health.resolved_count, 0);
    assert_eq!(health.stale_lease_count, 0);
    assert!(health.oldest_pending_at.is_some());
    assert!(health.oldest_in_flight_at.is_some());
    assert_eq!(health.remote_sync_status, RemoteSyncStatus::Unconfigured);
    assert_eq!(
        health.local_store_status,
        LocalStoreStatus::Quarantined {
            quarantined_count: 1
        }
    );
    let serialized = serde_json::to_string(&health).expect("serialize health");
    for forbidden in [
        "portable-health-event-",
        "portable-health-object-",
        "portable batch smoke",
        "portable-health-peer",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "health serialization leaked forbidden content/private id: {forbidden}"
        );
    }
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
        RemoteSyncStatus::Unconfigured,
        "no remote exists in this leaf, so the status is explicitly unconfigured"
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
    assert_eq!(drained.remote_sync_status, RemoteSyncStatus::Unconfigured);
    assert_eq!(drained.pending_count, 0);
    assert_eq!(drained.oldest_pending_at, None);
    assert_eq!(
        drained.last_successful_sync, None,
        "a local acknowledgement cannot fabricate remote success"
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

/// tachi#1644: the reconciliation protocol is portable surface for the same
/// reason A1's outbox is — #1630's premise is a host-owned sync loop with no
/// Tachi daemon, so a portable build must be able to claim, report outcomes and
/// resolve conflicts on its own.
#[test]
fn portable_build_outbox_reconciliation_protocol() {
    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
    store
        .commit_with_outbox_event(
            &smoke_entry("portable-outbox-a2"),
            &outbox_meta("portable-evt-a2"),
        )
        .expect("commit");

    // Claim: pending -> in_flight, with the digest a push leg would transmit.
    let claimed = store
        .claim_outbox_events(&OutboxClaimRequest::first_claims_only(8))
        .expect("claim");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].event.state, OutboxState::InFlight);
    assert_eq!(claimed[0].claim, OutboxClaimKind::First);
    let stored = store
        .get("portable-outbox-a2")
        .expect("get")
        .expect("present");
    assert_eq!(
        claimed[0].event.payload_digest,
        outbox_payload_digest(&stored).expect("digest")
    );

    // A consumer reports divergence. The kernel does not resolve it.
    let conflicted = store
        .apply_outbox_outcome(
            "portable-evt-a2",
            &OutboxOutcome::Conflicted {
                error_class: "divergent_revision".into(),
            },
            &OutboxOutcomeEvidence::from_reporter("portable_peer"),
        )
        .expect("conflict");
    assert_eq!(conflicted.application, OutboxOutcomeApplication::Applied);
    assert_eq!(conflicted.prior_state, OutboxState::InFlight);
    assert_eq!(conflicted.event.state, OutboxState::Conflicted);

    // An outcome for an event nobody was handed is a typed protocol refusal.
    store
        .commit_with_outbox_event(
            &smoke_entry("portable-outbox-a2-b"),
            &outbox_meta("portable-evt-a2-b"),
        )
        .expect("second commit");
    let refusal = store
        .apply_outbox_outcome(
            "portable-evt-a2-b",
            &OutboxOutcome::Acknowledged,
            &OutboxOutcomeEvidence::from_reporter("portable_peer"),
        )
        .expect_err("an outcome for a never-claimed event must be refused");
    match &refusal {
        MemoryError::OutboxOutcomeRefused { reason, state, .. } => {
            assert_eq!(*reason, OutboxOutcomeRefusal::NeverClaimed);
            assert_eq!(state, "pending");
        }
        other => panic!("unexpected error variant: {other:?}"),
    }

    // The explicit decision: the local mutation stands, carried by a NEW event.
    let receipt = store
        .resolve_outbox_conflict("portable-evt-a2", &OutboxConflictResolution::LocalWins)
        .expect("local wins");
    let successor = match receipt {
        OutboxConflictResolutionReceipt::LocalWins {
            resolved,
            successor,
        } => {
            assert_eq!(resolved.state, OutboxState::Quarantined);
            assert_eq!(
                resolved.last_error_class.as_deref(),
                Some(OUTBOX_LOCAL_WINS_RESOLVED_CLASS)
            );
            successor
        }
        other => panic!("unexpected receipt shape: {other:?}"),
    };
    assert_eq!(
        successor.event_id,
        outbox_local_wins_successor_id("portable-evt-a2")
    );
    assert_eq!(successor.state, OutboxState::Pending);

    // Duplicate acknowledgement of the successor is an idempotent no-op.
    store
        .claim_outbox_events(&OutboxClaimRequest::first_claims_only(8))
        .expect("claim the successor");
    let first = store
        .apply_outbox_outcome(
            &successor.event_id,
            &OutboxOutcome::Acknowledged,
            &OutboxOutcomeEvidence::from_reporter("portable_peer"),
        )
        .expect("acknowledge");
    let second = store
        .apply_outbox_outcome(
            &successor.event_id,
            &OutboxOutcome::Acknowledged,
            &OutboxOutcomeEvidence::from_reporter("portable_peer"),
        )
        .expect("a duplicate acknowledgement is not an error");
    assert_eq!(first.application, OutboxOutcomeApplication::Applied);
    assert_eq!(second.application, OutboxOutcomeApplication::AlreadyApplied);
    assert_eq!(second.event, first.event);
}
