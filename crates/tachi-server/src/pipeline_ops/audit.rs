use chrono::Utc;
use memcore::MemoryStore;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, watch, Mutex};
use tokio::task::JoinHandle;

use crate::server_state::{DbScope, MemoryServer};
use crate::utils::stable_hash;

const INGEST_CLAIM_LEASE_SECS: i64 = 5 * 60;
const INGEST_HEARTBEAT_INTERVAL: Duration =
    Duration::from_secs((INGEST_CLAIM_LEASE_SECS as u64) / 3);
const INGEST_HEARTBEAT_SQLITE_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub(crate) struct RetryableIngestClaim {
    owner_token: String,
}

pub(crate) struct RetryableIngestLease {
    server: MemoryServer,
    target_db: DbScope,
    project: Option<String>,
    worker: String,
    event_hash: String,
    claim: RetryableIngestClaim,
    gate: Arc<Mutex<()>>,
    heartbeat_failure: watch::Receiver<Option<String>>,
    stop_tx: Option<watch::Sender<bool>>,
    heartbeat: Option<JoinHandle<()>>,
    join_notify: Option<oneshot::Sender<()>>,
}

struct HeartbeatOptions {
    interval: Duration,
    join_notify: Option<oneshot::Sender<()>>,
    refresh_notify: Option<oneshot::Sender<()>>,
    refresh_complete_notify: Option<oneshot::Sender<()>>,
}

impl RetryableIngestLease {
    pub(crate) fn start(
        server: &MemoryServer,
        target_db: DbScope,
        project: Option<&str>,
        worker: &str,
        event_hash: &str,
        claim: RetryableIngestClaim,
    ) -> Self {
        Self::start_with_interval(
            server,
            target_db,
            project,
            worker,
            event_hash,
            claim,
            INGEST_HEARTBEAT_INTERVAL,
        )
    }

    fn start_with_interval(
        server: &MemoryServer,
        target_db: DbScope,
        project: Option<&str>,
        worker: &str,
        event_hash: &str,
        claim: RetryableIngestClaim,
        interval: Duration,
    ) -> Self {
        Self::start_inner(
            server,
            target_db,
            project,
            worker,
            event_hash,
            claim,
            HeartbeatOptions {
                interval,
                join_notify: None,
                refresh_notify: None,
                refresh_complete_notify: None,
            },
        )
    }

    #[cfg(test)]
    fn start_with_join_receipt(
        server: &MemoryServer,
        target_db: DbScope,
        project: Option<&str>,
        worker: &str,
        event_hash: &str,
        claim: RetryableIngestClaim,
        interval: Duration,
    ) -> (Self, oneshot::Receiver<()>) {
        let (join_notify, joined) = oneshot::channel();
        (
            Self::start_inner(
                server,
                target_db,
                project,
                worker,
                event_hash,
                claim,
                HeartbeatOptions {
                    interval,
                    join_notify: Some(join_notify),
                    refresh_notify: None,
                    refresh_complete_notify: None,
                },
            ),
            joined,
        )
    }

    #[cfg(test)]
    fn start_with_heartbeat_receipts(
        server: &MemoryServer,
        target_db: DbScope,
        project: Option<&str>,
        worker: &str,
        event_hash: &str,
        claim: RetryableIngestClaim,
        interval: Duration,
    ) -> (
        Self,
        oneshot::Receiver<()>,
        oneshot::Receiver<()>,
        oneshot::Receiver<()>,
    ) {
        let (join_notify, joined) = oneshot::channel();
        let (refresh_notify, refresh_started) = oneshot::channel();
        let (refresh_complete_notify, refresh_completed) = oneshot::channel();
        (
            Self::start_inner(
                server,
                target_db,
                project,
                worker,
                event_hash,
                claim,
                HeartbeatOptions {
                    interval,
                    join_notify: Some(join_notify),
                    refresh_notify: Some(refresh_notify),
                    refresh_complete_notify: Some(refresh_complete_notify),
                },
            ),
            joined,
            refresh_started,
            refresh_completed,
        )
    }

    fn start_inner(
        server: &MemoryServer,
        target_db: DbScope,
        project: Option<&str>,
        worker: &str,
        event_hash: &str,
        claim: RetryableIngestClaim,
        options: HeartbeatOptions,
    ) -> Self {
        let HeartbeatOptions {
            interval,
            join_notify,
            mut refresh_notify,
            mut refresh_complete_notify,
        } = options;
        let gate = Arc::new(Mutex::new(()));
        let (failure_tx, heartbeat_failure) = watch::channel(None);
        let (stop_tx, mut stop_rx) = watch::channel(false);
        let heartbeat_server = server.clone();
        let heartbeat_project = project.map(str::to_string);
        let heartbeat_worker = worker.to_string();
        let heartbeat_hash = event_hash.to_string();
        let heartbeat_claim = claim.clone();
        let heartbeat_gate = Arc::clone(&gate);
        let heartbeat = tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed = stop_rx.changed() => {
                        if changed.is_err() || *stop_rx.borrow() {
                            return;
                        }
                    }
                    _ = tokio::time::sleep(interval) => {}
                }

                let guard = tokio::select! {
                    changed = stop_rx.changed() => {
                        if changed.is_err() || *stop_rx.borrow() {
                            return;
                        }
                        continue;
                    }
                    guard = heartbeat_gate.lock() => guard,
                };
                if *stop_rx.borrow() {
                    return;
                }
                if let Some(refresh_notify) = refresh_notify.take() {
                    let _ = refresh_notify.send(());
                }
                let refresh_path = ingest_claim_db_path(
                    &heartbeat_server,
                    target_db,
                    heartbeat_project.as_deref(),
                );
                let refresh_worker = heartbeat_worker.clone();
                let refresh_hash = heartbeat_hash.clone();
                let refresh_claim = heartbeat_claim.clone();
                let refresh = match refresh_path {
                    Ok(path) => tokio::task::spawn_blocking(move || {
                        refresh_retryable_ingest_claim_at_path(
                            &path,
                            &refresh_worker,
                            &refresh_hash,
                            &refresh_claim,
                        )
                    })
                    .await
                    .map_err(|error| format!("join bounded ingest heartbeat refresh: {error}"))
                    .and_then(|result| result),
                    Err(error) => Err(error),
                };
                if let Some(refresh_complete_notify) = refresh_complete_notify.take() {
                    let _ = refresh_complete_notify.send(());
                }
                if let Err(error) = refresh {
                    let _ =
                        failure_tx.send(Some(format!("ingest claim heartbeat failed: {error}")));
                    drop(guard);
                    return;
                }
                drop(guard);
            }
        });

        Self {
            server: server.clone(),
            target_db,
            project: project.map(str::to_string),
            worker: worker.to_string(),
            event_hash: event_hash.to_string(),
            claim,
            gate,
            heartbeat_failure,
            stop_tx: Some(stop_tx),
            heartbeat: Some(heartbeat),
            join_notify,
        }
    }

    fn check_heartbeat(&self) -> Result<(), String> {
        match self.heartbeat_failure.borrow().clone() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub(crate) async fn ensure_owned(&self) -> Result<(), String> {
        let _guard = self.gate.lock().await;
        self.check_heartbeat()?;
        refresh_retryable_ingest_claim(
            &self.server,
            self.target_db,
            self.project.as_deref(),
            &self.worker,
            &self.event_hash,
            &self.claim,
        )
    }

    pub(crate) async fn write_owned<T, F>(&self, action: F) -> Result<T, String>
    where
        T: Send,
        F: FnOnce(&mut MemoryStore) -> Result<T, String> + Send,
    {
        let _guard = self.gate.lock().await;
        self.check_heartbeat()?;
        let fenced_action = |store: &mut MemoryStore| {
            owner_fenced_write(store, &self.worker, &self.event_hash, &self.claim, action)
        };
        if let Some(project_name) = self.project.as_deref() {
            self.server
                .with_named_project_store(project_name, fenced_action)
        } else {
            self.server
                .with_store_for_scope(self.target_db, fenced_action)
        }
    }

    pub(crate) async fn write_idempotent<T, F>(&self, action: F) -> Result<T, String>
    where
        T: Send,
        F: FnOnce(&mut MemoryStore) -> Result<T, String> + Send,
    {
        let _guard = self.gate.lock().await;
        self.check_heartbeat()?;
        let fenced_action = |store: &mut MemoryStore| {
            owner_fenced_stable_write(store, &self.worker, &self.event_hash, &self.claim, action)
        };
        if let Some(project_name) = self.project.as_deref() {
            self.server
                .with_named_project_store(project_name, fenced_action)
        } else {
            self.server
                .with_store_for_scope(self.target_db, fenced_action)
        }
    }

    pub(crate) async fn complete(mut self, label: &str, audit_key: &str) -> Result<(), String> {
        let gate = Arc::clone(&self.gate);
        let completion = {
            let _guard = gate.lock().await;
            let ownership = self.check_heartbeat().and_then(|_| {
                refresh_retryable_ingest_claim(
                    &self.server,
                    self.target_db,
                    self.project.as_deref(),
                    &self.worker,
                    &self.event_hash,
                    &self.claim,
                )
            });
            if let Err(error) = ownership {
                Err(("claim_ownership_lost", error))
            } else if let Err(error) =
                insert_required_ingest_audit(&self.server, label, audit_key, true, None)
            {
                Err((
                    "success_audit_failed",
                    format!("ingest writes completed but success audit failed: {error}"),
                ))
            } else {
                self.signal_stop();
                Ok(())
            }
        };

        match completion {
            Ok(()) => self.join_heartbeat().await,
            Err((error_kind, error)) => Err(self.fail(label, audit_key, error_kind, error).await),
        }
    }

    pub(crate) async fn finish(mut self) -> Result<(), String> {
        let gate = Arc::clone(&self.gate);
        {
            let _guard = gate.lock().await;
            self.signal_stop();
        }
        self.join_heartbeat().await
    }

    pub(crate) async fn fail(
        mut self,
        label: &str,
        audit_key: &str,
        error_kind: &str,
        error: String,
    ) -> String {
        let gate = Arc::clone(&self.gate);
        let (heartbeat_error, audit_error, release_error) = {
            let _guard = gate.lock().await;
            let heartbeat_error = self.check_heartbeat().err();
            let audit_error = insert_required_ingest_audit(
                &self.server,
                label,
                audit_key,
                false,
                Some(error_kind),
            )
            .err();
            let release_error = release_retryable_ingest_claim(
                &self.server,
                self.target_db,
                self.project.as_deref(),
                &self.worker,
                &self.event_hash,
                &self.claim,
            )
            .err();
            self.signal_stop();
            (heartbeat_error, audit_error, release_error)
        };
        let join_error = self.join_heartbeat().await.err();

        let mut failures = Vec::new();
        if let Some(heartbeat_error) = heartbeat_error {
            if !error.contains(&heartbeat_error) {
                failures.push(format!("heartbeat failure: {heartbeat_error}"));
            }
        }
        if let Some(audit_error) = audit_error {
            failures.push(format!("failed to record ingest failure: {audit_error}"));
        }
        if let Some(release_error) = release_error {
            failures.push(format!(
                "failed to release ingest claim for retry: {release_error}"
            ));
        }
        if let Some(join_error) = join_error {
            failures.push(join_error);
        }
        if failures.is_empty() {
            error
        } else {
            format!("{error}; {}", failures.join("; "))
        }
    }

    fn signal_stop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(true);
        }
    }

    async fn join_heartbeat(&mut self) -> Result<(), String> {
        let Some(heartbeat) = self.heartbeat.take() else {
            self.notify_joined();
            return Ok(());
        };
        let result = heartbeat
            .await
            .map_err(|error| format!("join ingest claim heartbeat: {error}"));
        self.notify_joined();
        result
    }

    fn notify_joined(&mut self) {
        if let Some(join_notify) = self.join_notify.take() {
            let _ = join_notify.send(());
        }
    }
}

impl Drop for RetryableIngestLease {
    fn drop(&mut self) {
        self.signal_stop();
        if let Some(heartbeat) = self.heartbeat.take() {
            let join_notify = self.join_notify.take();
            if let Err(error) = std::thread::Builder::new()
                .name("ingest-heartbeat-join".to_string())
                .spawn(move || {
                    let _ = futures::executor::block_on(heartbeat);
                    if let Some(join_notify) = join_notify {
                        let _ = join_notify.send(());
                    }
                })
            {
                tracing::error!(%error, "failed to start ingest heartbeat joiner");
            }
        } else {
            self.notify_joined();
        }
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

pub(crate) fn ingest_success_audit_exists(
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

fn ingest_claim_db_path(
    server: &MemoryServer,
    target_db: DbScope,
    project: Option<&str>,
) -> Result<PathBuf, String> {
    if let Some(project_name) = project {
        return server
            .resolve_server_named_project_db_path(project_name)
            .map_err(|error| format!("resolve ingest heartbeat project database: {error}"));
    }
    match target_db {
        DbScope::Global => Ok(server.global_db_path_buf()),
        DbScope::Project => server
            .project_db_path_buf()
            .ok_or_else(|| "ingest heartbeat project database is unavailable".to_string()),
    }
}

fn refresh_retryable_ingest_claim_at_path(
    db_path: &std::path::Path,
    worker: &str,
    event_hash: &str,
    claim: &RetryableIngestClaim,
) -> Result<(), String> {
    let connection = rusqlite::Connection::open(db_path)
        .map_err(|error| format!("open ingest heartbeat database: {error}"))?;
    connection
        .busy_timeout(INGEST_HEARTBEAT_SQLITE_TIMEOUT)
        .map_err(|error| format!("bound ingest heartbeat SQLite wait: {error}"))?;
    let rows_changed = connection
        .execute(
            "UPDATE processed_events \
             SET created_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE event_hash = ?1 AND worker = ?2 AND event_id = ?3",
            rusqlite::params![event_hash, worker, claim.owner_token],
        )
        .map_err(|error| format!("refresh ingest claim: {error}"))?;
    if rows_changed == 1 {
        Ok(())
    } else {
        Err("ingest claim ownership was lost before durable completion".to_string())
    }
}

fn owner_fenced_write<T, F>(
    store: &mut MemoryStore,
    worker: &str,
    event_hash: &str,
    claim: &RetryableIngestClaim,
    action: F,
) -> Result<T, String>
where
    F: FnOnce(&mut MemoryStore) -> Result<T, String>,
{
    store
        .connection()
        .execute_batch("SAVEPOINT ingest_owner_fence")
        .map_err(|error| format!("begin ingest owner fence: {error}"))?;

    let owned = store.connection().execute(
        "UPDATE processed_events \
         SET created_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE event_hash = ?1 AND worker = ?2 AND event_id = ?3",
        rusqlite::params![event_hash, worker, claim.owner_token],
    );
    match owned {
        Ok(1) => {}
        Ok(_) => {
            return Err(rollback_owner_fence(
                store,
                "ingest claim ownership was lost before durable write".to_string(),
            ));
        }
        Err(error) => {
            return Err(rollback_owner_fence(
                store,
                format!("refresh ingest claim inside owner fence: {error}"),
            ));
        }
    }

    let value = match action(store) {
        Ok(value) => value,
        Err(error) => return Err(rollback_owner_fence(store, error)),
    };
    if let Err(error) = store
        .connection()
        .execute_batch("RELEASE ingest_owner_fence")
    {
        return Err(rollback_owner_fence(
            store,
            format!("commit ingest owner-fenced write: {error}"),
        ));
    }
    Ok(value)
}

fn owner_fenced_stable_write<T, F>(
    store: &mut MemoryStore,
    worker: &str,
    event_hash: &str,
    claim: &RetryableIngestClaim,
    action: F,
) -> Result<T, String>
where
    F: FnOnce(&mut MemoryStore) -> Result<T, String>,
{
    let rows_changed = store
        .connection()
        .execute(
            "UPDATE processed_events \
             SET created_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE event_hash = ?1 AND worker = ?2 AND event_id = ?3",
            rusqlite::params![event_hash, worker, claim.owner_token],
        )
        .map_err(|error| format!("refresh ingest claim before stable row fence: {error}"))?;
    if rows_changed != 1 {
        return Err("ingest claim ownership was lost before stable row write".to_string());
    }

    store.with_ingest_stable_owner_fence(event_hash, worker, &claim.owner_token, action)
}

fn rollback_owner_fence(store: &mut MemoryStore, error: String) -> String {
    match store
        .connection()
        .execute_batch("ROLLBACK TO ingest_owner_fence; RELEASE ingest_owner_fence")
    {
        Ok(()) => error,
        Err(rollback_error) => {
            format!("{error}; rollback ingest owner-fenced write: {rollback_error}")
        }
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

    fn with_offline_global_fixture_connection<T>(
        server: &MemoryServer,
        action: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>,
    ) -> Result<T, String> {
        let connection = rusqlite::Connection::open(server.global_db_path_buf())
            .map_err(|error| format!("open offline pipeline audit fixture: {error}"))?;
        action(&connection).map_err(|error| format!("run offline pipeline audit fixture: {error}"))
    }

    fn graph_entry(id: &str, text: &str) -> memcore::MemoryEntry {
        memcore::MemoryEntry {
            id: id.to_string(),
            path: format!("/wiki/ingest-lease/{id}"),
            summary: text.to_string(),
            text: text.to_string(),
            importance: 0.8,
            timestamp: Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: "ingest lease".to_string(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            source: "test".to_string(),
            scope: "global".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata: serde_json::json!({}),
            vector: None,
            retention_policy: None,
            domain: Some("general".to_string()),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

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

    #[tokio::test(start_paused = true)]
    async fn heartbeat_renews_database_time_lease_before_stale_takeover() {
        let server = crate::tests::make_server();
        let event_hash = "heartbeat-renewal-event";
        let audit_key = ingest_audit_key("ingest_source", DbScope::Global, None, event_hash);
        let claim = claim_retryable_ingest_event(
            &server,
            DbScope::Global,
            None,
            "ingest_source",
            &audit_key,
            "ingest_source",
            event_hash,
            "owner-a",
        )
        .expect("claim A")
        .expect("A owns claim");
        let (lease, _joined, _refresh_started, refresh_completed) =
            RetryableIngestLease::start_with_heartbeat_receipts(
                &server,
                DbScope::Global,
                None,
                "ingest_source",
                event_hash,
                claim,
                std::time::Duration::from_secs(1),
            );
        tokio::task::yield_now().await;

        server
            .with_global_store(|store| {
                store
                    .connection()
                    .execute(
                        "UPDATE processed_events \
                         SET created_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now', '-10 minutes') \
                         WHERE event_hash = ?1 AND worker = 'ingest_source'",
                        [event_hash],
                    )
                    .map(|_| ())
                    .map_err(|error| format!("age active claim: {error}"))
            })
            .expect("age active claim");
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        refresh_completed
            .await
            .expect("database-time heartbeat refresh completes");

        let takeover = claim_retryable_ingest_event(
            &server,
            DbScope::Global,
            None,
            "ingest_source",
            &audit_key,
            "ingest_source",
            event_hash,
            "owner-b",
        )
        .expect("attempt takeover");
        assert!(
            takeover.is_none(),
            "a renewed live lease must not be reclaimed"
        );

        let error = lease
            .fail(
                "ingest_source",
                &audit_key,
                "test_cleanup",
                "cleanup".to_string(),
            )
            .await;
        assert_eq!(error, "cleanup");
    }

    #[tokio::test(start_paused = true)]
    // The process-global TACHI_HOME override must remain serialized through
    // the awaited heartbeat lifecycle.
    #[allow(clippy::await_holding_lock)]
    async fn async_heartbeat_uses_server_home_after_environment_drift() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let project_root = crate::utils::find_project_git_root().expect("test project root");
        let project_name = crate::path_utils::plan_c_dir_name_from_root(&project_root)
            .expect("test project identity");
        let (server, _project_db) = crate::tests::make_server_with_project_fixture(&project_name);
        let event_hash = "heartbeat-environment-drift";
        let audit_key = ingest_audit_key(
            "ingest_source",
            DbScope::Project,
            Some(&project_name),
            event_hash,
        );
        let claim = claim_retryable_ingest_event(
            &server,
            DbScope::Project,
            Some(&project_name),
            "ingest_source",
            &audit_key,
            "ingest_source",
            event_hash,
            "owner-a",
        )
        .expect("claim fixture project")
        .expect("fixture claim acquired");

        let ambient_home = tempfile::tempdir().expect("ambient home");
        let ambient_db = ambient_home.path().join("ambient-project.db");
        MemoryStore::open(ambient_db.to_str().expect("utf8 ambient DB")).expect("ambient DB");
        crate::manifest::Manifest {
            schema_version: 1,
            generated_at: Utc::now().to_rfc3339(),
            comment: String::new(),
            dbs: vec![crate::manifest::DbEntry {
                path: ambient_db.display().to_string(),
                role: crate::manifest::DbRole::Project,
                owner: "test".to_string(),
                schema_kind: "tachi".to_string(),
                vec_enabled: true,
                allow_write: true,
                last_doctor_at: Utc::now().to_rfc3339(),
                last_classification: "healthy".to_string(),
                scope_hint: format!("project:{project_name}"),
                notes: String::new(),
            }],
        }
        .save(&ambient_home.path().join("manifest.json"))
        .expect("ambient manifest");
        let _ambient = crate::test_support::EnvRestore::set_path("TACHI_HOME", ambient_home.path());

        let (lease, _joined, _refresh_started, refresh_completed) =
            RetryableIngestLease::start_with_heartbeat_receipts(
                &server,
                DbScope::Project,
                Some(&project_name),
                "ingest_source",
                event_hash,
                claim,
                std::time::Duration::from_secs(1),
            );
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        refresh_completed
            .await
            .expect("heartbeat refresh completes");
        lease
            .ensure_owned()
            .await
            .expect("heartbeat remains bound to fixture project");
        lease.finish().await.expect("stop heartbeat");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stale_owner_cannot_commit_idempotent_row_after_takeover() {
        let temp = tempfile::tempdir().expect("temp stable-row fence database");
        let db_path = temp.path().join("memory.db");
        let server_a = MemoryServer::new(db_path.clone(), None).expect("open owner A database");
        let server_b = MemoryServer::new(db_path, None).expect("open owner B database");
        let event_hash = "stable-row-takeover-event";
        let audit_key = ingest_audit_key("ingest_source", DbScope::Global, None, event_hash);
        let claim_a = claim_retryable_ingest_event(
            &server_a,
            DbScope::Global,
            None,
            "ingest_source",
            &audit_key,
            "ingest_source",
            event_hash,
            "owner-a",
        )
        .expect("claim A")
        .expect("A owns claim");
        let lease_a = RetryableIngestLease::start_with_interval(
            &server_a,
            DbScope::Global,
            None,
            "ingest_source",
            event_hash,
            claim_a,
            Duration::from_secs(60),
        );
        let mut entry = graph_entry("stable-row-after-takeover", "stale row must not commit");
        entry.path = "/ingest-lease/stable-row-after-takeover".to_string();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let owner_a = tokio::spawn(async move {
            let write = lease_a
                .write_idempotent(move |store| {
                    started_tx.send(()).expect("signal stalled row write");
                    resume_rx.recv().expect("resume stalled row write");
                    let first_error = store
                        .insert_if_absent(&entry)
                        .expect_err("stale owner insert must hit the owner fence");
                    assert!(
                        first_error
                            .to_string()
                            .contains("ingest claim ownership lost before stable row write"),
                        "stale owner insert failed for the wrong reason: {first_error}"
                    );

                    let tamper_error = store
                        .connection()
                        .execute(
                            "UPDATE temp.ingest_owner_fence_context \
                             SET owner_token = 'owner-b'",
                            [],
                        )
                        .expect_err("raw context tamper must be denied");
                    assert!(
                        matches!(
                            &tamper_error,
                            rusqlite::Error::SqliteFailure(error, _)
                                if error.extended_code == rusqlite::ffi::SQLITE_AUTH
                        ),
                        "raw context tamper did not return SQLITE_AUTH: {tamper_error:?}"
                    );

                    store
                        .insert_if_absent(&entry)
                        .map_err(|error| format!("stale row insert after denied tamper: {error}"))
                })
                .await;
            (write, lease_a)
        });
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("A reaches the pre-insert stall");

        server_b
            .with_global_store(|store| {
                store
                    .connection()
                    .execute(
                        "UPDATE processed_events \
                         SET created_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now', '-10 minutes') \
                         WHERE event_hash = ?1 AND worker = 'ingest_source'",
                        [event_hash],
                    )
                    .map(|_| ())
                    .map_err(|error| format!("age stalled A claim: {error}"))
            })
            .expect("age stalled A claim");
        let claim_b = claim_retryable_ingest_event(
            &server_b,
            DbScope::Global,
            None,
            "ingest_source",
            &audit_key,
            "ingest_source",
            event_hash,
            "owner-b",
        )
        .expect("claim B")
        .expect("B takes stalled claim");
        let lease_b = RetryableIngestLease::start_with_interval(
            &server_b,
            DbScope::Global,
            None,
            "ingest_source",
            event_hash,
            claim_b,
            Duration::from_secs(60),
        );

        resume_tx.send(()).expect("resume A after takeover");
        let (stale_write, lease_a) = owner_a.await.expect("join stalled owner A");
        let stale_error = stale_write.expect_err("stale A must not commit after B takeover");
        assert!(
            stale_error.contains("ingest claim ownership lost before stable row write"),
            "post-tamper stale insert failed for the wrong reason: {stale_error}"
        );
        let _ = lease_a
            .fail(
                "ingest_source",
                &audit_key,
                "claim_ownership_lost",
                stale_error,
            )
            .await;

        let mut current_entry = graph_entry("stable-row-after-takeover", "current owner row");
        current_entry.path = "/ingest-lease/stable-row-after-takeover".to_string();
        lease_b
            .write_idempotent(|store| {
                store
                    .insert_if_absent(&current_entry)
                    .map_err(|error| format!("current row insert: {error}"))
            })
            .await
            .expect("B commits stable row");
        lease_b
            .complete("ingest_source", &audit_key)
            .await
            .expect("B completes ingest");

        let rows = server_b
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM memories WHERE id = 'stable-row-after-takeover' \
                         AND text = 'current owner row'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|error| format!("count current owner row: {error}"))
            })
            .expect("count current owner row");
        assert_eq!(rows, 1, "only B's stable row may commit");
    }

    #[tokio::test]
    async fn heartbeat_is_joined_after_error_panic_and_cancellation() {
        let server = crate::tests::make_server();

        let make_claim = |event_hash: &str| {
            let audit_key = ingest_audit_key("ingest_source", DbScope::Global, None, event_hash);
            let claim = claim_retryable_ingest_event(
                &server,
                DbScope::Global,
                None,
                "ingest_source",
                &audit_key,
                "ingest_source",
                event_hash,
                event_hash,
            )
            .expect("create lifecycle claim")
            .expect("lifecycle claim is owned");
            (audit_key, claim)
        };

        let (error_audit_key, error_claim) = make_claim("heartbeat-error-exit");
        let (error_lease, error_joined) = RetryableIngestLease::start_with_join_receipt(
            &server,
            DbScope::Global,
            None,
            "ingest_source",
            "heartbeat-error-exit",
            error_claim,
            Duration::from_secs(60),
        );
        let error = error_lease
            .fail(
                "ingest_source",
                &error_audit_key,
                "injected_error",
                "injected error".to_string(),
            )
            .await;
        assert_eq!(error, "injected error");
        tokio::time::timeout(Duration::from_secs(2), error_joined)
            .await
            .expect("error exit joins heartbeat in time")
            .expect("error exit join receipt");

        let (_, panic_claim) = make_claim("heartbeat-panic-exit");
        let (panic_lease, panic_joined) = RetryableIngestLease::start_with_join_receipt(
            &server,
            DbScope::Global,
            None,
            "ingest_source",
            "heartbeat-panic-exit",
            panic_claim,
            Duration::from_secs(60),
        );
        let panic_task = tokio::spawn(async move {
            let _lease = panic_lease;
            panic!("injected ingest owner panic");
        });
        assert!(
            panic_task.await.is_err(),
            "panic task must report its panic"
        );
        tokio::time::timeout(Duration::from_secs(2), panic_joined)
            .await
            .expect("panic exit joins heartbeat in time")
            .expect("panic exit join receipt");

        let (_, cancel_claim) = make_claim("heartbeat-cancel-exit");
        let (cancel_lease, cancel_joined) = RetryableIngestLease::start_with_join_receipt(
            &server,
            DbScope::Global,
            None,
            "ingest_source",
            "heartbeat-cancel-exit",
            cancel_claim,
            Duration::from_secs(60),
        );
        let cancel_task = tokio::spawn(async move {
            let _lease = cancel_lease;
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        cancel_task.abort();
        assert!(
            cancel_task
                .await
                .expect_err("task is cancelled")
                .is_cancelled(),
            "cancelled task must report cancellation"
        );
        tokio::time::timeout(Duration::from_secs(2), cancel_joined)
            .await
            .expect("cancel exit joins heartbeat in time")
            .expect("cancel exit join receipt");
    }

    #[tokio::test]
    async fn contended_heartbeat_refresh_stops_with_bounded_join_and_no_orphan() {
        let server = crate::tests::make_server();
        let event_hash = "contended-heartbeat-exit";
        let audit_key = ingest_audit_key("ingest_source", DbScope::Global, None, event_hash);
        let claim = claim_retryable_ingest_event(
            &server,
            DbScope::Global,
            None,
            "ingest_source",
            &audit_key,
            "ingest_source",
            event_hash,
            event_hash,
        )
        .expect("create contended heartbeat claim")
        .expect("contended heartbeat claim is owned");
        let created_at = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT created_at FROM processed_events WHERE event_hash = ?1 AND worker = 'ingest_source'",
                        [event_hash],
                        |row| row.get::<_, String>(0),
                    )
                    .map_err(|error| error.to_string())
            })
            .expect("read initial claim timestamp");
        let locker = rusqlite::Connection::open(server.global_db_path_buf())
            .expect("open independent lock connection");
        locker
            .execute_batch("BEGIN IMMEDIATE")
            .expect("hold SQLite writer lock");

        let (lease, joined, refresh_started, _refresh_completed) =
            RetryableIngestLease::start_with_heartbeat_receipts(
                &server,
                DbScope::Global,
                None,
                "ingest_source",
                event_hash,
                claim,
                Duration::from_millis(1),
            );
        tokio::time::timeout(Duration::from_secs(1), refresh_started)
            .await
            .expect("heartbeat reaches contended refresh")
            .expect("refresh-start receipt");
        drop(lease);
        tokio::time::timeout(Duration::from_secs(1), joined)
            .await
            .expect("contended heartbeat drop reaches terminal join")
            .expect("contended heartbeat join receipt");
        locker
            .execute_batch("ROLLBACK")
            .expect("release writer lock");
        tokio::time::sleep(Duration::from_millis(300)).await;

        let after = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT created_at FROM processed_events WHERE event_hash = ?1 AND worker = 'ingest_source'",
                        [event_hash],
                        |row| row.get::<_, String>(0),
                    )
                    .map_err(|error| error.to_string())
            })
            .expect("read claim timestamp after heartbeat join");
        assert_eq!(
            after, created_at,
            "joined heartbeat must not refresh after drop"
        );
    }

    #[test]
    fn runtime_shutdown_joins_contended_heartbeat_without_orphan_refresh() {
        let server = crate::tests::make_server();
        let event_hash = "runtime-shutdown-heartbeat-exit";
        let audit_key = ingest_audit_key("ingest_source", DbScope::Global, None, event_hash);
        let claim = claim_retryable_ingest_event(
            &server,
            DbScope::Global,
            None,
            "ingest_source",
            &audit_key,
            "ingest_source",
            event_hash,
            event_hash,
        )
        .expect("create runtime-shutdown heartbeat claim")
        .expect("runtime-shutdown heartbeat claim is owned");
        let created_at = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT created_at FROM processed_events WHERE event_hash = ?1 AND worker = 'ingest_source'",
                        [event_hash],
                        |row| row.get::<_, String>(0),
                    )
                    .map_err(|error| error.to_string())
            })
            .expect("read runtime-shutdown claim timestamp");
        let locker = rusqlite::Connection::open(server.global_db_path_buf())
            .expect("open runtime-shutdown lock connection");
        locker
            .execute_batch("BEGIN IMMEDIATE")
            .expect("hold runtime-shutdown SQLite writer lock");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build heartbeat shutdown runtime");
        let (lease, joined, refresh_started, _refresh_completed) = runtime.block_on(async {
            RetryableIngestLease::start_with_heartbeat_receipts(
                &server,
                DbScope::Global,
                None,
                "ingest_source",
                event_hash,
                claim,
                Duration::from_millis(1),
            )
        });
        let lease_task = runtime.spawn(async move {
            let _lease = lease;
            std::future::pending::<()>().await;
        });
        drop(lease_task);
        runtime
            .block_on(async { tokio::time::timeout(Duration::from_secs(1), refresh_started).await })
            .expect("heartbeat reaches contended refresh before runtime shutdown")
            .expect("runtime-shutdown refresh-start receipt");

        drop(runtime);
        joined
            .blocking_recv()
            .expect("runtime shutdown reaches terminal heartbeat join");
        locker
            .execute_batch("ROLLBACK")
            .expect("release runtime-shutdown writer lock");
        std::thread::sleep(Duration::from_millis(300));
        let after = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT created_at FROM processed_events WHERE event_hash = ?1 AND worker = 'ingest_source'",
                        [event_hash],
                        |row| row.get::<_, String>(0),
                    )
                    .map_err(|error| error.to_string())
            })
            .expect("read claim timestamp after runtime shutdown");
        assert_eq!(
            after, created_at,
            "runtime shutdown must leave no heartbeat refresh behind"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lost_heartbeat_fences_stale_graph_observation_after_takeover() {
        let server = crate::tests::make_server();
        let source = graph_entry(
            "lease-source",
            "durable ingest heartbeat owner fencing graph observation",
        );
        let target = graph_entry(
            "lease-target",
            "durable ingest heartbeat owner fencing graph observation reference",
        );
        server
            .with_global_store(|store| {
                store.upsert(&source).map_err(|error| error.to_string())?;
                store.upsert(&target).map_err(|error| error.to_string())
            })
            .expect("seed graph entries");

        let event_hash = "heartbeat-loss-event";
        let audit_key = ingest_audit_key("ingest_source", DbScope::Global, None, event_hash);
        let claim_a = claim_retryable_ingest_event(
            &server,
            DbScope::Global,
            None,
            "ingest_source",
            &audit_key,
            "ingest_source",
            event_hash,
            "owner-a",
        )
        .expect("claim A")
        .expect("A owns claim");
        let (lease_a, _joined, _refresh_started, refresh_completed) =
            RetryableIngestLease::start_with_heartbeat_receipts(
                &server,
                DbScope::Global,
                None,
                "ingest_source",
                event_hash,
                claim_a,
                std::time::Duration::from_secs(1),
            );
        tokio::task::yield_now().await;
        with_offline_global_fixture_connection(&server, |connection| {
            connection.execute_batch(
                "CREATE TRIGGER fail_ingest_heartbeat \
                 BEFORE UPDATE OF created_at ON processed_events \
                 WHEN OLD.event_hash = 'heartbeat-loss-event' \
                 BEGIN SELECT RAISE(FAIL, 'injected heartbeat failure'); END;",
            )
        })
        .map_err(|error| format!("install heartbeat fault: {error}"))
        .expect("install heartbeat fault");
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        refresh_completed
            .await
            .expect("injected heartbeat failure reaches terminal refresh");
        with_offline_global_fixture_connection(&server, |connection| {
            connection.execute_batch("DROP TRIGGER fail_ingest_heartbeat;")
        })
        .map_err(|error| format!("remove heartbeat fault: {error}"))
        .expect("remove heartbeat fault");
        server
            .with_global_store(|store| {
                store
                    .connection()
                    .execute(
                        "UPDATE processed_events \
                         SET created_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now', '-10 minutes') \
                         WHERE event_hash = ?1 AND worker = 'ingest_source'",
                        [event_hash],
                    )
                    .map(|_| ())
                    .map_err(|error| format!("age failed claim: {error}"))
            })
            .expect("make failed heartbeat claim reclaimable");

        let claim_b = claim_retryable_ingest_event(
            &server,
            DbScope::Global,
            None,
            "ingest_source",
            &audit_key,
            "ingest_source",
            event_hash,
            "owner-b",
        )
        .expect("claim B")
        .expect("B takes stale claim");
        let lease_b = RetryableIngestLease::start_with_interval(
            &server,
            DbScope::Global,
            None,
            "ingest_source",
            event_hash,
            claim_b,
            std::time::Duration::from_secs(1),
        );

        let mut stale_edges_written = 0usize;
        let stale_error = super::super::auto_ingest::build_similarity_edges(
            &server,
            DbScope::Global,
            None,
            Some("general"),
            std::slice::from_ref(&source),
            &lease_a,
            &mut stale_edges_written,
        )
        .await
        .expect_err("A must be fenced after heartbeat failure and takeover");
        assert!(
            stale_error.contains("heartbeat") || stale_error.contains("ownership"),
            "ownership loss must be loud: {stale_error}"
        );
        let _ = lease_a
            .fail(
                "ingest_source",
                &audit_key,
                "claim_ownership_lost",
                stale_error,
            )
            .await;

        let mut current_edges_written = 0usize;
        super::super::auto_ingest::build_similarity_edges(
            &server,
            DbScope::Global,
            None,
            Some("general"),
            std::slice::from_ref(&source),
            &lease_b,
            &mut current_edges_written,
        )
        .await
        .expect("current owner writes graph observation");
        lease_b
            .complete("ingest_source", &audit_key)
            .await
            .expect("current owner completes");

        with_offline_global_fixture_connection(&server, |connection| {
            connection.execute_batch(
                "CREATE TABLE heartbeat_after_join_marker (seen INTEGER NOT NULL); \
                 CREATE TRIGGER mark_heartbeat_after_join \
                 AFTER UPDATE OF created_at ON processed_events \
                 WHEN OLD.event_hash = 'heartbeat-loss-event' \
                 BEGIN INSERT INTO heartbeat_after_join_marker VALUES (1); END;",
            )
        })
        .map_err(|error| format!("install post-join heartbeat marker: {error}"))
        .expect("install post-join heartbeat marker");
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        tokio::task::yield_now().await;

        let (observations, post_join_heartbeats) = server
            .with_global_store_read(|store| {
                let observations = store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM edge_observations WHERE source_id = ?1",
                        [&source.id],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|error| format!("count graph observations: {error}"))?;
                let post_join_heartbeats = store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM heartbeat_after_join_marker",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|error| format!("count post-join heartbeats: {error}"))?;
                Ok((observations, post_join_heartbeats))
            })
            .expect("count graph observations");
        assert_eq!(observations, 1, "only the current owner may append");
        assert_eq!(
            post_join_heartbeats, 0,
            "joined heartbeat must not outlive lease"
        );
    }
}
