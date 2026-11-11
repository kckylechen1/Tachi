//! The retained mailbox schema and its versioned migrations.
use super::*;

const KIND_CHECK: &str = "CHECK (kind IN ('turn_response/v1','coordination/v1'))";
const IDEMPOTENCY_CHECK: &str =
    "UNIQUE (issuer_agent_identity_id, canonical_project, idempotency_key)";
const PROJECT_CHECK: &str = "CHECK ((kind = 'turn_response/v1' AND canonical_project = '') OR (kind = 'coordination/v1' AND length(canonical_project) = 64))";

/// Preserve every legacy envelope/receipt and its replay key. New coordination
/// envelopes use the same FK pair, scoped by a server-derived project digest.
pub(crate) fn migrate_v42_coordination_mailbox(conn: &Connection) -> Result<(), MemoryError> {
    validate_a2a_mailbox_schema_version(conn, 32)?;
    execute_batch_retry(
        conn,
        "ALTER TABLE identity_admissions ADD COLUMN canonical_project TEXT NOT NULL DEFAULT '';
        DROP INDEX idx_a2a_envelopes_recipient_state;
        DROP INDEX idx_a2a_envelopes_issuer_created;
        DROP INDEX idx_a2a_receipts_envelope_version;
        ALTER TABLE a2a_delivery_receipts RENAME TO a2a_delivery_receipts_v41;
        ALTER TABLE a2a_envelopes RENAME TO a2a_envelopes_v41;",
    )?;
    let sql = ddl::A2A_MAILBOX_V32_SQL
        .replace(ddl::A2A_KIND_CHECK_CLAUSE, KIND_CHECK)
        .replace(ddl::A2A_IDEMPOTENCY_UNIQUE_CLAUSE, &format!(
            "canonical_project TEXT NOT NULL DEFAULT '',\n            {IDEMPOTENCY_CHECK},\n            {PROJECT_CHECK}"))
        .replace("recipient_agent_identity_id, current_state, expires_at", "recipient_agent_identity_id, canonical_project, current_state, expires_at");
    execute_batch_retry(conn, &sql)?;
    execute_batch_retry(
        conn,
        "INSERT INTO a2a_envelopes
        (envelope_id,kind,issuer_agent_identity_id,issuer_admission_id,recipient_agent_identity_id,
         recipient_admission_id,subject_ref,body,body_digest,issuer_identity_assurance,
         recipient_identity_assurance,issuer_trust_domain,recipient_trust_domain,
         issuer_trust_basis,recipient_trust_basis,idempotency_key,created_at,expires_at,
         current_state,state_version)
        SELECT * FROM a2a_envelopes_v41;
        INSERT INTO a2a_delivery_receipts SELECT * FROM a2a_delivery_receipts_v41;
        DROP TABLE a2a_delivery_receipts_v41;
        DROP TABLE a2a_envelopes_v41;",
    )?;
    validate_a2a_mailbox_schema_version(conn, 42)
}

pub(crate) fn install_a2a_mailbox_schema(conn: &Connection) -> Result<(), MemoryError> {
    execute_batch_retry(conn, ddl::A2A_MAILBOX_V31_SQL)
}

/// Validate both Product-only A2A tables as one schema unit. A half-present
/// pair is always corruption; portable callers avoid this validator because
/// their effective profile intentionally owns neither table.
pub(crate) fn validate_a2a_mailbox_v31_schema(conn: &Connection) -> Result<(), MemoryError> {
    validate_a2a_mailbox_schema_version(conn, 31)
}

pub(crate) fn validate_a2a_mailbox_schema(conn: &Connection) -> Result<(), MemoryError> {
    let version = if super::super::version_policy::VersionHeader::read(conn)?.stored >= 42 {
        42
    } else {
        32
    };
    validate_a2a_mailbox_schema_version(conn, version)
}

fn validate_a2a_mailbox_schema_version(conn: &Connection, version: u32) -> Result<(), MemoryError> {
    let label = if version == 31 {
        "incomplete v31 A2A mailbox".to_string()
    } else {
        // Keep the established diagnostic token for callers/tests that key on
        // the A2A corruption family while adding the authoritative stamp.
        format!("incomplete current schema v{version}: incomplete v31 A2A mailbox")
    };
    const REQUIRED_OBJECTS: &[(&str, &str, &str)] = &[
        ("table", "a2a_envelopes", "a2a_envelopes"),
        (
            "index",
            "idx_a2a_envelopes_recipient_state",
            "a2a_envelopes",
        ),
        ("index", "idx_a2a_envelopes_issuer_created", "a2a_envelopes"),
        ("table", "a2a_delivery_receipts", "a2a_delivery_receipts"),
        (
            "index",
            "idx_a2a_receipts_envelope_version",
            "a2a_delivery_receipts",
        ),
    ];
    for (object_type, name, table) in REQUIRED_OBJECTS {
        let present = conn
            .query_row(
                "SELECT 1 FROM main.sqlite_schema WHERE type=?1 AND name=?2 AND tbl_name=?3",
                params![object_type, name, table],
                |_| Ok(()),
            )
            .optional()?;
        if present.is_none() {
            return Err(MemoryError::InvalidArg(format!(
                "{label}: required {object_type} '{name}' on '{table}' is missing"
            )));
        }
    }

    let mut envelope_columns = vec![
        ("envelope_id", "TEXT", true, 1),
        ("kind", "TEXT", true, 0),
        ("issuer_agent_identity_id", "TEXT", true, 0),
        ("issuer_admission_id", "TEXT", true, 0),
        ("recipient_agent_identity_id", "TEXT", true, 0),
        ("recipient_admission_id", "TEXT", true, 0),
        ("subject_ref", "TEXT", true, 0),
        ("body", "TEXT", version == 31, 0),
        ("body_digest", "TEXT", true, 0),
        ("issuer_identity_assurance", "TEXT", true, 0),
        ("recipient_identity_assurance", "TEXT", true, 0),
        ("issuer_trust_domain", "TEXT", true, 0),
        ("recipient_trust_domain", "TEXT", true, 0),
        ("issuer_trust_basis", "TEXT", true, 0),
        ("recipient_trust_basis", "TEXT", true, 0),
        ("idempotency_key", "TEXT", true, 0),
        ("created_at", "TEXT", true, 0),
        ("expires_at", "TEXT", true, 0),
        ("current_state", "TEXT", true, 0),
        ("state_version", "INTEGER", true, 0),
    ];
    if version >= 42 {
        envelope_columns.push(("canonical_project", "TEXT", true, 0));
    }
    const RECEIPT_COLUMNS: &[(&str, &str, bool, i64)] = &[
        ("receipt_id", "TEXT", true, 1),
        ("envelope_id", "TEXT", true, 0),
        ("envelope_version", "INTEGER", true, 0),
        ("state", "TEXT", true, 0),
        ("actor_agent_identity_id", "TEXT", true, 0),
        ("actor_admission_id", "TEXT", true, 0),
        ("identity_assurance", "TEXT", true, 0),
        ("trust_domain", "TEXT", true, 0),
        ("trust_basis", "TEXT", true, 0),
        ("occurred_at", "TEXT", true, 0),
    ];
    let validate_columns = |table: &str,
                            expected: &[(&str, &str, bool, i64)]|
     -> Result<(), MemoryError> {
        let mut stmt = conn.prepare(&format!(
            "SELECT name,upper(type),[notnull] != 0,pk FROM pragma_table_info('{table}') ORDER BY cid"
        ))?;
        let actual = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let expected = expected
            .iter()
            .map(|(name, ty, not_null, pk)| {
                ((*name).to_string(), (*ty).to_string(), *not_null, *pk)
            })
            .collect::<Vec<_>>();
        if actual != expected {
            return Err(MemoryError::InvalidArg(format!(
                "{label}: table '{table}' has non-canonical column shape"
            )));
        }
        Ok(())
    };
    validate_columns("a2a_envelopes", &envelope_columns)?;
    validate_columns("a2a_delivery_receipts", RECEIPT_COLUMNS)?;

    let envelope_sql: String = conn.query_row(
        "SELECT COALESCE(sql,'') FROM main.sqlite_schema WHERE type='table' AND name='a2a_envelopes'",
        [],
        |row| row.get(0),
    )?;
    let receipt_sql: String = conn.query_row(
        "SELECT COALESCE(sql,'') FROM main.sqlite_schema WHERE type='table' AND name='a2a_delivery_receipts'",
        [],
        |row| row.get(0),
    )?;
    let envelope_sql = normalize_schema_sql(&envelope_sql);
    for clause in [
        if version >= 42 {
            KIND_CHECK
        } else {
            ddl::A2A_KIND_CHECK_CLAUSE
        },
        ddl::A2A_BODY_DIGEST_CHECK_CLAUSE,
        ddl::A2A_ISSUER_ASSURANCE_CHECK_CLAUSE,
        ddl::A2A_RECIPIENT_ASSURANCE_CHECK_CLAUSE,
        ddl::A2A_ISSUER_TRUST_DOMAIN_CHECK_CLAUSE,
        ddl::A2A_RECIPIENT_TRUST_DOMAIN_CHECK_CLAUSE,
        ddl::A2A_ISSUER_TRUST_BASIS_CHECK_CLAUSE,
        ddl::A2A_RECIPIENT_TRUST_BASIS_CHECK_CLAUSE,
        ddl::A2A_EXPIRY_CHECK_CLAUSE,
        if version >= 42 {
            IDEMPOTENCY_CHECK
        } else {
            ddl::A2A_IDEMPOTENCY_UNIQUE_CLAUSE
        },
        ddl::A2A_ISSUER_IDENTITY_FK_CLAUSE,
        ddl::A2A_ISSUER_ADMISSION_FK_CLAUSE,
        ddl::A2A_RECIPIENT_IDENTITY_FK_CLAUSE,
        ddl::A2A_RECIPIENT_ADMISSION_FK_CLAUSE,
        ddl::A2A_ENVELOPE_STATE_CHECK_CLAUSE,
    ] {
        if !envelope_sql.contains(&normalize_schema_sql(clause)) {
            return Err(MemoryError::InvalidArg(format!(
                "{label}: envelope constraint drifted: {clause}"
            )));
        }
    }
    if version >= 42 && !envelope_sql.contains(&normalize_schema_sql(PROJECT_CHECK)) {
        return Err(MemoryError::InvalidArg(format!(
            "{label}: envelope project constraint drifted"
        )));
    }
    let body_clause = if version == 31 {
        ddl::A2A_BODY_SIZE_CHECK_CLAUSE
    } else {
        ddl::A2A_BODY_SIZE_V32_CHECK_CLAUSE
    };
    if !envelope_sql.contains(&normalize_schema_sql(body_clause)) {
        return Err(MemoryError::InvalidArg(format!(
            "{label}: envelope body constraint drifted: {body_clause}"
        )));
    }
    let receipt_sql = normalize_schema_sql(&receipt_sql);
    for clause in [
        ddl::A2A_RECEIPT_STATE_CHECK_CLAUSE,
        ddl::A2A_RECEIPT_ENVELOPE_FK_CLAUSE,
        ddl::A2A_RECEIPT_ACTOR_IDENTITY_FK_CLAUSE,
        ddl::A2A_RECEIPT_ACTOR_ADMISSION_FK_CLAUSE,
        "CHECK (identity_assurance = 'self_asserted')",
        "CHECK (trust_domain = 'same_host')",
        "CHECK (trust_basis IN ('current_local_connection','historical_local_admission'))",
        "UNIQUE (envelope_id, envelope_version)",
        "UNIQUE (envelope_id, state)",
    ] {
        if !receipt_sql.contains(&normalize_schema_sql(clause)) {
            return Err(MemoryError::InvalidArg(format!(
                "{label}: receipt constraint drifted: {clause}"
            )));
        }
    }
    Ok(())
}

/// Rebuild the v31 Product A2A FK pair into the v32 nullable-body shape.
/// The caller owns the encompassing migration transaction, so either both
/// tables, all rows, indexes and the version sentinel move together or none do.
pub(crate) fn rebuild_a2a_mailbox_to_v32(conn: &Connection) -> Result<(), MemoryError> {
    validate_a2a_mailbox_v31_schema(conn)?;
    execute_batch_retry(
        conn,
        "DROP INDEX idx_a2a_envelopes_recipient_state;
         DROP INDEX idx_a2a_envelopes_issuer_created;
         DROP INDEX idx_a2a_receipts_envelope_version;
         ALTER TABLE a2a_delivery_receipts RENAME TO a2a_delivery_receipts_v31;
         ALTER TABLE a2a_envelopes RENAME TO a2a_envelopes_v31;",
    )?;
    execute_batch_retry(conn, ddl::A2A_MAILBOX_V32_SQL)?;
    execute_batch_retry(
        conn,
        "INSERT INTO a2a_envelopes
             (envelope_id,kind,issuer_agent_identity_id,issuer_admission_id,
              recipient_agent_identity_id,recipient_admission_id,subject_ref,body,
              body_digest,issuer_identity_assurance,recipient_identity_assurance,
              issuer_trust_domain,recipient_trust_domain,issuer_trust_basis,
              recipient_trust_basis,idempotency_key,created_at,expires_at,current_state,state_version)
         SELECT envelope_id,kind,issuer_agent_identity_id,issuer_admission_id,
                recipient_agent_identity_id,recipient_admission_id,subject_ref,body,
                body_digest,issuer_identity_assurance,recipient_identity_assurance,
                issuer_trust_domain,recipient_trust_domain,issuer_trust_basis,
                recipient_trust_basis,idempotency_key,created_at,expires_at,current_state,state_version
           FROM a2a_envelopes_v31;
         INSERT INTO a2a_delivery_receipts
             (receipt_id,envelope_id,envelope_version,state,actor_agent_identity_id,
              actor_admission_id,identity_assurance,trust_domain,trust_basis,occurred_at)
         SELECT receipt_id,envelope_id,envelope_version,state,actor_agent_identity_id,
                actor_admission_id,identity_assurance,trust_domain,trust_basis,occurred_at
           FROM a2a_delivery_receipts_v31;
         DROP TABLE a2a_delivery_receipts_v31;
         DROP TABLE a2a_envelopes_v31;",
    )?;
    validate_a2a_mailbox_schema_version(conn, 32)
}
