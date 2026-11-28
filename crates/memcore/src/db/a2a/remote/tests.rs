use super::*;
use crate::db::common::now_utc_iso;
use crate::db::session_claims::{record_unverified_admission, UnverifiedAdmissionState};
use crate::db::{has_current_verified_admission, StoreProfile};
use crate::types::GcConfig;
use serde_json::json;

const REPOSITORY: &str = "github.com/example/tachi";
const OTHER_REPOSITORY: &str = "github.com/example/other";

fn enable_extensions() {
    crate::db::enable_simple_auto_extension().unwrap();
    crate::db::register_sqlite_vec();
}

fn memory_connection() -> Connection {
    enable_extensions();
    Connection::open_in_memory().unwrap()
}

fn initialize(conn: &Connection) {
    crate::db::configure_connection(conn).unwrap();
    crate::db::init_schema(conn).unwrap();
}

fn identity(conn: &Connection, identity: &str) {
    conn.execute(
        "INSERT OR IGNORE INTO agent_identities(agent_identity_id,created_at) VALUES (?1,?2)",
        params![identity, now_utc_iso()],
    )
    .unwrap();
}

// Test-only evidence representing receipts produced by the real server TLS
// writer. The public remote API still rechecks every binding and revocation.
fn verified(
    conn: &Connection,
    identity_id: &str,
    suffix: &str,
    domain: &str,
    method: &str,
) -> VerifiedAdmissionBinding {
    identity(conn, identity_id);
    let binding = VerifiedAdmissionBinding {
        admission_id: format!("admission-{suffix}"),
        agent_identity_id: identity_id.into(),
        connection_id: format!("connection-{suffix}"),
        issuer_id: "gateway-issuer".into(),
        verification_method: method.into(),
        verification_version: "v1".into(),
        trust_domain: domain.into(),
        verification_scope: "agent_identity:remote_admission".into(),
    };
    conn.execute(
        "INSERT INTO identity_admissions(admission_id,agent_identity_id,connection_id,state,created_at)
         VALUES (?1,?2,?3,'verified',?4)",
        params![binding.admission_id, identity_id, binding.connection_id, now_utc_iso()],
    ).unwrap();
    conn.execute(
        "INSERT INTO identity_admission_verification_receipts
         (receipt_id,admission_id,agent_identity_id,connection_id,issuer_id,verification_method,
          verification_version,trust_domain,verification_scope,evidence_digest,evidence_ref,
          evidence_issued_at,evidence_expires_at,nonce,idempotency_key,request_digest,current_state,verified_at)
         VALUES (?1,?2,?3,?4,?5,?6,'v1',?7,'agent_identity:remote_admission',?8,?9,
                 strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 hour'),
                 strftime('%Y-%m-%dT%H:%M:%fZ','now','+1 day'),?10,?11,?8,'verified',?12)",
        params![format!("receipt-{suffix}"), binding.admission_id, identity_id,
            binding.connection_id, binding.issuer_id, method, domain, "a".repeat(64),
            format!("attestation:mtls-peer:{:x}", Sha256::digest(identity_id.as_bytes())),
            format!("nonce-{suffix}"), format!("key-{suffix}"), now_utc_iso()],
    ).unwrap();
    binding
}

fn setup(
    conn: &Connection,
) -> (
    VerifiedAdmissionBinding,
    A2aTransitionActor,
    RemoteTaskAccess,
) {
    initialize(conn);
    identity(conn, "gateway");
    record_unverified_admission(
        conn,
        "gateway-admission",
        "gateway",
        "gateway-connection",
        UnverifiedAdmissionState::SelfAsserted,
    )
    .unwrap();
    let binding = verified(
        conn,
        "origin",
        "origin-first",
        "workspace:test",
        "mtls-peer",
    );
    let gateway = A2aTransitionActor {
        agent_identity_id: "gateway".into(),
        connection_id: "gateway-connection".into(),
    };
    let access = RemoteTaskAccess {
        destination_agent_identity_id: "gateway".into(),
        destination_host_id: "destination-host".into(),
        grants: vec![RemoteJobGrant {
            repository: REPOSITORY.into(),
            job: "review_revision".into(),
        }],
    };
    (binding, gateway, access)
}

fn body(request: &mut NewRemoteTask, binding: &VerifiedAdmissionBinding) {
    request.canonical_body = serde_json::to_string(&json!({
        "kind": A2A_REMOTE_JOB_KIND,
        "sender": binding.agent_identity_id,
        "recipient": request.recipient_agent_identity_id,
        "trust_domain": binding.trust_domain,
        "idempotency_key": request.idempotency_key,
        "nonce": request.nonce,
        "issued_at": request.issued_at,
        "expires_at": request.expires_at,
        "repository": request.header.repository,
        "head_sha": request.header.head_sha,
        "origin_claim": {"claim_id": request.header.origin_claim_id,
                         "transition_version": request.header.origin_claim_version},
        "job": request.header.job,
        "question": "private request text must never appear in a task snapshot"
    }))
    .unwrap();
}

fn request(id: &str, binding: &VerifiedAdmissionBinding) -> NewRemoteTask {
    let now = jiff::Timestamp::now();
    let mut request = NewRemoteTask {
        envelope_id: id.into(),
        canonical_body: String::new(),
        nonce: format!("nonce-{id}"),
        header: RemoteTaskHeader {
            repository: REPOSITORY.into(),
            job: "review_revision".into(),
            head_sha: "a".repeat(40),
            pull_request: None,
            origin_claim_id: "origin-claim".into(),
            origin_claim_version: 3,
            destination_host_id: "destination-host".into(),
        },
        idempotency_key: format!("key-{id}"),
        issued_at: crate::time_compat::millis_z(now - jiff::SignedDuration::from_secs(60)),
        expires_at: crate::time_compat::millis_z(now + jiff::SignedDuration::from_secs(300)),
        recipient_agent_identity_id: "gateway".into(),
    };
    body(&mut request, binding);
    request
}

fn counts(conn: &Connection) -> (i64, i64) {
    conn.query_row(
        "SELECT (SELECT COUNT(*) FROM a2a_envelopes),(SELECT COUNT(*) FROM a2a_delivery_receipts)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap()
}

fn created(outcome: RemoteTaskInsertOutcome) -> RemoteTask {
    match outcome {
        RemoteTaskInsertOutcome::Created(task) => task,
        RemoteTaskInsertOutcome::Replay(_) => panic!("expected new task"),
    }
}

#[test]
fn remote_rejection_has_real_gateway_receipts_and_atomic_local_isolation() {
    let mut conn = memory_connection();
    let (binding, gateway, access) = setup(&conn);
    let mut submission = request("first", &binding);
    let mut value: serde_json::Value = serde_json::from_str(&submission.canonical_body).unwrap();
    value["question"] = "x".repeat(5000).into();
    submission.canonical_body = serde_json::to_string(&value).unwrap();
    let task =
        created(insert_remote_rejection(&conn, &binding, &gateway, &access, &submission).unwrap());
    assert_eq!(task.envelope.current_state, "remote_rejected");
    assert_eq!(task.envelope.state_version, 2);
    assert_eq!(task.envelope.issuer_identity_assurance, "verified");
    assert_eq!(task.envelope.issuer_admission_id, binding.admission_id);
    assert_eq!(
        task.envelope.body_digest,
        format!("{:x}", Sha256::digest(submission.canonical_body.as_bytes()))
    );
    assert_eq!(task.receipts.len(), 2);
    for (receipt, state, version) in [
        (&task.receipts[0], "remote_received", 1),
        (&task.receipts[1], "remote_rejected", 2),
    ] {
        assert_eq!(receipt.receipt.state, state);
        assert_eq!(receipt.receipt.envelope_version, version);
        assert_eq!(
            receipt.receipt.actor_agent_identity_id,
            gateway.agent_identity_id
        );
        assert_eq!(receipt.receipt.actor_admission_id, "gateway-admission");
        assert_eq!(receipt.receipt.identity_assurance, "self_asserted");
        assert_eq!(receipt.receipt.trust_domain, "same_host");
    }
    assert_eq!(task.receipts[0].detail, None);
    assert_eq!(
        task.receipts[1].detail,
        Some(RemoteReceiptDetail::Refusal {
            reason: RemoteRejectionReason::ExecutionUnavailable
        })
    );
    let serialized = serde_json::to_value(&task).unwrap();
    assert!(serialized["envelope"].get("body").is_none());
    assert!(!serialized.to_string().contains(&"x".repeat(5000)));
    for identity in [&binding.agent_identity_id, &gateway.agent_identity_id] {
        assert!(crate::db::a2a::list_a2a_status(&conn, identity, 100)
            .unwrap()
            .is_empty());
    }
    assert!(
        crate::db::a2a::consume_a2a_for_recipient(&mut conn, &gateway, 100, &now_utc_iso())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM session_claims", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );

    conn.execute_batch("CREATE TEMP TRIGGER refuse_remote_rejection BEFORE INSERT ON a2a_delivery_receipts WHEN NEW.state='remote_rejected' BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;").unwrap();
    let failure = request("atomic-failure", &binding);
    assert!(insert_remote_rejection(&conn, &binding, &gateway, &access, &failure).is_err());
    assert_eq!(
        counts(&conn),
        (1, 2),
        "neither a new envelope nor its received receipt survives failure"
    );
}

#[test]
fn replay_survives_reconnect_expiry_and_real_retention_without_body_disclosure() {
    let mut conn = memory_connection();
    let (binding, gateway, access) = setup(&conn);
    let first = request("live", &binding);
    let task =
        created(insert_remote_rejection(&conn, &binding, &gateway, &access, &first).unwrap());
    let reconnect = verified(
        &conn,
        "origin",
        "origin-reconnect",
        "workspace:test",
        "mtls-peer",
    );
    let mut retransmit = first.clone();
    retransmit.envelope_id = "fresh-id-must-not-be-used".into();
    assert_eq!(
        insert_remote_rejection(&conn, &reconnect, &gateway, &access, &retransmit).unwrap(),
        RemoteTaskInsertOutcome::Replay(task)
    );

    // A historical persisted snapshot predates this test's wall clock. It is
    // seeded from a real producer row, with matching historical body/digest,
    // to exercise expiry and the actual 90-day GC without sleeping or changing
    // the authority clock used by production writes.
    let mut historical = request("historical", &binding);
    let old = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(100 * 86_400);
    historical.issued_at = crate::time_compat::millis_z(old);
    historical.expires_at =
        crate::time_compat::millis_z(old + jiff::SignedDuration::from_secs(600));
    body(&mut historical, &binding);
    assert!(
        insert_remote_rejection(&conn, &reconnect, &gateway, &access, &historical).is_err(),
        "expired first arrivals cannot create tasks"
    );
    conn.execute(
        "INSERT INTO a2a_envelopes
         SELECT ?1,kind,issuer_agent_identity_id,issuer_admission_id,recipient_agent_identity_id,
                recipient_admission_id,subject_ref,?2,?3,issuer_identity_assurance,recipient_identity_assurance,
                issuer_trust_domain,recipient_trust_domain,issuer_trust_basis,recipient_trust_basis,?4,?5,?6,
                current_state,state_version,canonical_project,?7,remote_metadata
         FROM a2a_envelopes WHERE envelope_id='live'",
        params![historical.envelope_id, historical.canonical_body,
            format!("{:x}", Sha256::digest(historical.canonical_body.as_bytes())), historical.idempotency_key,
            historical.issued_at, historical.expires_at, historical.nonce],
    ).unwrap();
    conn.execute(
        "INSERT INTO a2a_delivery_receipts
         SELECT ?1 || ':' || state || ':' || envelope_version,?1,envelope_version,state,actor_agent_identity_id,
                actor_admission_id,identity_assurance,trust_domain,trust_basis,?2,detail
         FROM a2a_delivery_receipts WHERE envelope_id='live'",
        params![historical.envelope_id, historical.issued_at],
    ).unwrap();
    let before = read_remote_task(&conn, &reconnect, &access, "historical")
        .unwrap()
        .unwrap();
    assert_eq!(
        insert_remote_rejection(&conn, &reconnect, &gateway, &access, &historical).unwrap(),
        RemoteTaskInsertOutcome::Replay(before.clone())
    );
    let gc =
        crate::db::stats_gc::gc_tables(&mut conn, &GcConfig::default(), StoreProfile::TachiFull)
            .unwrap();
    assert_eq!(gc["a2a_bodies_scrubbed"], 1);
    assert!(conn
        .query_row(
            "SELECT body IS NULL FROM a2a_envelopes WHERE envelope_id='historical'",
            [],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());
    assert_eq!(
        read_remote_task(&conn, &reconnect, &access, "historical").unwrap(),
        Some(before.clone())
    );
    assert_eq!(
        insert_remote_rejection(&conn, &reconnect, &gateway, &access, &historical).unwrap(),
        RemoteTaskInsertOutcome::Replay(before)
    );
    assert_eq!(counts(&conn), (2, 4));
}

#[test]
fn sender_wide_replay_nonce_and_body_header_binding_cannot_be_reassigned() {
    let conn = memory_connection();
    let (binding, gateway, mut access) = setup(&conn);
    access.grants.push(RemoteJobGrant {
        repository: OTHER_REPOSITORY.into(),
        job: "review_revision".into(),
    });
    let first = request("first", &binding);
    insert_remote_rejection(&conn, &binding, &gateway, &access, &first).unwrap();
    let mut changed = first.clone();
    changed.envelope_id = "cross-repository".into();
    changed.header.repository = OTHER_REPOSITORY.into();
    body(&mut changed, &binding);
    assert!(matches!(
        insert_remote_rejection(&conn, &binding, &gateway, &access, &changed),
        Err(MemoryError::Duplicate(_))
    ));
    changed.idempotency_key = "different-key".into();
    body(&mut changed, &binding);
    assert!(
        matches!(
            insert_remote_rejection(&conn, &binding, &gateway, &access, &changed),
            Err(MemoryError::Duplicate(_))
        ),
        "nonce cannot move to another key"
    );
    let mut mismatched = request("header-mismatch", &binding);
    mismatched.header.repository = OTHER_REPOSITORY.into();
    assert!(matches!(
        insert_remote_rejection(&conn, &binding, &gateway, &access, &mismatched),
        Err(MemoryError::InvalidArg(_))
    ));
    let mut future = request("future", &binding);
    future.issued_at = future.expires_at.clone();
    future.expires_at =
        crate::time_compat::millis_z(jiff::Timestamp::now() + jiff::SignedDuration::from_secs(600));
    body(&mut future, &binding);
    assert!(insert_remote_rejection(&conn, &binding, &gateway, &access, &future).is_err());
    let mut overlong = request("overlong", &binding);
    overlong.expires_at =
        crate::time_compat::millis_z(jiff::Timestamp::now() + jiff::SignedDuration::from_secs(901));
    body(&mut overlong, &binding);
    assert!(insert_remote_rejection(&conn, &binding, &gateway, &access, &overlong).is_err());
    assert_eq!(counts(&conn), (1, 2));
}

#[test]
fn first_admission_tolerates_a_bounded_origin_clock_ahead_but_never_local_expiry() {
    let conn = memory_connection();
    let (binding, gateway, access) = setup(&conn);
    let stamped_by = |id: &str, issued_in: i64| {
        let now = jiff::Timestamp::now();
        let mut request = request(id, &binding);
        // `issued_in` > 0 models an origin whose clock runs ahead of this host.
        request.issued_at =
            crate::time_compat::millis_z(now + jiff::SignedDuration::from_secs(issued_in));
        request.expires_at =
            crate::time_compat::millis_z(now + jiff::SignedDuration::from_secs(issued_in + 300));
        body(&mut request, &binding);
        request
    };
    let half = REMOTE_CLOCK_SKEW_TOLERANCE_SECS / 2;
    assert!(matches!(
        insert_remote_rejection(
            &conn,
            &binding,
            &gateway,
            &access,
            &stamped_by("ahead-within-tolerance", half)
        )
        .unwrap(),
        RemoteTaskInsertOutcome::Created(_)
    ));
    assert!(matches!(
        insert_remote_rejection(
            &conn,
            &binding,
            &gateway,
            &access,
            &stamped_by(
                "ahead-beyond-tolerance",
                REMOTE_CLOCK_SKEW_TOLERANCE_SECS + half
            )
        ),
        Err(MemoryError::InvalidArg(_))
    ));
    // The tolerance is not a lease extension: a window that closed on this
    // host's clock less than the tolerance ago is still refused.
    let now = jiff::Timestamp::now();
    let mut closed = request("closed-window", &binding);
    closed.issued_at =
        crate::time_compat::millis_z(now - jiff::SignedDuration::from_secs(2 * half));
    closed.expires_at = crate::time_compat::millis_z(now - jiff::SignedDuration::from_secs(half));
    body(&mut closed, &binding);
    assert!(matches!(
        insert_remote_rejection(&conn, &binding, &gateway, &access, &closed),
        Err(MemoryError::InvalidArg(_))
    ));
    assert_eq!(counts(&conn), (1, 2));
}

#[test]
fn retained_task_quota_bounds_one_issuer_and_never_blocks_replay() {
    let conn = memory_connection();
    let (binding, gateway, access) = setup(&conn);
    let first = request("first", &binding);
    let task =
        created(insert_remote_rejection(&conn, &binding, &gateway, &access, &first).unwrap());
    let other = verified(
        &conn,
        "other-origin",
        "other-first",
        "workspace:test",
        "mtls-peer",
    );
    insert_remote_rejection(&conn, &other, &gateway, &access, &request("other", &other)).unwrap();
    // Fill this issuer's bound by copying its real producer row.
    conn.execute_batch(&format!(
        "WITH RECURSIVE seq(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM seq
                                   WHERE x < {})
         INSERT INTO a2a_envelopes
         SELECT 'bulk-' || x,kind,issuer_agent_identity_id,issuer_admission_id,
                recipient_agent_identity_id,recipient_admission_id,subject_ref,body,body_digest,
                issuer_identity_assurance,recipient_identity_assurance,issuer_trust_domain,
                recipient_trust_domain,issuer_trust_basis,recipient_trust_basis,
                'bulk-key-' || x,created_at,expires_at,current_state,state_version,
                canonical_project,'bulk-nonce-' || x,remote_metadata
         FROM a2a_envelopes JOIN seq WHERE envelope_id='first'",
        MAX_RETAINED_REMOTE_TASKS_PER_PEER - 1,
    ))
    .unwrap();
    let refused = insert_remote_rejection(
        &conn,
        &binding,
        &gateway,
        &access,
        &request("over-quota", &binding),
    )
    .unwrap_err();
    assert!(is_retained_task_quota_exceeded(&refused), "{refused}");
    // Another issuer is unaffected, and an exact replay still returns its task.
    assert!(matches!(
        insert_remote_rejection(
            &conn,
            &other,
            &gateway,
            &access,
            &request("other-next", &other)
        )
        .unwrap(),
        RemoteTaskInsertOutcome::Created(_)
    ));
    assert_eq!(
        insert_remote_rejection(&conn, &binding, &gateway, &access, &first).unwrap(),
        RemoteTaskInsertOutcome::Replay(task)
    );
    // Retention scrubs bodies; capacity returns with them.
    conn.execute(
        "UPDATE a2a_envelopes SET body=NULL WHERE envelope_id LIKE 'bulk-%'",
        [],
    )
    .unwrap();
    assert!(matches!(
        insert_remote_rejection(
            &conn,
            &binding,
            &gateway,
            &access,
            &request("after-scrub", &binding)
        )
        .unwrap(),
        RemoteTaskInsertOutcome::Created(_)
    ));
}

#[test]
fn reads_counts_cursors_and_replay_recheck_principal_domain_destination_host_and_grants() {
    let conn = memory_connection();
    let (binding, gateway, access) = setup(&conn);
    for id in ["visible-a", "visible-b"] {
        insert_remote_rejection(&conn, &binding, &gateway, &access, &request(id, &binding))
            .unwrap();
    }
    let mut broad = access.clone();
    broad.grants.push(RemoteJobGrant {
        repository: OTHER_REPOSITORY.into(),
        job: "review_revision".into(),
    });
    let mut hidden = request("other-repository", &binding);
    hidden.header.repository = OTHER_REPOSITORY.into();
    body(&mut hidden, &binding);
    insert_remote_rejection(&conn, &binding, &gateway, &broad, &hidden).unwrap();
    let page = list_remote_tasks(&conn, &binding, &access, 1, None).unwrap();
    assert_eq!(page.total_size, 2);
    assert_eq!(page.tasks.len(), 1);
    let reconnect = verified(
        &conn,
        "origin",
        "reconnected",
        "workspace:test",
        "mtls-peer",
    );
    let next =
        list_remote_tasks(&conn, &reconnect, &access, 1, page.next_cursor.as_deref()).unwrap();
    assert_eq!(next.total_size, 2);
    assert_eq!(next.tasks.len(), 1);
    assert_ne!(
        page.tasks[0].envelope.envelope_id,
        next.tasks[0].envelope.envelope_id
    );
    assert_eq!(next.next_cursor, None);
    assert_eq!(
        read_remote_task(&conn, &binding, &access, "other-repository").unwrap(),
        None
    );
    let unavailable_cursor =
        list_remote_tasks(&conn, &binding, &access, 1, Some("other-repository"))
            .unwrap_err()
            .to_string();
    assert_eq!(
        unavailable_cursor,
        list_remote_tasks(&conn, &binding, &access, 1, Some("missing"))
            .unwrap_err()
            .to_string()
    );

    let other_peer = verified(
        &conn,
        "another-origin",
        "another-peer",
        "workspace:test",
        "mtls-peer",
    );
    let other_domain = verified(
        &conn,
        "origin",
        "another-domain",
        "workspace:other",
        "mtls-peer",
    );
    for caller in [&other_peer, &other_domain] {
        assert_eq!(
            read_remote_task(&conn, caller, &access, "visible-a").unwrap(),
            None
        );
        let page = list_remote_tasks(&conn, caller, &access, 100, None).unwrap();
        assert_eq!(page.total_size, 0);
        assert!(page.tasks.is_empty());
        assert_eq!(
            list_remote_tasks(&conn, caller, &access, 1, Some("visible-a"))
                .unwrap_err()
                .to_string(),
            unavailable_cursor
        );
    }
    let first = request("access-replay", &binding);
    insert_remote_rejection(&conn, &binding, &gateway, &access, &first).unwrap();
    let mut other_host = access.clone();
    other_host.destination_host_id = "new-destination-host".into();
    let mut other_destination = access.clone();
    other_destination.destination_agent_identity_id = "another-gateway".into();
    let mut grant_removed = access.clone();
    grant_removed.grants.clear();
    for scope in [&other_host, &other_destination, &grant_removed] {
        assert_eq!(
            read_remote_task(&conn, &binding, scope, "access-replay").unwrap(),
            None
        );
        assert_eq!(
            list_remote_tasks(&conn, &binding, scope, 100, None)
                .unwrap()
                .total_size,
            0
        );
        assert!(insert_remote_rejection(&conn, &binding, &gateway, scope, &first).is_err());
    }
    // Host is not part of the business digest. A moved gateway still cannot
    // return its predecessor's task merely by supplying the identical bytes.
    let mut moved = first.clone();
    moved.header.destination_host_id = other_host.destination_host_id.clone();
    assert!(matches!(
        insert_remote_rejection(&conn, &binding, &gateway, &other_host, &moved),
        Err(MemoryError::Duplicate(_))
    ));
}

#[test]
fn revoked_or_wrong_policy_admissions_cannot_read_or_mint_remote_receipts() {
    let conn = memory_connection();
    let (binding, gateway, access) = setup(&conn);
    let first = request("before-revocation", &binding);
    insert_remote_rejection(&conn, &binding, &gateway, &access, &first).unwrap();
    let device = verified(
        &conn,
        "device-origin",
        "device",
        "workspace:test",
        "device-envelope",
    );
    assert!(has_current_verified_admission(
        &conn,
        &device.admission_id,
        &device.agent_identity_id,
        &device.connection_id,
        &device.issuer_id,
        &device.verification_method,
        &device.verification_version,
        &device.trust_domain,
        &device.verification_scope
    )
    .unwrap());
    assert!(insert_remote_rejection(
        &conn,
        &device,
        &gateway,
        &access,
        &request("wrong-policy", &device)
    )
    .is_err());
    let fake_gateway = A2aTransitionActor {
        agent_identity_id: gateway.agent_identity_id.clone(),
        connection_id: binding.connection_id.clone(),
    };
    assert!(insert_remote_rejection(
        &conn,
        &binding,
        &fake_gateway,
        &access,
        &request("fake-gateway", &binding)
    )
    .is_err());
    conn.execute(
        "INSERT INTO identity_admission_verification_revocations
         (revocation_id,admission_id,issuer_id,evidence_digest,evidence_ref,nonce,revoked_at)
         VALUES ('revoke-origin',?1,?2,?3,'attestation:operator:retired-key','revoke-origin',?4)",
        params![
            binding.admission_id,
            binding.issuer_id,
            "b".repeat(64),
            now_utc_iso()
        ],
    )
    .unwrap();
    let fresh_connection = verified(
        &conn,
        "origin",
        "after-key-revocation",
        "workspace:test",
        "mtls-peer",
    );
    for caller in [&binding, &fresh_connection] {
        assert!(insert_remote_rejection(&conn, caller, &gateway, &access, &first).is_err());
        assert!(insert_remote_rejection(
            &conn,
            caller,
            &gateway,
            &access,
            &request("new-after-revoke", caller)
        )
        .is_err());
        assert!(read_remote_task(&conn, caller, &access, "before-revocation").is_err());
        assert!(list_remote_tasks(&conn, caller, &access, 100, None).is_err());
    }
    assert_eq!(
        counts(&conn),
        (1, 2),
        "revocation preserves historical task/receipt evidence"
    );
}

#[test]
fn concurrent_same_key_creates_one_task_and_one_receipt_pair() {
    use std::sync::{Arc, Barrier};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("owned-remote-mailbox.db");
    enable_extensions();
    let conn = Connection::open(&path).unwrap();
    let (binding, gateway, access) = setup(&conn);
    let request = request("concurrent", &binding);
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|index| {
            let path = path.clone();
            let binding = binding.clone();
            let gateway = gateway.clone();
            let access = access.clone();
            let mut request = request.clone();
            request.envelope_id = format!("concurrent-{index}");
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let conn = Connection::open(path).unwrap();
                crate::db::configure_connection(&conn).unwrap();
                conn.busy_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                barrier.wait();
                insert_remote_rejection(&conn, &binding, &gateway, &access, &request).unwrap()
            })
        })
        .collect();
    let outcomes: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, RemoteTaskInsertOutcome::Created(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, RemoteTaskInsertOutcome::Replay(_)))
            .count(),
        1
    );
    let tasks: Vec<_> = outcomes
        .into_iter()
        .map(|outcome| match outcome {
            RemoteTaskInsertOutcome::Created(task) | RemoteTaskInsertOutcome::Replay(task) => task,
        })
        .collect();
    assert_eq!(tasks[0], tasks[1]);
    assert_eq!(counts(&conn), (1, 2));
}

#[test]
fn admit_remote_task_allocates_local_claim_run_and_receipts_atomically() {
    let (_dir, conn, binding, gateway, access) = {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("admit.db");
        enable_extensions();
        let conn = Connection::open(&path).unwrap();
        let (binding, gateway, access) = setup(&conn);
        (dir, conn, binding, gateway, access)
    };
    let canonical_project = "d".repeat(64);
    let spec = RemoteLocalAdmissionSpec {
        canonical_project: canonical_project.clone(),
        adapter_kind: RemoteAdapterKind::NativeReview,
        policy_profile: "codex_sol_review".into(),
        resource_slot: "slot-mac-review".into(),
        max_concurrent_runs: 2,
        lease_ttl_secs: 600,
        issue_ref: "owner/repo#review_revision".into(),
        worktree_path: "/fixture/repo".into(),
        writable: false,
    };
    let req = request("admit-1", &binding);
    let outcome = admit_remote_task(
        &conn,
        &binding,
        &gateway,
        &access,
        &req,
        &RemoteAdmissionDecision::Admit(spec),
    )
    .unwrap();
    let RemoteTaskInsertOutcome::Created(task) = outcome else {
        panic!("expected Created outcome");
    };
    assert_eq!(task.envelope.current_state, "remote_admitted");
    assert_eq!(task.envelope.state_version, 2);
    let run = task.run.as_ref().expect("run row must exist");
    assert_eq!(run.canonical_project, canonical_project);
    assert_eq!(run.lifecycle_state, RemoteRunLifecycleState::Admitted);
    assert_eq!(run.cleanup_state, RemoteCleanupStatus::Pending);
    assert_eq!(task.receipts.len(), 2);
    assert_eq!(task.receipts[0].receipt.state, "remote_received");
    assert_eq!(task.receipts[1].receipt.state, "remote_admitted");
    let claim = crate::db::session_claims::get_claim(&conn, &run.local_claim_id)
        .unwrap()
        .expect("local WorkClaim must exist");
    assert_eq!(claim.state, crate::db::session_claims::ClaimState::Active);
    assert_eq!(claim.transition_version, 0);
    assert_eq!(
        crate::db::session_claims::work_claim_project(&conn, &run.local_claim_id).unwrap(),
        Some(canonical_project)
    );

    let replay = admit_remote_task(
        &conn,
        &binding,
        &gateway,
        &access,
        &req,
        &RemoteAdmissionDecision::Reject(RemoteRejectionReason::ExecutionUnavailable),
    )
    .unwrap();
    assert_eq!(replay, RemoteTaskInsertOutcome::Replay(task));
}

#[test]
fn admit_remote_task_enforces_resource_slot_limit_and_claim_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slot-claim.db");
    enable_extensions();
    let conn = Connection::open(&path).unwrap();
    let (binding, gateway, access) = setup(&conn);
    let canonical_project = "e".repeat(64);
    let spec = RemoteLocalAdmissionSpec {
        canonical_project: canonical_project.clone(),
        adapter_kind: RemoteAdapterKind::NativeReview,
        policy_profile: "codex_sol_review".into(),
        resource_slot: "slot-1".into(),
        max_concurrent_runs: 1,
        lease_ttl_secs: 600,
        issue_ref: "owner/repo#review_revision".into(),
        worktree_path: "/fixture/repo".into(),
        writable: false,
    };
    let first = admit_remote_task(
        &conn,
        &binding,
        &gateway,
        &access,
        &request("slot-first", &binding),
        &RemoteAdmissionDecision::Admit(spec.clone()),
    )
    .unwrap();
    assert!(
        matches!(first, RemoteTaskInsertOutcome::Created(t) if t.envelope.current_state == "remote_admitted")
    );

    let exhausted = admit_remote_task(
        &conn,
        &binding,
        &gateway,
        &access,
        &request("slot-second", &binding),
        &RemoteAdmissionDecision::Admit(RemoteLocalAdmissionSpec {
            issue_ref: "owner/repo#other".into(),
            ..spec.clone()
        }),
    )
    .unwrap();
    let RemoteTaskInsertOutcome::Created(exhausted_task) = exhausted else {
        panic!("expected durable rejection");
    };
    assert_eq!(exhausted_task.envelope.current_state, "remote_rejected");
    assert!(exhausted_task.run.is_none());
    assert_eq!(
        exhausted_task.receipts[1].detail,
        Some(RemoteReceiptDetail::Refusal {
            reason: RemoteRejectionReason::ResourceExhausted
        })
    );

    let mut conflicting_head_req = request("conflict-head", &binding);
    conflicting_head_req.header.head_sha = "b".repeat(40);
    body(&mut conflicting_head_req, &binding);
    let conflict = admit_remote_task(
        &conn,
        &binding,
        &gateway,
        &access,
        &conflicting_head_req,
        &RemoteAdmissionDecision::Admit(RemoteLocalAdmissionSpec {
            max_concurrent_runs: 4,
            ..spec
        }),
    )
    .unwrap();
    let RemoteTaskInsertOutcome::Created(conflict_task) = conflict else {
        panic!("expected durable claim conflict");
    };
    assert_eq!(conflict_task.envelope.current_state, "remote_rejected");
    assert!(conflict_task.run.is_none());
    assert_eq!(
        conflict_task.receipts[1].detail,
        Some(RemoteReceiptDetail::Refusal {
            reason: RemoteRejectionReason::ClaimConflict
        })
    );
}

#[test]
fn lifecycle_transitions_enforce_revocation_claim_start_terminal_and_cleanup_invariants() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lifecycle.db");
    enable_extensions();
    let conn = Connection::open(&path).unwrap();
    let (binding, gateway, access) = setup(&conn);
    let spec = RemoteLocalAdmissionSpec {
        canonical_project: "f".repeat(64),
        adapter_kind: RemoteAdapterKind::NativeReview,
        policy_profile: "codex_sol_review".into(),
        resource_slot: "slot-life".into(),
        max_concurrent_runs: 4,
        lease_ttl_secs: 600,
        issue_ref: "owner/repo#life".into(),
        worktree_path: "/fixture/repo".into(),
        writable: false,
    };

    // Completed without Started must be refused; Failed from Admitted succeeds.
    let req_never_started = request("life-admitted", &binding);
    admit_remote_task(
        &conn,
        &binding,
        &gateway,
        &access,
        &req_never_started,
        &RemoteAdmissionDecision::Admit(spec.clone()),
    )
    .unwrap();
    assert!(record_remote_task_terminal(
        &conn,
        &gateway,
        &req_never_started.envelope_id,
        RemoteTerminalOutcome::Completed,
        Some(&"c".repeat(64)),
        None,
        None,
    )
    .is_err());
    let failed = record_remote_task_terminal(
        &conn,
        &gateway,
        &req_never_started.envelope_id,
        RemoteTerminalOutcome::Failed,
        None,
        Some("spawn_failed"),
        None,
    )
    .unwrap();
    assert!(matches!(
        failed,
        RemoteTerminalTransition::Applied(ref run)
            if run.lifecycle_state == RemoteRunLifecycleState::Failed
    ));

    // Released local WorkClaim refuses Started.
    let req_released = request("life-released", &binding);
    let RemoteTaskInsertOutcome::Created(released_task) = admit_remote_task(
        &conn,
        &binding,
        &gateway,
        &access,
        &req_released,
        &RemoteAdmissionDecision::Admit(RemoteLocalAdmissionSpec {
            issue_ref: "owner/repo#released".into(),
            ..spec.clone()
        }),
    )
    .unwrap() else {
        panic!();
    };
    crate::db::session_claims::release_work_claim(
        &conn,
        &released_task.run.as_ref().unwrap().local_claim_id,
        &gateway.agent_identity_id,
        0,
        "operator_release",
    )
    .unwrap();
    assert!(record_remote_task_started(&conn, &gateway, &req_released.envelope_id, None).is_err());
    let reconciled = reconcile_active_remote_runs(&conn, &gateway).unwrap();
    assert_eq!(reconciled, vec![req_released.envelope_id.clone()]);

    // Happy path: Admitted -> Started -> Completed -> Cleanup + late terminal noop.
    let req_ok = request("life-ok", &binding);
    admit_remote_task(
        &conn,
        &binding,
        &gateway,
        &access,
        &req_ok,
        &RemoteAdmissionDecision::Admit(RemoteLocalAdmissionSpec {
            issue_ref: "owner/repo#ok".into(),
            ..spec.clone()
        }),
    )
    .unwrap();
    let started = record_remote_task_started(
        &conn,
        &gateway,
        &req_ok.envelope_id,
        Some("session:native-1"),
    )
    .unwrap();
    assert_eq!(started.lifecycle_state, RemoteRunLifecycleState::Started);
    let completed = record_remote_task_terminal(
        &conn,
        &gateway,
        &req_ok.envelope_id,
        RemoteTerminalOutcome::Completed,
        Some(&"a".repeat(64)),
        None,
        None,
    )
    .unwrap();
    assert!(matches!(
        completed,
        RemoteTerminalTransition::Applied(ref run)
            if run.lifecycle_state == RemoteRunLifecycleState::Completed
    ));
    let late = record_remote_task_terminal(
        &conn,
        &gateway,
        &req_ok.envelope_id,
        RemoteTerminalOutcome::Failed,
        None,
        Some("late_error"),
        None,
    )
    .unwrap();
    assert!(matches!(late, RemoteTerminalTransition::AlreadyTerminal(_)));

    let cleaned = record_remote_task_cleanup(
        &conn,
        &gateway,
        &req_ok.envelope_id,
        false,
        Some("cleanup_busy"),
    )
    .unwrap();
    assert_eq!(cleaned.cleanup_state, RemoteCleanupStatus::Failed);
    let reread = read_remote_task(&conn, &binding, &access, &req_ok.envelope_id)
        .unwrap()
        .unwrap();
    assert_eq!(reread.envelope.current_state, "remote_completed");
    assert_eq!(reread.envelope.state_version, 5);
    assert_eq!(
        reread.receipts.last().unwrap().receipt.state,
        "remote_cleanup"
    );

    // Revoked mTLS sender key refuses Started at launch boundary.
    let req_revoked = request("life-revoked", &binding);
    admit_remote_task(
        &conn,
        &binding,
        &gateway,
        &access,
        &req_revoked,
        &RemoteAdmissionDecision::Admit(RemoteLocalAdmissionSpec {
            issue_ref: "owner/repo#revoked".into(),
            ..spec
        }),
    )
    .unwrap();
    conn.execute(
        "INSERT INTO identity_admission_verification_revocations
         (revocation_id,admission_id,issuer_id,evidence_digest,evidence_ref,nonce,revoked_at)
         VALUES ('rev-life',?1,?2,?3,?4,'rev-life',?5)",
        params![
            binding.admission_id,
            binding.issuer_id,
            "a".repeat(64),
            format!("attestation:mtls-peer:{:x}", Sha256::digest(b"origin")),
            now_utc_iso()
        ],
    )
    .unwrap();
    assert!(record_remote_task_started(&conn, &gateway, &req_revoked.envelope_id, None).is_err());
}

#[test]
fn cancel_requested_is_idempotent_and_isolated_by_peer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cancel.db");
    enable_extensions();
    let conn = Connection::open(&path).unwrap();
    let (binding, gateway, access) = setup(&conn);
    let other_binding = verified(
        &conn,
        "other-origin",
        "other-conn",
        "workspace:test",
        "mtls-peer",
    );
    let req = request("cancel-1", &binding);
    admit_remote_task(
        &conn,
        &binding,
        &gateway,
        &access,
        &req,
        &RemoteAdmissionDecision::Admit(RemoteLocalAdmissionSpec {
            canonical_project: "1".repeat(64),
            adapter_kind: RemoteAdapterKind::NativeReview,
            policy_profile: "codex_sol_review".into(),
            resource_slot: "slot-cancel".into(),
            max_concurrent_runs: 2,
            lease_ttl_secs: 600,
            issue_ref: "owner/repo#cancel".into(),
            worktree_path: "/fixture/repo".into(),
            writable: false,
        }),
    )
    .unwrap();
    assert!(record_remote_task_cancel_requested(
        &conn,
        &other_binding,
        &gateway,
        &access,
        &req.envelope_id
    )
    .unwrap()
    .is_none());
    let canceled =
        record_remote_task_cancel_requested(&conn, &binding, &gateway, &access, &req.envelope_id)
            .unwrap()
            .unwrap();
    assert_eq!(canceled.envelope.current_state, "remote_cancel_requested");
    assert_eq!(canceled.envelope.state_version, 3);
    let idempotent =
        record_remote_task_cancel_requested(&conn, &binding, &gateway, &access, &req.envelope_id)
            .unwrap()
            .unwrap();
    assert_eq!(idempotent, canceled);
}

#[test]
fn expired_admitted_or_canceled_run_reconciles_freeing_slot_for_next_task() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reconcile-slot.db");
    enable_extensions();
    let conn = Connection::open(&path).unwrap();
    let (binding, gateway, access) = setup(&conn);
    let spec = RemoteLocalAdmissionSpec {
        canonical_project: "2".repeat(64),
        adapter_kind: RemoteAdapterKind::NativeReview,
        policy_profile: "codex_sol_review".into(),
        resource_slot: "slot-reconcile".into(),
        max_concurrent_runs: 1,
        lease_ttl_secs: 600,
        issue_ref: "owner/repo#slot-reconcile".into(),
        worktree_path: "/fixture/repo".into(),
        writable: false,
    };
    let req1 = request("slot-rec-1", &binding);
    let RemoteTaskInsertOutcome::Created(task1) = admit_remote_task(
        &conn,
        &binding,
        &gateway,
        &access,
        &req1,
        &RemoteAdmissionDecision::Admit(spec.clone()),
    )
    .unwrap() else {
        panic!("expected Created");
    };
    record_remote_task_cancel_requested(&conn, &binding, &gateway, &access, &req1.envelope_id)
        .unwrap();
    // Expire the start window of the unstarted cancel_requested run cleanly via DB timestamp.
    conn.execute(
        "UPDATE a2a_envelopes SET created_at='1999-01-01T00:00:00.000Z', expires_at='2000-01-01T00:00:00.000Z' WHERE envelope_id=?1",
        [&req1.envelope_id],
    )
    .unwrap();
    let req2 = request("slot-rec-2", &binding);
    let RemoteTaskInsertOutcome::Created(task2) = admit_remote_task(
        &conn,
        &binding,
        &gateway,
        &access,
        &req2,
        &RemoteAdmissionDecision::Admit(spec),
    )
    .unwrap() else {
        panic!("expected Created");
    };
    assert_eq!(task2.envelope.current_state, "remote_admitted");
    let failed1 = read_remote_task(&conn, &binding, &access, &task1.envelope.envelope_id)
        .unwrap()
        .unwrap();
    assert_eq!(failed1.envelope.current_state, "remote_failed");
    assert_eq!(
        failed1.run.as_ref().unwrap().lifecycle_state,
        RemoteRunLifecycleState::Failed
    );
    let released_claim =
        crate::db::session_claims::get_claim(&conn, &task1.run.unwrap().local_claim_id)
            .unwrap()
            .unwrap();
    assert_eq!(
        released_claim.state,
        crate::db::session_claims::ClaimState::Released
    );
}

#[test]
fn gc_session_claims_prunes_released_remote_claims_without_fk_failure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gc-claims.db");
    enable_extensions();
    let conn = Connection::open(&path).unwrap();
    let (binding, gateway, access) = setup(&conn);
    let canonical_project = "3".repeat(64);
    let req = request("gc-claim-run", &binding);
    let RemoteTaskInsertOutcome::Created(task) = admit_remote_task(
        &conn,
        &binding,
        &gateway,
        &access,
        &req,
        &RemoteAdmissionDecision::Admit(RemoteLocalAdmissionSpec {
            canonical_project: canonical_project.clone(),
            adapter_kind: RemoteAdapterKind::NativeReview,
            policy_profile: "codex_sol_review".into(),
            resource_slot: "slot-gc".into(),
            max_concurrent_runs: 1,
            lease_ttl_secs: 600,
            issue_ref: "owner/repo#gc".into(),
            worktree_path: "/fixture/repo".into(),
            writable: false,
        }),
    )
    .unwrap() else {
        panic!("expected Created");
    };
    record_remote_task_terminal(
        &conn,
        &gateway,
        &req.envelope_id,
        RemoteTerminalOutcome::Failed,
        None,
        Some("spawn_failed"),
        None,
    )
    .unwrap();
    let remote_claim_id = task.run.unwrap().local_claim_id;

    // Seed an unrelated released claim to confirm whole-batch GC is not blocked.
    let now = jiff::Timestamp::now();
    crate::db::session_claims::insert_local_project_work_claim(
        &conn,
        &crate::db::session_claims::NewWorkClaim {
            claim_id: "unrelated-claim".into(),
            agent_identity_id: gateway.agent_identity_id.clone(),
            session_client: None,
            issue_ref: Some("owner/repo#unrelated".into()),
            flow_id: None,
            dispatch_id: None,
            branch: "main".into(),
            worktree_path: "/fixture/other".into(),
            declared_file_scope: "src/lib.rs".into(),
            role: "reviewer".into(),
            mode: crate::db::session_claims::WorkClaimMode::ReadOnly,
            expected_head: "a".repeat(40),
            lease_expires_at: crate::time_compat::millis_z(
                now + jiff::SignedDuration::from_mins(5),
            ),
            created_at: crate::time_compat::millis_z(now),
        },
        &gateway.connection_id,
        &canonical_project,
    )
    .unwrap();
    crate::db::session_claims::release_work_claim(
        &conn,
        "unrelated-claim",
        &gateway.agent_identity_id,
        0,
        "done",
    )
    .unwrap();

    conn.execute(
        "UPDATE session_claims SET released_at='2000-01-01T00:00:00.000Z'
         WHERE claim_id IN (?1,'unrelated-claim')",
        [&remote_claim_id],
    )
    .unwrap();
    let gc = crate::db::session_claims::gc_session_claims(&conn, now, 30, 1).unwrap();
    assert_eq!(gc.released_pruned, 2);
    assert!(
        crate::db::session_claims::get_claim(&conn, &remote_claim_id)
            .unwrap()
            .is_none()
    );
    let preserved = read_remote_task(&conn, &binding, &access, &req.envelope_id)
        .unwrap()
        .unwrap();
    assert_eq!(preserved.run.unwrap().local_claim_id, remote_claim_id);
}

#[test]
fn expired_supervised_run_retains_slot_until_supervisor_records_terminal() {
    let dir = tempfile::tempdir().unwrap();
    enable_extensions();
    let conn = Connection::open(dir.path().join("supervised-slot.db")).unwrap();
    let (binding, gateway, access) = setup(&conn);
    let decision = RemoteAdmissionDecision::AdmitSupervised(RemoteLocalAdmissionSpec {
        canonical_project: "e".repeat(64),
        adapter_kind: RemoteAdapterKind::NativeReview,
        policy_profile: "codex_sol_review".into(),
        resource_slot: "supervised-slot".into(),
        max_concurrent_runs: 1,
        lease_ttl_secs: 600,
        issue_ref: "owner/repo#review_revision".into(),
        worktree_path: "/fixture/repo".into(),
        writable: false,
    });
    let admit = |id| {
        let RemoteTaskInsertOutcome::Created(task) = admit_remote_task(
            &conn,
            &binding,
            &gateway,
            &access,
            &request(id, &binding),
            &decision,
        )
        .unwrap() else {
            panic!("expected a new task");
        };
        task
    };
    let task = admit("supervised-first");
    let run = task.run.unwrap();
    record_remote_task_started(&conn, &gateway, &task.envelope.envelope_id, None).unwrap();
    conn.execute(
        "UPDATE session_claims SET lease_expires_at='2000-01-01T00:00:00.000Z' WHERE claim_id=?1",
        [&run.local_claim_id],
    )
    .unwrap();

    let blocked = admit("supervised-blocked");
    assert_eq!(
        blocked.receipts[1].detail,
        Some(RemoteReceiptDetail::Refusal {
            reason: RemoteRejectionReason::ResourceExhausted
        })
    );
    assert_eq!(
        get_claim(&conn, &run.local_claim_id)
            .unwrap()
            .unwrap()
            .state,
        ClaimState::Active
    );

    record_remote_task_terminal(
        &conn,
        &gateway,
        &task.envelope.envelope_id,
        RemoteTerminalOutcome::Failed,
        None,
        Some("execution_timeout"),
        None,
    )
    .unwrap();
    assert_eq!(
        admit("supervised-next").envelope.current_state,
        "remote_admitted"
    );
}
