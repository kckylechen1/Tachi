//! Product v47: destination-local execution runs, lifecycle states, and
//! widened submission state tracking (PortableKernel remains v44).
//!
//! Preserves all local v42/v45/v46 mailbox and refusal semantics while adding
//! explicit `a2a_remote_runs` correlation and execution lifecycle receipts.

use super::*;

pub(super) const BODY_CHECK: &str = remote::BODY_CHECK;

const ENVELOPES: &str = r#"
CREATE TABLE a2a_envelopes (
    envelope_id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('turn_response/v1','coordination/v1','tachi.remote_job/v1')),
    issuer_agent_identity_id TEXT NOT NULL,
    issuer_admission_id TEXT NOT NULL,
    recipient_agent_identity_id TEXT NOT NULL,
    recipient_admission_id TEXT NOT NULL,
    subject_ref TEXT NOT NULL,
    body TEXT,
    body_digest TEXT NOT NULL CHECK (length(body_digest) = 64 AND body_digest = lower(body_digest) AND body_digest NOT GLOB '*[^0-9a-f]*'),
    issuer_identity_assurance TEXT NOT NULL CHECK ((kind IN ('turn_response/v1','coordination/v1') AND issuer_identity_assurance = 'self_asserted') OR (kind = 'tachi.remote_job/v1' AND issuer_identity_assurance = 'verified')),
    recipient_identity_assurance TEXT NOT NULL CHECK (recipient_identity_assurance = 'self_asserted'),
    issuer_trust_domain TEXT NOT NULL CHECK ((kind IN ('turn_response/v1','coordination/v1') AND issuer_trust_domain = 'same_host') OR (kind = 'tachi.remote_job/v1' AND length(trim(issuer_trust_domain)) > 0)),
    recipient_trust_domain TEXT NOT NULL CHECK (recipient_trust_domain = 'same_host'),
    issuer_trust_basis TEXT NOT NULL CHECK ((kind IN ('turn_response/v1','coordination/v1') AND issuer_trust_basis = 'current_local_connection') OR (kind = 'tachi.remote_job/v1' AND issuer_trust_basis = 'current_verified_mtls_peer')),
    recipient_trust_basis TEXT NOT NULL CHECK ((kind IN ('turn_response/v1','coordination/v1') AND recipient_trust_basis = 'historical_local_admission') OR (kind = 'tachi.remote_job/v1' AND recipient_trust_basis = 'current_local_connection')),
    idempotency_key TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    current_state TEXT NOT NULL CHECK ((kind IN ('turn_response/v1','coordination/v1') AND current_state IN ('received','accepted','consumed','expired')) OR (kind = 'tachi.remote_job/v1' AND current_state IN ('remote_received','remote_admitted','remote_rejected','remote_started','remote_cancel_requested','remote_completed','remote_failed','remote_canceled') AND state_version >= 1)),
    state_version INTEGER NOT NULL CHECK (state_version > 0),
    canonical_project TEXT NOT NULL DEFAULT '',
    remote_nonce TEXT,
    remote_metadata TEXT,
    UNIQUE (issuer_agent_identity_id, canonical_project, idempotency_key),
    CHECK (length(trim(subject_ref)) > 0),
    CHECK (body IS NULL OR (kind IN ('turn_response/v1','coordination/v1') AND length(CAST(body AS BLOB)) BETWEEN 1 AND 4096) OR (kind = 'tachi.remote_job/v1' AND length(CAST(body AS BLOB)) BETWEEN 1 AND 32768)),
    CHECK (expires_at > created_at),
    CHECK ((kind = 'turn_response/v1' AND canonical_project = '') OR (kind = 'coordination/v1' AND length(canonical_project) = 64) OR (kind = 'tachi.remote_job/v1' AND substr(canonical_project,1,7) = 'remote:' AND length(canonical_project) > 7)),
    CHECK ((kind IN ('turn_response/v1','coordination/v1') AND remote_nonce IS NULL AND remote_metadata IS NULL) OR (kind = 'tachi.remote_job/v1' AND remote_nonce IS NOT NULL AND length(trim(remote_nonce)) > 0 AND remote_metadata IS NOT NULL AND json_valid(remote_metadata) AND json_type(remote_metadata) = 'object' AND json_type(remote_metadata,'$.repository') IS 'text' AND canonical_project = 'remote:' || json_extract(remote_metadata,'$.repository'))),
    FOREIGN KEY (issuer_agent_identity_id) REFERENCES agent_identities(agent_identity_id),
    FOREIGN KEY (issuer_admission_id) REFERENCES identity_admissions(admission_id),
    FOREIGN KEY (recipient_agent_identity_id) REFERENCES agent_identities(agent_identity_id),
    FOREIGN KEY (recipient_admission_id) REFERENCES identity_admissions(admission_id)
)
"#;

const RECEIPTS: &str = r#"
CREATE TABLE a2a_delivery_receipts (
    receipt_id TEXT PRIMARY KEY NOT NULL,
    envelope_id TEXT NOT NULL,
    envelope_version INTEGER NOT NULL CHECK (envelope_version > 0),
    state TEXT NOT NULL CHECK (state IN ('received','accepted','consumed','expired','remote_received','remote_admitted','remote_rejected','remote_started','remote_cancel_requested','remote_completed','remote_failed','remote_canceled','remote_cleanup')),
    actor_agent_identity_id TEXT NOT NULL,
    actor_admission_id TEXT NOT NULL,
    identity_assurance TEXT NOT NULL CHECK (identity_assurance = 'self_asserted'),
    trust_domain TEXT NOT NULL CHECK (trust_domain = 'same_host'),
    trust_basis TEXT NOT NULL CHECK (trust_basis IN ('current_local_connection','historical_local_admission')),
    occurred_at TEXT NOT NULL,
    detail TEXT,
    CHECK (
        (state IN ('received','accepted','consumed','expired') AND detail IS NULL)
        OR (state = 'remote_received' AND envelope_version = 1 AND detail IS NULL)
        OR (state = 'remote_rejected' AND envelope_version = 2 AND detail IS NOT NULL AND json_valid(detail) AND json_type(detail) = 'object' AND json_type(detail,'$.reason') = 'text')
        OR (state = 'remote_admitted' AND envelope_version = 2 AND detail IS NOT NULL AND json_valid(detail) AND json_type(detail) = 'object')
        OR (state IN ('remote_started','remote_cancel_requested','remote_completed','remote_failed','remote_canceled','remote_cleanup') AND envelope_version >= 3 AND detail IS NOT NULL AND json_valid(detail) AND json_type(detail) = 'object')
    ),
    UNIQUE (envelope_id, envelope_version),
    UNIQUE (envelope_id, state),
    FOREIGN KEY (envelope_id) REFERENCES a2a_envelopes(envelope_id),
    FOREIGN KEY (actor_agent_identity_id) REFERENCES agent_identities(agent_identity_id),
    FOREIGN KEY (actor_admission_id) REFERENCES identity_admissions(admission_id)
)
"#;

const REMOTE_RUNS: &str = r#"
CREATE TABLE a2a_remote_runs (
    envelope_id TEXT PRIMARY KEY NOT NULL,
    local_run_id TEXT NOT NULL UNIQUE,
    canonical_project TEXT NOT NULL CHECK (length(trim(canonical_project)) > 0),
    local_claim_id TEXT NOT NULL UNIQUE CHECK (length(trim(local_claim_id)) > 0),
    local_claim_version INTEGER NOT NULL CHECK (local_claim_version >= 0),
    repository TEXT NOT NULL CHECK (length(trim(repository)) > 0),
    job TEXT NOT NULL CHECK (job IN ('review_revision','verify_pr')),
    head_sha TEXT NOT NULL CHECK (length(head_sha) IN (40,64) AND head_sha = lower(head_sha) AND head_sha NOT GLOB '*[^0-9a-f]*'),
    pull_request INTEGER CHECK ((job = 'verify_pr' AND pull_request > 0) OR (job = 'review_revision' AND pull_request IS NULL)),
    adapter_kind TEXT NOT NULL CHECK (adapter_kind IN ('native_review','dgx2_verify_pr')),
    policy_profile TEXT NOT NULL CHECK (length(trim(policy_profile)) > 0),
    resource_slot TEXT NOT NULL CHECK (length(trim(resource_slot)) > 0),
    lifecycle_state TEXT NOT NULL CHECK (lifecycle_state IN ('admitted','started','cancel_requested','completed','failed','canceled')),
    cleanup_state TEXT NOT NULL DEFAULT 'pending' CHECK (cleanup_state IN ('pending','cleaned','failed')),
    external_ref TEXT,
    admitted_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (envelope_id) REFERENCES a2a_envelopes(envelope_id)
)
"#;

const SUBMISSIONS: &str = r#"
CREATE TABLE a2a_remote_submissions (
    submission_id TEXT PRIMARY KEY NOT NULL,
    origin_agent_identity_id TEXT NOT NULL,
    origin_admission_id TEXT NOT NULL,
    binding TEXT NOT NULL CHECK (json_valid(binding) AND json_type(binding) = 'object'),
    idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 128),
    nonce TEXT NOT NULL CHECK (length(nonce) BETWEEN 1 AND 128 AND nonce != idempotency_key),
    trust_domain TEXT NOT NULL,
    body TEXT CHECK (body IS NULL OR length(CAST(body AS BLOB)) BETWEEN 1 AND 32768),
    body_digest TEXT NOT NULL CHECK (length(body_digest) = 64 AND body_digest NOT GLOB '*[^0-9a-f]*'),
    issued_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    current_state TEXT NOT NULL CHECK (current_state IN ('pending','in_flight','unknown','submitted','working','completed','failed','canceled','refused','blocked')),
    state_version INTEGER NOT NULL CHECK (state_version > 0),
    attempts INTEGER NOT NULL CHECK (attempts BETWEEN 0 AND 3),
    attempt_token TEXT,
    attempt_until TEXT,
    last_error TEXT,
    retry_allowed INTEGER NOT NULL CHECK (retry_allowed IN (0,1)),
    remote_task_id TEXT,
    remote_receipt TEXT CHECK (remote_receipt IS NULL OR (json_valid(remote_receipt) AND length(CAST(remote_receipt AS BLOB)) <= 16384)),
    updated_at TEXT NOT NULL,
    UNIQUE (origin_agent_identity_id, idempotency_key),
    UNIQUE (origin_agent_identity_id, trust_domain, nonce),
    CHECK ((current_state = 'in_flight' AND attempt_token IS NOT NULL AND attempt_until IS NOT NULL AND attempts > 0) OR (current_state != 'in_flight' AND attempt_token IS NULL AND attempt_until IS NULL)),
    CHECK ((current_state IN ('submitted','working','completed','failed','canceled','refused') AND remote_task_id IS NOT NULL AND remote_receipt IS NOT NULL) OR (current_state IN ('pending','blocked') AND remote_task_id IS NULL AND remote_receipt IS NULL) OR (current_state IN ('in_flight','unknown') AND ((remote_task_id IS NULL AND remote_receipt IS NULL) OR (remote_task_id IS NOT NULL AND remote_receipt IS NOT NULL)))),
    CHECK (current_state NOT IN ('pending','blocked') OR attempts = 0),
    CHECK (current_state NOT IN ('unknown','submitted','working','completed','failed','canceled','refused') OR attempts > 0),
    FOREIGN KEY (origin_agent_identity_id) REFERENCES agent_identities(agent_identity_id),
    FOREIGN KEY (origin_admission_id) REFERENCES identity_admissions(admission_id)
)
"#;

const OBJECTS: &[(&str, &str, &str)] = &[
    ("table", "a2a_envelopes", ENVELOPES),
    ("index", "idx_a2a_envelopes_recipient_state", "CREATE INDEX idx_a2a_envelopes_recipient_state ON a2a_envelopes(recipient_agent_identity_id, canonical_project, current_state, expires_at, created_at, envelope_id)"),
    ("index", "idx_a2a_envelopes_issuer_created", "CREATE INDEX idx_a2a_envelopes_issuer_created ON a2a_envelopes(issuer_agent_identity_id, created_at DESC, envelope_id DESC)"),
    ("index", "idx_a2a_remote_sender_key", "CREATE UNIQUE INDEX idx_a2a_remote_sender_key ON a2a_envelopes(issuer_agent_identity_id, idempotency_key) WHERE kind = 'tachi.remote_job/v1'"),
    ("index", "idx_a2a_remote_sender_nonce", "CREATE UNIQUE INDEX idx_a2a_remote_sender_nonce ON a2a_envelopes(issuer_agent_identity_id, issuer_trust_domain, remote_nonce) WHERE kind = 'tachi.remote_job/v1'"),
    ("table", "a2a_delivery_receipts", RECEIPTS),
    ("index", "idx_a2a_receipts_envelope_version", "CREATE INDEX idx_a2a_receipts_envelope_version ON a2a_delivery_receipts(envelope_id, envelope_version)"),
    ("table", "a2a_remote_runs", REMOTE_RUNS),
    ("index", "idx_a2a_remote_runs_slot_state", "CREATE INDEX idx_a2a_remote_runs_slot_state ON a2a_remote_runs(resource_slot, lifecycle_state)"),
    ("table", "a2a_remote_submissions", SUBMISSIONS),
];

pub(crate) fn migrate_v47_remote_execution_runs(conn: &Connection) -> Result<(), MemoryError> {
    remote::validate(conn)?;
    submissions::validate(conn)?;
    execute_batch_retry(
        conn,
        "DROP INDEX idx_a2a_envelopes_recipient_state;
         DROP INDEX idx_a2a_envelopes_issuer_created;
         DROP INDEX idx_a2a_remote_sender_key;
         DROP INDEX idx_a2a_remote_sender_nonce;
         DROP INDEX idx_a2a_receipts_envelope_version;
         ALTER TABLE a2a_delivery_receipts RENAME TO a2a_delivery_receipts_v46;
         ALTER TABLE a2a_envelopes RENAME TO a2a_envelopes_v46;
         ALTER TABLE a2a_remote_submissions RENAME TO a2a_remote_submissions_v46;",
    )?;
    for (_, _, sql) in OBJECTS {
        execute_batch_retry(conn, sql)?;
    }
    execute_batch_retry(
        conn,
        "INSERT INTO a2a_envelopes
         SELECT * FROM a2a_envelopes_v46;
         INSERT INTO a2a_delivery_receipts
         SELECT * FROM a2a_delivery_receipts_v46;
         INSERT INTO a2a_remote_submissions
         SELECT * FROM a2a_remote_submissions_v46;
         DROP TABLE a2a_remote_submissions_v46;
         DROP TABLE a2a_delivery_receipts_v46;
         DROP TABLE a2a_envelopes_v46;",
    )?;
    validate(conn)
}

pub(super) fn validate(conn: &Connection) -> Result<(), MemoryError> {
    for (kind, name, sql) in OBJECTS {
        let actual: Option<String> = conn
            .query_row(
                "SELECT sql FROM main.sqlite_schema WHERE type=?1 AND name=?2",
                params![kind, name],
                |row| row.get(0),
            )
            .optional()?;
        if *name == "a2a_envelopes"
            && actual.as_ref().is_some_and(|actual| {
                !normalize_schema_sql(actual).contains(&normalize_schema_sql(BODY_CHECK))
            })
        {
            return Err(MemoryError::InvalidArg(
                "incomplete current schema v47: incomplete v31 A2A mailbox: envelope body constraint drifted".into(),
            ));
        }
        if actual.is_none_or(|actual| normalize_schema_sql(&actual) != normalize_schema_sql(sql)) {
            return Err(MemoryError::InvalidArg(format!(
                "incomplete current schema v47: incomplete v31 A2A mailbox: {kind} '{name}' is missing or non-canonical"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::a2a::{
        insert_a2a_envelope, insert_remote_rejection, A2aTransitionActor, NewA2aEnvelope,
        NewRemoteTask, RemoteJobGrant, RemoteTaskAccess, RemoteTaskHeader, RemoteTaskInsertOutcome,
    };
    use crate::db::migrations::catalogue::test_support::with_prefix;
    use crate::db::schema::inventory::{query_rows, schema_inventory};
    use crate::db::session_claims::{record_unverified_admission, UnverifiedAdmissionState};
    use crate::db::{DbOpenContext, StoreProfile, VerifiedAdmissionBinding};
    use crate::MemoryStore;
    use serde_json::json;
    use sha2::{Digest, Sha256};

    #[test]
    fn v46_upgrade_preserves_local_refusals_and_submissions_and_keeps_portable_44() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("destination.db");
        let now = jiff::Timestamp::now();
        with_prefix(46, || {
            let store = MemoryStore::open_with_context(
                path.to_str().unwrap(),
                &DbOpenContext::create_fresh(),
            )
            .unwrap();
            let conn = store.connection();
            for identity in ["local", "gateway", "origin"] {
                conn.execute(
                    "INSERT INTO agent_identities(agent_identity_id,created_at) VALUES(?1,?2)",
                    params![identity, now.to_string()],
                )
                .unwrap();
            }
            record_unverified_admission(
                conn,
                "local-adm",
                "local",
                "local-conn",
                UnverifiedAdmissionState::SelfAsserted,
            )
            .unwrap();
            record_unverified_admission(
                conn,
                "gateway-adm",
                "gateway",
                "gateway-conn",
                UnverifiedAdmissionState::SelfAsserted,
            )
            .unwrap();
            conn.execute(
                "INSERT INTO identity_admissions(admission_id,agent_identity_id,connection_id,state,created_at)
                 VALUES('origin-adm','origin','origin-conn','verified',?1)",
                [now.to_string()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO identity_admission_verification_receipts
                 (receipt_id,admission_id,agent_identity_id,connection_id,issuer_id,verification_method,
                  verification_version,trust_domain,verification_scope,evidence_digest,evidence_ref,
                  evidence_issued_at,evidence_expires_at,nonce,idempotency_key,request_digest,current_state,verified_at)
                 VALUES('origin-rcpt','origin-adm','origin','origin-conn','gw-issuer','mtls-peer','v1',
                        'workspace:test','agent_identity:remote_admission',?1,?2,
                        strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 hour'),
                        strftime('%Y-%m-%dT%H:%M:%fZ','now','+1 day'),
                        'n-1','k-1',?1,'verified',?3)",
                params![
                    "a".repeat(64),
                    format!("attestation:mtls-peer:{:x}", Sha256::digest(b"origin")),
                    now.to_string()
                ],
            )
            .unwrap();
            let mut local_conn = Connection::open(&path).unwrap();
            insert_a2a_envelope(
                &mut local_conn,
                &NewA2aEnvelope {
                    envelope_id: "local-v46".into(),
                    issuer_agent_identity_id: "local".into(),
                    issuer_connection_id: "local-conn".into(),
                    recipient_agent_identity_id: "local".into(),
                    subject_ref: "peer_publication:v46".into(),
                    body: "local mailbox stays intact".into(),
                    idempotency_key: "local-k".into(),
                    created_at: now.to_string(),
                    expires_at: (now + jiff::SignedDuration::from_mins(20)).to_string(),
                },
            )
            .unwrap();
            let binding = VerifiedAdmissionBinding {
                admission_id: "origin-adm".into(),
                agent_identity_id: "origin".into(),
                connection_id: "origin-conn".into(),
                issuer_id: "gw-issuer".into(),
                verification_method: "mtls-peer".into(),
                verification_version: "v1".into(),
                trust_domain: "workspace:test".into(),
                verification_scope: "agent_identity:remote_admission".into(),
            };
            let gateway = A2aTransitionActor {
                agent_identity_id: "gateway".into(),
                connection_id: "gateway-conn".into(),
            };
            let access = RemoteTaskAccess {
                destination_agent_identity_id: "gateway".into(),
                destination_host_id: "dest-host".into(),
                grants: vec![RemoteJobGrant {
                    repository: "owner/repo".into(),
                    job: "review_revision".into(),
                }],
            };
            let body = serde_json::to_string(&json!({
                "expires_at": crate::time_compat::millis_z(now + jiff::SignedDuration::from_mins(10)),
                "head_sha": "a".repeat(40),
                "idempotency_key": "hist-k",
                "issued_at": crate::time_compat::millis_z(now - jiff::SignedDuration::from_secs(30)),
                "job": "review_revision",
                "kind": "tachi.remote_job/v1",
                "nonce": "hist-nonce",
                "origin_claim": {"claim_id": "oc-1", "transition_version": 1},
                "recipient": "gateway",
                "repository": "owner/repo",
                "sender": "origin",
                "trust_domain": "workspace:test"
            }))
            .unwrap();
            insert_remote_rejection(
                conn,
                &binding,
                &gateway,
                &access,
                &NewRemoteTask {
                    envelope_id: "remote-v46".into(),
                    canonical_body: body,
                    nonce: "hist-nonce".into(),
                    header: RemoteTaskHeader {
                        repository: "owner/repo".into(),
                        job: "review_revision".into(),
                        head_sha: "a".repeat(40),
                        pull_request: None,
                        origin_claim_id: "oc-1".into(),
                        origin_claim_version: 1,
                        destination_host_id: "dest-host".into(),
                    },
                    idempotency_key: "hist-k".into(),
                    issued_at: crate::time_compat::millis_z(
                        now - jiff::SignedDuration::from_secs(30),
                    ),
                    expires_at: crate::time_compat::millis_z(
                        now + jiff::SignedDuration::from_mins(10),
                    ),
                    recipient_agent_identity_id: "gateway".into(),
                },
            )
            .unwrap();
            // Also seed a legacy v46 verify_pr refusal whose remote_metadata omits pull_request.
            let verify_issued =
                crate::time_compat::millis_z(now - jiff::SignedDuration::from_secs(30));
            let verify_expires =
                crate::time_compat::millis_z(now + jiff::SignedDuration::from_mins(10));
            let verify_body = serde_json::to_string(&json!({
                "expires_at": verify_expires,
                "head_sha": "a".repeat(40),
                "idempotency_key": "hist-verify-k",
                "issued_at": verify_issued,
                "job": "verify_pr",
                "kind": "tachi.remote_job/v1",
                "nonce": "hist-verify-nonce",
                "origin_claim": {"claim_id": "oc-1", "transition_version": 1},
                "pull_request": 42,
                "recipient": "gateway",
                "repository": "owner/repo",
                "sender": "origin",
                "trust_domain": "workspace:test"
            }))
            .unwrap();
            let verify_digest = format!("{:x}", Sha256::digest(verify_body.as_bytes()));
            let legacy_verify_meta = serde_json::to_string(&json!({
                "repository": "owner/repo",
                "job": "verify_pr",
                "head_sha": "a".repeat(40),
                "origin_claim_id": "oc-1",
                "origin_claim_version": 1,
                "destination_host_id": "dest-host"
            }))
            .unwrap();
            conn.execute(
                "INSERT INTO a2a_envelopes
                 (envelope_id,kind,issuer_agent_identity_id,issuer_admission_id,recipient_agent_identity_id,
                  recipient_admission_id,subject_ref,body,body_digest,issuer_identity_assurance,
                  recipient_identity_assurance,issuer_trust_domain,recipient_trust_domain,
                  issuer_trust_basis,recipient_trust_basis,idempotency_key,created_at,expires_at,
                  current_state,state_version,canonical_project,remote_nonce,remote_metadata)
                 VALUES ('remote-v46-verify','tachi.remote_job/v1','origin','origin-adm','gateway',
                         'gateway-adm','work_claim:oc-1',?1,?2,'verified','self_asserted','workspace:test',
                         'same_host','current_verified_mtls_peer','current_local_connection',
                         'hist-verify-k',?3,?4,'remote_rejected',2,'remote:owner/repo','hist-verify-nonce',?5)",
                params![verify_body, verify_digest, verify_issued, verify_expires, legacy_verify_meta],
            )
            .unwrap();
            let verify_ts = crate::time_compat::millis_z(now);
            conn.execute(
                "INSERT INTO a2a_delivery_receipts
                 (receipt_id,envelope_id,envelope_version,state,actor_agent_identity_id,
                  actor_admission_id,identity_assurance,trust_domain,trust_basis,occurred_at,detail)
                 VALUES ('rv46v:1','remote-v46-verify',1,'remote_received','gateway','gateway-adm','self_asserted','same_host','current_local_connection',?1,NULL),
                        ('rv46v:2','remote-v46-verify',2,'remote_rejected','gateway','gateway-adm','self_asserted','same_host','current_local_connection',?1,'{\"reason\":\"execution_unavailable\"}')",
                params![verify_ts],
            )
            .unwrap();
        });

        let conn = Connection::open(&path).unwrap();
        let before_envelopes =
            query_rows(&conn, "SELECT * FROM a2a_envelopes ORDER BY envelope_id");
        let before_receipts = query_rows(
            &conn,
            "SELECT * FROM a2a_delivery_receipts ORDER BY receipt_id",
        );
        drop(conn);

        assert!(MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &DbOpenContext::open_existing_deny()
        )
        .is_err());

        let store = MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &DbOpenContext::open_existing_allow("v47 migration fixture"),
        )
        .unwrap();
        assert_eq!(
            query_rows(
                store.connection(),
                "SELECT * FROM a2a_envelopes ORDER BY envelope_id"
            ),
            before_envelopes
        );
        assert_eq!(
            query_rows(
                store.connection(),
                "SELECT * FROM a2a_delivery_receipts ORDER BY receipt_id"
            ),
            before_receipts
        );
        assert!(query_rows(store.connection(), "PRAGMA foreign_key_check").is_empty());
        validate(store.connection()).unwrap();

        let binding = VerifiedAdmissionBinding {
            admission_id: "origin-adm".into(),
            agent_identity_id: "origin".into(),
            connection_id: "origin-conn".into(),
            issuer_id: "gw-issuer".into(),
            verification_method: "mtls-peer".into(),
            verification_version: "v1".into(),
            trust_domain: "workspace:test".into(),
            verification_scope: "agent_identity:remote_admission".into(),
        };
        let gateway = A2aTransitionActor {
            agent_identity_id: "gateway".into(),
            connection_id: "gateway-conn".into(),
        };
        let access = RemoteTaskAccess {
            destination_agent_identity_id: "gateway".into(),
            destination_host_id: "dest-host".into(),
            grants: vec![
                RemoteJobGrant {
                    repository: "owner/repo".into(),
                    job: "review_revision".into(),
                },
                RemoteJobGrant {
                    repository: "owner/repo".into(),
                    job: "verify_pr".into(),
                },
            ],
        };
        let body = serde_json::to_string(&json!({
            "expires_at": crate::time_compat::millis_z(now + jiff::SignedDuration::from_mins(10)),
            "head_sha": "a".repeat(40),
            "idempotency_key": "hist-k",
            "issued_at": crate::time_compat::millis_z(now - jiff::SignedDuration::from_secs(30)),
            "job": "review_revision",
            "kind": "tachi.remote_job/v1",
            "nonce": "hist-nonce",
            "origin_claim": {"claim_id": "oc-1", "transition_version": 1},
            "recipient": "gateway",
            "repository": "owner/repo",
            "sender": "origin",
            "trust_domain": "workspace:test"
        }))
        .unwrap();
        let replay = insert_remote_rejection(
            store.connection(),
            &binding,
            &gateway,
            &access,
            &NewRemoteTask {
                envelope_id: "remote-v46-new-id".into(),
                canonical_body: body,
                nonce: "hist-nonce".into(),
                header: RemoteTaskHeader {
                    repository: "owner/repo".into(),
                    job: "review_revision".into(),
                    head_sha: "a".repeat(40),
                    pull_request: None,
                    origin_claim_id: "oc-1".into(),
                    origin_claim_version: 1,
                    destination_host_id: "dest-host".into(),
                },
                idempotency_key: "hist-k".into(),
                issued_at: crate::time_compat::millis_z(now - jiff::SignedDuration::from_secs(30)),
                expires_at: crate::time_compat::millis_z(now + jiff::SignedDuration::from_mins(10)),
                recipient_agent_identity_id: "gateway".into(),
            },
        )
        .unwrap();
        assert!(matches!(replay, RemoteTaskInsertOutcome::Replay(_)));
        let verify_body = serde_json::to_string(&json!({
            "expires_at": crate::time_compat::millis_z(now + jiff::SignedDuration::from_mins(10)),
            "head_sha": "a".repeat(40),
            "idempotency_key": "hist-verify-k",
            "issued_at": crate::time_compat::millis_z(now - jiff::SignedDuration::from_secs(30)),
            "job": "verify_pr",
            "kind": "tachi.remote_job/v1",
            "nonce": "hist-verify-nonce",
            "origin_claim": {"claim_id": "oc-1", "transition_version": 1},
            "pull_request": 42,
            "recipient": "gateway",
            "repository": "owner/repo",
            "sender": "origin",
            "trust_domain": "workspace:test"
        }))
        .unwrap();
        let verify_replay = insert_remote_rejection(
            store.connection(),
            &binding,
            &gateway,
            &access,
            &NewRemoteTask {
                envelope_id: "remote-v46-verify-new-id".into(),
                canonical_body: verify_body,
                nonce: "hist-verify-nonce".into(),
                header: RemoteTaskHeader {
                    repository: "owner/repo".into(),
                    job: "verify_pr".into(),
                    head_sha: "a".repeat(40),
                    pull_request: Some(42),
                    origin_claim_id: "oc-1".into(),
                    origin_claim_version: 1,
                    destination_host_id: "dest-host".into(),
                },
                idempotency_key: "hist-verify-k".into(),
                issued_at: crate::time_compat::millis_z(now - jiff::SignedDuration::from_secs(30)),
                expires_at: crate::time_compat::millis_z(now + jiff::SignedDuration::from_mins(10)),
                recipient_agent_identity_id: "gateway".into(),
            },
        )
        .unwrap();
        assert!(
            matches!(verify_replay, RemoteTaskInsertOutcome::Replay(ref t) if t.envelope.envelope_id == "remote-v46-verify")
        );
        drop(store);

        let portable_dir = tempfile::tempdir().unwrap();
        let portable_path = portable_dir.path().join("portable.db");
        drop(
            MemoryStore::open_with_context(
                portable_path.to_str().unwrap(),
                &DbOpenContext::create_fresh().with_profile(StoreProfile::PortableKernel),
            )
            .unwrap(),
        );
        let portable = Connection::open(&portable_path).unwrap();
        assert_eq!(
            crate::db::migrations::read_schema_version(&portable).unwrap(),
            44
        );

        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE a2a_remote_runs;").unwrap();
        let broken = schema_inventory(&conn);
        drop(conn);
        assert!(MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &DbOpenContext::open_existing_deny()
        )
        .is_err());
        assert_eq!(schema_inventory(&Connection::open(&path).unwrap()), broken);
    }
}
