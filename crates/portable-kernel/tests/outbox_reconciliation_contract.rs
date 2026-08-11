//! #1666 / #1630-A4 portable outbox and reconciliation conformance.
//!
//! This target uses two independently created, file-backed `PortableKernel`
//! stores.  Transport only assembles the public #1718 envelope; destination
//! reconciliation is always driven by `apply_outbox_destination`.

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use portable_kernel::store::outbox_protocol::OUTBOX_REMOTE_WINS_RESOLVED_CLASS_PREFIX;
use portable_kernel::{
    outbox_local_wins_successor_id, outbox_payload_digest, ClaimedOutboxEvent, DbOpenContext,
    LocalStoreStatus, MemoryEntry, MemoryError, MemoryStore, OutboxClaimKind, OutboxClaimRequest,
    OutboxConflictResolution, OutboxConflictResolutionReceipt, OutboxDestinationApplyApplication,
    OutboxDestinationApplyEnvelope, OutboxDestinationApplyReceipt, OutboxDestinationApplyResult,
    OutboxDestinationConflictReason, OutboxDestinationIdentity, OutboxEventMeta, OutboxOutcome,
    OutboxOutcomeApplication, OutboxOutcomeEvidence, OutboxOutcomeRefusal, OutboxState,
    RemoteSyncStatus, StoreProfile, ADMIN_SURFACE_ENABLED, IS_PORTABLE_BUILD,
    OUTBOX_LOCAL_WINS_RESOLVED_CLASS,
};
use tempfile::{tempdir, TempDir};

const SOURCE_STORE: &str = "outbox-source-a";
const DESTINATION_STORE: &str = "outbox-destination-b";
const SOURCE_PARTITION: &str = "source-partition-a";
const DESTINATION_PARTITION: &str = "destination-partition-b";
const HEALTH_STALE_AFTER: Duration = Duration::from_secs(3_600);
const LEASE_BOUND: Duration = Duration::from_millis(250);
const CHILD_MODE_ENV: &str = "TACHI_PORTABLE_RECONCILIATION_CHILD_MODE";
const CHILD_PATH_ENV: &str = "TACHI_PORTABLE_RECONCILIATION_CHILD_PATH";

const _: () = {
    assert!(!ADMIN_SURFACE_ENABLED);
    assert!(IS_PORTABLE_BUILD);
};

fn entry(id: &str, text: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.into(),
        path: "/scratch/outbox-conformance".into(),
        summary: format!("{id} summary"),
        text: text.into(),
        importance: 0.7,
        timestamp: "2026-08-10T00:00:00.000Z".into(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".into(),
        topic: "outbox-conformance".into(),
        keywords: vec!["outbox".into(), "portable".into()],
        persons: vec![],
        entities: vec!["portable-kernel".into()],
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
        metadata: serde_json::json!({"contract": "1666"}),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".into(),
    }
}

fn meta(event_id: &str) -> OutboxEventMeta {
    OutboxEventMeta {
        event_id: event_id.into(),
        object_class: "memory".into(),
        authority_class: "host".into(),
        source_store: SOURCE_STORE.into(),
        source_partition: SOURCE_PARTITION.into(),
    }
}

fn open_fresh(path: &Path, label: &str) -> MemoryStore {
    let path = path.to_string_lossy();
    MemoryStore::open_with_label_and_context(
        &path,
        label,
        &DbOpenContext::create_fresh().with_profile(StoreProfile::PortableKernel),
    )
    .unwrap_or_else(|error| panic!("open fresh PortableKernel store {label}: {error}"))
}

fn open_existing(path: &Path, label: &str) -> MemoryStore {
    let path = path.to_string_lossy();
    MemoryStore::open_with_label_and_context(
        &path,
        label,
        &DbOpenContext::open_existing_deny().with_profile(StoreProfile::PortableKernel),
    )
    .unwrap_or_else(|error| panic!("reopen PortableKernel store {label}: {error}"))
}

fn fresh_pair() -> (TempDir, MemoryStore, MemoryStore) {
    let directory = tempdir().expect("outbox conformance tempdir");
    let source = open_fresh(&directory.path().join("source.sqlite"), SOURCE_STORE);
    let destination = open_fresh(
        &directory.path().join("destination.sqlite"),
        DESTINATION_STORE,
    );
    assert_eq!(source.db_label(), SOURCE_STORE);
    assert_eq!(destination.db_label(), DESTINATION_STORE);
    assert_eq!(source.store_profile(), StoreProfile::PortableKernel);
    assert_eq!(destination.store_profile(), StoreProfile::PortableKernel);
    assert_ne!(source.db_label(), destination.db_label());
    assert_ne!(
        source.opened_physical_db_identity(),
        destination.opened_physical_db_identity()
    );
    (directory, source, destination)
}

fn claim_one(source: &mut MemoryStore, event_id: &str) -> ClaimedOutboxEvent {
    let claimed = source
        .claim_outbox_events(&OutboxClaimRequest::first_claims_only(16))
        .expect("claim source event");
    let item = claimed
        .into_iter()
        .find(|item| item.event.event_id == event_id)
        .expect("expected claimed event");
    assert_eq!(item.event.state, OutboxState::InFlight);
    assert_eq!(item.claim, OutboxClaimKind::First);
    item
}

/// Transport may only read the source payload and assemble a public envelope.
fn transport_envelope(
    source: &MemoryStore,
    claimed: ClaimedOutboxEvent,
) -> OutboxDestinationApplyEnvelope {
    let payload = source
        .get(&claimed.event.object_id)
        .expect("source payload read")
        .expect("claimed source payload");
    assert_eq!(payload.id, claimed.event.object_id);
    assert_eq!(payload.revision, claimed.event.source_revision);
    assert_eq!(
        outbox_payload_digest(&payload).expect("source payload digest"),
        claimed.event.payload_digest
    );
    OutboxDestinationApplyEnvelope {
        claimed,
        payload,
        destination: OutboxDestinationIdentity {
            store: DESTINATION_STORE.into(),
            partition: DESTINATION_PARTITION.into(),
        },
    }
}

/// Counts are ordered as pending, in-flight, acknowledged, rejected,
/// conflicted, quarantined, resolved, stale-lease.
fn assert_health(store: &MemoryStore, expected: [u64; 8]) {
    let [pending, in_flight, acknowledged, rejected, conflicted, quarantined, resolved, stale] =
        expected;
    let health = store
        .outbox_health_with_stale_after(HEALTH_STALE_AFTER)
        .expect("explicit-bound health");
    assert_eq!(health.pending_count, pending);
    assert_eq!(health.in_flight_count, in_flight);
    assert_eq!(health.acknowledged_count, acknowledged);
    assert_eq!(health.rejected_count, rejected);
    assert_eq!(health.conflicted_count, conflicted);
    assert_eq!(health.quarantined_count, quarantined);
    assert_eq!(health.resolved_count, resolved);
    assert_eq!(health.stale_lease_count, stale);
    assert_eq!(health.remote_sync_status, RemoteSyncStatus::Unconfigured);
}

fn assert_destination_receipt(
    receipt: &OutboxDestinationApplyReceipt,
    envelope: &OutboxDestinationApplyEnvelope,
    destination: &MemoryStore,
    application: OutboxDestinationApplyApplication,
) {
    let object = destination
        .get(&envelope.claimed.event.object_id)
        .expect("destination readback")
        .expect("destination object exists for receipt");
    let digest = outbox_payload_digest(&object).expect("destination readback digest");
    assert_eq!(receipt.event_id, envelope.claimed.event.event_id);
    assert_eq!(receipt.object_id, envelope.claimed.event.object_id);
    assert_eq!(receipt.source_store, envelope.claimed.event.source_store);
    assert_eq!(
        receipt.source_partition,
        envelope.claimed.event.source_partition
    );
    assert_eq!(
        receipt.source_revision,
        envelope.claimed.event.source_revision
    );
    assert_eq!(
        receipt.source_payload_digest,
        envelope.claimed.event.payload_digest
    );
    assert_eq!(receipt.destination_store, envelope.destination.store);
    assert_eq!(
        receipt.destination_partition,
        envelope.destination.partition
    );
    assert_eq!(receipt.destination_object_revision, object.revision);
    assert_eq!(receipt.destination_payload_digest, digest);
    assert_eq!(receipt.application, application);
    assert_eq!(object.id, receipt.object_id);
    assert_eq!(object.revision, receipt.source_revision);
    assert_eq!(digest, receipt.source_payload_digest);
}

/// Project only the production ack-evidence fields after every durable
/// destination receipt binding has been read back and checked.  The public
/// source outcome API carries reporter, peer revision, and peer digest; the
/// remaining receipt fields are enforced by `assert_destination_receipt`
/// before this projection can be passed to `apply_outbox_outcome`.
fn ack_evidence_from_durable_receipt(
    receipt: &OutboxDestinationApplyReceipt,
    envelope: &OutboxDestinationApplyEnvelope,
    destination: &MemoryStore,
    application: OutboxDestinationApplyApplication,
) -> OutboxOutcomeEvidence {
    assert_destination_receipt(receipt, envelope, destination, application);
    OutboxOutcomeEvidence {
        reported_by: receipt.destination_store.clone(),
        peer_revision: Some(receipt.destination_object_revision),
        peer_payload_digest: Some(receipt.destination_payload_digest.clone()),
    }
}

fn assert_destination_conflict(
    receipt: &portable_kernel::OutboxDestinationConflictReceipt,
    envelope: &OutboxDestinationApplyEnvelope,
    destination: &MemoryStore,
    reason: OutboxDestinationConflictReason,
) {
    let object = destination
        .get(&envelope.claimed.event.object_id)
        .expect("destination conflict readback")
        .expect("divergent destination object");
    assert_eq!(receipt.event_id, envelope.claimed.event.event_id);
    assert_eq!(receipt.object_id, envelope.claimed.event.object_id);
    assert_eq!(receipt.source_store, envelope.claimed.event.source_store);
    assert_eq!(
        receipt.source_partition,
        envelope.claimed.event.source_partition
    );
    assert_eq!(
        receipt.source_revision,
        envelope.claimed.event.source_revision
    );
    assert_eq!(
        receipt.source_payload_digest,
        envelope.claimed.event.payload_digest
    );
    assert_eq!(receipt.destination_store, envelope.destination.store);
    assert_eq!(
        receipt.destination_partition,
        envelope.destination.partition
    );
    assert_eq!(receipt.destination_object_revision, object.revision);
    assert_eq!(
        receipt.destination_payload_digest,
        outbox_payload_digest(&object).expect("destination conflict digest")
    );
    assert_eq!(receipt.reason, reason);
}

fn assert_destination_refusal_before_mutation(
    envelope: &OutboxDestinationApplyEnvelope,
    expected_error: &str,
    mutate: impl FnOnce(&mut OutboxDestinationApplyEnvelope),
) {
    let directory = tempdir().expect("destination refusal tempdir");
    let path = directory.path().join("destination.sqlite");
    let mut destination = open_fresh(&path, DESTINATION_STORE);
    let mut malformed = envelope.clone();
    mutate(&mut malformed);
    let error = destination
        .apply_outbox_destination(&malformed)
        .expect_err("malformed destination envelope must refuse");
    assert!(
        error.to_string().contains(expected_error),
        "expected {expected_error} in {error}"
    );
    assert!(destination
        .get(&envelope.claimed.event.object_id)
        .expect("destination read after refusal")
        .is_none());
    assert_health(&destination, [0, 0, 0, 0, 0, 0, 0, 0]);
    let mut valid = envelope.clone();
    valid.destination.partition = "post-refusal-partition".into();
    let applied = match destination
        .apply_outbox_destination(&valid)
        .expect("valid envelope after refusal")
    {
        OutboxDestinationApplyResult::Applied(receipt) => receipt,
        other => panic!("expected production Applied after refusal, got {other:?}"),
    };
    assert_destination_receipt(
        &applied,
        &valid,
        &destination,
        OutboxDestinationApplyApplication::Applied,
    );
}

fn run_child(test_name: &str, mode: &str, path: &Path) {
    let status = Command::new(env::current_exe().expect("reconciliation test executable"))
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env(CHILD_MODE_ENV, mode)
        .env(CHILD_PATH_ENV, path)
        .status()
        .expect("spawn real outbox process-loss child");
    assert!(
        status.success(),
        "outbox child {mode} exited unsuccessfully: {status}"
    );
}

fn child_path() -> Option<PathBuf> {
    env::var_os(CHILD_MODE_ENV)
        .map(|_| PathBuf::from(env::var_os(CHILD_PATH_ENV).expect("child path")))
}

#[test]
fn local_commit_claim_remote_apply_acknowledge() {
    let (directory, mut source, mut destination) = fresh_pair();
    let entry = entry("obj-happy", "happy body");
    let receipt = source
        .commit_with_outbox_event(&entry, &meta("evt-happy"))
        .expect("local commit");
    assert_eq!(receipt.event.state, OutboxState::Pending);
    assert_eq!(receipt.event.source_store, SOURCE_STORE);
    assert_eq!(receipt.event.source_partition, SOURCE_PARTITION);
    assert_eq!(receipt.object_revision, 1);
    assert_health(&source, [1, 0, 0, 0, 0, 0, 0, 0]);

    let claimed = claim_one(&mut source, "evt-happy");
    let envelope = transport_envelope(&source, claimed);
    let applied = match destination
        .apply_outbox_destination(&envelope)
        .expect("production destination apply")
    {
        OutboxDestinationApplyResult::Applied(receipt) => receipt,
        other => panic!("expected production Applied, got {other:?}"),
    };
    assert_destination_receipt(
        &applied,
        &envelope,
        &destination,
        OutboxDestinationApplyApplication::Applied,
    );

    // Ack only after a fresh open reads the immutable destination receipt back
    // through the production Duplicate branch.  The evidence passed below is
    // therefore receipt-bound, not a free-standing reporter token.
    let destination_path = directory.path().join("destination.sqlite");
    drop(destination);
    let mut destination = open_existing(&destination_path, DESTINATION_STORE);
    let durable_receipt = match destination
        .apply_outbox_destination(&envelope)
        .expect("durable destination receipt replay")
    {
        OutboxDestinationApplyResult::Duplicate(receipt) => receipt,
        other => panic!("expected durable Duplicate receipt, got {other:?}"),
    };
    let ack_evidence = ack_evidence_from_durable_receipt(
        &durable_receipt,
        &envelope,
        &destination,
        OutboxDestinationApplyApplication::Duplicate,
    );
    assert_eq!(ack_evidence.reported_by, durable_receipt.destination_store);
    assert_eq!(
        ack_evidence.peer_revision,
        Some(durable_receipt.destination_object_revision)
    );
    assert_eq!(
        ack_evidence.peer_payload_digest.as_deref(),
        Some(durable_receipt.destination_payload_digest.as_str())
    );

    let acknowledged = source
        .apply_outbox_outcome("evt-happy", &OutboxOutcome::Acknowledged, &ack_evidence)
        .expect("ack after destination readback");
    assert_eq!(acknowledged.application, OutboxOutcomeApplication::Applied);
    assert_eq!(acknowledged.prior_state, OutboxState::InFlight);
    assert_eq!(acknowledged.event.state, OutboxState::Acknowledged);
    assert_eq!(acknowledged.event.event_id, "evt-happy");
    assert_eq!(
        acknowledged.event.payload_digest,
        receipt.event.payload_digest
    );
    assert_eq!(acknowledged.evidence, ack_evidence);
    assert_health(&source, [0, 0, 1, 0, 0, 0, 0, 0]);
}

#[test]
fn local_commit_remote_reject() {
    let (directory, mut source, destination) = fresh_pair();
    let receipt = source
        .commit_with_outbox_event(&entry("obj-reject", "reject body"), &meta("evt-reject"))
        .expect("local commit");
    let claimed = claim_one(&mut source, "evt-reject");
    let _envelope = transport_envelope(&source, claimed);

    // A remote policy rejection is not a destination apply.  The production
    // #1718 Applied/Duplicate/Conflict branches are exercised by the other
    // rows; this row proves that a refusal leaves both destination and source
    // object state untouched while #1659 records Rejected.
    assert!(destination
        .get("obj-reject")
        .expect("destination read")
        .is_none());
    let reject_outcome = OutboxOutcome::Rejected {
        error_class: "remote_refused".into(),
    };
    let rejection_evidence = OutboxOutcomeEvidence::from_reporter(DESTINATION_STORE);
    let rejected = source
        .apply_outbox_outcome("evt-reject", &reject_outcome, &rejection_evidence)
        .expect("remote rejection");
    assert_eq!(rejected.application, OutboxOutcomeApplication::Applied);
    assert_eq!(rejected.prior_state, OutboxState::InFlight);
    assert_eq!(rejected.event.state, OutboxState::Rejected);
    assert_eq!(
        rejected.event.last_error_class.as_deref(),
        Some("remote_refused")
    );
    assert_eq!(rejected.event.payload_digest, receipt.event.payload_digest);
    let source_object = source
        .get("obj-reject")
        .expect("source read after rejection")
        .expect("source object survives rejection");
    assert_eq!(source_object.revision, 1);
    assert_eq!(
        outbox_payload_digest(&source_object).expect("source digest"),
        receipt.event.payload_digest
    );
    assert_health(&source, [0, 0, 0, 1, 0, 0, 0, 0]);

    let replay = source
        .apply_outbox_outcome("evt-reject", &reject_outcome, &rejected.evidence)
        .expect("replayed remote rejection");
    assert_eq!(replay.application, OutboxOutcomeApplication::AlreadyApplied);
    assert_eq!(replay.prior_state, OutboxState::Rejected);
    assert_eq!(replay.event, rejected.event);
    assert_eq!(replay.evidence, rejected.evidence);
    assert_health(&source, [0, 0, 0, 1, 0, 0, 0, 0]);

    drop(source);
    let mut restarted = open_existing(&directory.path().join("source.sqlite"), SOURCE_STORE);
    let durable_replay = restarted
        .apply_outbox_outcome("evt-reject", &reject_outcome, &rejected.evidence)
        .expect("durable replayed remote rejection");
    assert_eq!(
        durable_replay.application,
        OutboxOutcomeApplication::AlreadyApplied
    );
    assert_eq!(durable_replay.prior_state, OutboxState::Rejected);
    assert_eq!(durable_replay.event, rejected.event);
    assert_eq!(durable_replay.evidence, rejected.evidence);
    let durable_row = restarted
        .outbox_event("evt-reject")
        .expect("durable rejected row read")
        .expect("durable rejected row");
    assert_eq!(durable_row, rejected.event);
    assert_health(&restarted, [0, 0, 0, 1, 0, 0, 0, 0]);
}

#[test]
fn local_commit_remote_conflict() {
    let (_directory, mut source, mut destination) = fresh_pair();
    let receipt = source
        .commit_with_outbox_event(&entry("obj-conflict", "local body"), &meta("evt-conflict"))
        .expect("local commit");
    destination
        .upsert(&entry("obj-conflict", "remote divergent body"))
        .expect("seed divergent destination head");
    let peer = destination
        .get("obj-conflict")
        .expect("peer read")
        .expect("peer object");
    let peer_digest = outbox_payload_digest(&peer).expect("peer digest");
    assert_ne!(peer_digest, receipt.event.payload_digest);

    let claimed = claim_one(&mut source, "evt-conflict");
    let envelope = transport_envelope(&source, claimed);
    let conflict = match destination
        .apply_outbox_destination(&envelope)
        .expect("production destination conflict classification")
    {
        OutboxDestinationApplyResult::Conflict(receipt) => receipt,
        other => panic!("expected production Conflict, got {other:?}"),
    };
    assert_destination_conflict(
        &conflict,
        &envelope,
        &destination,
        OutboxDestinationConflictReason::ExistingObjectDiverges,
    );
    let conflicted = source
        .apply_outbox_outcome(
            "evt-conflict",
            &OutboxOutcome::Conflicted {
                error_class: "divergent_revision".into(),
            },
            &OutboxOutcomeEvidence {
                reported_by: DESTINATION_STORE.into(),
                peer_revision: Some(peer.revision),
                peer_payload_digest: Some(peer_digest.clone()),
            },
        )
        .expect("typed source conflict");
    assert_eq!(conflicted.application, OutboxOutcomeApplication::Applied);
    assert_eq!(conflicted.prior_state, OutboxState::InFlight);
    assert_eq!(conflicted.event.state, OutboxState::Conflicted);
    assert_eq!(
        conflicted.event.last_error_class.as_deref(),
        Some("divergent_revision")
    );
    assert_eq!(conflicted.evidence.peer_revision, Some(peer.revision));
    assert_eq!(
        conflicted.evidence.peer_payload_digest.as_deref(),
        Some(peer_digest.as_str())
    );
    let source_object = source
        .get("obj-conflict")
        .expect("source read after conflict")
        .expect("source object remains after conflict");
    assert_eq!(source_object.revision, 1);
    assert_eq!(
        outbox_payload_digest(&source_object).expect("source digest after conflict"),
        receipt.event.payload_digest
    );
    assert_health(&source, [0, 0, 0, 0, 1, 0, 0, 0]);
}

#[test]
fn conflict_local_wins_successor_and_quarantine_lineage() {
    let (_directory, mut source, mut destination) = fresh_pair();
    let receipt = source
        .commit_with_outbox_event(&entry("obj-lw", "local wins body"), &meta("evt-lw"))
        .expect("local commit");
    destination
        .upsert(&entry("obj-lw", "peer divergent for local wins"))
        .expect("seed local-wins peer");
    let peer = destination
        .get("obj-lw")
        .expect("peer read")
        .expect("peer object");
    let peer_digest = outbox_payload_digest(&peer).expect("peer digest");
    let claimed = claim_one(&mut source, "evt-lw");
    let envelope = transport_envelope(&source, claimed);
    match destination
        .apply_outbox_destination(&envelope)
        .expect("production local-wins conflict")
    {
        OutboxDestinationApplyResult::Conflict(receipt) => assert_destination_conflict(
            &receipt,
            &envelope,
            &destination,
            OutboxDestinationConflictReason::ExistingObjectDiverges,
        ),
        other => panic!("expected production Conflict, got {other:?}"),
    }
    source
        .apply_outbox_outcome(
            "evt-lw",
            &OutboxOutcome::Conflicted {
                error_class: "divergent_revision".into(),
            },
            &OutboxOutcomeEvidence {
                reported_by: DESTINATION_STORE.into(),
                peer_revision: Some(peer.revision),
                peer_payload_digest: Some(peer_digest),
            },
        )
        .expect("source conflict");
    source
        .upsert(&entry("obj-lw", "local wins successor body"))
        .expect("advance local object after conflict");
    let current = source
        .get("obj-lw")
        .expect("current source read")
        .expect("current source object");
    let current_digest = outbox_payload_digest(&current).expect("current digest");
    assert_eq!(current.revision, 2);
    assert_ne!(current_digest, receipt.event.payload_digest);

    let resolution = source
        .resolve_outbox_conflict("evt-lw", &OutboxConflictResolution::LocalWins)
        .expect("LocalWins resolution");
    let (resolved, successor) = match resolution {
        OutboxConflictResolutionReceipt::LocalWins {
            resolved,
            successor,
        } => (resolved, successor),
        other => panic!("unexpected LocalWins receipt: {other:?}"),
    };
    assert_eq!(resolved.event_id, "evt-lw");
    assert_eq!(resolved.state, OutboxState::Quarantined);
    assert_eq!(
        resolved.last_error_class.as_deref(),
        Some(OUTBOX_LOCAL_WINS_RESOLVED_CLASS)
    );
    assert_eq!(resolved.source_revision, 1);
    assert_eq!(resolved.payload_digest, receipt.event.payload_digest);
    assert_eq!(successor.event_id, outbox_local_wins_successor_id("evt-lw"));
    assert_eq!(successor.event_id, "evt-lw::local-wins");
    assert_eq!(successor.object_id, "obj-lw");
    assert_eq!(successor.source_store, SOURCE_STORE);
    assert_eq!(successor.source_partition, SOURCE_PARTITION);
    assert_eq!(successor.source_revision, current.revision);
    assert_eq!(successor.payload_digest, current_digest);
    assert_eq!(successor.state, OutboxState::Pending);
    assert_eq!(successor.last_error_class, None);
    let health = source
        .outbox_health_with_stale_after(HEALTH_STALE_AFTER)
        .expect("local-wins health");
    assert_eq!(health.local_store_status, LocalStoreStatus::Healthy);
    assert_eq!(health.pending_count, 1);
    assert_eq!(health.in_flight_count, 0);
    assert_eq!(health.acknowledged_count, 0);
    assert_eq!(health.rejected_count, 0);
    assert_eq!(health.conflicted_count, 0);
    assert_eq!(health.quarantined_count, 1);
    assert_eq!(health.resolved_count, 1);
    assert_eq!(health.stale_lease_count, 0);
    assert_eq!(health.remote_sync_status, RemoteSyncStatus::Unconfigured);
    let replay = source
        .resolve_outbox_conflict("evt-lw", &OutboxConflictResolution::LocalWins)
        .expect_err("replayed LocalWins must refuse after quarantine");
    assert!(matches!(
        replay,
        MemoryError::OutboxOutcomeRefused {
            reason: OutboxOutcomeRefusal::NotConflicted,
            ..
        }
    ));
}

#[test]
fn conflict_remote_wins_typed_local_transition() {
    let (_directory, mut source, mut destination) = fresh_pair();
    let receipt = source
        .commit_with_outbox_event(&entry("obj-rw", "remote wins body"), &meta("evt-rw"))
        .expect("local commit");
    let before = source
        .get("obj-rw")
        .expect("source read")
        .expect("source object");
    destination
        .upsert(&entry("obj-rw", "peer authority body"))
        .expect("seed remote-wins peer");
    let peer = destination
        .get("obj-rw")
        .expect("peer read")
        .expect("peer object");
    let peer_digest = outbox_payload_digest(&peer).expect("peer digest");
    let claimed = claim_one(&mut source, "evt-rw");
    let envelope = transport_envelope(&source, claimed);
    match destination
        .apply_outbox_destination(&envelope)
        .expect("production remote-wins conflict")
    {
        OutboxDestinationApplyResult::Conflict(receipt) => assert_destination_conflict(
            &receipt,
            &envelope,
            &destination,
            OutboxDestinationConflictReason::ExistingObjectDiverges,
        ),
        other => panic!("expected production Conflict, got {other:?}"),
    }
    source
        .apply_outbox_outcome(
            "evt-rw",
            &OutboxOutcome::Conflicted {
                error_class: "divergent_revision".into(),
            },
            &OutboxOutcomeEvidence {
                reported_by: DESTINATION_STORE.into(),
                peer_revision: Some(peer.revision),
                peer_payload_digest: Some(peer_digest),
            },
        )
        .expect("source conflict");
    let resolution = source
        .resolve_outbox_conflict(
            "evt-rw",
            &OutboxConflictResolution::RemoteWins {
                error_class: "peer_authority_wins".into(),
            },
        )
        .expect("RemoteWins resolution");
    let quarantined = match resolution {
        OutboxConflictResolutionReceipt::RemoteWins { quarantined } => quarantined,
        other => panic!("unexpected RemoteWins receipt: {other:?}"),
    };
    let expected_class = format!("{OUTBOX_REMOTE_WINS_RESOLVED_CLASS_PREFIX}.peer_authority_wins");
    assert_eq!(quarantined.event_id, "evt-rw");
    assert_eq!(quarantined.state, OutboxState::Quarantined);
    assert_eq!(
        quarantined.last_error_class.as_deref(),
        Some(expected_class.as_str())
    );
    assert_eq!(quarantined.source_revision, 1);
    assert_eq!(quarantined.payload_digest, receipt.event.payload_digest);
    assert!(source
        .outbox_event("evt-rw::local-wins")
        .expect("successor probe")
        .is_none());
    let after = source
        .get("obj-rw")
        .expect("source read")
        .expect("source object");
    assert_eq!(after.revision, before.revision);
    assert_eq!(
        outbox_payload_digest(&after).expect("source digest"),
        outbox_payload_digest(&before).expect("before digest")
    );
    let health = source
        .outbox_health_with_stale_after(HEALTH_STALE_AFTER)
        .expect("remote-wins health");
    assert_eq!(health.local_store_status, LocalStoreStatus::Healthy);
    assert_eq!(health.pending_count, 0);
    assert_eq!(health.in_flight_count, 0);
    assert_eq!(health.acknowledged_count, 0);
    assert_eq!(health.rejected_count, 0);
    assert_eq!(health.conflicted_count, 0);
    assert_eq!(health.quarantined_count, 1);
    assert_eq!(health.resolved_count, 1);
    assert_eq!(health.stale_lease_count, 0);
    assert_eq!(health.remote_sync_status, RemoteSyncStatus::Unconfigured);
}

#[test]
fn crash_before_claim_pending_survives_reopen() {
    if let Some(path) = child_path() {
        assert_eq!(env::var(CHILD_MODE_ENV).as_deref(), Ok("before-claim"));
        let mut source = open_fresh(&path, SOURCE_STORE);
        let receipt = source
            .commit_with_outbox_event(
                &entry("obj-crash-before-claim", "crash before claim body"),
                &meta("evt-crash-before-claim"),
            )
            .expect("child local commit");
        assert_eq!(receipt.event.state, OutboxState::Pending);
        std::process::exit(0);
    }

    let directory = tempdir().expect("crash-before-claim tempdir");
    let path = directory.path().join("source.sqlite");
    run_child(
        "crash_before_claim_pending_survives_reopen",
        "before-claim",
        &path,
    );
    let mut source = open_existing(&path, SOURCE_STORE);
    assert_eq!(source.db_label(), SOURCE_STORE);
    assert_eq!(source.store_profile(), StoreProfile::PortableKernel);
    let event = source
        .outbox_event("evt-crash-before-claim")
        .expect("reopen pending event")
        .expect("pending event survives process loss");
    assert_eq!(event.state, OutboxState::Pending);
    assert_eq!(event.source_revision, 1);
    let object = source
        .get("obj-crash-before-claim")
        .expect("reopen source object")
        .expect("source object survives process loss");
    assert_eq!(object.revision, 1);
    assert_eq!(
        outbox_payload_digest(&object).expect("reopened source digest"),
        event.payload_digest
    );
    assert_health(&source, [1, 0, 0, 0, 0, 0, 0, 0]);

    let destination_path = directory.path().join("destination.sqlite");
    let mut destination = open_fresh(&destination_path, DESTINATION_STORE);
    let claimed = claim_one(&mut source, "evt-crash-before-claim");
    let envelope = transport_envelope(&source, claimed);
    let applied = match destination
        .apply_outbox_destination(&envelope)
        .expect("post-restart production apply")
    {
        OutboxDestinationApplyResult::Applied(receipt) => receipt,
        other => panic!("expected production Applied, got {other:?}"),
    };
    assert_destination_receipt(
        &applied,
        &envelope,
        &destination,
        OutboxDestinationApplyApplication::Applied,
    );
    source
        .apply_outbox_outcome(
            "evt-crash-before-claim",
            &OutboxOutcome::Acknowledged,
            &OutboxOutcomeEvidence::from_reporter(DESTINATION_STORE),
        )
        .expect("ack after pending restart");
    assert_health(&source, [0, 0, 1, 0, 0, 0, 0, 0]);
}

#[test]
fn crash_after_claim_stale_health_and_exactly_one_reclaimed() {
    if let Some(path) = child_path() {
        assert_eq!(env::var(CHILD_MODE_ENV).as_deref(), Ok("after-claim"));
        let mut source = open_fresh(&path, SOURCE_STORE);
        source
            .commit_with_outbox_event(
                &entry("obj-crash-after-claim", "crash after claim body"),
                &meta("evt-crash-after-claim"),
            )
            .expect("child local commit");
        let claimed = source
            .claim_outbox_events(&OutboxClaimRequest::first_claims_only(8))
            .expect("child first claim");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].event.state, OutboxState::InFlight);
        assert_eq!(claimed[0].claim, OutboxClaimKind::First);
        assert!(
            source
                .claim_outbox_events(&OutboxClaimRequest::with_reclaim(8, LEASE_BOUND))
                .expect("child positive-window reclaim probe")
                .is_empty(),
            "a fresh lease must not be reclaimed inside the positive window"
        );
        std::process::exit(0);
    }

    let directory = tempdir().expect("crash-after-claim tempdir");
    let path = directory.path().join("source.sqlite");
    run_child(
        "crash_after_claim_stale_health_and_exactly_one_reclaimed",
        "after-claim",
        &path,
    );
    let mut source = open_existing(&path, SOURCE_STORE);
    let before = source
        .outbox_event("evt-crash-after-claim")
        .expect("reopen in-flight event")
        .expect("in-flight event survives process loss");
    assert_eq!(before.state, OutboxState::InFlight);
    let health_deadline = Instant::now() + Duration::from_secs(5);
    let stale_health = loop {
        let health = source
            .outbox_health_with_stale_after(LEASE_BOUND)
            .expect("stale lease health");
        if health.stale_lease_count == 1 {
            break health;
        }
        assert!(
            Instant::now() < health_deadline,
            "lease did not become stale before the explicit deadline"
        );
        thread::yield_now();
    };
    assert_eq!(stale_health.pending_count, 0);
    assert_eq!(stale_health.in_flight_count, 1);
    assert_eq!(stale_health.acknowledged_count, 0);
    assert_eq!(stale_health.rejected_count, 0);
    assert_eq!(stale_health.conflicted_count, 0);
    assert_eq!(stale_health.quarantined_count, 0);
    assert_eq!(stale_health.resolved_count, 0);
    assert_eq!(
        stale_health.remote_sync_status,
        RemoteSyncStatus::Unconfigured
    );

    let reclaimed = source
        .claim_outbox_events(&OutboxClaimRequest::with_reclaim(8, LEASE_BOUND))
        .expect("reclaim stale child lease");
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].event.event_id, "evt-crash-after-claim");
    assert_eq!(reclaimed[0].event.state, OutboxState::InFlight);
    match &reclaimed[0].claim {
        OutboxClaimKind::Reclaimed {
            previous_claim_at,
            stale_claim_cutoff,
        } => {
            assert_eq!(previous_claim_at, &before.state_changed_at);
            assert!(previous_claim_at <= stale_claim_cutoff);
        }
        other => panic!("expected exactly one Reclaimed claim, got {other:?}"),
    }
    assert!(
        source
            .claim_outbox_events(&OutboxClaimRequest::with_reclaim(8, LEASE_BOUND))
            .expect("immediate repeated reclaim")
            .is_empty(),
        "a reclaimed event's renewed lease must exclude a second owner"
    );
    let ack = source
        .apply_outbox_outcome(
            "evt-crash-after-claim",
            &OutboxOutcome::Acknowledged,
            &OutboxOutcomeEvidence::from_reporter(DESTINATION_STORE),
        )
        .expect("ack after exactly-one reclaim");
    assert_eq!(ack.application, OutboxOutcomeApplication::Applied);
    assert_health(&source, [0, 0, 1, 0, 0, 0, 0, 0]);
    drop(source);
    let mut restarted = open_existing(&path, SOURCE_STORE);
    let duplicate = restarted
        .apply_outbox_outcome(
            "evt-crash-after-claim",
            &OutboxOutcome::Acknowledged,
            &OutboxOutcomeEvidence::from_reporter("replay-after-restart"),
        )
        .expect("durable duplicate acknowledgement after restart");
    assert_eq!(
        duplicate.application,
        OutboxOutcomeApplication::AlreadyApplied
    );
    assert_eq!(duplicate.event, ack.event);
    assert_health(&restarted, [0, 0, 1, 0, 0, 0, 0, 0]);
}

#[test]
fn lost_acknowledgement_duplicate_replay() {
    let (_directory, mut source, mut destination) = fresh_pair();
    let receipt = source
        .commit_with_outbox_event(
            &entry("obj-lost-ack", "lost ack body"),
            &meta("evt-lost-ack"),
        )
        .expect("local commit");
    let claimed = claim_one(&mut source, "evt-lost-ack");
    let envelope = transport_envelope(&source, claimed);
    let first = match destination
        .apply_outbox_destination(&envelope)
        .expect("first production destination apply")
    {
        OutboxDestinationApplyResult::Applied(receipt) => receipt,
        other => panic!("expected production Applied, got {other:?}"),
    };
    assert_destination_receipt(
        &first,
        &envelope,
        &destination,
        OutboxDestinationApplyApplication::Applied,
    );
    assert_health(&source, [0, 1, 0, 0, 0, 0, 0, 0]);
    let duplicate = match destination
        .apply_outbox_destination(&envelope)
        .expect("lost-ack destination replay")
    {
        OutboxDestinationApplyResult::Duplicate(receipt) => receipt,
        other => panic!("expected production Duplicate, got {other:?}"),
    };
    assert_destination_receipt(
        &duplicate,
        &envelope,
        &destination,
        OutboxDestinationApplyApplication::Duplicate,
    );
    let mut expected_duplicate = first.clone();
    expected_duplicate.application = OutboxDestinationApplyApplication::Duplicate;
    assert_eq!(duplicate, expected_duplicate);
    let ack = source
        .apply_outbox_outcome(
            "evt-lost-ack",
            &OutboxOutcome::Acknowledged,
            &OutboxOutcomeEvidence::from_reporter(DESTINATION_STORE),
        )
        .expect("ack after durable Duplicate");
    assert_eq!(ack.event.payload_digest, receipt.event.payload_digest);
    assert_health(&source, [0, 0, 1, 0, 0, 0, 0, 0]);

    let source_before = source
        .get("obj-lost-ack")
        .expect("source read before event replay")
        .expect("source object");
    let duplicate_commit = source
        .commit_with_outbox_event(
            &entry("obj-lost-ack", "replayed different body"),
            &meta("evt-lost-ack"),
        )
        .expect_err("completed event id must refuse a second commit");
    assert!(matches!(duplicate_commit, MemoryError::Duplicate(_)));
    let source_after = source
        .get("obj-lost-ack")
        .expect("source read after event replay")
        .expect("source object remains");
    assert_eq!(source_after.revision, source_before.revision);
    assert_eq!(
        outbox_payload_digest(&source_after).expect("source digest after event replay"),
        outbox_payload_digest(&source_before).expect("source digest before event replay")
    );
    assert_health(&destination, [0, 0, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn out_of_order_independent_events() {
    let (_directory, mut source, mut destination) = fresh_pair();
    let receipt_a = source
        .commit_with_outbox_event(&entry("obj-ooo-a", "independent a"), &meta("evt-ooo-a"))
        .expect("commit independent A");
    let receipt_b = source
        .commit_with_outbox_event(&entry("obj-ooo-b", "independent b"), &meta("evt-ooo-b"))
        .expect("commit independent B");
    assert_eq!(receipt_a.object_revision, 1);
    assert_eq!(receipt_b.object_revision, 1);
    let claims = source
        .claim_outbox_events(&OutboxClaimRequest::first_claims_only(16))
        .expect("claim independent events");
    assert_eq!(claims.len(), 2);
    assert_eq!(claims[0].event.event_id, "evt-ooo-a");
    assert_eq!(claims[1].event.event_id, "evt-ooo-b");
    assert_eq!(claims[0].claim, OutboxClaimKind::First);
    assert_eq!(claims[1].claim, OutboxClaimKind::First);
    let envelope_a = transport_envelope(&source, claims[0].clone());
    let envelope_b = transport_envelope(&source, claims[1].clone());
    let applied_b = match destination
        .apply_outbox_destination(&envelope_b)
        .expect("production apply B out of order")
    {
        OutboxDestinationApplyResult::Applied(receipt) => receipt,
        other => panic!("expected production Applied B, got {other:?}"),
    };
    assert_destination_receipt(
        &applied_b,
        &envelope_b,
        &destination,
        OutboxDestinationApplyApplication::Applied,
    );
    let applied_a = match destination
        .apply_outbox_destination(&envelope_a)
        .expect("production apply A out of order")
    {
        OutboxDestinationApplyResult::Applied(receipt) => receipt,
        other => panic!("expected production Applied A, got {other:?}"),
    };
    assert_destination_receipt(
        &applied_a,
        &envelope_a,
        &destination,
        OutboxDestinationApplyApplication::Applied,
    );
    let ack_b = source
        .apply_outbox_outcome(
            "evt-ooo-b",
            &OutboxOutcome::Acknowledged,
            &OutboxOutcomeEvidence::from_reporter(DESTINATION_STORE),
        )
        .expect("ack B out of order");
    assert_eq!(ack_b.event.state, OutboxState::Acknowledged);
    assert_eq!(
        source
            .outbox_event("evt-ooo-a")
            .expect("A read")
            .expect("A row")
            .state,
        OutboxState::InFlight
    );
    let ack_a = source
        .apply_outbox_outcome(
            "evt-ooo-a",
            &OutboxOutcome::Acknowledged,
            &OutboxOutcomeEvidence::from_reporter(DESTINATION_STORE),
        )
        .expect("ack A out of order");
    assert_eq!(ack_a.event.state, OutboxState::Acknowledged);
    assert_health(&source, [0, 0, 2, 0, 0, 0, 0, 0]);
}

#[test]
fn same_event_concurrent_drainers_exactly_one_owner() {
    let directory = tempdir().expect("concurrent drainer tempdir");
    let path = directory.path().join("source.sqlite");
    {
        let mut source = open_fresh(&path, SOURCE_STORE);
        let receipt = source
            .commit_with_outbox_event(&entry("obj-race", "race body"), &meta("evt-race"))
            .expect("race commit");
        assert_eq!(receipt.event.state, OutboxState::Pending);
    }
    let barrier = Arc::new(Barrier::new(2));
    let winner_count = Arc::new(AtomicUsize::new(0));
    let batches: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let mut workers = Vec::new();
    for worker_id in 0..2 {
        let barrier = Arc::clone(&barrier);
        let winner_count = Arc::clone(&winner_count);
        let batches = Arc::clone(&batches);
        let path = path.clone();
        workers.push(thread::spawn(move || {
            let mut source = open_existing(&path, SOURCE_STORE);
            barrier.wait();
            let claimed = source
                .claim_outbox_events(&OutboxClaimRequest::first_claims_only(16))
                .unwrap_or_else(|error| panic!("worker {worker_id} claim: {error}"));
            let ids: Vec<String> = claimed
                .iter()
                .map(|item| {
                    assert_eq!(item.event.state, OutboxState::InFlight);
                    assert_eq!(item.claim, OutboxClaimKind::First);
                    item.event.event_id.clone()
                })
                .collect();
            if !ids.is_empty() {
                winner_count.fetch_add(1, Ordering::SeqCst);
            }
            batches.lock().expect("batches lock").push(ids);
        }));
    }
    for worker in workers {
        worker.join().expect("drainer thread join");
    }
    assert_eq!(winner_count.load(Ordering::SeqCst), 1);
    let batches = batches.lock().expect("batches lock");
    assert_eq!(batches.len(), 2);
    let mut all: Vec<String> = batches.iter().flatten().cloned().collect();
    all.sort();
    assert_eq!(all, vec!["evt-race".to_string()]);
    assert_eq!(batches.iter().filter(|batch| batch.is_empty()).count(), 1);
    assert_eq!(batches.iter().filter(|batch| batch.len() == 1).count(), 1);
    let source = open_existing(&path, SOURCE_STORE);
    assert_health(&source, [0, 1, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn destination_identity_and_closed_namespace_refusals() {
    let (_directory, mut source, _unused_destination) = fresh_pair();
    source
        .commit_with_outbox_event(
            &entry("obj-identity", "identity body"),
            &meta("evt-identity"),
        )
        .expect("identity source commit");
    let claimed = claim_one(&mut source, "evt-identity");
    let envelope = transport_envelope(&source, claimed);

    let directory = tempdir().expect("identity mismatch tempdir");
    let path = directory.path().join("destination.sqlite");
    let mut destination = open_fresh(&path, DESTINATION_STORE);
    let mut wrong_store = envelope.clone();
    wrong_store.destination.store = SOURCE_STORE.into();
    let error = destination
        .apply_outbox_destination(&wrong_store)
        .expect_err("destination store identity mismatch must refuse");
    assert!(error.to_string().contains("store identity mismatch"));
    assert!(destination
        .get("obj-identity")
        .expect("identity refusal read")
        .is_none());
    assert_health(&destination, [0, 0, 0, 0, 0, 0, 0, 0]);
    let applied = destination
        .apply_outbox_destination(&envelope)
        .expect("valid destination identity");
    assert!(matches!(applied, OutboxDestinationApplyResult::Applied(_)));
    let mut wrong_partition = envelope.clone();
    wrong_partition.destination.partition = "destination-partition-other".into();
    let error = destination
        .apply_outbox_destination(&wrong_partition)
        .expect_err("destination partition identity mismatch must refuse");
    assert!(error.to_string().contains("partition mismatch"));
    assert_health(&destination, [0, 0, 0, 0, 0, 0, 0, 0]);

    assert_destination_refusal_before_mutation(&envelope, "classification token", |bad| {
        bad.claimed.event.object_class = "Invalid Class".into();
    });
    assert_destination_refusal_before_mutation(&envelope, "classification token", |bad| {
        bad.claimed.event.authority_class = "Invalid Class".into();
    });
    assert_destination_refusal_before_mutation(
        &envelope,
        "reserved resolved-conflict class",
        |bad| {
            bad.claimed.event.object_class = "conflict_resolved_operator".into();
        },
    );
    assert_destination_refusal_before_mutation(&envelope, "reserved successor suffix", |bad| {
        bad.claimed.event.event_id.push_str("::local-wins");
    });
    assert_destination_refusal_before_mutation(&envelope, "source_store", |bad| {
        bad.claimed.event.source_store.clear();
    });
}

#[test]
fn full_replay_projection_equivalence() {
    let (directory, mut source, mut destination) = fresh_pair();
    let object_ids = vec![
        "obj-replay-a".to_string(),
        "obj-replay-b".to_string(),
        "obj-replay-c".to_string(),
    ];
    for object_id in &object_ids {
        let event_id = format!("evt-{object_id}");
        let text = format!("full replay {object_id}");
        source
            .commit_with_outbox_event(&entry(object_id, &text), &meta(&event_id))
            .expect("full replay commit");
    }
    let claims = source
        .claim_outbox_events(&OutboxClaimRequest::first_claims_only(16))
        .expect("full replay claims");
    assert_eq!(claims.len(), object_ids.len());
    let envelopes: Vec<OutboxDestinationApplyEnvelope> = claims
        .into_iter()
        .map(|claim| transport_envelope(&source, claim))
        .collect();

    for envelope in envelopes.iter().rev() {
        let applied = match destination
            .apply_outbox_destination(envelope)
            .expect("full replay production apply")
        {
            OutboxDestinationApplyResult::Applied(receipt) => receipt,
            other => panic!("expected production Applied in full replay, got {other:?}"),
        };
        assert_destination_receipt(
            &applied,
            envelope,
            &destination,
            OutboxDestinationApplyApplication::Applied,
        );
    }
    for envelope in envelopes.iter().rev() {
        source
            .apply_outbox_outcome(
                &envelope.claimed.event.event_id,
                &OutboxOutcome::Acknowledged,
                &OutboxOutcomeEvidence::from_reporter(DESTINATION_STORE),
            )
            .expect("full replay source acknowledgement");
    }
    assert_health(&source, [0, 0, 3, 0, 0, 0, 0, 0]);
    drop(destination);
    let destination_path = directory.path().join("destination.sqlite");
    let mut replayed = open_existing(&destination_path, DESTINATION_STORE);
    for envelope in &envelopes {
        let duplicate = match replayed
            .apply_outbox_destination(envelope)
            .expect("full replay Duplicate")
        {
            OutboxDestinationApplyResult::Duplicate(receipt) => receipt,
            other => panic!("expected production Duplicate in full replay, got {other:?}"),
        };
        assert_destination_receipt(
            &duplicate,
            envelope,
            &replayed,
            OutboxDestinationApplyApplication::Duplicate,
        );
    }

    let source_projection: Vec<Vec<u8>> = object_ids
        .iter()
        .map(|id| {
            serde_json::to_vec(
                &source
                    .get(id)
                    .expect("source projection read")
                    .expect("source projection object"),
            )
            .expect("source projection serialization")
        })
        .collect();
    let destination_projection: Vec<Vec<u8>> = object_ids
        .iter()
        .map(|id| {
            serde_json::to_vec(
                &replayed
                    .get(id)
                    .expect("destination projection read")
                    .expect("destination projection object"),
            )
            .expect("destination projection serialization")
        })
        .collect();
    assert_eq!(source_projection, destination_projection);
    assert_health(&replayed, [0, 0, 0, 0, 0, 0, 0, 0]);
}
