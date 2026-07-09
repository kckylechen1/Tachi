use chrono::Utc;
use memcore::MemoryStore;

use crate::server_state::{DbScope, MemoryServer};
use crate::shared_defs::{push_dead_letter_with_limits, DeadLetter};
use crate::utils::stable_hash;

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
