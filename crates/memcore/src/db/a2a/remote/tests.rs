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
        task.receipts[1].detail.as_ref().unwrap().reason,
        RemoteRejectionReason::ExecutionUnavailable
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
