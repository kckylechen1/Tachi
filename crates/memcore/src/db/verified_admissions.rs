//! Read-side contract and durable schema for verified AgentIdentity admissions (#1938).
//!
//! Verified construction deliberately does not exist in memcore's exported API.
//! The trusted server adapter owns verification and persistence; memcore owns the
//! canonical append-only schema and exact-bound authority query.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::MemoryError;

#[cfg(test)]
mod rowid_tests;

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
) WITHOUT ROWID
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
) WITHOUT ROWID
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
    WHERE state = 'verified'
      AND (
          rowid = NEW.rowid
          OR admission_id = NEW.admission_id
          OR (
              NEW.agent_identity_id IS NOT NULL
              AND agent_identity_id = NEW.agent_identity_id
              AND connection_id = NEW.connection_id
          )
      )
)
BEGIN
    SELECT RAISE(ABORT, 'verified identity admissions are append-only');
END
"#;

const ADMISSION_NO_UPDATE_SQL: &str = r#"
CREATE TRIGGER identity_verified_admissions_no_update
BEFORE UPDATE ON identity_admissions
WHEN OLD.state = 'verified'
  OR NEW.state = 'verified'
  OR EXISTS (
      SELECT 1 FROM identity_admissions
      WHERE state = 'verified'
        AND rowid != OLD.rowid
        AND (
            rowid = NEW.rowid
            OR admission_id = NEW.admission_id
            OR (
                NEW.agent_identity_id IS NOT NULL
                AND agent_identity_id = NEW.agent_identity_id
                AND connection_id = NEW.connection_id
            )
        )
  )
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

type CanonicalConflictTarget = (&'static str, &'static [&'static str]);

const CANONICAL_UNIQUE_CONFLICT_TARGETS: &[(&str, &[CanonicalConflictTarget])] = &[
    (
        "identity_admissions",
        &[
            ("c", &["admission_id", "agent_identity_id", "connection_id"]),
            ("pk", &["admission_id"]),
            ("u", &["agent_identity_id", "connection_id"]),
        ],
    ),
    (
        "identity_admission_verification_receipts",
        &[
            ("pk", &["receipt_id"]),
            ("u", &["admission_id"]),
            ("u", &["issuer_id", "idempotency_key"]),
            ("u", &["issuer_id", "trust_domain", "nonce"]),
        ],
    ),
    (
        "identity_admission_verification_revocations",
        &[
            ("pk", &["revocation_id"]),
            ("u", &["admission_id"]),
            ("u", &["issuer_id", "nonce"]),
        ],
    ),
];

#[derive(Debug, PartialEq, Eq)]
struct UniqueConflictTarget {
    origin: String,
    columns: Vec<String>,
}

/// Exact receipt coordinates a trusted consumer must present when a verified
/// remote identity authorizes a write. Constructing this value grants no
/// authority: the write gate resolves every field against current durable
/// state inside the same SQLite transaction as the mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAdmissionBinding {
    pub admission_id: String,
    pub agent_identity_id: String,
    pub connection_id: String,
    pub issuer_id: String,
    pub verification_method: String,
    pub verification_version: String,
    pub trust_domain: String,
    pub verification_scope: String,
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

/// Serialize a verified-authority check with the write it authorizes.
///
/// `BEGIN IMMEDIATE` is acquired before the current receipt is read. A
/// concurrent revocation therefore commits either before this check (and the
/// write is refused) or after this transaction commits; it can never land
/// between a successful check and the mutation.
pub fn with_current_verified_admission_write<T>(
    conn: &Connection,
    binding: &VerifiedAdmissionBinding,
    write: impl FnOnce(&Connection) -> Result<T, MemoryError>,
) -> Result<T, MemoryError> {
    super::common::with_composable_write(conn, |conn| {
        if !has_current_verified_admission(
            conn,
            &binding.admission_id,
            &binding.agent_identity_id,
            &binding.connection_id,
            &binding.issuer_id,
            &binding.verification_method,
            &binding.verification_version,
            &binding.trust_domain,
            &binding.verification_scope,
        )? {
            return Err(MemoryError::InvalidArg(
                "verified admission is expired, revoked, or no longer bound to this connection"
                    .to_string(),
            ));
        }
        write(conn)
    })
}

pub(crate) fn expected_verified_admission_trigger(
    name: &str,
) -> Option<(&'static str, &'static str, &'static str)> {
    CANONICAL_TRIGGERS
        .iter()
        .copied()
        .find(|(canonical, _, _)| name.eq_ignore_ascii_case(canonical))
}

// The append-only triggers address SQLite's physical rowid. An explicit
// alias column, including a generated column hidden from table_info, changes
// that meaning. Inspect main explicitly so a TEMP namesake cannot attest it.
const IDENTITY_ADMISSION_ROWID_LAYOUT_SQL: &str = r#"
SELECT EXISTS (
    SELECT 1 FROM pragma_table_list
    WHERE schema = 'main' AND name = 'identity_admissions'
      AND type = 'table' AND wr = 0
) AND NOT EXISTS (
    SELECT 1 FROM pragma_table_xinfo('identity_admissions', 'main')
    WHERE name COLLATE NOCASE IN ('rowid', 'oid', '_rowid_')
)
"#;

fn validate_identity_admission_rowid_layout(conn: &Connection) -> Result<(), MemoryError> {
    let canonical: bool =
        conn.query_row(IDENTITY_ADMISSION_ROWID_LAYOUT_SQL, [], |row| row.get(0))?;
    if !canonical {
        return Err(MemoryError::InvalidArg(
            "incomplete v37 verified admission schema: identity_admissions has a non-canonical rowid layout"
                .to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn install_verified_admission_schema(conn: &Connection) -> Result<(), MemoryError> {
    validate_identity_admission_rowid_layout(conn)?;
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
    sql.trim_end_matches(|ch: char| ch == ';' || ch.is_ascii_whitespace())
        .split_ascii_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn unique_conflict_targets(
    conn: &Connection,
    table: &str,
) -> Result<Vec<UniqueConflictTarget>, MemoryError> {
    let mut statement = conn.prepare(
        "SELECT name, origin, partial FROM pragma_index_list(?1) \
         WHERE \"unique\"=1 ORDER BY name",
    )?;
    let indexes = statement
        .query_map([table], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    indexes
        .into_iter()
        .map(|(name, origin, partial)| {
            if partial {
                return Err(MemoryError::InvalidArg(format!(
                    "incomplete v37 verified admission schema: table '{table}' has a partial unique conflict target"
                )));
            }
            let mut columns = conn.prepare(
                "SELECT name, coll, desc FROM pragma_index_xinfo(?1) \
                 WHERE key=1 ORDER BY seqno",
            )?;
            let columns = columns
                .query_map([name], |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut canonical_columns = Vec::with_capacity(columns.len());
            for (column, collation, descending) in columns {
                let Some(column) = column else {
                    return Err(MemoryError::InvalidArg(format!(
                        "incomplete v37 verified admission schema: table '{table}' has an expression unique conflict target"
                    )));
                };
                if collation.as_deref() != Some("BINARY") || descending {
                    return Err(MemoryError::InvalidArg(format!(
                        "incomplete v37 verified admission schema: table '{table}' has a non-canonical unique conflict target"
                    )));
                }
                canonical_columns.push(column);
            }
            Ok(UniqueConflictTarget {
                origin,
                columns: canonical_columns,
            })
        })
        .collect()
}

fn validate_identity_admission_conflict_policy(conn: &Connection) -> Result<(), MemoryError> {
    let table_sql: String = conn.query_row(
        "SELECT COALESCE(sql, '') FROM main.sqlite_schema \
         WHERE type='table' AND name='identity_admissions'",
        [],
        |row| row.get(0),
    )?;
    if normalize_schema_sql(&table_sql)
        .to_ascii_uppercase()
        .contains("CONFLICT")
    {
        return Err(MemoryError::InvalidArg(
            "incomplete v37 verified admission schema: identity_admissions has a non-canonical conflict policy"
                .to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_verified_admission_schema(conn: &Connection) -> Result<(), MemoryError> {
    validate_identity_admission_rowid_layout(conn)?;
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
    validate_identity_admission_conflict_policy(conn)?;
    for (table, expected_targets) in CANONICAL_UNIQUE_CONFLICT_TARGETS.iter().copied() {
        let targets = unique_conflict_targets(conn, table)?;
        if targets.len() != expected_targets.len()
            || expected_targets.iter().any(|(origin, columns)| {
                targets
                    .iter()
                    .filter(|target| {
                        target.origin == *origin
                            && target
                                .columns
                                .iter()
                                .map(String::as_str)
                                .eq(columns.iter().copied())
                    })
                    .count()
                    != 1
            })
        {
            return Err(MemoryError::InvalidArg(format!(
                "incomplete v37 verified admission schema: table '{table}' has unexpected unique conflict targets"
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

    fn insert_admission(conn: &Connection, suffix: &str) -> (String, String, String) {
        let identity = format!("agent-{suffix}");
        let admission = format!("admission-{suffix}");
        let connection = format!("connection-{suffix}");
        conn.execute(
            "INSERT INTO agent_identities (agent_identity_id, created_at) VALUES (?1, '2026-09-19T00:00:00Z')",
            [&identity],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO identity_admissions
             (admission_id, agent_identity_id, connection_id, state, created_at)
             VALUES (?1, ?2, ?3, 'verified', '2026-09-19T00:00:00Z')",
            params![admission, identity, connection],
        )
        .unwrap();
        (admission, identity, connection)
    }

    fn insert_receipt(conn: &Connection, suffix: &str) -> (String, String, String, String) {
        let (admission, identity, connection) = insert_admission(conn, suffix);
        let receipt = format!("receipt-{suffix}");
        conn.execute(
            "INSERT INTO identity_admission_verification_receipts
             (receipt_id, admission_id, agent_identity_id, connection_id, issuer_id,
              verification_method, verification_version, trust_domain, verification_scope,
              evidence_digest, evidence_ref, evidence_issued_at, evidence_expires_at, nonce,
              idempotency_key, request_digest, current_state, verified_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'device-envelope', 'v1', ?6,
                     'agent_identity:remote_admission', ?7, ?8,
                     '2026-09-19T00:00:00Z', '2099-01-01T00:00:00Z', ?9, ?10, ?11,
                     'verified', '2026-09-19T00:00:00Z')",
            params![
                receipt,
                admission,
                identity,
                connection,
                format!("issuer-{suffix}"),
                format!("workspace:{suffix}"),
                "a".repeat(64),
                format!("attestation:device:{suffix}"),
                format!("nonce-{suffix}"),
                format!("key-{suffix}"),
                "b".repeat(64),
            ],
        )
        .unwrap();
        (receipt, admission, identity, connection)
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

        let admission_rowid: i64 = conn
            .query_row(
                "SELECT rowid FROM identity_admissions WHERE admission_id='admission-v'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO agent_identities (agent_identity_id, created_at)
             VALUES ('agent-source', '2026-09-19T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO identity_admissions
             (admission_id, agent_identity_id, connection_id, state, created_at)
             VALUES ('admission-source', 'agent-source', 'connection-source', 'self_asserted',
                     '2026-09-19T00:00:00Z')",
            [],
        )
        .unwrap();
        for (label, sql) in [
            (
                "hidden-rowid insert replacement",
                format!(
                    "INSERT OR REPLACE INTO identity_admissions
                     (rowid, admission_id, agent_identity_id, connection_id, state, created_at)
                     VALUES ({admission_rowid}, 'admission-rowid-mutant', 'agent-source',
                             'connection-rowid-mutant', 'unavailable', '2026-09-19T00:00:00Z')"
                ),
            ),
            (
                "admission-id victim update replacement",
                "UPDATE OR REPLACE identity_admissions SET admission_id='admission-v'
                 WHERE admission_id='admission-source'"
                    .to_string(),
            ),
            (
                "identity-connection victim update replacement",
                "UPDATE OR REPLACE identity_admissions
                 SET agent_identity_id='agent-v', connection_id='connection-v'
                 WHERE admission_id='admission-source'"
                    .to_string(),
            ),
            (
                "hidden-rowid victim update replacement",
                format!(
                    "UPDATE OR REPLACE identity_admissions SET rowid={admission_rowid}
                     WHERE admission_id='admission-source'"
                ),
            ),
        ] {
            let error = conn.execute(&sql, []).expect_err(label);
            assert!(
                error.to_string().contains("append-only"),
                "{label}: {error}"
            );
        }
        let preserved: (String, String) = conn
            .query_row(
                "SELECT state, connection_id FROM identity_admissions
                 WHERE admission_id='admission-v'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            preserved,
            ("verified".to_string(), "connection-v".to_string())
        );
    }

    #[test]
    fn every_receipt_and_revocation_replace_target_is_append_only() {
        let conn = open_conn();
        let (_, admission_a, identity_a, connection_a) = insert_receipt(&conn, "a");
        let (_, admission_b, identity_b, connection_b) = insert_receipt(&conn, "b");
        let (admission_c, identity_c, connection_c) = insert_admission(&conn, "c");
        let (admission_d, identity_d, connection_d) = insert_admission(&conn, "d");
        let (admission_e, identity_e, connection_e) = insert_admission(&conn, "e");

        assert!(
            conn.query_row(
                "SELECT rowid FROM identity_admission_verification_receipts LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .is_err(),
            "receipt table must expose no hidden rowid replacement target"
        );
        let receipt_insert = |receipt: &str,
                              admission: &str,
                              identity: &str,
                              connection: &str,
                              issuer: &str,
                              domain: &str,
                              nonce: &str,
                              key: &str| {
            conn.execute(
                "INSERT OR REPLACE INTO identity_admission_verification_receipts
                 (receipt_id, admission_id, agent_identity_id, connection_id, issuer_id,
                  verification_method, verification_version, trust_domain, verification_scope,
                  evidence_digest, evidence_ref, evidence_issued_at, evidence_expires_at, nonce,
                  idempotency_key, request_digest, current_state, verified_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'device-envelope', 'v1', ?6,
                         'agent_identity:remote_admission', ?7, 'attestation:device:mutant',
                         '2026-09-19T00:00:00Z', '2099-01-01T00:00:00Z', ?8, ?9, ?10,
                         'verified', '2026-09-19T00:00:00Z')",
                params![
                    receipt,
                    admission,
                    identity,
                    connection,
                    issuer,
                    domain,
                    "c".repeat(64),
                    nonce,
                    key,
                    "d".repeat(64)
                ],
            )
        };
        for (label, result) in [
            (
                "receipt_id",
                receipt_insert(
                    "receipt-a",
                    &admission_c,
                    &identity_c,
                    &connection_c,
                    "issuer-c",
                    "workspace:c",
                    "nonce-c",
                    "key-c",
                ),
            ),
            (
                "admission_id",
                receipt_insert(
                    "receipt-admission-mutant",
                    &admission_a,
                    &identity_a,
                    &connection_a,
                    "issuer-admission-mutant",
                    "workspace:admission-mutant",
                    "nonce-admission-mutant",
                    "key-admission-mutant",
                ),
            ),
            (
                "issuer-idempotency",
                receipt_insert(
                    "receipt-idempotency-mutant",
                    &admission_d,
                    &identity_d,
                    &connection_d,
                    "issuer-a",
                    "workspace:idempotency-mutant",
                    "nonce-idempotency-mutant",
                    "key-a",
                ),
            ),
            (
                "issuer-domain-nonce",
                receipt_insert(
                    "receipt-nonce-mutant",
                    &admission_e,
                    &identity_e,
                    &connection_e,
                    "issuer-a",
                    "workspace:a",
                    "nonce-a",
                    "key-nonce-mutant",
                ),
            ),
        ] {
            let error = result.expect_err(label);
            assert!(
                error.to_string().contains("append-only"),
                "{label}: {error}"
            );
        }

        conn.execute(
            "INSERT INTO identity_admission_verification_revocations
             (revocation_id, admission_id, issuer_id, evidence_digest, evidence_ref, nonce, revoked_at)
             VALUES ('revocation-a', ?1, 'issuer-a', ?2, 'attestation:device:revoke-a',
                     'revocation-nonce-a', '2026-09-19T00:00:00Z')",
            params![admission_a, "e".repeat(64)],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT OR REPLACE INTO identity_admission_verification_revocations
                 (rowid, revocation_id, admission_id, issuer_id, evidence_digest, evidence_ref,
                  nonce, revoked_at)
                 VALUES (1, 'revocation-rowid-mutant', ?1, 'issuer-b', ?2,
                         'attestation:device:rowid-mutant', 'revocation-rowid-mutant',
                         '2026-09-19T00:00:00Z')",
                params![admission_b, "f".repeat(64)],
            )
            .is_err(),
            "revocation table must expose no hidden rowid replacement target"
        );
        for (label, revocation_id, admission, issuer, nonce) in [
            (
                "revocation_id",
                "revocation-a",
                &admission_b,
                "issuer-b",
                "unique-b",
            ),
            (
                "admission_id",
                "revocation-admission",
                &admission_a,
                "issuer-x",
                "unique-x",
            ),
            (
                "issuer-nonce",
                "revocation-nonce",
                &admission_b,
                "issuer-a",
                "revocation-nonce-a",
            ),
        ] {
            let error = conn
                .execute(
                    "INSERT OR REPLACE INTO identity_admission_verification_revocations
                     (revocation_id, admission_id, issuer_id, evidence_digest, evidence_ref, nonce,
                      revoked_at)
                     VALUES (?1, ?2, ?3, ?4, 'attestation:device:revocation-mutant', ?5,
                             '2026-09-19T00:00:00Z')",
                    params![revocation_id, admission, issuer, "f".repeat(64), nonce],
                )
                .expect_err(label);
            assert!(
                error.to_string().contains("append-only"),
                "{label}: {error}"
            );
        }
        assert!(!has_current_verified_admission(
            &conn,
            &admission_a,
            &identity_a,
            &connection_a,
            "issuer-a",
            VERIFIED_ADMISSION_METHOD,
            VERIFIED_ADMISSION_VERSION,
            "workspace:a",
            VERIFIED_ADMISSION_SCOPE,
        )
        .unwrap());
        assert!(has_current_verified_admission(
            &conn,
            &admission_b,
            &identity_b,
            &connection_b,
            "issuer-b",
            VERIFIED_ADMISSION_METHOD,
            VERIFIED_ADMISSION_VERSION,
            "workspace:b",
            VERIFIED_ADMISSION_SCOPE,
        )
        .unwrap());
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

    #[test]
    fn schema_validation_rejects_additional_unique_conflict_targets() {
        let conn = open_conn();
        for (index, table, column) in [
            (
                "unexpected_identity_admission_unique",
                "identity_admissions",
                "created_at",
            ),
            (
                "unexpected_verified_receipt_unique",
                "identity_admission_verification_receipts",
                "evidence_ref",
            ),
            (
                "unexpected_verified_revocation_unique",
                "identity_admission_verification_revocations",
                "evidence_ref",
            ),
        ] {
            conn.execute_batch(&format!(
                "CREATE UNIQUE INDEX {index} ON {table}({column})"
            ))
            .unwrap();
            let error = validate_verified_admission_schema(&conn)
                .expect_err("an additional OR REPLACE victim target must fail closed");
            assert!(
                error
                    .to_string()
                    .contains("unexpected unique conflict targets"),
                "{table}: {error}"
            );
            conn.execute_batch(&format!("DROP INDEX {index}"))
                .unwrap();
        }
        validate_verified_admission_schema(&conn).unwrap();
    }

    fn open_with_identity_admission_constraint(
        constraint: &str,
        recursive_triggers: bool,
    ) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "PRAGMA foreign_keys=ON;
             PRAGMA recursive_triggers={};
             CREATE TABLE agent_identities (
                 agent_identity_id TEXT PRIMARY KEY,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE identity_admissions (
                 admission_id TEXT PRIMARY KEY,
                 agent_identity_id TEXT,
                 connection_id TEXT NOT NULL,
                 state TEXT NOT NULL,
                 rejection_evidence TEXT,
                 created_at TEXT NOT NULL DEFAULT '',
                 {constraint}
             );",
            i64::from(recursive_triggers)
        ))
        .unwrap();
        install_verified_admission_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn schema_validation_rejects_altered_or_reordered_identity_conflict_targets() {
        for recursive_triggers in [false, true] {
            for target in ["created_at", "connection_id, agent_identity_id"] {
                let conn = open_with_identity_admission_constraint(
                    &format!("UNIQUE({target})"),
                    recursive_triggers,
                );
                if !recursive_triggers && target == "created_at" {
                    conn.execute_batch(
                        "INSERT INTO agent_identities (agent_identity_id, created_at)
                         VALUES ('agent-victim', '2026-09-20T00:00:00Z'),
                                ('agent-replacement', '2026-09-20T00:00:00Z');
                         INSERT INTO identity_admissions
                             (admission_id, agent_identity_id, connection_id, state, created_at)
                         VALUES ('admission-victim', 'agent-victim', 'connection-victim',
                                 'verified', 'shared-conflict-value');
                         INSERT OR REPLACE INTO identity_admissions
                             (admission_id, agent_identity_id, connection_id, state, created_at)
                         VALUES ('admission-replacement', 'agent-replacement',
                                 'connection-replacement', 'self_asserted',
                                 'shared-conflict-value');",
                    )
                    .expect("the name-only schema admits the recursive_triggers=OFF victim delete");
                    let victim_survives: bool = conn
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM identity_admissions
                             WHERE admission_id='admission-victim')",
                            [],
                            |row| row.get(0),
                        )
                        .unwrap();
                    assert!(
                        !victim_survives,
                        "the altered conflict target is a real append-only bypass"
                    );
                }
                let error = validate_verified_admission_schema(&conn)
                    .expect_err("autoindex names must not substitute for canonical targets");
                assert!(
                    error
                        .to_string()
                        .contains("unexpected unique conflict targets"),
                    "recursive_triggers={recursive_triggers}, target={target}: {error}"
                );
            }
        }
    }

    #[test]
    fn schema_validation_rejects_replace_conflict_policy_on_canonical_target() {
        for policy in [
            "ON CONFLICT REPLACE",
            "ON /* comments cannot hide policy tokens */ CONFLICT REPLACE",
        ] {
            let conn = open_with_identity_admission_constraint(
                &format!("UNIQUE(agent_identity_id, connection_id) {policy}"),
                false,
            );
            let error = validate_verified_admission_schema(&conn)
                .expect_err("canonical columns with replacement policy must fail closed");
            assert!(
                error.to_string().contains("non-canonical conflict policy"),
                "{policy}: {error}"
            );
        }
    }
}
