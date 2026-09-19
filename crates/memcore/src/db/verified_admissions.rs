//! Durable verified AgentIdentity admission receipts (#1938).
//!
//! This module owns persistence only. It never verifies remote evidence and it
//! never accepts a caller-selected [`super::session_claims::AdmissionState`].
//! The trusted server adapter must first validate issuer evidence, identity,
//! domain, freshness, revocation, and nonce semantics; this writer then binds
//! that decision into one immutable, secret-negative receipt.

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::error::MemoryError;

use super::common::{normalize_utc_iso, normalize_utc_iso_or_now};

const RECEIPT_COLUMNS: &str = "receipt_id, admission_id, agent_identity_id, connection_id, \
    issuer_id, verification_method, verification_version, trust_domain, verification_scope, \
    evidence_digest, evidence_ref, evidence_issued_at, evidence_expires_at, nonce, \
    idempotency_key, request_digest, current_state, verified_at";

const MAX_ID_BYTES: usize = 256;
const MAX_EVIDENCE_REF_BYTES: usize = 512;
const SHA256_HEX_BYTES: usize = 64;

pub(crate) const VERIFIED_ADMISSION_SCHEMA_SQL: &str = r#"
CREATE UNIQUE INDEX IF NOT EXISTS idx_identity_admissions_verified_binding
    ON identity_admissions(admission_id, agent_identity_id, connection_id);
CREATE TABLE IF NOT EXISTS identity_admission_verification_receipts (
    receipt_id TEXT PRIMARY KEY,
    admission_id TEXT NOT NULL UNIQUE REFERENCES identity_admissions(admission_id),
    agent_identity_id TEXT NOT NULL REFERENCES agent_identities(agent_identity_id),
    connection_id TEXT NOT NULL,
    issuer_id TEXT NOT NULL,
    verification_method TEXT NOT NULL,
    verification_version TEXT NOT NULL,
    trust_domain TEXT NOT NULL,
    verification_scope TEXT NOT NULL,
    evidence_digest TEXT NOT NULL CHECK (
        length(evidence_digest) = 64
        AND evidence_digest NOT GLOB '*[^0-9a-f]*'
    ),
    evidence_ref TEXT NOT NULL,
    evidence_issued_at TEXT NOT NULL,
    evidence_expires_at TEXT NOT NULL,
    nonce TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL CHECK (
        length(request_digest) = 64
        AND request_digest NOT GLOB '*[^0-9a-f]*'
    ),
    current_state TEXT NOT NULL CHECK (current_state = 'verified'),
    verified_at TEXT NOT NULL,
    UNIQUE (issuer_id, idempotency_key),
    UNIQUE (issuer_id, trust_domain, nonce),
    FOREIGN KEY (admission_id, agent_identity_id, connection_id)
        REFERENCES identity_admissions(admission_id, agent_identity_id, connection_id)
);
CREATE INDEX IF NOT EXISTS idx_identity_verification_receipts_identity
    ON identity_admission_verification_receipts(agent_identity_id, trust_domain, verification_scope);
CREATE TRIGGER IF NOT EXISTS identity_verification_receipts_no_update
BEFORE UPDATE ON identity_admission_verification_receipts
BEGIN
    SELECT RAISE(ABORT, 'verified admission receipts are append-only');
END;
CREATE TRIGGER IF NOT EXISTS identity_verification_receipts_no_delete
BEFORE DELETE ON identity_admission_verification_receipts
BEGIN
    SELECT RAISE(ABORT, 'verified admission receipts are append-only');
END;
"#;

/// Secret-negative fields accepted from the trusted server verifier boundary.
/// Raw attestation bytes, verifier keys, credentials, and host/model display
/// claims have no representation here and therefore cannot be persisted by
/// this writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewVerifiedAdmission {
    pub receipt_id: String,
    pub admission_id: String,
    pub agent_identity_id: String,
    pub connection_id: String,
    pub issuer_id: String,
    pub verification_method: String,
    pub verification_version: String,
    pub trust_domain: String,
    pub verification_scope: String,
    pub evidence_digest: String,
    pub evidence_ref: String,
    pub evidence_issued_at: String,
    pub evidence_expires_at: String,
    pub nonce: String,
    pub idempotency_key: String,
}

/// Immutable public/read-side receipt for a verified admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAdmissionReceipt {
    pub receipt_id: String,
    pub admission_id: String,
    pub agent_identity_id: String,
    pub connection_id: String,
    pub issuer_id: String,
    pub verification_method: String,
    pub verification_version: String,
    pub trust_domain: String,
    pub verification_scope: String,
    pub evidence_digest: String,
    pub evidence_ref: String,
    pub evidence_issued_at: String,
    pub evidence_expires_at: String,
    pub nonce: String,
    pub idempotency_key: String,
    pub request_digest: String,
    pub current_state: String,
    pub verified_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifiedAdmissionWriteOutcome {
    Created(VerifiedAdmissionReceipt),
    Replayed(VerifiedAdmissionReceipt),
}

impl VerifiedAdmissionWriteOutcome {
    pub fn receipt(&self) -> &VerifiedAdmissionReceipt {
        match self {
            Self::Created(receipt) | Self::Replayed(receipt) => receipt,
        }
    }
}

fn refuse_invalid_field(name: &str, value: &str, max_bytes: usize) -> Result<(), MemoryError> {
    if value.trim().is_empty() || value.len() > max_bytes || value.as_bytes().contains(&0) {
        return Err(MemoryError::InvalidArg(format!(
            "verified admission {name} must be non-empty, at most {max_bytes} bytes, and contain no NUL"
        )));
    }
    Ok(())
}

fn refuse_non_canonical_sha256(name: &str, value: &str) -> Result<(), MemoryError> {
    if value.len() != SHA256_HEX_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(MemoryError::InvalidArg(format!(
            "verified admission {name} must be a lowercase SHA-256 digest"
        )));
    }
    Ok(())
}

fn canonical_request_digest(
    input: &NewVerifiedAdmission,
    issued_at: &str,
    expires_at: &str,
) -> String {
    crate::canonical_json_digest_hex(&serde_json::json!({
        "admission_id": input.admission_id,
        "agent_identity_id": input.agent_identity_id,
        "connection_id": input.connection_id,
        "current_state": "verified",
        "evidence_digest": input.evidence_digest,
        "evidence_expires_at": expires_at,
        "evidence_issued_at": issued_at,
        "evidence_ref": input.evidence_ref,
        "idempotency_key": input.idempotency_key,
        "issuer_id": input.issuer_id,
        "nonce": input.nonce,
        "receipt_id": input.receipt_id,
        "trust_domain": input.trust_domain,
        "verification_method": input.verification_method,
        "verification_scope": input.verification_scope,
        "verification_version": input.verification_version,
    }))
}

fn row_to_receipt(row: &rusqlite::Row<'_>) -> rusqlite::Result<VerifiedAdmissionReceipt> {
    Ok(VerifiedAdmissionReceipt {
        receipt_id: row.get(0)?,
        admission_id: row.get(1)?,
        agent_identity_id: row.get(2)?,
        connection_id: row.get(3)?,
        issuer_id: row.get(4)?,
        verification_method: row.get(5)?,
        verification_version: row.get(6)?,
        trust_domain: row.get(7)?,
        verification_scope: row.get(8)?,
        evidence_digest: row.get(9)?,
        evidence_ref: row.get(10)?,
        evidence_issued_at: row.get(11)?,
        evidence_expires_at: row.get(12)?,
        nonce: row.get(13)?,
        idempotency_key: row.get(14)?,
        request_digest: row.get(15)?,
        current_state: row.get(16)?,
        verified_at: row.get(17)?,
    })
}

fn find_by_idempotency(
    tx: &Transaction<'_>,
    issuer_id: &str,
    idempotency_key: &str,
) -> Result<Option<VerifiedAdmissionReceipt>, MemoryError> {
    Ok(tx
        .query_row(
            &format!(
                "SELECT {RECEIPT_COLUMNS} FROM identity_admission_verification_receipts \
                 WHERE issuer_id=?1 AND idempotency_key=?2"
            ),
            params![issuer_id, idempotency_key],
            row_to_receipt,
        )
        .optional()?)
}

/// Persist a verifier-approved admission and its receipt atomically.
///
/// This is a storage seam, not a verifier: callers must reach it only after a
/// trusted adapter has produced bounded evidence. Exact issuer/idempotency
/// replay returns the original bytes; any changed binding conflicts. Reusing a
/// nonce under the same issuer/domain but another request also conflicts at the
/// database boundary. No path updates an earlier unavailable, rejected, or
/// self-asserted admission.
pub fn record_verified_admission(
    conn: &mut Connection,
    input: &NewVerifiedAdmission,
) -> Result<VerifiedAdmissionWriteOutcome, MemoryError> {
    for (name, value, max_bytes) in [
        ("receipt id", input.receipt_id.as_str(), MAX_ID_BYTES),
        ("admission id", input.admission_id.as_str(), MAX_ID_BYTES),
        (
            "agent identity id",
            input.agent_identity_id.as_str(),
            MAX_ID_BYTES,
        ),
        ("connection id", input.connection_id.as_str(), MAX_ID_BYTES),
        ("issuer id", input.issuer_id.as_str(), MAX_ID_BYTES),
        (
            "verification method",
            input.verification_method.as_str(),
            MAX_ID_BYTES,
        ),
        (
            "verification version",
            input.verification_version.as_str(),
            MAX_ID_BYTES,
        ),
        ("trust domain", input.trust_domain.as_str(), MAX_ID_BYTES),
        (
            "verification scope",
            input.verification_scope.as_str(),
            MAX_ID_BYTES,
        ),
        (
            "evidence ref",
            input.evidence_ref.as_str(),
            MAX_EVIDENCE_REF_BYTES,
        ),
        ("nonce", input.nonce.as_str(), MAX_ID_BYTES),
        (
            "idempotency key",
            input.idempotency_key.as_str(),
            MAX_ID_BYTES,
        ),
    ] {
        refuse_invalid_field(name, value, max_bytes)?;
    }
    refuse_non_canonical_sha256("evidence digest", &input.evidence_digest)?;
    let issued_at = normalize_utc_iso(&input.evidence_issued_at)?;
    let expires_at = normalize_utc_iso(&input.evidence_expires_at)?;
    if expires_at <= issued_at {
        return Err(MemoryError::InvalidArg(
            "verified admission evidence expiry must be after issuance".to_string(),
        ));
    }
    let request_digest = canonical_request_digest(input, &issued_at, &expires_at);

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(existing) =
        find_by_idempotency(&tx, &input.issuer_id, &input.idempotency_key)?
    {
        if existing.request_digest != request_digest {
            return Err(MemoryError::Duplicate(format!(
                "verified admission idempotency conflict for issuer '{}' and key '{}'",
                input.issuer_id, input.idempotency_key
            )));
        }
        tx.commit()?;
        return Ok(VerifiedAdmissionWriteOutcome::Replayed(existing));
    }

    let nonce_owner: Option<String> = tx
        .query_row(
            "SELECT idempotency_key FROM identity_admission_verification_receipts \
             WHERE issuer_id=?1 AND trust_domain=?2 AND nonce=?3",
            params![input.issuer_id, input.trust_domain, input.nonce],
            |row| row.get(0),
        )
        .optional()?;
    if nonce_owner.is_some() {
        return Err(MemoryError::Duplicate(format!(
            "verified admission nonce replay for issuer '{}' and trust domain '{}'",
            input.issuer_id, input.trust_domain
        )));
    }

    let verified_at = normalize_utc_iso_or_now("");
    tx.execute(
        "INSERT INTO agent_identities (agent_identity_id, created_at) VALUES (?1, ?2) \
         ON CONFLICT(agent_identity_id) DO NOTHING",
        params![input.agent_identity_id, verified_at],
    )?;
    tx.execute(
        "INSERT INTO identity_admissions \
         (admission_id, agent_identity_id, connection_id, state, created_at) \
         VALUES (?1, ?2, ?3, 'verified', ?4)",
        params![
            input.admission_id,
            input.agent_identity_id,
            input.connection_id,
            verified_at
        ],
    )?;
    tx.execute(
        "INSERT INTO identity_admission_verification_receipts \
         (receipt_id, admission_id, agent_identity_id, connection_id, issuer_id, \
          verification_method, verification_version, trust_domain, verification_scope, \
          evidence_digest, evidence_ref, evidence_issued_at, evidence_expires_at, nonce, \
          idempotency_key, request_digest, current_state, verified_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, \
                 ?15, ?16, 'verified', ?17)",
        params![
            input.receipt_id,
            input.admission_id,
            input.agent_identity_id,
            input.connection_id,
            input.issuer_id,
            input.verification_method,
            input.verification_version,
            input.trust_domain,
            input.verification_scope,
            input.evidence_digest,
            input.evidence_ref,
            issued_at,
            expires_at,
            input.nonce,
            input.idempotency_key,
            request_digest,
            verified_at,
        ],
    )?;
    let receipt = find_by_idempotency(&tx, &input.issuer_id, &input.idempotency_key)?
        .ok_or_else(|| MemoryError::Internal("verified admission receipt insert vanished".into()))?;
    tx.commit()?;
    Ok(VerifiedAdmissionWriteOutcome::Created(receipt))
}

pub fn get_verified_admission_receipt(
    conn: &Connection,
    admission_id: &str,
) -> Result<Option<VerifiedAdmissionReceipt>, MemoryError> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {RECEIPT_COLUMNS} FROM identity_admission_verification_receipts \
                 WHERE admission_id=?1"
            ),
            [admission_id],
            row_to_receipt,
        )
        .optional()?)
}

/// Read-side gate for consumers that require fresh verified identity. A
/// self-asserted admission cannot satisfy this query because the join requires
/// an immutable verifier receipt in addition to `state='verified'`.
pub fn has_fresh_verified_admission(
    conn: &Connection,
    agent_identity_id: &str,
    trust_domain: &str,
    verification_scope: &str,
    as_of: &str,
) -> Result<bool, MemoryError> {
    let as_of = normalize_utc_iso(as_of)?;
    let found: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM identity_admission_verification_receipts r
             JOIN identity_admissions a ON a.admission_id = r.admission_id
             WHERE r.agent_identity_id=?1 AND r.trust_domain=?2
               AND r.verification_scope=?3 AND r.current_state='verified'
               AND a.state='verified' AND r.evidence_issued_at <= ?4
               AND r.evidence_expires_at > ?4
         )",
        params![agent_identity_id, trust_domain, verification_scope, as_of],
        |row| row.get(0),
    )?;
    Ok(found)
}

pub(crate) fn install_verified_admission_schema(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute_batch(VERIFIED_ADMISSION_SCHEMA_SQL)?;
    Ok(())
}

pub(crate) fn validate_verified_admission_schema(conn: &Connection) -> Result<(), MemoryError> {
    for (object_type, name) in [
        ("index", "idx_identity_admissions_verified_binding"),
        ("table", "identity_admission_verification_receipts"),
        ("index", "idx_identity_verification_receipts_identity"),
        ("trigger", "identity_verification_receipts_no_update"),
        ("trigger", "identity_verification_receipts_no_delete"),
    ] {
        let present: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE type=?1 AND name=?2)",
            params![object_type, name],
            |row| row.get(0),
        )?;
        if !present {
            return Err(MemoryError::InvalidArg(format!(
                "incomplete v37 verified admission schema: required {object_type} '{name}' is missing"
            )));
        }
    }
    let table_sql: String = conn.query_row(
        "SELECT COALESCE(sql, '') FROM main.sqlite_schema \
         WHERE type='table' AND name='identity_admission_verification_receipts'",
        [],
        |row| row.get(0),
    )?;
    for clause in [
        "UNIQUE (issuer_id, idempotency_key)",
        "UNIQUE (issuer_id, trust_domain, nonce)",
        "FOREIGN KEY (admission_id, agent_identity_id, connection_id)",
        "current_state TEXT NOT NULL CHECK (current_state = 'verified')",
        "length(evidence_digest) = 64",
        "length(request_digest) = 64",
    ] {
        if !table_sql.contains(clause) {
            return Err(MemoryError::InvalidArg(format!(
                "incomplete v37 verified admission schema: missing canonical clause {clause:?}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        insert_agent_identity, record_unverified_admission, AgentIdentity,
        UnverifiedAdmissionState,
    };

    fn open_conn() -> Connection {
        crate::db::enable_simple_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn
    }

    fn verified_input(key: &str, nonce: &str) -> NewVerifiedAdmission {
        NewVerifiedAdmission {
            receipt_id: format!("receipt-{key}"),
            admission_id: format!("admission-{key}"),
            agent_identity_id: "agent-verified".into(),
            connection_id: format!("connection-{key}"),
            issuer_id: "issuer-device-trust".into(),
            verification_method: "signed-device-envelope".into(),
            verification_version: "v1".into(),
            trust_domain: "workspace:kckylechen1/tachi".into(),
            verification_scope: "agent_identity:remote_admission".into(),
            evidence_digest: "a".repeat(64),
            evidence_ref: "attestation:device:receipt-7".into(),
            evidence_issued_at: "2026-09-19T11:59:00Z".into(),
            evidence_expires_at: "2026-09-19T12:09:00Z".into(),
            nonce: nonce.into(),
            idempotency_key: key.into(),
        }
    }

    #[test]
    fn exact_replay_is_byte_stable_and_changed_digest_conflicts() {
        let mut conn = open_conn();
        let input = verified_input("same-key", "nonce-1");
        let created = record_verified_admission(&mut conn, &input).unwrap();
        assert!(matches!(created, VerifiedAdmissionWriteOutcome::Created(_)));
        let replay = record_verified_admission(&mut conn, &input).unwrap();
        assert!(matches!(replay, VerifiedAdmissionWriteOutcome::Replayed(_)));
        assert_eq!(created.receipt(), replay.receipt());

        let mut conflict = input;
        conflict.evidence_digest = "b".repeat(64);
        let error = record_verified_admission(&mut conn, &conflict)
            .expect_err("same issuer/idempotency with changed evidence must conflict");
        assert!(matches!(error, MemoryError::Duplicate(_)), "{error}");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM identity_admission_verification_receipts",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn nonce_reuse_conflicts_and_verified_receipts_are_append_only() {
        let mut conn = open_conn();
        let first = verified_input("first-key", "one-use-nonce");
        record_verified_admission(&mut conn, &first).unwrap();
        let second = verified_input("second-key", "one-use-nonce");
        let error = record_verified_admission(&mut conn, &second)
            .expect_err("same issuer/domain nonce must not back another admission");
        assert!(matches!(error, MemoryError::Duplicate(_)), "{error}");

        for sql in [
            "UPDATE identity_admission_verification_receipts SET current_state='verified'",
            "DELETE FROM identity_admission_verification_receipts",
        ] {
            let error = conn
                .execute(sql, [])
                .expect_err("verified receipt history must be append-only");
            assert!(error.to_string().contains("append-only"), "{error}");
        }
        let state: String = conn
            .query_row(
                "SELECT current_state FROM identity_admission_verification_receipts",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "verified");
    }

    #[test]
    fn verified_requirement_never_accepts_self_asserted_or_expired_receipt() {
        let mut conn = open_conn();
        insert_agent_identity(
            &conn,
            &AgentIdentity {
                agent_identity_id: "agent-verified".into(),
                display_name: Some("verified-looking-name".into()),
                seat: Some("verified".into()),
                capability_json: None,
                created_at: String::new(),
            },
        )
        .unwrap();
        record_unverified_admission(
            &conn,
            "admission-self",
            "agent-verified",
            "connection-self",
            UnverifiedAdmissionState::SelfAsserted,
        )
        .unwrap();
        assert!(!has_fresh_verified_admission(
            &conn,
            "agent-verified",
            "workspace:kckylechen1/tachi",
            "agent_identity:remote_admission",
            "2026-09-19T12:00:00Z",
        )
        .unwrap());

        record_verified_admission(&mut conn, &verified_input("fresh", "nonce-fresh")).unwrap();
        assert!(has_fresh_verified_admission(
            &conn,
            "agent-verified",
            "workspace:kckylechen1/tachi",
            "agent_identity:remote_admission",
            "2026-09-19T12:00:00Z",
        )
        .unwrap());
        assert!(!has_fresh_verified_admission(
            &conn,
            "agent-verified",
            "workspace:kckylechen1/tachi",
            "agent_identity:remote_admission",
            "2026-09-19T12:09:00Z",
        )
        .unwrap());
        let states = conn
            .prepare(
                "SELECT state FROM identity_admissions WHERE agent_identity_id='agent-verified' \
                 ORDER BY admission_id",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(states, vec!["verified", "self_asserted"]);
    }
}
