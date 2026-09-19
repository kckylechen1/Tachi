//! Read-side contract and durable schema for verified AgentIdentity admissions (#1938).
//!
//! Verified construction deliberately does not exist in memcore's exported API.
//! The trusted server adapter owns verification and persistence; memcore owns the
//! canonical append-only schema and exact-bound authority query.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::MemoryError;

pub const VERIFIED_ADMISSION_METHOD: &str = "device-envelope";
pub const VERIFIED_ADMISSION_VERSION: &str = "v1";
pub const VERIFIED_ADMISSION_SCOPE: &str = "agent_identity:remote_admission";

const RECEIPT_COLUMNS: &str = "receipt_id, admission_id, agent_identity_id, connection_id, \
    issuer_id, verification_method, verification_version, trust_domain, verification_scope, \
    evidence_digest, evidence_ref, evidence_issued_at, evidence_expires_at, nonce, \
    idempotency_key, request_digest, current_state, verified_at";

const VERIFIED_BINDING_INDEX_SQL: &str = r#"
CREATE UNIQUE INDEX idx_identity_admissions_verified_binding
ON identity_admissions(admission_id, agent_identity_id, connection_id)
"#;

const RECEIPTS_TABLE_SQL: &str = r#"
CREATE TABLE identity_admission_verification_receipts (
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
    evidence_ref TEXT NOT NULL CHECK (
        length(evidence_ref) BETWEEN 1 AND 256
        AND evidence_ref NOT GLOB '*[^a-zA-Z0-9:._/-]*'
    ),
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
)
"#;

const RECEIPTS_IDENTITY_INDEX_SQL: &str = r#"
CREATE INDEX idx_identity_verification_receipts_identity
ON identity_admission_verification_receipts(agent_identity_id, trust_domain, verification_scope)
"#;

const REVOCATIONS_TABLE_SQL: &str = r#"
CREATE TABLE identity_admission_verification_revocations (
    revocation_id TEXT PRIMARY KEY,
    admission_id TEXT NOT NULL UNIQUE
        REFERENCES identity_admission_verification_receipts(admission_id),
    issuer_id TEXT NOT NULL,
    evidence_digest TEXT NOT NULL CHECK (
        length(evidence_digest) = 64
        AND evidence_digest NOT GLOB '*[^0-9a-f]*'
    ),
    evidence_ref TEXT NOT NULL CHECK (
        length(evidence_ref) BETWEEN 1 AND 256
        AND evidence_ref NOT GLOB '*[^a-zA-Z0-9:._/-]*'
    ),
    nonce TEXT NOT NULL,
    revoked_at TEXT NOT NULL,
    UNIQUE (issuer_id, nonce)
)
"#;

const REVOCATIONS_ADMISSION_INDEX_SQL: &str = r#"
CREATE INDEX idx_identity_verification_revocations_admission
ON identity_admission_verification_revocations(admission_id, revoked_at)
"#;

const RECEIPT_NO_REPLACE_TRIGGER: &str = "identity_verification_receipts_no_replace";
const RECEIPT_NO_UPDATE_TRIGGER: &str = "identity_verification_receipts_no_update";
const RECEIPT_NO_DELETE_TRIGGER: &str = "identity_verification_receipts_no_delete";
const ADMISSION_NO_REPLACE_TRIGGER: &str = "identity_verified_admissions_no_replace";
const ADMISSION_NO_UPDATE_TRIGGER: &str = "identity_verified_admissions_no_update";
const ADMISSION_NO_DELETE_TRIGGER: &str = "identity_verified_admissions_no_delete";
const REVOCATION_NO_REPLACE_TRIGGER: &str = "identity_verification_revocations_no_replace";
const REVOCATION_NO_UPDATE_TRIGGER: &str = "identity_verification_revocations_no_update";
const REVOCATION_NO_DELETE_TRIGGER: &str = "identity_verification_revocations_no_delete";

const RECEIPT_NO_REPLACE_SQL: &str = r#"
CREATE TRIGGER identity_verification_receipts_no_replace
BEFORE INSERT ON identity_admission_verification_receipts
WHEN EXISTS (
    SELECT 1 FROM identity_admission_verification_receipts
    WHERE receipt_id = NEW.receipt_id
       OR admission_id = NEW.admission_id
       OR (issuer_id = NEW.issuer_id AND idempotency_key = NEW.idempotency_key)
       OR (issuer_id = NEW.issuer_id AND trust_domain = NEW.trust_domain AND nonce = NEW.nonce)
)
BEGIN
    SELECT RAISE(ABORT, 'verified admission receipts are append-only');
END
"#;

const RECEIPT_NO_UPDATE_SQL: &str = r#"
CREATE TRIGGER identity_verification_receipts_no_update
BEFORE UPDATE ON identity_admission_verification_receipts
BEGIN
    SELECT RAISE(ABORT, 'verified admission receipts are append-only');
END
"#;

const RECEIPT_NO_DELETE_SQL: &str = r#"
CREATE TRIGGER identity_verification_receipts_no_delete
BEFORE DELETE ON identity_admission_verification_receipts
BEGIN
    SELECT RAISE(ABORT, 'verified admission receipts are append-only');
END
"#;

const ADMISSION_NO_REPLACE_SQL: &str = r#"
CREATE TRIGGER identity_verified_admissions_no_replace
BEFORE INSERT ON identity_admissions
WHEN EXISTS (
    SELECT 1 FROM identity_admissions
    WHERE admission_id = NEW.admission_id AND state = 'verified'
)
BEGIN
    SELECT RAISE(ABORT, 'verified identity admissions are append-only');
END
"#;

const ADMISSION_NO_UPDATE_SQL: &str = r#"
CREATE TRIGGER identity_verified_admissions_no_update
BEFORE UPDATE ON identity_admissions
WHEN OLD.state = 'verified'
BEGIN
    SELECT RAISE(ABORT, 'verified identity admissions are append-only');
END
"#;

const ADMISSION_NO_DELETE_SQL: &str = r#"
CREATE TRIGGER identity_verified_admissions_no_delete
BEFORE DELETE ON identity_admissions
WHEN OLD.state = 'verified'
BEGIN
    SELECT RAISE(ABORT, 'verified identity admissions are append-only');
END
"#;

const REVOCATION_NO_REPLACE_SQL: &str = r#"
CREATE TRIGGER identity_verification_revocations_no_replace
BEFORE INSERT ON identity_admission_verification_revocations
WHEN EXISTS (
    SELECT 1 FROM identity_admission_verification_revocations
    WHERE revocation_id = NEW.revocation_id
       OR admission_id = NEW.admission_id
       OR (issuer_id = NEW.issuer_id AND nonce = NEW.nonce)
)
BEGIN
    SELECT RAISE(ABORT, 'verified admission revocations are append-only');
END
"#;

const REVOCATION_NO_UPDATE_SQL: &str = r#"
CREATE TRIGGER identity_verification_revocations_no_update
BEFORE UPDATE ON identity_admission_verification_revocations
BEGIN
    SELECT RAISE(ABORT, 'verified admission revocations are append-only');
END
"#;

const REVOCATION_NO_DELETE_SQL: &str = r#"
CREATE TRIGGER identity_verification_revocations_no_delete
BEFORE DELETE ON identity_admission_verification_revocations
BEGIN
    SELECT RAISE(ABORT, 'verified admission revocations are append-only');
END
"#;

const CANONICAL_TRIGGERS: &[(&str, &str, &str)] = &[
    (
        RECEIPT_NO_REPLACE_TRIGGER,
        "identity_admission_verification_receipts",
        RECEIPT_NO_REPLACE_SQL,
    ),
    (
        RECEIPT_NO_UPDATE_TRIGGER,
        "identity_admission_verification_receipts",
        RECEIPT_NO_UPDATE_SQL,
    ),
    (
        RECEIPT_NO_DELETE_TRIGGER,
        "identity_admission_verification_receipts",
        RECEIPT_NO_DELETE_SQL,
    ),
    (
        ADMISSION_NO_REPLACE_TRIGGER,
        "identity_admissions",
        ADMISSION_NO_REPLACE_SQL,
    ),
    (
        ADMISSION_NO_UPDATE_TRIGGER,
        "identity_admissions",
        ADMISSION_NO_UPDATE_SQL,
    ),
    (
        ADMISSION_NO_DELETE_TRIGGER,
        "identity_admissions",
        ADMISSION_NO_DELETE_SQL,
    ),
    (
        REVOCATION_NO_REPLACE_TRIGGER,
        "identity_admission_verification_revocations",
        REVOCATION_NO_REPLACE_SQL,
    ),
    (
        REVOCATION_NO_UPDATE_TRIGGER,
        "identity_admission_verification_revocations",
        REVOCATION_NO_UPDATE_SQL,
    ),
    (
        REVOCATION_NO_DELETE_TRIGGER,
        "identity_admission_verification_revocations",
        REVOCATION_NO_DELETE_SQL,
    ),
];

const CANONICAL_OBJECTS: &[(&str, &str, &str, &str)] = &[
    (
        "index",
        "idx_identity_admissions_verified_binding",
        "identity_admissions",
        VERIFIED_BINDING_INDEX_SQL,
    ),
    (
        "table",
        "identity_admission_verification_receipts",
        "identity_admission_verification_receipts",
        RECEIPTS_TABLE_SQL,
    ),
    (
        "index",
        "idx_identity_verification_receipts_identity",
        "identity_admission_verification_receipts",
        RECEIPTS_IDENTITY_INDEX_SQL,
    ),
    (
        "table",
        "identity_admission_verification_revocations",
        "identity_admission_verification_revocations",
        REVOCATIONS_TABLE_SQL,
    ),
    (
        "index",
        "idx_identity_verification_revocations_admission",
        "identity_admission_verification_revocations",
        REVOCATIONS_ADMISSION_INDEX_SQL,
    ),
];

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

/// Exact-bound, authoritative-clock gate for remote verified authority.
///
/// The admission id, identity, live connection/placement, issuer policy,
/// method/version, domain, and scope must all match one unexpired receipt with
/// no append-only revocation event. Self-asserted admissions cannot satisfy it.
///
/// Verified construction is intentionally absent from the public API:
/// ```compile_fail
/// let _writer = memcore::record_verified_admission;
/// ```
#[allow(clippy::too_many_arguments)]
pub fn has_current_verified_admission(
    conn: &Connection,
    admission_id: &str,
    agent_identity_id: &str,
    connection_id: &str,
    issuer_id: &str,
    verification_method: &str,
    verification_version: &str,
    trust_domain: &str,
    verification_scope: &str,
) -> Result<bool, MemoryError> {
    let found: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM identity_admission_verification_receipts r
             JOIN identity_admissions a ON a.admission_id = r.admission_id
             WHERE r.admission_id=?1 AND r.agent_identity_id=?2 AND r.connection_id=?3
               AND r.issuer_id=?4 AND r.verification_method=?5
               AND r.verification_version=?6 AND r.trust_domain=?7
               AND r.verification_scope=?8 AND r.current_state='verified'
               AND a.agent_identity_id=r.agent_identity_id
               AND a.connection_id=r.connection_id AND a.state='verified'
               AND julianday(r.evidence_issued_at) <= julianday('now')
               AND julianday(r.evidence_expires_at) > julianday('now')
               AND NOT EXISTS (
                   SELECT 1 FROM identity_admission_verification_revocations v
                   WHERE v.admission_id=r.admission_id
               )
         )",
        params![
            admission_id,
            agent_identity_id,
            connection_id,
            issuer_id,
            verification_method,
            verification_version,
            trust_domain,
            verification_scope,
        ],
        |row| row.get(0),
    )?;
    Ok(found)
}

pub(crate) fn expected_verified_admission_trigger(
    name: &str,
) -> Option<(&'static str, &'static str, &'static str)> {
    CANONICAL_TRIGGERS
        .iter()
        .copied()
        .find(|(canonical, _, _)| name.eq_ignore_ascii_case(canonical))
}

pub(crate) fn install_verified_admission_schema(conn: &Connection) -> Result<(), MemoryError> {
    for (object_type, name, _, sql) in CANONICAL_OBJECTS.iter().copied() {
        let present: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE type=?1 AND name=?2)",
            params![object_type, name],
            |row| row.get(0),
        )?;
        if !present {
            conn.execute_batch(sql)?;
        }
    }
    for (name, _, sql) in CANONICAL_TRIGGERS.iter().copied() {
        let present: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE type='trigger' AND name=?1)",
            [name],
            |row| row.get(0),
        )?;
        if !present {
            conn.execute_batch(sql)?;
        }
    }
    Ok(())
}

fn normalize_schema_sql(sql: &str) -> String {
    sql.trim_end_matches(|ch: char| ch == ';' || ch.is_whitespace())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn validate_verified_admission_schema(conn: &Connection) -> Result<(), MemoryError> {
    for (object_type, name, table, canonical_sql) in CANONICAL_OBJECTS.iter().copied() {
        let found: Option<(String, String)> = conn
            .query_row(
                "SELECT tbl_name, COALESCE(sql, '') FROM main.sqlite_schema \
                 WHERE type=?1 AND name=?2",
                params![object_type, name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if found.as_ref().is_none_or(|(actual_table, actual_sql)| {
            actual_table != table
                || normalize_schema_sql(actual_sql) != normalize_schema_sql(canonical_sql)
        }) {
            return Err(MemoryError::InvalidArg(format!(
                "incomplete v37 verified admission schema: {object_type} '{name}' is missing or non-canonical"
            )));
        }
    }
    for (name, table, canonical_sql) in CANONICAL_TRIGGERS.iter().copied() {
        let found: Option<(String, String)> = conn
            .query_row(
                "SELECT tbl_name, COALESCE(sql, '') FROM main.sqlite_schema \
                 WHERE type='trigger' AND name=?1",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if found.as_ref().is_none_or(|(actual_table, actual_sql)| {
            actual_table != table
                || normalize_schema_sql(actual_sql) != normalize_schema_sql(canonical_sql)
        }) {
            return Err(MemoryError::InvalidArg(format!(
                "incomplete v37 verified admission schema: trigger '{name}' is missing or non-canonical"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_conn() -> Connection {
        crate::db::enable_simple_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn replace_update_and_delete_cannot_rewrite_verified_history() {
        let conn = open_conn();
        conn.execute(
            "INSERT INTO agent_identities (agent_identity_id, created_at) VALUES ('agent-v', '2026-09-19T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO identity_admissions (admission_id, agent_identity_id, connection_id, state, created_at)
             VALUES ('admission-v', 'agent-v', 'connection-v', 'verified', '2026-09-19T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO identity_admission_verification_receipts
             (receipt_id, admission_id, agent_identity_id, connection_id, issuer_id,
              verification_method, verification_version, trust_domain, verification_scope,
              evidence_digest, evidence_ref, evidence_issued_at, evidence_expires_at, nonce,
              idempotency_key, request_digest, current_state, verified_at)
             VALUES ('receipt-v', 'admission-v', 'agent-v', 'connection-v', 'issuer-v',
              'device-envelope', 'v1', 'workspace:owner/repo', 'agent_identity:remote_admission',
              ?1, 'attestation:device:one', '2026-09-19T00:00:00Z', '2099-01-01T00:00:00Z',
              'nonce-v', 'key-v', ?2, 'verified', '2026-09-19T00:00:00Z')",
            params!["a".repeat(64), "b".repeat(64)],
        )
        .unwrap();

        for sql in [
            "UPDATE identity_admission_verification_receipts SET evidence_ref='attestation:device:mutant'",
            "DELETE FROM identity_admission_verification_receipts",
            "INSERT OR REPLACE INTO identity_admission_verification_receipts
             (receipt_id, admission_id, agent_identity_id, connection_id, issuer_id,
              verification_method, verification_version, trust_domain, verification_scope,
              evidence_digest, evidence_ref, evidence_issued_at, evidence_expires_at, nonce,
              idempotency_key, request_digest, current_state, verified_at)
             SELECT receipt_id, admission_id, agent_identity_id, connection_id, issuer_id,
              verification_method, verification_version, trust_domain, verification_scope,
              evidence_digest, 'attestation:device:mutant', evidence_issued_at,
              evidence_expires_at, nonce, idempotency_key, request_digest, current_state,
              verified_at FROM identity_admission_verification_receipts",
        ] {
            let error = conn.execute(sql, []).expect_err("history mutation must fail");
            assert!(error.to_string().contains("append-only"), "{error}");
        }
        let evidence_ref: String = conn
            .query_row(
                "SELECT evidence_ref FROM identity_admission_verification_receipts",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(evidence_ref, "attestation:device:one");
    }

    #[test]
    fn schema_validation_rejects_same_name_trigger_drift() {
        let conn = open_conn();
        conn.execute_batch(
            "DROP TRIGGER identity_verification_receipts_no_update;
             CREATE TRIGGER identity_verification_receipts_no_update
             BEFORE UPDATE ON identity_admission_verification_receipts BEGIN SELECT 1; END;",
        )
        .unwrap();
        let error = validate_verified_admission_schema(&conn)
            .expect_err("same-name non-canonical trigger must fail closed");
        assert!(error.to_string().contains("missing or non-canonical"));
    }
}
