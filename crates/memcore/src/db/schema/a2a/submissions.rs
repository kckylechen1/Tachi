//! Product v46: origin intent is distinct from a destination's received task.
use super::*;

const TABLE: &str = r#"
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
    current_state TEXT NOT NULL CHECK (current_state IN ('pending','in_flight','unknown','refused','blocked')),
    state_version INTEGER NOT NULL CHECK (state_version > 0),
    attempts INTEGER NOT NULL CHECK (attempts BETWEEN 0 AND 3),
    attempt_token TEXT,
    attempt_until TEXT,
    last_error TEXT,
    retry_allowed INTEGER NOT NULL CHECK (retry_allowed IN (0,1)),
    remote_task_id TEXT,
    remote_receipt TEXT CHECK (remote_receipt IS NULL OR (json_valid(remote_receipt) AND length(CAST(remote_receipt AS BLOB)) <= 8192)),
    updated_at TEXT NOT NULL,
    UNIQUE (origin_agent_identity_id, idempotency_key),
    UNIQUE (origin_agent_identity_id, trust_domain, nonce),
    CHECK ((current_state = 'in_flight' AND attempt_token IS NOT NULL AND attempt_until IS NOT NULL AND attempts > 0) OR (current_state != 'in_flight' AND attempt_token IS NULL AND attempt_until IS NULL)),
    CHECK ((current_state = 'refused' AND remote_task_id IS NOT NULL AND remote_receipt IS NOT NULL) OR (current_state != 'refused' AND remote_task_id IS NULL AND remote_receipt IS NULL)),
    CHECK (current_state NOT IN ('pending','blocked') OR attempts = 0),
    CHECK (current_state != 'unknown' OR attempts > 0),
    FOREIGN KEY (origin_agent_identity_id) REFERENCES agent_identities(agent_identity_id),
    FOREIGN KEY (origin_admission_id) REFERENCES identity_admissions(admission_id)
)
"#;

pub(crate) fn migrate_v46_remote_submissions(conn: &Connection) -> Result<(), MemoryError> {
    remote::validate(conn)?;
    execute_batch_retry(conn, TABLE)?;
    validate(conn)
}

pub(super) fn validate(conn: &Connection) -> Result<(), MemoryError> {
    let actual: Option<String> = conn.query_row(
        "SELECT sql FROM main.sqlite_schema WHERE type='table' AND name='a2a_remote_submissions'",
        [], |row| row.get(0),
    ).optional()?;
    if actual.is_none_or(|sql| normalize_schema_sql(&sql) != normalize_schema_sql(TABLE)) {
        return Err(MemoryError::InvalidArg(
            "incomplete current schema v46: a2a_remote_submissions is missing or non-canonical"
                .into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::catalogue::test_support::with_prefix;
    use crate::db::schema::inventory::{query_rows, schema_inventory};
    use crate::db::DbOpenContext;
    use crate::MemoryStore;

    #[test]
    fn v45_upgrade_preserves_existing_rows_and_current_reopen_never_repairs_missing_outbox() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("origin.db");
        with_prefix(45, || {
            let now = jiff::Timestamp::now();
            let store = MemoryStore::open_with_context(
                path.to_str().unwrap(),
                &DbOpenContext::create_fresh(),
            )
            .unwrap();
            store.connection().execute("INSERT INTO agent_identities(agent_identity_id,created_at) VALUES('origin',?1)", [now.to_string()]).unwrap();
            crate::db::session_claims::record_unverified_admission(
                store.connection(),
                "existing-admission",
                "origin",
                "existing-connection",
                crate::db::session_claims::UnverifiedAdmissionState::SelfAsserted,
            )
            .unwrap();
            crate::db::a2a::insert_a2a_envelope(
                &mut Connection::open(&path).unwrap(),
                &crate::db::a2a::NewA2aEnvelope {
                    envelope_id: "existing-mailbox".into(),
                    issuer_agent_identity_id: "origin".into(),
                    issuer_connection_id: "existing-connection".into(),
                    recipient_agent_identity_id: "origin".into(),
                    subject_ref: "peer_publication:1940".into(),
                    body: "existing local response".into(),
                    idempotency_key: "existing-key".into(),
                    created_at: now.to_string(),
                    expires_at: (now + jiff::SignedDuration::from_mins(30)).to_string(),
                },
            )
            .unwrap();
        });
        let conn = Connection::open(&path).unwrap();
        let before: Vec<_> = [
            "agent_identities",
            "identity_admissions",
            "a2a_envelopes",
            "a2a_delivery_receipts",
        ]
        .into_iter()
        .map(|table| query_rows(&conn, &format!("SELECT * FROM {table}")))
        .collect();
        drop(conn);
        assert!(MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &DbOpenContext::open_existing_deny()
        )
        .is_err());
        let context = DbOpenContext::open_existing_allow("v46 migration fixture");
        let store = MemoryStore::open_with_context(path.to_str().unwrap(), &context).unwrap();
        for (table, expected) in [
            "agent_identities",
            "identity_admissions",
            "a2a_envelopes",
            "a2a_delivery_receipts",
        ]
        .into_iter()
        .zip(before)
        {
            assert_eq!(
                query_rows(store.connection(), &format!("SELECT * FROM {table}")),
                expected,
                "{table}"
            );
        }
        validate(store.connection()).unwrap();
        drop(store);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE a2a_remote_submissions")
            .unwrap();
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
