use chrono::Utc;
use memcore::MemoryStore;
use rusqlite::OptionalExtension;

use crate::server_state::{DbScope, MemoryServer};
use crate::shared_defs::{push_dead_letter_with_limits, DeadLetter};
use crate::utils::stable_hash;

const INGEST_CLAIM_LEASE_SECS: i64 = 5 * 60;

pub(crate) fn enqueue_dead_letter(
    server: &MemoryServer,
    tool_name: &str,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
    error: String,
) {
    let dl = DeadLetter {
        id: uuid::Uuid::new_v4().to_string(),
        tool_name: tool_name.to_string(),
        arguments,
        error,
        error_category: "internal".to_string(),
        timestamp: Utc::now().to_rfc3339(),
        retry_count: 0,
        max_retries: 3,
        status: "pending".to_string(),
    };
    let mut dlq = server.dead_letters_lock();
    push_dead_letter_with_limits(&mut dlq, dl, Utc::now());
}

pub(crate) fn insert_ingest_audit(server: &MemoryServer, label: &str, event_hash: &str) {
    if let Err(error) = server.with_global_store(|store| {
        store
            .audit_log_insert(
                &Utc::now().to_rfc3339(),
                "ingest",
                label,
                event_hash,
                true,
                0,
                None,
            )
            .map_err(|e| format!("audit insert: {e}"))
    }) {
        tracing::warn!(
            label,
            event_hash,
            error = %error,
            "failed to write ingest audit log"
        );
    }
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
    audit_key: &str,
    error_kind: &str,
    error: String,
) -> String {
    let audit_error =
        insert_required_ingest_audit(server, label, audit_key, false, Some(error_kind)).err();
    let release_error =
        release_retryable_ingest_claim(server, target_db, project, worker, event_hash).err();

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
) {
    let args_hash = stable_hash(&format!("{label}:{reason}:{context}"));
    if let Err(error) = server.with_global_store(|store| {
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
            .map_err(|e| format!("audit insert: {e}"))
    }) {
        tracing::warn!(
            label,
            reason,
            args_hash,
            error = %error,
            "failed to write skipped ingest audit log"
        );
    }
}

pub(crate) fn claim_ingest_event(
    server: &MemoryServer,
    target_db: DbScope,
    project: Option<&str>,
    worker: &str,
    event_hash: &str,
    event_id: &str,
) -> Result<bool, String> {
    let action = |store: &mut MemoryStore| {
        store
            .try_claim_event(event_hash, event_id, worker)
            .map_err(|e| format!("Failed to claim event: {e}"))
    };

    if let Some(project_name) = project {
        server.with_named_project_store(project_name, action)
    } else {
        server.with_store_for_scope(target_db, action)
    }
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
) -> Result<bool, String> {
    if ingest_success_audit_exists(server, label, audit_key)? {
        return Ok(false);
    }

    if try_claim_ingest_event(server, target_db, project, worker, event_hash, event_id)? {
        return Ok(true);
    }

    if !ingest_claim_is_expired(server, target_db, project, worker, event_hash)? {
        return Ok(false);
    }

    release_retryable_ingest_claim(server, target_db, project, worker, event_hash)?;
    try_claim_ingest_event(server, target_db, project, worker, event_hash, event_id)
}

fn ingest_success_audit_exists(
    server: &MemoryServer,
    label: &str,
    audit_key: &str,
) -> Result<bool, String> {
    server.with_global_store_read(|store| {
        store
            .connection()
            .query_row(
                "SELECT EXISTS(\
                    SELECT 1 FROM audit_log \
                    WHERE server_id = 'ingest' AND tool_name = ?1 AND args_hash = ?2 AND success = 1\
                )",
                rusqlite::params![label, audit_key],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count != 0)
            .map_err(|e| format!("read ingest completion audit: {e}"))
    })
}

fn try_claim_ingest_event(
    server: &MemoryServer,
    target_db: DbScope,
    project: Option<&str>,
    worker: &str,
    event_hash: &str,
    event_id: &str,
) -> Result<bool, String> {
    let action = |store: &mut MemoryStore| {
        store
            .try_claim_event(event_hash, event_id, worker)
            .map_err(|e| format!("Failed to claim event: {e}"))
    };

    if let Some(project_name) = project {
        server.with_named_project_store(project_name, action)
    } else {
        server.with_store_for_scope(target_db, action)
    }
}

fn ingest_claim_is_expired(
    server: &MemoryServer,
    target_db: DbScope,
    project: Option<&str>,
    worker: &str,
    event_hash: &str,
) -> Result<bool, String> {
    let cutoff = Utc::now() - chrono::Duration::seconds(INGEST_CLAIM_LEASE_SECS);
    let action = |store: &mut MemoryStore| {
        let created_at: Option<String> = store
            .connection()
            .query_row(
                "SELECT created_at FROM processed_events WHERE event_hash = ?1 AND worker = ?2",
                rusqlite::params![event_hash, worker],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("read ingest claim: {e}"))?;

        match created_at {
            Some(created_at) => chrono::DateTime::parse_from_rfc3339(&created_at)
                .map(|created_at| created_at.with_timezone(&Utc) <= cutoff)
                .map_err(|e| format!("parse ingest claim timestamp: {e}")),
            None => Ok(false),
        }
    };

    if let Some(project_name) = project {
        server.with_named_project_store(project_name, action)
    } else {
        server.with_store_for_scope(target_db, action)
    }
}

pub(crate) fn release_ingest_claim(
    server: &MemoryServer,
    target_db: DbScope,
    project: Option<&str>,
    worker: &str,
    event_hash: &str,
) {
    let action = |store: &mut MemoryStore| {
        store
            .release_event_claim(event_hash, worker)
            .map_err(|e| format!("{e}"))
    };

    let _ = if let Some(project_name) = project {
        server.with_named_project_store(project_name, action)
    } else {
        server.with_store_for_scope(target_db, action)
    };
}

pub(crate) fn release_retryable_ingest_claim(
    server: &MemoryServer,
    target_db: DbScope,
    project: Option<&str>,
    worker: &str,
    event_hash: &str,
) -> Result<(), String> {
    let action = |store: &mut MemoryStore| {
        store
            .release_event_claim(event_hash, worker)
            .map_err(|e| format!("release ingest claim: {e}"))
    };

    if let Some(project_name) = project {
        server.with_named_project_store(project_name, action)
    } else {
        server.with_store_for_scope(target_db, action)
    }
}
