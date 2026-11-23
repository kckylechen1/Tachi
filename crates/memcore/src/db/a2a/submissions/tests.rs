use super::*;
use crate::db::session_claims::{
    insert_local_project_work_claim, record_unverified_admission, NewWorkClaim,
    UnverifiedAdmissionState, WorkClaimMode,
};
use crate::{AgentIdentity, MemoryStore};
use serde_json::json;

fn fixture(store: &MemoryStore) -> (A2aTransitionActor, RemoteSubmissionBinding, String) {
    let conn = store.connection();
    let now = jiff::Timestamp::now();
    crate::insert_agent_identity(
        conn,
        &AgentIdentity {
            agent_identity_id: "origin".into(),
            display_name: None,
            seat: None,
            capability_json: None,
            created_at: now.to_string(),
        },
    )
    .unwrap();
    record_unverified_admission(
        conn,
        "origin-admission",
        "origin",
        "origin-connection",
        UnverifiedAdmissionState::SelfAsserted,
    )
    .unwrap();
    insert_local_project_work_claim(
        conn,
        &NewWorkClaim {
            claim_id: "origin-claim".into(),
            agent_identity_id: "origin".into(),
            session_client: None,
            issue_ref: Some("owner/repo#1940".into()),
            flow_id: None,
            dispatch_id: None,
            branch: "feature".into(),
            worktree_path: "/fixture/worktree".into(),
            declared_file_scope: "src/**".into(),
            role: "executor".into(),
            mode: WorkClaimMode::ReadOnly,
            expected_head: "a".repeat(40),
            lease_expires_at: (now + jiff::SignedDuration::from_mins(20)).to_string(),
            created_at: now.to_string(),
        },
        "origin-connection",
        "owner/repo",
    )
    .unwrap();
    let binding = RemoteSubmissionBinding {
        issuer_id: "issuer-origin".into(),
        canonical_project: "owner/repo".into(),
        destination: "destination".into(),
        destination_host_id: "host-destination".into(),
        destination_key_sha256: "b".repeat(64),
        endpoint: "https://localhost:9443".into(),
        repository: "owner/repo".into(),
        job: "review_revision".into(),
        head_sha: "a".repeat(40),
        origin_claim_id: "origin-claim".into(),
        origin_claim_version: 0,
    };
    let body = json!({"kind":"tachi.remote_job/v1","sender":"origin","recipient":"destination",
        "trust_domain":"owner:domain","idempotency_key":"frozen-key","nonce":"frozen-nonce",
        "issued_at":now.to_string(),"expires_at":(now + jiff::SignedDuration::from_mins(10)).to_string(),
        "repository":"owner/repo","head_sha":"a".repeat(40),"job":"review_revision",
        "origin_claim":{"claim_id":"origin-claim","transition_version":0},"question":"private selected context"}).to_string();
    (
        A2aTransitionActor {
            agent_identity_id: "origin".into(),
            connection_id: "origin-connection".into(),
        },
        binding,
        body,
    )
}

fn uncertain() -> RemoteSubmissionResult {
    RemoteSubmissionResult::Unknown {
        error_class: "transport_unknown",
        retry_allowed: true,
    }
}

#[test]
fn frozen_submission_survives_reopen_and_rejects_key_or_nonce_rebinding() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("origin.db");
    let store = MemoryStore::open(path.to_str().unwrap()).unwrap();
    let (actor, binding, body) = fixture(&store);
    let prepared = prepare_remote_submission(store.connection(), &actor, &binding, &body).unwrap();
    drop(store);
    let store = MemoryStore::open(path.to_str().unwrap()).unwrap();
    let replay = prepare_remote_submission(store.connection(), &actor, &binding, &body).unwrap();
    assert_eq!(prepared.submission_id, replay.submission_id);
    assert_eq!(replay.body.as_deref(), Some(body.as_str()));
    assert!(!serde_json::to_string(&replay)
        .unwrap()
        .contains("private selected context"));
    let mut changed: Value = serde_json::from_str(&body).unwrap();
    changed["question"] = json!("changed private context");
    assert!(
        prepare_remote_submission(store.connection(), &actor, &binding, &changed.to_string())
            .is_err()
    );
    changed["idempotency_key"] = json!("another-key");
    assert!(
        prepare_remote_submission(store.connection(), &actor, &binding, &changed.to_string())
            .is_err()
    );
    let mut other_destination = binding.clone();
    other_destination.endpoint = "https://another.example".into();
    assert!(
        prepare_remote_submission(store.connection(), &actor, &other_destination, &body).is_err()
    );
    assert_eq!(
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM a2a_remote_submissions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        read_remote_submission(store.connection(), &actor, &prepared.submission_id)
            .unwrap()
            .state_version,
        1
    );
}

#[test]
fn durable_attempt_budget_fences_concurrent_and_late_senders() {
    let store = MemoryStore::open_in_memory().unwrap();
    let (actor, binding, body) = fixture(&store);
    let conn = store.connection();
    let prepared = prepare_remote_submission(conn, &actor, &binding, &body).unwrap();
    let first = reserve_remote_submission(
        conn,
        &actor,
        &binding,
        "owner:domain",
        &prepared.submission_id,
    )
    .unwrap()
    .unwrap();
    assert_eq!(first.submission.attempts, 1);
    assert!(reserve_remote_submission(
        conn,
        &actor,
        &binding,
        "owner:domain",
        &prepared.submission_id
    )
    .unwrap()
    .is_none());
    // A crashed process left its durable reservation; advancing only that
    // fixture deadline exercises the actual recovery path without a sleep.
    conn.execute(
        "UPDATE a2a_remote_submissions SET attempt_until=?1",
        [(jiff::Timestamp::now() - jiff::SignedDuration::from_secs(1)).to_string()],
    )
    .unwrap();
    let second = reserve_remote_submission(
        conn,
        &actor,
        &binding,
        "owner:domain",
        &prepared.submission_id,
    )
    .unwrap()
    .unwrap();
    assert_eq!(second.body, first.body);
    assert_eq!(second.submission.body_digest, first.submission.body_digest);
    assert!(!remote_submission_attempt_is_current(conn, &actor, &first).unwrap());
    assert!(finish_remote_submission(conn, &actor, &first, &uncertain()).is_err());
    assert_eq!(
        finish_remote_submission(conn, &actor, &second, &uncertain())
            .unwrap()
            .current_state,
        "unknown"
    );
    let third = reserve_remote_submission(
        conn,
        &actor,
        &binding,
        "owner:domain",
        &prepared.submission_id,
    )
    .unwrap()
    .unwrap();
    let exhausted = finish_remote_submission(conn, &actor, &third, &uncertain()).unwrap();
    assert_eq!(exhausted.attempts, 3);
    assert!(!exhausted.retry_allowed);
    assert_eq!(exhausted.current_state, "unknown");
    assert!(exhausted.remote_task_id.is_none());
    assert!(reserve_remote_submission(
        conn,
        &actor,
        &binding,
        "owner:domain",
        &prepared.submission_id
    )
    .unwrap()
    .is_none());
}

#[test]
fn claim_revision_is_authoritative_at_reservation_and_after_tls() {
    let store = MemoryStore::open_in_memory().unwrap();
    let (actor, binding, body) = fixture(&store);
    let conn = store.connection();
    let prepared = prepare_remote_submission(conn, &actor, &binding, &body).unwrap();
    let attempt = reserve_remote_submission(
        conn,
        &actor,
        &binding,
        "owner:domain",
        &prepared.submission_id,
    )
    .unwrap()
    .unwrap();
    assert!(remote_submission_attempt_is_current(conn, &actor, &attempt).unwrap());
    conn.execute("UPDATE session_claims SET transition_version=transition_version+1 WHERE claim_id='origin-claim'", []).unwrap();
    assert!(remote_submission_attempt_is_current(conn, &actor, &attempt).is_err());
    finish_remote_submission(conn, &actor, &attempt, &uncertain()).unwrap();
    assert!(reserve_remote_submission(
        conn,
        &actor,
        &binding,
        "owner:domain",
        &prepared.submission_id
    )
    .unwrap()
    .is_none());
    let stopped = read_remote_submission(conn, &actor, &prepared.submission_id).unwrap();
    assert_eq!(stopped.current_state, "unknown");
    assert_eq!(stopped.attempts, 1);
    assert_eq!(
        stopped.last_error.as_deref(),
        Some("origin_authority_changed")
    );
    assert!(stopped.remote_receipt.is_none());
}

#[test]
fn origin_holder_project_head_and_lease_cannot_be_asserted_by_the_request() {
    for change in [
        "UPDATE session_claims SET agent_identity_id=NULL",
        "UPDATE session_claims SET canonical_project='different-project'",
        "UPDATE session_claims SET expected_head='different-head'",
        "UPDATE session_claims SET lease_expires_at='2000-01-01T00:00:00Z'",
        "UPDATE session_claims SET state='released'",
    ] {
        let store = MemoryStore::open_in_memory().unwrap();
        let (actor, binding, body) = fixture(&store);
        store.connection().execute(change, []).unwrap();
        assert!(
            prepare_remote_submission(store.connection(), &actor, &binding, &body).is_err(),
            "{change}"
        );
        assert_eq!(
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM a2a_remote_submissions", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn expiry_is_local_block_or_unknown_and_body_gc_keeps_replay_tombstone() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    let (actor, binding, body) = fixture(&store);
    let prepared = prepare_remote_submission(store.connection(), &actor, &binding, &body).unwrap();
    store
        .connection()
        .execute(
            "UPDATE a2a_remote_submissions SET expires_at='2000-01-01T00:00:00Z'",
            [],
        )
        .unwrap();
    assert!(reserve_remote_submission(
        store.connection(),
        &actor,
        &binding,
        "owner:domain",
        &prepared.submission_id
    )
    .unwrap()
    .is_none());
    let blocked =
        read_remote_submission(store.connection(), &actor, &prepared.submission_id).unwrap();
    assert_eq!(blocked.current_state, "blocked");
    assert_eq!(blocked.attempts, 0);
    crate::db::gc_tables(
        store.connection_mut(),
        &crate::types::GcConfig::default(),
        crate::db::StoreProfile::TachiFull,
    )
    .unwrap();
    let scrubbed =
        read_remote_submission(store.connection(), &actor, &prepared.submission_id).unwrap();
    assert!(scrubbed.body.is_none());
    assert_eq!(scrubbed.body_digest, prepared.body_digest);
    assert_eq!(scrubbed.idempotency_key, prepared.idempotency_key);
    assert_eq!(scrubbed.nonce, prepared.nonce);
    assert_eq!(scrubbed.binding, prepared.binding);
}
