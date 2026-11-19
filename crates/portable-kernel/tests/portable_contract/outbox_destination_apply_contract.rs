//! #1718 portable destination-side outbox apply/readback contract.
//!
//! This test target is compiled in isolation with memcore's admin feature
//! disabled.  The source store creates a real claimed envelope through the
//! public outbox API; the destination only sees the typed envelope and its
//! own stamped identity.  No raw SQL or Tachi facade is used here.

use portable_kernel::{
    outbox_payload_digest, MemoryEntry, MemoryStore, OutboxClaimRequest,
    OutboxDestinationApplyApplication, OutboxDestinationApplyEnvelope,
    OutboxDestinationApplyResult, OutboxDestinationConflictReason, OutboxDestinationIdentity,
    OutboxEventMeta,
};
use tempfile::tempdir;

fn entry(id: &str, text: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.into(),
        path: "/scratch/portable/destination-apply".into(),
        summary: format!("{id} summary"),
        text: text.into(),
        importance: 0.7,
        timestamp: "2026-08-10T00:00:00Z".into(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".into(),
        topic: "destination-apply".into(),
        keywords: vec!["portable".into(), "outbox".into()],
        persons: vec![],
        entities: vec!["memcore".into()],
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
        metadata: serde_json::json!({"contract": "1718"}),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".into(),
    }
}

fn source_and_envelope(
    source_path: &std::path::Path,
    object_id: &str,
) -> (MemoryStore, OutboxDestinationApplyEnvelope) {
    let source_path = source_path.to_string_lossy().into_owned();
    let mut source =
        MemoryStore::open_with_label(&source_path, "global").expect("open stamped source store");
    let payload = entry(object_id, "portable destination apply payload");
    source
        .commit_with_outbox_event(
            &payload,
            &OutboxEventMeta {
                event_id: format!("evt-{object_id}"),
                object_class: "memory".into(),
                authority_class: "host".into(),
                source_store: "global".into(),
                source_partition: "source-a".into(),
            },
        )
        .expect("source commit");
    let claimed = source
        .claim_outbox_events(&OutboxClaimRequest::first_claims_only(1))
        .expect("claim source event")
        .pop()
        .expect("one claimed event");
    let stored_payload = source
        .get_with_options(object_id, true)
        .expect("source readback")
        .expect("source object");
    (
        source,
        OutboxDestinationApplyEnvelope {
            claimed,
            payload: stored_payload,
            destination: OutboxDestinationIdentity {
                store: "global".into(),
                partition: "destination-a".into(),
            },
        },
    )
}

fn flip_digest(digest: &str) -> String {
    let mut changed = digest.to_string();
    let replacement = if changed.starts_with('0') { '1' } else { '0' };
    changed.replace_range(0..1, &replacement.to_string());
    changed
}

fn assert_refuses_before_partition_stamp(
    base: &OutboxDestinationApplyEnvelope,
    object_id: &str,
    expected_error: &str,
    mutate: impl FnOnce(&mut OutboxDestinationApplyEnvelope),
) {
    let destination_dir = tempdir().expect("destination tempdir");
    let destination_path = destination_dir.path().join("memory.db");
    let mut destination =
        MemoryStore::open_with_label(&destination_path.to_string_lossy(), "global")
            .expect("open destination");
    let mut malformed = base.clone();
    mutate(&mut malformed);
    let error = destination
        .apply_outbox_destination(&malformed)
        .expect_err("malformed envelope must refuse");
    assert!(
        error.to_string().contains(expected_error),
        "expected {expected_error} in {error}"
    );
    assert!(destination
        .get_with_options(object_id, true)
        .expect("destination read")
        .is_none());

    let mut valid = base.clone();
    valid.destination.partition = "post-invalid-probe".into();
    assert!(matches!(
        destination
            .apply_outbox_destination(&valid)
            .expect("valid apply after malformed refusal"),
        OutboxDestinationApplyResult::Applied(_)
    ));
}

#[test]
fn portable_destination_apply_applies_and_replays_from_durable_receipt() {
    let source_dir = tempdir().expect("source tempdir");
    let destination_dir = tempdir().expect("destination tempdir");
    let source_path = source_dir.path().join("memory.db");
    let destination_path = destination_dir.path().join("memory.db");
    let (_source, envelope) = source_and_envelope(&source_path, "destination-apply-happy");

    let mut destination =
        MemoryStore::open_with_label(&destination_path.to_string_lossy(), "global")
            .expect("open stamped destination store");
    let applied = destination
        .apply_outbox_destination(&envelope)
        .expect("first destination apply");
    let applied_receipt = match applied {
        OutboxDestinationApplyResult::Applied(receipt) => {
            assert_eq!(
                receipt.application,
                OutboxDestinationApplyApplication::Applied
            );
            receipt
        }
        other => panic!("expected Applied, got {other:?}"),
    };
    assert_eq!(
        destination
            .get_with_options("destination-apply-happy", true)
            .expect("destination read")
            .expect("destination object")
            .revision,
        applied_receipt.destination_object_revision
    );
    let applied_object = destination
        .get_with_options("destination-apply-happy", true)
        .expect("destination readback")
        .expect("destination object readback");
    assert_eq!(
        applied_receipt.source_payload_digest, envelope.claimed.event.payload_digest,
        "receipt source digest must bind the claimed event"
    );
    assert_eq!(
        applied_receipt.destination_payload_digest,
        outbox_payload_digest(&applied_object).expect("destination digest"),
        "receipt destination digest must bind the exact readback object"
    );
    assert_eq!(applied_receipt.event_id, envelope.claimed.event.event_id);
    assert_eq!(applied_receipt.object_id, applied_object.id);
    assert_eq!(
        applied_receipt.source_store,
        envelope.claimed.event.source_store
    );
    assert_eq!(
        applied_receipt.source_partition,
        envelope.claimed.event.source_partition
    );
    assert_eq!(
        applied_receipt.source_revision,
        envelope.claimed.event.source_revision
    );
    assert_eq!(
        applied_receipt.destination_store,
        envelope.destination.store
    );
    assert_eq!(
        applied_receipt.destination_partition,
        envelope.destination.partition
    );

    drop(destination);
    let mut restarted = MemoryStore::open_with_label(&destination_path.to_string_lossy(), "global")
        .expect("reopen destination store");
    let duplicate = restarted
        .apply_outbox_destination(&envelope)
        .expect("duplicate destination apply");
    match duplicate {
        OutboxDestinationApplyResult::Duplicate(receipt) => {
            assert_eq!(
                receipt.application,
                OutboxDestinationApplyApplication::Duplicate
            );
            assert_eq!(receipt.event_id, applied_receipt.event_id);
            assert_eq!(receipt.object_id, applied_receipt.object_id);
            assert_eq!(receipt.source_store, applied_receipt.source_store);
            assert_eq!(receipt.source_partition, applied_receipt.source_partition);
            assert_eq!(receipt.source_revision, applied_receipt.source_revision);
            assert_eq!(
                receipt.source_payload_digest,
                applied_receipt.source_payload_digest
            );
            assert_eq!(receipt.destination_store, applied_receipt.destination_store);
            assert_eq!(
                receipt.destination_partition,
                applied_receipt.destination_partition
            );
            assert_eq!(
                receipt.destination_payload_digest,
                applied_receipt.destination_payload_digest
            );
            assert_eq!(
                receipt.destination_object_revision,
                applied_receipt.destination_object_revision
            );
        }
        other => panic!("expected Duplicate, got {other:?}"),
    }
}

#[test]
fn portable_destination_apply_refuses_partition_mismatch_before_mutation() {
    let source_dir = tempdir().expect("source tempdir");
    let destination_dir = tempdir().expect("destination tempdir");
    let source_path = source_dir.path().join("memory.db");
    let destination_path = destination_dir.path().join("memory.db");
    let (_source, envelope) = source_and_envelope(&source_path, "destination-apply-partition");

    let mut destination =
        MemoryStore::open_with_label(&destination_path.to_string_lossy(), "global")
            .expect("open destination");
    destination
        .apply_outbox_destination(&envelope)
        .expect("partition A apply");

    let mut wrong_partition = envelope.clone();
    wrong_partition.destination.partition = "destination-b".into();
    let error = destination
        .apply_outbox_destination(&wrong_partition)
        .expect_err("partition B must refuse");
    assert!(error.to_string().contains("partition mismatch"), "{error}");

    match destination
        .apply_outbox_destination(&envelope)
        .expect("original partition remains usable")
    {
        OutboxDestinationApplyResult::Duplicate(receipt) => {
            assert_eq!(receipt.destination_partition, "destination-a");
        }
        other => panic!("expected unchanged Duplicate, got {other:?}"),
    }
}

#[test]
fn portable_destination_apply_rejects_malformed_binding_before_partition_stamp() {
    let source_dir = tempdir().expect("source tempdir");
    let object_id = "destination-apply-malformed-matrix";
    let (_source, envelope) = source_and_envelope(&source_dir.path().join("memory.db"), object_id);

    assert_refuses_before_partition_stamp(&envelope, object_id, "event_id", |malformed| {
        malformed.claimed.event.event_id.clear();
    });
    assert_refuses_before_partition_stamp(&envelope, object_id, "payload id", |malformed| {
        malformed.payload.id = "different-object".into();
    });
    assert_refuses_before_partition_stamp(&envelope, object_id, "source_revision", |malformed| {
        malformed.claimed.event.source_revision = 0;
    });
    assert_refuses_before_partition_stamp(&envelope, object_id, "payload revision", |malformed| {
        malformed.payload.revision = 0;
    });
    assert_refuses_before_partition_stamp(&envelope, object_id, "payload_digest", |malformed| {
        malformed.claimed.event.payload_digest =
            flip_digest(&malformed.claimed.event.payload_digest);
    });
    assert_refuses_before_partition_stamp(&envelope, object_id, "digest", |malformed| {
        malformed.payload.text.push_str(" changed");
    });
    assert_refuses_before_partition_stamp(&envelope, object_id, "in_flight", |malformed| {
        malformed.claimed.event.state = portable_kernel::OutboxState::Pending;
    });
    assert_refuses_before_partition_stamp(
        &envelope,
        object_id,
        "classification token",
        |malformed| {
            malformed.claimed.event.object_class = "Invalid Class".into();
        },
    );
    assert_refuses_before_partition_stamp(
        &envelope,
        object_id,
        "classification token",
        |malformed| {
            malformed.claimed.event.authority_class = "Invalid Class".into();
        },
    );
    assert_refuses_before_partition_stamp(
        &envelope,
        object_id,
        "reserved resolved-conflict class",
        |malformed| {
            malformed.claimed.event.object_class = "conflict_resolved_operator".into();
        },
    );
}

#[test]
fn portable_destination_apply_classifies_replay_and_object_conflicts_without_lww() {
    let source_dir = tempdir().expect("source tempdir");
    let destination_dir = tempdir().expect("destination tempdir");
    let source_path = source_dir.path().join("memory.db");
    let destination_path = destination_dir.path().join("memory.db");
    let (_source, envelope) = source_and_envelope(&source_path, "destination-apply-conflicts");

    let mut destination =
        MemoryStore::open_with_label(&destination_path.to_string_lossy(), "global")
            .expect("open destination");
    destination
        .apply_outbox_destination(&envelope)
        .expect("initial apply");

    let mut replay_mismatch = envelope.clone();
    replay_mismatch.claimed.event.source_partition = "source-b".into();
    match destination
        .apply_outbox_destination(&replay_mismatch)
        .expect("replay mismatch is a typed result")
    {
        OutboxDestinationApplyResult::Conflict(receipt) => {
            assert_eq!(
                receipt.reason,
                OutboxDestinationConflictReason::EventReplayMismatch
            );
            assert_eq!(receipt.source_partition, "source-b");
        }
        other => panic!("expected replay conflict, got {other:?}"),
    }

    let divergent_dir = tempdir().expect("divergent destination tempdir");
    let divergent_path = divergent_dir.path().join("memory.db");
    let mut divergent = MemoryStore::open_with_label(&divergent_path.to_string_lossy(), "global")
        .expect("open divergent destination");
    let mut existing = envelope.payload.clone();
    existing.text = "destination already contains another payload".into();
    existing.revision = 7;
    divergent.upsert(&existing).expect("seed divergent object");
    match divergent
        .apply_outbox_destination(&envelope)
        .expect("object divergence is a typed result")
    {
        OutboxDestinationApplyResult::Conflict(receipt) => {
            assert_eq!(
                receipt.reason,
                OutboxDestinationConflictReason::ExistingObjectDiverges
            );
            assert_eq!(receipt.destination_object_revision, 7);
        }
        other => panic!("expected object conflict, got {other:?}"),
    }
    let preserved = divergent
        .get_with_options(&existing.id, true)
        .expect("read divergent object")
        .expect("divergent object survives");
    assert_eq!(preserved.text, existing.text);
    assert_eq!(preserved.revision, 7);
}

#[test]
fn portable_destination_apply_rejects_malformed_claim_before_partition_or_object_mutation() {
    let source_dir = tempdir().expect("source tempdir");
    let destination_dir = tempdir().expect("destination tempdir");
    let source_path = source_dir.path().join("memory.db");
    let destination_path = destination_dir.path().join("memory.db");
    let (_source, mut envelope) = source_and_envelope(&source_path, "destination-apply-invalid");
    envelope.claimed.event.state = portable_kernel::OutboxState::Pending;

    let mut destination =
        MemoryStore::open_with_label(&destination_path.to_string_lossy(), "global")
            .expect("open destination");
    let error = destination
        .apply_outbox_destination(&envelope)
        .expect_err("pending claim must refuse before mutation");
    assert!(error.to_string().contains("in_flight"), "{error}");
    assert!(destination
        .get_with_options("destination-apply-invalid", true)
        .expect("destination read")
        .is_none());
}

#[test]
fn portable_destination_apply_rejects_reserved_local_wins_event_id_before_mutation() {
    let source_dir = tempdir().expect("source tempdir");
    let destination_dir = tempdir().expect("destination tempdir");
    let object_id = "destination-apply-local-wins";
    let (_source, mut envelope) =
        source_and_envelope(&source_dir.path().join("memory.db"), object_id);
    envelope.claimed.event.event_id.push_str("::local-wins");

    let mut destination = MemoryStore::open_with_label(
        &destination_dir.path().join("memory.db").to_string_lossy(),
        "global",
    )
    .expect("open destination");
    let error = destination
        .apply_outbox_destination(&envelope)
        .expect_err("reserved LocalWins event ids must refuse");
    assert!(
        error.to_string().contains("reserved successor suffix"),
        "{error}"
    );
    assert!(destination
        .get_with_options(object_id, true)
        .expect("destination read")
        .is_none());

    envelope.claimed.event.event_id = format!("evt-{object_id}");
    envelope.destination.partition = "destination-probe".into();
    assert!(matches!(
        destination
            .apply_outbox_destination(&envelope)
            .expect("valid envelope after refusal"),
        OutboxDestinationApplyResult::Applied(_)
    ));
}
