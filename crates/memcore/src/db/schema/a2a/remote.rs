//! Product v45: rejected remote jobs share the retained mailbox and receipts.
//!
//! Local rows keep their v42 authority, size, project and transition rules.
//! Remote senders are verified peers; the destination and every receipt actor
//! remain a real local gateway admission. No execution state is introduced.

use super::*;

pub(super) const BODY_CHECK: &str = "CHECK (body IS NULL OR (kind IN ('turn_response/v1','coordination/v1') AND length(CAST(body AS BLOB)) BETWEEN 1 AND 4096) OR (kind = 'tachi.remote_job/v1' AND length(CAST(body AS BLOB)) BETWEEN 1 AND 32768))";

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
    current_state TEXT NOT NULL CHECK ((kind IN ('turn_response/v1','coordination/v1') AND current_state IN ('received','accepted','consumed','expired')) OR (kind = 'tachi.remote_job/v1' AND current_state = 'remote_rejected' AND state_version = 2)),
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
    state TEXT NOT NULL CHECK (state IN ('received','accepted','consumed','expired','remote_received','remote_rejected')),
    actor_agent_identity_id TEXT NOT NULL,
    actor_admission_id TEXT NOT NULL,
    identity_assurance TEXT NOT NULL CHECK (identity_assurance = 'self_asserted'),
    trust_domain TEXT NOT NULL CHECK (trust_domain = 'same_host'),
    trust_basis TEXT NOT NULL CHECK (trust_basis IN ('current_local_connection','historical_local_admission')),
    occurred_at TEXT NOT NULL,
    detail TEXT,
    CHECK ((state IN ('received','accepted','consumed','expired') AND detail IS NULL) OR (state = 'remote_received' AND envelope_version = 1 AND detail IS NULL) OR (state = 'remote_rejected' AND envelope_version = 2 AND detail IS NOT NULL AND detail = '{"reason":"execution_unavailable"}')),
    UNIQUE (envelope_id, envelope_version),
    UNIQUE (envelope_id, state),
    FOREIGN KEY (envelope_id) REFERENCES a2a_envelopes(envelope_id),
    FOREIGN KEY (actor_agent_identity_id) REFERENCES agent_identities(agent_identity_id),
    FOREIGN KEY (actor_admission_id) REFERENCES identity_admissions(admission_id)
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
];

pub(crate) fn migrate_v45_remote_mailbox(conn: &Connection) -> Result<(), MemoryError> {
    validate_a2a_mailbox_schema_version(conn, 42)?;
    execute_batch_retry(
        conn,
        "DROP INDEX idx_a2a_envelopes_recipient_state;
         DROP INDEX idx_a2a_envelopes_issuer_created;
         DROP INDEX idx_a2a_receipts_envelope_version;
         ALTER TABLE a2a_delivery_receipts RENAME TO a2a_delivery_receipts_v44;
         ALTER TABLE a2a_envelopes RENAME TO a2a_envelopes_v44;",
    )?;
    for (_, _, sql) in OBJECTS {
        execute_batch_retry(conn, sql)?;
    }
    execute_batch_retry(
        conn,
        "INSERT INTO a2a_envelopes
         (envelope_id,kind,issuer_agent_identity_id,issuer_admission_id,recipient_agent_identity_id,
          recipient_admission_id,subject_ref,body,body_digest,issuer_identity_assurance,
          recipient_identity_assurance,issuer_trust_domain,recipient_trust_domain,
          issuer_trust_basis,recipient_trust_basis,idempotency_key,created_at,expires_at,
          current_state,state_version,canonical_project)
         SELECT * FROM a2a_envelopes_v44;
         INSERT INTO a2a_delivery_receipts
         (receipt_id,envelope_id,envelope_version,state,actor_agent_identity_id,actor_admission_id,
          identity_assurance,trust_domain,trust_basis,occurred_at)
         SELECT * FROM a2a_delivery_receipts_v44;
         DROP TABLE a2a_delivery_receipts_v44;
         DROP TABLE a2a_envelopes_v44;",
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
                "incomplete current schema v45: incomplete v31 A2A mailbox: envelope body constraint drifted".into(),
            ));
        }
        if actual.is_none_or(|actual| normalize_schema_sql(&actual) != normalize_schema_sql(sql)) {
            return Err(MemoryError::InvalidArg(format!(
                "incomplete current schema v45: incomplete v31 A2A mailbox: {kind} '{name}' is missing or non-canonical"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
