use chrono::Utc;
use memcore::MemoryStore;

use crate::server_state::{DbScope, MemoryServer};
use crate::utils::stable_hash;

const INGEST_CLAIM_LEASE_SECS: i64 = 5 * 60;

pub(crate) struct RetryableIngestClaim {
    owner_token: String,
}

pub(crate) fn ingest_audit_key(
    label: &str,
    target_db: DbScope,
    project: Option<&str>,
    event_hash: &str,
) -> String {
    let target = project.unwrap_or_else(|| target_db.as_str());
    stable_hash(&format!("{label}:{target}:{event_hash}"))
}

pub(crate) fn insert_required_ingest_audit(
    server: &MemoryServer,
    label: &str,
    audit_key: &str,
    success: bool,
    error_kind: Option<&str>,
) -> Result<(), String> {
    server.with_global_store(|store| {
        store
            .audit_log_insert(
                &Utc::now().to_rfc3339(),
                "ingest",
                label,
                audit_key,
                success,
                0,
                error_kind,
            )
            .map_err(|e| format!("required ingest audit insert: {e}"))
    })
}

pub(crate) fn fail_retryable_ingest_event(
    server: &MemoryServer,
    target_db: DbScope,
    project: Option<&str>,
    label: &str,
    event_hash: &str,
    worker: &str,
    claim: &RetryableIngestClaim,
    audit_key: &str,
    error_kind: &str,
    error: String,
) -> String {
    let audit_error =
        insert_required_ingest_audit(server, label, audit_key, false, Some(error_kind)).err();
    let release_error =
        release_retryable_ingest_claim(server, target_db, project, worker, event_hash, claim).err();

    match (audit_error, release_error) {
        (None, None) => error,
        (Some(audit_error), None) => format!("{error}; failed to record ingest failure: {audit_error}"),
        (None, Some(release_error)) => {
            format!("{error}; failed to release ingest claim for retry: {release_error}")
        }
        (Some(audit_error), Some(release_error)) => format!(
            "{error}; failed to record ingest failure: {audit_error}; failed to release ingest claim for retry: {release_error}"
        ),
    }
}

pub(crate) fn insert_ingest_skip_audit(
    server: &MemoryServer,
    label: &str,
    reason: &str,
    context: &str,
) -> Result<(), String> {
    let args_hash = stable_hash(&format!("{label}:{reason}:{context}"));
    server.with_global_store(|store| {
        store
            .audit_log_insert(
                &Utc::now().to_rfc3339(),
                "ingest",
                label,
                &args_hash,
                false,
                0,
                Some(reason),
            )
            .map_err(|e| format!("required ingest skip audit insert: {e}"))
    })
}

pub(crate) fn claim_retryable_ingest_event(
    server: &MemoryServer,
    target_db: DbScope,
    project: Option<&str>,
    label: &str,
    audit_key: &str,
    worker: &str,
    event_hash: &str,
    event_id: &str,
) -> Result<Option<RetryableIngestClaim>, String> {
    if ingest_success_audit_exists(server, label, audit_key, event_hash)? {
        return Ok(None);
    }

    let claim = RetryableIngestClaim {
        owner_token: format!("{event_id}:{}", uuid::Uuid::new_v4()),
    };
    let stale_modifier = format!("-{INGEST_CLAIM_LEASE_SECS} seconds");
    let action = |store: &mut MemoryStore| {
        store
            .connection()
            .execute(
                "INSERT INTO processed_events (event_hash, event_id, worker, created_at) \
                 VALUES (?1, ?2, ?3, STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now')) \
                 ON CONFLICT(event_hash, worker) DO UPDATE SET \
                     event_id = excluded.event_id, \
                     created_at = excluded.created_at \
                 WHERE processed_events.created_at = '' \
                    OR julianday(processed_events.created_at) IS NULL \
                    OR julianday(processed_events.created_at) <= julianday('now', ?4)",
                rusqlite::params![event_hash, claim.owner_token, worker, stale_modifier],
            )
            .map(|rows_changed| rows_changed == 1)
            .map_err(|error| format!("atomically claim ingest event: {error}"))
    };
    let claimed = if let Some(project_name) = project {
        server.with_named_project_store(project_name, action)?
    } else {
        server.with_store_for_scope(target_db, action)?
    };

    Ok(claimed.then_some(claim))
}

fn ingest_success_audit_exists(
    server: &MemoryServer,
    label: &str,
    audit_key: &str,
    legacy_event_hash: &str,
) -> Result<bool, String> {
    server.with_global_store_read(|store| {
        store
            .connection()
            .query_row(
                "SELECT EXISTS(\
                    SELECT 1 FROM audit_log \
                    WHERE server_id = 'ingest' AND tool_name = ?1 \
                      AND args_hash IN (?2, ?3) AND success = 1\
                )",
                rusqlite::params![label, audit_key, legacy_event_hash],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count != 0)
            .map_err(|e| format!("read ingest completion audit: {e}"))
    })
}

pub(crate) fn refresh_retryable_ingest_claim(
    server: &MemoryServer,
    target_db: DbScope,
    project: Option<&str>,
    worker: &str,
    event_hash: &str,
    claim: &RetryableIngestClaim,
) -> Result<(), String> {
    let action = |store: &mut MemoryStore| {
        store
            .connection()
            .execute(
                "UPDATE processed_events \
                 SET created_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now') \
                 WHERE event_hash = ?1 AND worker = ?2 AND event_id = ?3",
                rusqlite::params![event_hash, worker, claim.owner_token],
            )
            .map_err(|error| format!("refresh ingest claim: {error}"))
    };
    let rows_changed = if let Some(project_name) = project {
        server.with_named_project_store(project_name, action)?
    } else {
        server.with_store_for_scope(target_db, action)?
    };

    if rows_changed == 1 {
        Ok(())
    } else {
        Err("ingest claim ownership was lost before durable completion".to_string())
    }
}

pub(crate) fn release_retryable_ingest_claim(
    server: &MemoryServer,
    target_db: DbScope,
    project: Option<&str>,
    worker: &str,
    event_hash: &str,
    claim: &RetryableIngestClaim,
) -> Result<(), String> {
    let action = |store: &mut MemoryStore| {
        store
            .connection()
            .execute(
                "DELETE FROM processed_events \
                 WHERE event_hash = ?1 AND worker = ?2 AND event_id = ?3",
                rusqlite::params![event_hash, worker, claim.owner_token],
            )
            .map(|_| ())
            .map_err(|error| format!("release owned ingest claim: {error}"))
    };

    if let Some(project_name) = project {
        server.with_named_project_store(project_name, action)
    } else {
        server.with_store_for_scope(target_db, action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_owner_cannot_release_a_reclaimed_ingest_claim() {
        let server = crate::tests::make_server();
        let event_hash = "owner-token-event";
        let audit_key = ingest_audit_key("ingest_event", DbScope::Global, None, event_hash);

        let first = claim_retryable_ingest_event(
            &server,
            DbScope::Global,
            None,
            "ingest_event",
            &audit_key,
            "ingest",
            event_hash,
            "owner-a",
        )
        .expect("first claim")
        .expect("first claim must be acquired");
        server
            .with_global_store(|store| {
                store
                    .connection()
                    .execute(
                        "UPDATE processed_events \
                         SET created_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now', '-10 minutes') \
                         WHERE event_hash = ?1 AND worker = 'ingest'",
                        [event_hash],
                    )
                    .map_err(|error| format!("age first claim with database time: {error}"))?;
                Ok(())
            })
            .expect("age first claim");

        let second = claim_retryable_ingest_event(
            &server,
            DbScope::Global,
            None,
            "ingest_event",
            &audit_key,
            "ingest",
            event_hash,
            "owner-b",
        )
        .expect("stale claim takeover")
        .expect("stale claim must be acquired");

        release_retryable_ingest_claim(
            &server,
            DbScope::Global,
            None,
            "ingest",
            event_hash,
            &first,
        )
        .expect("old owner release attempt");

        let current_owner = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT event_id FROM processed_events WHERE event_hash = ?1 AND worker = 'ingest'",
                        [event_hash],
                        |row| row.get::<_, String>(0),
                    )
                    .map_err(|error| format!("read current claim owner: {error}"))
            })
            .expect("replacement claim must remain");
        assert_eq!(current_owner, second.owner_token);
    }

    #[test]
    fn legacy_success_audit_prevents_reprocessing_after_upgrade() {
        let server = crate::tests::make_server();
        let event_hash = "legacy-completed-event";
        server
            .with_global_store(|store| {
                store
                    .audit_log_insert(
                        &Utc::now().to_rfc3339(),
                        "ingest",
                        "ingest_source",
                        event_hash,
                        true,
                        0,
                        None,
                    )
                    .map_err(|error| format!("seed legacy success audit: {error}"))
            })
            .expect("seed pre-upgrade ingest completion");

        let audit_key = ingest_audit_key("ingest_source", DbScope::Global, None, event_hash);
        let claim = claim_retryable_ingest_event(
            &server,
            DbScope::Global,
            None,
            "ingest_source",
            &audit_key,
            "ingest_source",
            event_hash,
            "/legacy/source",
        )
        .expect("check legacy completion");
        assert!(claim.is_none(), "legacy success must remain deduplicated");
    }
}
