mod completion;
mod input;
use crate::foundry_runtime_ops::enqueue_foundry_capture_maintenance;
use crate::server_state::{DbScope, MemoryServer};
use completion::admitted_enrichment_is_terminal;
use input::embedding_input_for_item;
use memcore::store::enrichment::EnrichmentInvocationReceipts;
use memcore::{MemoryEntry, MemoryStore};
use rusqlite::OptionalExtension;
use std::collections::HashSet;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Notify};

const ADMITTED_ENRICHMENT_INTENT_WORKER: &str = "auto_ingest_enrichment_intent";
const ADMITTED_ENRICHMENT_INTENT_VERSION: u8 = 1;
const ADMITTED_ENRICHMENT_LEASE_SECS: i64 = 5 * 60;
#[cfg(not(test))]
const ADMITTED_ENRICHMENT_HEARTBEAT: Duration = Duration::from_secs(60);
#[cfg(test)]
const ADMITTED_ENRICHMENT_HEARTBEAT: Duration = Duration::from_millis(10);
const ADMITTED_LEASE_ACTIVE: u8 = 0;
const ADMITTED_LEASE_STOPPED: u8 = 1;
const ADMITTED_LEASE_LOST: u8 = 2;

#[derive(Debug)]
struct AdmittedLeaseControl {
    state: AtomicU8,
    changed: Notify,
}

impl AdmittedLeaseControl {
    fn active() -> Self {
        Self {
            state: AtomicU8::new(ADMITTED_LEASE_ACTIVE),
            changed: Notify::new(),
        }
    }

    fn is_active(&self) -> bool {
        self.state.load(Ordering::Acquire) == ADMITTED_LEASE_ACTIVE
    }

    fn mark_lost(&self) {
        self.state.store(ADMITTED_LEASE_LOST, Ordering::Release);
        self.changed.notify_waiters();
    }

    fn stop(&self) {
        self.state.store(ADMITTED_LEASE_STOPPED, Ordering::Release);
        self.changed.notify_waiters();
    }
}

#[cfg(test)]
static FORCE_NEXT_ADMITTED_HEARTBEAT_OWNERSHIP_LOSS: std::sync::OnceLock<
    std::sync::Mutex<Option<PathBuf>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) fn force_next_admitted_heartbeat_ownership_loss_for_test(server: &MemoryServer) {
    *FORCE_NEXT_ADMITTED_HEARTBEAT_OWNERSHIP_LOSS
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("lock admitted heartbeat ownership-loss hook") = Some(server.global_db_path_buf());
}

#[cfg(test)]
fn take_admitted_heartbeat_ownership_loss_for_test(server: &MemoryServer) -> bool {
    let mut target = FORCE_NEXT_ADMITTED_HEARTBEAT_OWNERSHIP_LOSS
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("lock admitted heartbeat ownership-loss hook");
    if target.as_deref() == Some(server.global_db_path_buf().as_path()) {
        *target = None;
        true
    } else {
        false
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct AdmittedEnrichmentIntentV1 {
    version: u8,
    job_id: String,
    idempotency_key: String,
    entry_id: String,
    revision: i64,
    target_db: String,
    named_project: Option<String>,
    needs_embedding: bool,
    needs_summary: bool,
    needs_metadata: bool,
    #[serde(default, alias = "dispatch_runtime_id")]
    owner_token: Option<String>,
}

pub(crate) enum DurableEnrichmentDispatch {
    Completed,
    Pending {
        item: Box<EnrichmentItem>,
        should_enqueue: bool,
    },
}

// ─── Enrichment Batcher ──────────────────────────────────────────────────────

/// Cap on merged write-side keyword sets to bound FTS index bloat (#921).
pub(crate) const MAX_ENRICHED_KEYWORDS: usize = 24;

/// Per-keyword max length at the enrichment boundary (#943 / FTS safety).
pub(crate) const MAX_KEYWORD_LEN: usize = 64;

/// Env flag for write-side synonym/bilingual keyword enrichment (default OFF).
pub(crate) const WRITE_ENRICH_KEYWORDS_ENV: &str = "TACHI_WRITE_ENRICH_KEYWORDS";

/// An item queued for background embedding + summary enrichment.
#[derive(Debug, Clone)]
pub(crate) struct EnrichmentItem {
    pub(crate) id: String,
    pub(crate) text: String,
    pub(crate) auto_chunked: bool,
    pub(crate) summary: String,
    pub(crate) keywords: Vec<String>,
    pub(crate) entities: Vec<String>,
    pub(crate) needs_embedding: bool,
    pub(crate) needs_summary: bool,
    pub(crate) needs_metadata: bool,
    /// Synonym + bilingual keyword expansion (#921). Gated by
    /// [`WRITE_ENRICH_KEYWORDS_ENV`]; default off ⇒ always false.
    pub(crate) needs_keyword_enrichment: bool,
    pub(crate) target_db: DbScope,
    pub(crate) named_project: Option<String>,
    pub(crate) db_path: Option<PathBuf>,
    pub(crate) foundry_agent_id: Option<String>,
    pub(crate) foundry_path_prefix: Option<String>,
    pub(crate) revision: i64,
    pub(crate) admitted_intent: Option<AdmittedEnrichmentIntentV1>,
    admitted_lease: Option<Arc<AdmittedLeaseControl>>,
}

/// Caller tags do not suppress extraction; see
/// [`memcore::store::enrichment::metadata_extraction_pending`].
pub(crate) fn needs_metadata_enrichment(entry: &MemoryEntry) -> bool {
    memcore::store::enrichment::metadata_extraction_pending(
        &entry.keywords,
        &entry.entities,
        &entry.metadata,
    )
}

/// Feature flag for write-side keyword enrichment. Default OFF.
pub(crate) fn write_keyword_enrichment_enabled() -> bool {
    std::env::var(WRITE_ENRICH_KEYWORDS_ENV)
        .map(|value| {
            let value = value.trim();
            value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

fn keywords_status_from_entry(entry: &MemoryEntry) -> Option<&str> {
    entry
        .metadata
        .get("enrichment")
        .and_then(|value| value.get("keywords_status"))
        .and_then(|value| value.as_str())
}

/// Whether this entry still needs write-side synonym/bilingual keyword expansion.
///
/// Fail-closed: when the flag is off, always false (zero behavior change).
pub(crate) fn needs_keyword_enrichment(entry: &MemoryEntry) -> bool {
    if !write_keyword_enrichment_enabled() {
        return false;
    }
    match keywords_status_from_entry(entry) {
        Some("enriched") | Some("skipped") => false,
        // pending/failed/absent: eligible when there is text to enrich
        _ => !entry.text.trim().is_empty(),
    }
}

/// Sanitize one keyword at the enrichment write boundary (#943).
///
/// - strips C0/C1 control characters (U+0000–U+001F, U+007F–U+009F)
/// - trims whitespace
/// - rejects empty / pure-punctuation tokens
/// - truncates to [`MAX_KEYWORD_LEN`]
///
/// FTS insert path (`update_enrichment_fields`) writes keywords via parameterized
/// SQL and copies column content into `memories_fts` with `WHERE id = ?1` — the
/// keyword text is document content, not a MATCH expression — so FTS-operator
/// tokens (AND/OR/NEAR) are not injection vectors. Still drop control noise and
/// bound length so a hostile LLM cannot bloat or corrupt the index surface.
pub(crate) fn sanitize_enriched_keyword(raw: &str) -> Option<String> {
    let stripped: String = raw
        .chars()
        .filter(|c| {
            let u = *c as u32;
            !(u <= 0x1F || (0x7F..=0x9F).contains(&u))
        })
        .collect();
    let trimmed = stripped.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Reject pure punctuation/whitespace (no letter or digit content).
    if !trimmed.chars().any(|c| c.is_alphanumeric()) {
        return None;
    }
    let truncated: String = trimmed.chars().take(MAX_KEYWORD_LEN).collect();
    if truncated.is_empty() {
        return None;
    }
    Some(truncated)
}

/// Keywords to write from a metadata extraction: the extraction itself when
/// the row has none, else caller keywords first with the extraction merged
/// after them. `None` when the extraction adds nothing.
pub(crate) fn accepted_metadata_keywords(
    existing: &[String],
    extracted: Vec<String>,
) -> Option<Vec<String>> {
    if extracted.is_empty() {
        return None;
    }
    if existing.is_empty() {
        return Some(extracted);
    }
    let merged = merge_enriched_keywords(existing, &extracted);
    (merged != merge_enriched_keywords(existing, &[])).then_some(merged)
}

/// Merge existing + expanded keywords with sanitization, case-insensitive dedupe, and a hard cap.
pub(crate) fn merge_enriched_keywords(existing: &[String], expanded: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for raw in existing.iter().chain(expanded.iter()) {
        let Some(clean) = sanitize_enriched_keyword(raw) else {
            continue;
        };
        let key = clean.to_ascii_lowercase();
        if !seen.insert(key) {
            continue;
        }
        out.push(clean);
        if out.len() >= MAX_ENRICHED_KEYWORDS {
            break;
        }
    }
    out
}

pub(crate) fn build_enrichment_item(
    entry: &MemoryEntry,
    needs_embedding: bool,
    needs_summary: bool,
    target_db: DbScope,
    named_project: Option<String>,
    db_path: Option<PathBuf>,
    foundry_agent_id: Option<String>,
    foundry_path_prefix: Option<String>,
    revision: i64,
) -> EnrichmentItem {
    EnrichmentItem {
        id: entry.id.clone(),
        text: entry.text.clone(),
        auto_chunked: entry
            .metadata
            .get("auto_chunked")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        summary: entry.summary.clone(),
        keywords: entry.keywords.clone(),
        entities: entry.entities.clone(),
        needs_embedding,
        needs_summary,
        needs_metadata: needs_metadata_enrichment(entry),
        needs_keyword_enrichment: needs_keyword_enrichment(entry),
        target_db,
        named_project,
        db_path,
        foundry_agent_id,
        foundry_path_prefix,
        revision,
        admitted_intent: None,
        admitted_lease: None,
    }
}

fn rollback_admitted_enrichment_intent(store: &mut MemoryStore, error: String) -> String {
    match store.connection().execute_batch("ROLLBACK") {
        Ok(()) => error,
        Err(rollback) => format!("{error}; rollback admitted enrichment intent: {rollback}"),
    }
}

fn admitted_enrichment_intent_hash(intent: &AdmittedEnrichmentIntentV1) -> String {
    crate::utils::stable_hash(&format!(
        "{}:{}:{}:{}",
        intent.job_id, intent.idempotency_key, intent.entry_id, intent.revision
    ))
}

impl MemoryServer {
    pub(crate) fn prepare_durable_admitted_enrichment(
        &self,
        mut item: EnrichmentItem,
        job_id: &str,
        idempotency_key: &str,
    ) -> Result<DurableEnrichmentDispatch, String> {
        let runtime_id = self
            .enrichment_lock()
            .durable_dispatch_runtime_id
            .to_string();
        let mut intent = AdmittedEnrichmentIntentV1 {
            version: ADMITTED_ENRICHMENT_INTENT_VERSION,
            job_id: job_id.to_string(),
            idempotency_key: idempotency_key.to_string(),
            entry_id: item.id.clone(),
            revision: item.revision,
            target_db: item.target_db.as_str().to_string(),
            named_project: item.named_project.clone(),
            needs_embedding: item.needs_embedding,
            needs_summary: item.needs_summary,
            needs_metadata: item.needs_metadata,
            owner_token: None,
        };
        let intent_hash = admitted_enrichment_intent_hash(&intent);
        let stale_modifier = format!("-{ADMITTED_ENRICHMENT_LEASE_SECS} seconds");
        let action = |store: &mut MemoryStore| {
            store
                .connection()
                .execute_batch("BEGIN IMMEDIATE")
                .map_err(|error| format!("begin admitted enrichment intent claim: {error}"))?;
            let result = (|| {
                let entry = store
                    .get(&intent.entry_id)
                    .map_err(|error| format!("load admitted enrichment entry: {error}"))?
                    .ok_or_else(|| {
                        "admitted enrichment entry disappeared before dispatch".to_string()
                    })?;
                let bound_job = entry
                    .metadata
                    .get("admitted_job_id")
                    .and_then(serde_json::Value::as_str);
                let bound_key = entry
                    .metadata
                    .get("admitted_idempotency_key")
                    .and_then(serde_json::Value::as_str);
                if bound_job != Some(intent.job_id.as_str())
                    || bound_key != Some(intent.idempotency_key.as_str())
                    || entry.revision != intent.revision
                {
                    return Err(
                        "admitted enrichment intent binding drift at mutation time".to_string()
                    );
                }

                let existing = store
                    .connection()
                    .query_row(
                        "SELECT event_id, \
                                created_at = '' OR julianday(created_at) IS NULL OR \
                                julianday(created_at) <= julianday('now', ?3) \
                         FROM processed_events WHERE event_hash = ?1 AND worker = ?2",
                        rusqlite::params![
                            intent_hash,
                            ADMITTED_ENRICHMENT_INTENT_WORKER,
                            stale_modifier,
                        ],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
                    )
                    .optional()
                    .map_err(|error| format!("load admitted enrichment intent: {error}"))?;

                let terminal = admitted_enrichment_is_terminal(&entry, &intent);
                let should_enqueue = match existing {
                    Some((payload, expired)) => {
                        let existing: AdmittedEnrichmentIntentV1 = serde_json::from_str(&payload)
                            .map_err(|error| {
                            format!("decode admitted enrichment intent: {error}")
                        })?;
                        let mut immutable_existing = existing.clone();
                        immutable_existing.owner_token = None;
                        if immutable_existing != intent {
                            return Err(
                                "admitted enrichment intent binding drift at mutation time"
                                    .to_string(),
                            );
                        }
                        let owner_is_live = existing.owner_token.is_some() && !expired;
                        if terminal {
                            if owner_is_live {
                                false
                            } else {
                                store
                                    .connection()
                                    .execute(
                                        "DELETE FROM processed_events \
                                         WHERE event_hash = ?1 AND worker = ?2 AND event_id = ?3",
                                        rusqlite::params![
                                            intent_hash,
                                            ADMITTED_ENRICHMENT_INTENT_WORKER,
                                            payload,
                                        ],
                                    )
                                    .map_err(|error| {
                                        format!(
                                            "retire expired terminal enrichment intent: {error}"
                                        )
                                    })?;
                                return Ok(None);
                            }
                        } else if owner_is_live {
                            false
                        } else {
                            intent.owner_token =
                                Some(format!("{}:{}", runtime_id, uuid::Uuid::new_v4()));
                            let claimed_payload =
                                serde_json::to_string(&intent).map_err(|error| {
                                    format!("serialize admitted enrichment intent claim: {error}")
                                })?;
                            let claimed = store
                                .connection()
                                .execute(
                                    "UPDATE processed_events SET event_id = ?1, \
                                       created_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now') \
                                     WHERE event_hash = ?2 AND worker = ?3 AND event_id = ?4",
                                    rusqlite::params![
                                        claimed_payload,
                                        intent_hash,
                                        ADMITTED_ENRICHMENT_INTENT_WORKER,
                                        payload,
                                    ],
                                )
                                .map_err(|error| {
                                    format!("claim admitted enrichment intent: {error}")
                                })?;
                            if claimed != 1 {
                                return Err(
                                    "admitted enrichment intent ownership changed during claim"
                                        .to_string(),
                                );
                            }
                            true
                        }
                    }
                    None if terminal => return Ok(None),
                    None => {
                        intent.owner_token =
                            Some(format!("{}:{}", runtime_id, uuid::Uuid::new_v4()));
                        let claimed_payload = serde_json::to_string(&intent).map_err(|error| {
                            format!("serialize admitted enrichment intent claim: {error}")
                        })?;
                        store
                            .connection()
                            .execute(
                                "INSERT INTO processed_events \
                                   (event_hash, event_id, worker, created_at) \
                                 VALUES (?1, ?2, ?3, STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now'))",
                                rusqlite::params![
                                    intent_hash,
                                    claimed_payload,
                                    ADMITTED_ENRICHMENT_INTENT_WORKER,
                                ],
                            )
                            .map_err(|error| {
                                format!("persist admitted enrichment intent: {error}")
                            })?;
                        true
                    }
                };
                Ok(Some(should_enqueue))
            })();
            match result {
                Ok(value) => {
                    store
                        .connection()
                        .execute_batch("COMMIT")
                        .map_err(|error| format!("commit admitted enrichment claim: {error}"))?;
                    Ok(value)
                }
                Err(error) => Err(rollback_admitted_enrichment_intent(store, error)),
            }
        };
        let pending = if let Some(project_name) = item.named_project.as_deref() {
            self.with_named_project_store(project_name, action)?
        } else {
            self.with_store_for_scope(item.target_db, action)?
        };
        let Some(should_enqueue) = pending else {
            return Ok(DurableEnrichmentDispatch::Completed);
        };
        item.admitted_intent = Some(intent.clone());
        if should_enqueue {
            item.admitted_lease = Some(self.start_admitted_enrichment_heartbeat(&intent)?);
        }
        Ok(DurableEnrichmentDispatch::Pending {
            item: Box::new(item),
            should_enqueue,
        })
    }

    fn start_admitted_enrichment_heartbeat(
        &self,
        intent: &AdmittedEnrichmentIntentV1,
    ) -> Result<Arc<AdmittedLeaseControl>, String> {
        if intent.owner_token.is_none() {
            return Err("admitted enrichment heartbeat requires an owner token".to_string());
        }
        let payload = serde_json::to_string(intent)
            .map_err(|error| format!("serialize admitted enrichment heartbeat: {error}"))?;
        let intent_hash = admitted_enrichment_intent_hash(intent);
        let target_db = if intent.target_db == DbScope::Global.as_str() {
            DbScope::Global
        } else {
            DbScope::Project
        };
        let named_project = intent.named_project.clone();
        let entry_id = intent.entry_id.clone();
        let control = Arc::new(AdmittedLeaseControl::active());
        let heartbeat_control = Arc::clone(&control);
        let server = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(ADMITTED_ENRICHMENT_HEARTBEAT).await;
                if !heartbeat_control.is_active() {
                    break;
                }
                #[cfg(test)]
                let forced_loss = take_admitted_heartbeat_ownership_loss_for_test(&server);
                #[cfg(not(test))]
                let forced_loss = false;
                let action = |store: &mut MemoryStore| -> Result<usize, String> {
                    if forced_loss {
                        return Ok(0);
                    }
                    store
                        .connection()
                        .execute(
                            "UPDATE processed_events \
                             SET created_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now') \
                             WHERE event_hash = ?1 AND worker = ?2 AND event_id = ?3",
                            rusqlite::params![
                                intent_hash,
                                ADMITTED_ENRICHMENT_INTENT_WORKER,
                                payload,
                            ],
                        )
                        .map_err(|error| format!("heartbeat admitted enrichment intent: {error}"))
                };
                let result = if let Some(project_name) = named_project.as_deref() {
                    server.with_named_project_store(project_name, action)
                } else {
                    server.with_store_for_scope(target_db, action)
                };
                match result {
                    Ok(1) => {}
                    Ok(_) => {
                        heartbeat_control.mark_lost();
                        tracing::warn!(%entry_id, "admitted enrichment heartbeat lost ownership");
                        break;
                    }
                    Err(error) => {
                        heartbeat_control.mark_lost();
                        tracing::error!(%error, %entry_id, "admitted enrichment heartbeat failed");
                        break;
                    }
                }
            }
        });
        Ok(control)
    }

    fn renew_admitted_enrichment_owner(
        store: &mut MemoryStore,
        intent: &AdmittedEnrichmentIntentV1,
    ) -> Result<bool, String> {
        let intent_hash = admitted_enrichment_intent_hash(intent);
        let payload = serde_json::to_string(intent)
            .map_err(|error| format!("serialize admitted enrichment owner: {error}"))?;
        store
            .connection()
            .execute(
                "UPDATE processed_events \
                 SET created_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now') \
                 WHERE event_hash = ?1 AND worker = ?2 AND event_id = ?3",
                rusqlite::params![intent_hash, ADMITTED_ENRICHMENT_INTENT_WORKER, payload],
            )
            .map(|updated| updated == 1)
            .map_err(|error| format!("renew admitted enrichment owner: {error}"))
    }

    fn admitted_enrichment_dispatch_is_active(&self, item: &EnrichmentItem) -> bool {
        let Some(intent) = item.admitted_intent.as_ref() else {
            return true;
        };
        let Some(control) = item.admitted_lease.as_ref() else {
            return false;
        };
        if !control.is_active() {
            return false;
        }
        let action = |store: &mut MemoryStore| Self::renew_admitted_enrichment_owner(store, intent);
        let result = if let Some(project_name) = intent.named_project.as_deref() {
            self.with_named_project_store(project_name, action)
        } else {
            self.with_store_for_scope(item.target_db, action)
        };
        match result {
            Ok(true) => true,
            Ok(false) => {
                control.mark_lost();
                tracing::warn!(entry_id = %intent.entry_id, "admitted enrichment consumer lost ownership");
                false
            }
            Err(error) => {
                control.mark_lost();
                tracing::error!(%error, entry_id = %intent.entry_id, "admitted enrichment ownership check failed");
                false
            }
        }
    }

    async fn run_admitted_enrichment_egress<T>(
        &self,
        item: &EnrichmentItem,
        future: impl Future<Output = T>,
    ) -> Option<T> {
        if !self.admitted_enrichment_dispatch_is_active(item) {
            return None;
        }
        let Some(control) = item.admitted_lease.as_ref() else {
            return Some(future.await);
        };
        let changed = control.changed.notified();
        tokio::pin!(changed);
        if !control.is_active() {
            return None;
        }
        tokio::select! {
            biased;
            _ = &mut changed => None,
            result = future => Some(result),
        }
    }

    pub(crate) fn mark_admitted_enrichment_pending(
        &self,
        item: &EnrichmentItem,
    ) -> Result<(), String> {
        let intent = item
            .admitted_intent
            .as_ref()
            .ok_or_else(|| "admitted enrichment dispatch has no durable intent".to_string())?;
        if let Some(control) = item.admitted_lease.as_ref() {
            control.stop();
        }
        let intent_hash = admitted_enrichment_intent_hash(intent);
        let owned_payload = serde_json::to_string(intent)
            .map_err(|error| format!("serialize owned enrichment intent: {error}"))?;
        let action = |store: &mut MemoryStore| {
            let mut pending = intent.clone();
            pending.owner_token = None;
            let payload = serde_json::to_string(&pending)
                .map_err(|error| format!("serialize pending enrichment intent: {error}"))?;
            let released = store
                .connection()
                .execute(
                    "UPDATE processed_events SET event_id = ?1, \
                       created_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now') \
                     WHERE event_hash = ?2 AND worker = ?3 AND event_id = ?4",
                    rusqlite::params![
                        payload,
                        intent_hash,
                        ADMITTED_ENRICHMENT_INTENT_WORKER,
                        owned_payload,
                    ],
                )
                .map_err(|error| format!("release admitted enrichment dispatch: {error}"))?;
            if released == 1 {
                Ok(())
            } else {
                Err("admitted enrichment dispatch ownership changed before release".to_string())
            }
        };
        if let Some(project_name) = intent.named_project.as_deref() {
            self.with_named_project_store(project_name, action)
        } else {
            self.with_store_for_scope(
                if intent.target_db == DbScope::Global.as_str() {
                    DbScope::Global
                } else {
                    DbScope::Project
                },
                action,
            )
        }
    }

    fn reconcile_admitted_enrichment_intent(&self, item: &EnrichmentItem) -> bool {
        let Some(intent) = item.admitted_intent.as_ref() else {
            return false;
        };
        if !self.admitted_enrichment_dispatch_is_active(item) {
            return false;
        }
        if let Some(control) = item.admitted_lease.as_ref() {
            control.stop();
        }
        let intent_hash = admitted_enrichment_intent_hash(intent);
        let owned_payload = match serde_json::to_string(intent) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(%error, entry_id = %intent.entry_id, "failed to serialize owned admitted enrichment intent");
                return false;
            }
        };
        let action = |store: &mut MemoryStore| {
            let terminal = store
                .get(&intent.entry_id)
                .map_err(|error| format!("load enriched admitted entry: {error}"))?
                .is_some_and(|entry| admitted_enrichment_is_terminal(&entry, intent));
            if terminal {
                store
                    .connection()
                    .execute(
                        "DELETE FROM processed_events \
                         WHERE event_hash = ?1 AND worker = ?2 AND event_id = ?3",
                        rusqlite::params![
                            intent_hash,
                            ADMITTED_ENRICHMENT_INTENT_WORKER,
                            owned_payload,
                        ],
                    )
                    .map_err(|error| format!("retire admitted enrichment intent: {error}"))
            } else {
                let mut pending = intent.clone();
                pending.owner_token = None;
                let payload = serde_json::to_string(&pending)
                    .map_err(|error| format!("serialize retryable enrichment intent: {error}"))?;
                store
                    .connection()
                    .execute(
                        "UPDATE processed_events SET event_id = ?1, \
                           created_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now') \
                         WHERE event_hash = ?2 AND worker = ?3 AND event_id = ?4",
                        rusqlite::params![
                            payload,
                            intent_hash,
                            ADMITTED_ENRICHMENT_INTENT_WORKER,
                            owned_payload,
                        ],
                    )
                    .map_err(|error| format!("retain admitted enrichment intent: {error}"))
            }
        };
        let result = if let Some(project_name) = intent.named_project.as_deref() {
            self.with_named_project_store(project_name, action)
        } else {
            self.with_store_for_scope(item.target_db, action)
        };
        match result {
            Ok(1) => true,
            Ok(_) => {
                tracing::warn!(entry_id = %intent.entry_id, "admitted enrichment reconciliation lost ownership");
                false
            }
            Err(error) => {
                tracing::error!(%error, entry_id = %intent.entry_id, "failed to reconcile admitted enrichment intent");
                false
            }
        }
    }

    fn wake_admitted_auto_ingest_completion(&self, item: &EnrichmentItem) {
        if item.admitted_intent.is_none() {
            return;
        }
        let server = self.clone();
        tokio::spawn(async move {
            // The first replay returns partial and releases its claim only
            // after this consumer flush began. Let that owner-fenced cleanup
            // finish before the terminal-evidence replay tries to claim.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if let Err(error) = crate::pipeline_ops::replay_pending_auto_ingest_once(&server).await
            {
                tracing::error!(%error, "durable enrichment could not wake auto-ingest completion");
            }
        });
    }
}

fn external_llm_input(text: &str) -> String {
    crate::memory_search_ops::scrub_secrets(text).0
}

/// Batch enrichment queue configuration.
pub(super) const ENRICH_BATCH_MAX: usize = 32;
pub(super) const ENRICH_FLUSH_INTERVAL_MS: u64 = 500;

impl MemoryServer {
    pub(crate) fn enqueue_enrichment(&self, item: EnrichmentItem) -> bool {
        if let Err(err) = self.enrichment_lock().enrich_tx.try_send(item) {
            tracing::warn!("[enrichment-batcher] failed to queue enrichment item: {err}");
            return false;
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn close_enrichment_channel_for_test(&self) {
        self.enrichment_lock()
            .retained_enrich_rx
            .lock()
            .expect("lock retained test enrichment receiver")
            .take();
    }

    #[cfg(test)]
    pub(crate) fn complete_retained_admitted_enrichments_for_test(&self) -> usize {
        let mut items = Vec::new();
        {
            let mut retained = self
                .enrichment_lock()
                .retained_enrich_rx
                .lock()
                .expect("lock retained test enrichment receiver");
            let receiver = retained.as_mut().expect("test receiver retained");
            while let Ok(item) = receiver.try_recv() {
                if item.admitted_intent.is_some() {
                    items.push(item);
                }
            }
        }
        for item in &items {
            if !self.admitted_enrichment_dispatch_is_active(item) {
                continue;
            }
            let summary = item.needs_summary.then_some("durable test summary");
            let embedding = item.needs_embedding.then(|| {
                let mut vector = vec![0.0_f32; 1024];
                vector[0] = 1.0;
                vector
            });
            let keywords = item
                .needs_metadata
                .then(|| vec!["durable-test-keyword".to_string()]);
            let update = |store: &mut MemoryStore| {
                if let Some(intent) = item.admitted_intent.as_ref() {
                    if !Self::renew_admitted_enrichment_owner(store, intent)? {
                        return Ok(false);
                    }
                }
                store
                    .update_enrichment_fields(
                        &item.id,
                        summary,
                        embedding.as_deref(),
                        keywords.as_deref(),
                        None,
                        item.revision,
                    )
                    .map_err(|error| format!("complete retained test enrichment: {error}"))
            };
            let updated = if let Some(project_name) = item.named_project.as_deref() {
                self.with_named_project_store(project_name, update)
            } else {
                self.with_store_for_scope(item.target_db, update)
            }
            .expect("write retained test enrichment");
            assert!(updated, "retained test enrichment revision must match");
            let _ = self.reconcile_admitted_enrichment_intent(item);
        }
        items.len()
    }

    pub(crate) fn requeue_auth_failed_enrichment_retries(&self, trigger: &str) -> usize {
        const LIMIT_PER_DB: usize = 64;
        let mut total = 0usize;

        match self.with_global_store(|store| {
            let candidates = store
                .claim_auth_failed_enrichment_retries(LIMIT_PER_DB)
                .map_err(|e| format!("claim global auth-failed enrichment retries: {e}"))?;
            Ok(self.enqueue_retry_candidates(candidates, DbScope::Global, None, None))
        }) {
            Ok(count) => total += count,
            Err(err) => tracing::warn!(
                "[enrichment-batcher] failed to requeue global auth-failed rows after {trigger}: {err}"
            ),
        }

        if self.project_db_path_buf().is_some() {
            match self.with_project_store(|store| {
                let candidates = store
                    .claim_auth_failed_enrichment_retries(LIMIT_PER_DB)
                    .map_err(|e| format!("claim project auth-failed enrichment retries: {e}"))?;
                Ok(self.enqueue_retry_candidates(candidates, DbScope::Project, None, None))
            }) {
                Ok(count) => total += count,
                Err(err) => tracing::warn!(
                    "[enrichment-batcher] failed to requeue project auth-failed rows after {trigger}: {err}"
                ),
            }
        }

        if total > 0 {
            tracing::info!(
                "[enrichment-batcher] requeued {total} auth-failed enrichment row(s) after {trigger}"
            );
        }
        total
    }

    fn enqueue_retry_candidates(
        &self,
        candidates: Vec<memcore::store::enrichment::EnrichmentRetryCandidate>,
        target_db: DbScope,
        named_project: Option<String>,
        db_path: Option<PathBuf>,
    ) -> usize {
        let mut queued = 0usize;
        let keyword_flag = write_keyword_enrichment_enabled();
        for candidate in candidates {
            let needs_keyword_enrichment = keyword_flag
                && (candidate.failed_stage == "keywords"
                    || candidate.needs_metadata
                    || candidate.keywords.is_empty());
            if !candidate.needs_embedding
                && !candidate.needs_summary
                && !candidate.needs_metadata
                && !needs_keyword_enrichment
            {
                continue;
            }
            let item = EnrichmentItem {
                id: candidate.id,
                text: candidate.text,
                auto_chunked: candidate.auto_chunked,
                summary: candidate.summary,
                keywords: candidate.keywords,
                entities: candidate.entities,
                needs_embedding: candidate.needs_embedding,
                needs_summary: candidate.needs_summary,
                needs_metadata: candidate.needs_metadata,
                needs_keyword_enrichment,
                target_db,
                named_project: named_project.clone(),
                db_path: db_path.clone(),
                foundry_agent_id: None,
                foundry_path_prefix: None,
                revision: candidate.revision,
                admitted_intent: None,
                admitted_lease: None,
            };
            if self.enqueue_enrichment(item) {
                queued += 1;
            }
        }
        queued
    }

    /// Background worker that batches enrichment requests (embedding + summary).
    /// Flushes every ENRICH_FLUSH_INTERVAL_MS or when ENRICH_BATCH_MAX items accumulate.
    pub(super) async fn run_enrichment_batcher(
        server: MemoryServer,
        mut rx: mpsc::Receiver<EnrichmentItem>,
    ) {
        let mut batch: Vec<EnrichmentItem> = Vec::with_capacity(ENRICH_BATCH_MAX);
        let flush_interval = Duration::from_millis(ENRICH_FLUSH_INTERVAL_MS);

        loop {
            // Wait for first item or channel close
            let item = if batch.is_empty() {
                match rx.recv().await {
                    Some(item) => Some(item),
                    None => break, // channel closed
                }
            } else {
                None
            };

            if let Some(item) = item {
                batch.push(item);
            }

            // Drain more items until batch is full or timeout expires
            let deadline = tokio::time::Instant::now() + flush_interval;
            while batch.len() < ENRICH_BATCH_MAX {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Ok(Some(item)) => batch.push(item),
                    Ok(None) => {
                        // Channel closed; flush remaining and exit
                        if !batch.is_empty() {
                            server.flush_enrichment_batch(&mut batch).await;
                        }
                        return;
                    }
                    Err(_timeout) => break, // timer expired, flush what we have
                }
            }

            if !batch.is_empty() {
                server.flush_enrichment_batch(&mut batch).await;
            }
        }

        tracing::debug!("[enrichment-batcher] channel closed, worker exiting");
    }

    /// Flush a batch: batch-embed all texts needing embedding, then update DB.
    pub(crate) async fn flush_enrichment_batch(&self, batch: &mut Vec<EnrichmentItem>) {
        let items: Vec<EnrichmentItem> = std::mem::take(batch);
        let batch_size = items.len();
        tracing::info!("[enrichment-batcher] flushing batch of {batch_size} items");

        let mut active = Vec::with_capacity(items.len());
        for item in &items {
            active.push(self.admitted_enrichment_dispatch_is_active(item));
        }

        // 1. Generate summaries first so long-memory embeddings include summary
        // context alongside the original facts.
        let summary_futures: Vec<_> = items
            .iter()
            .enumerate()
            .filter(|(i, item)| active[*i] && item.needs_summary)
            .map(|(i, item)| {
                let llm = self.llm.clone();
                let text = external_llm_input(&item.text);
                async move {
                    (
                        i,
                        self.run_admitted_enrichment_egress(
                            item,
                            llm.generate_summary_with_receipt(&text),
                        )
                        .await,
                    )
                }
            })
            .collect();

        let summary_results = futures::future::join_all(summary_futures).await;

        let mut summaries: Vec<Option<String>> = vec![None; items.len()];
        let mut summary_receipts: Vec<Option<serde_json::Value>> = vec![None; items.len()];
        for (idx, result) in summary_results {
            let Some(result) = result else {
                active[idx] = false;
                continue;
            };
            match result {
                Ok(generated) if generated.value.trim().is_empty() => {
                    tracing::debug!(
                        "[enrichment-batcher] summary returned no accepted field for {}",
                        items[idx].id
                    );
                }
                Ok(generated) => match serde_json::to_value(&generated.invocation) {
                    Ok(receipt) => {
                        summaries[idx] = Some(generated.value);
                        summary_receipts[idx] = Some(receipt);
                    }
                    Err(error) => {
                        let error = format!("serialize summary invocation receipt: {error}");
                        tracing::warn!(
                            "[enrichment-batcher] summary receipt failed for {}: {error}",
                            items[idx].id
                        );
                        record_enrichment_failure(self, &items[idx], "summary", &error);
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        "[enrichment-batcher] summary failed for {}: {e}",
                        items[idx].id
                    );
                    record_enrichment_failure(self, &items[idx], "summary", &e);
                }
            }
        }

        // 2. Extract keywords + entities for items missing structured metadata.
        let metadata_futures: Vec<_> = items
            .iter()
            .enumerate()
            .filter(|(i, item)| active[*i] && item.needs_metadata)
            .map(|(i, item)| {
                let llm = self.llm.clone();
                let text = external_llm_input(&item.text);
                async move {
                    (
                        i,
                        self.run_admitted_enrichment_egress(
                            item,
                            llm.extract_metadata_with_receipt(&text),
                        )
                        .await,
                    )
                }
            })
            .collect();

        let metadata_results = futures::future::join_all(metadata_futures).await;

        let mut keywords_out: Vec<Option<Vec<String>>> = vec![None; items.len()];
        let mut entities_out: Vec<Option<Vec<String>>> = vec![None; items.len()];
        let mut metadata_receipts: Vec<Option<serde_json::Value>> = vec![None; items.len()];
        // Extraction ran but accepted no field: recorded so tagged rows are
        // not extracted again (#2176).
        let mut metadata_attempted_out = vec![false; items.len()];
        // Operator-visible keyword enrichment status per item (enriched/skipped/failed).
        let mut keyword_status_out: Vec<Option<&'static str>> = vec![None; items.len()];
        for (idx, result) in metadata_results {
            let Some(result) = result else {
                active[idx] = false;
                continue;
            };
            match result {
                Ok(generated) => {
                    let (keywords, entities) = generated.value;
                    let mut accepted_field = false;
                    if let Some(keywords) =
                        accepted_metadata_keywords(&items[idx].keywords, keywords)
                    {
                        keywords_out[idx] = Some(keywords);
                        accepted_field = true;
                    }
                    if items[idx].entities.is_empty() && !entities.is_empty() {
                        entities_out[idx] = Some(entities);
                        accepted_field = true;
                    }
                    // Untagged rows keep retrying an empty extraction as before.
                    metadata_attempted_out[idx] =
                        !accepted_field && !items[idx].keywords.is_empty();
                    if accepted_field {
                        match serde_json::to_value(&generated.invocation) {
                            Ok(receipt) => metadata_receipts[idx] = Some(receipt),
                            Err(error) => {
                                // The values were not persisted yet, so suppress them rather
                                // than breaking the field/receipt atomic pairing.
                                keywords_out[idx] = None;
                                entities_out[idx] = None;
                                let error =
                                    format!("serialize metadata invocation receipt: {error}");
                                tracing::warn!(
                                    "[enrichment-batcher] metadata receipt failed for {}: {error}",
                                    items[idx].id
                                );
                                record_enrichment_failure(self, &items[idx], "metadata", &error);
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "[enrichment-batcher] metadata failed for {}: {e}",
                        items[idx].id
                    );
                    record_enrichment_failure(self, &items[idx], "metadata", &e);
                }
            }
        }

        // Deterministic domain-specific metadata (e.g. A-share ticker tagging)
        // is no longer derived in the generic engine — that domain logic lives
        // in the host project. LLM enrichment above already populated
        // keywords_out / entities_out.

        // 2b. Write-side synonym + bilingual keyword expansion (#921).
        // Gated by TACHI_WRITE_ENRICH_KEYWORDS (default off). Reuses the
        // extract-lane provider path and external_llm_input scrub (#568).
        let keyword_futures: Vec<_> = items
            .iter()
            .enumerate()
            .filter(|(i, item)| active[*i] && item.needs_keyword_enrichment)
            .filter_map(|(i, item)| {
                if item.text.trim().is_empty() {
                    keyword_status_out[i] = Some("skipped");
                    return None;
                }
                let llm = self.llm.clone();
                let text = external_llm_input(&item.text);
                let mut seed = item.keywords.clone();
                if let Some(kws) = keywords_out[i].as_ref() {
                    for kw in kws {
                        if !seed
                            .iter()
                            .any(|existing| existing.eq_ignore_ascii_case(kw))
                        {
                            seed.push(kw.clone());
                        }
                    }
                }
                // Scrub seed keywords with the same external_llm_input path as
                // memory text so credentials in existing_keywords cannot egress
                // into the extract-lane prompt (#943 / #568).
                let seed_for_llm: Vec<String> =
                    seed.iter().map(|kw| external_llm_input(kw)).collect();
                Some(async move {
                    (
                        i,
                        self.run_admitted_enrichment_egress(
                            item,
                            llm.expand_search_keywords_with_receipt(&text, &seed_for_llm),
                        )
                        .await,
                        seed,
                    )
                })
            })
            .collect();

        type KeywordEnrichmentResult = (
            usize,
            Option<Result<tachi_llm::Generated<Vec<String>>, String>>,
            Vec<String>,
        );
        let keyword_results: Vec<KeywordEnrichmentResult> =
            futures::future::join_all(keyword_futures).await;
        let mut keyword_receipts: Vec<Option<serde_json::Value>> = vec![None; items.len()];

        for (idx, result, seed) in keyword_results {
            let Some(result) = result else {
                active[idx] = false;
                continue;
            };
            match result {
                Ok(generated) => {
                    let merged = merge_enriched_keywords(&seed, &generated.value);
                    let normalized_seed = merge_enriched_keywords(&seed, &[]);
                    if merged == normalized_seed {
                        // A blank, duplicate-only, or fully-sanitized-away
                        // expansion did not generate an accepted field. Do
                        // not rewrite the seed or persist a success receipt.
                        keyword_status_out[idx] = Some("skipped");
                    } else {
                        match serde_json::to_value(&generated.invocation) {
                            Ok(receipt) => {
                                keywords_out[idx] = Some(merged);
                                keyword_receipts[idx] = Some(receipt);
                                keyword_status_out[idx] = Some("enriched");
                            }
                            Err(error) => {
                                let error =
                                    format!("serialize keywords invocation receipt: {error}");
                                tracing::warn!(
                                    "[enrichment-batcher] keyword receipt failed for {}: {error}",
                                    items[idx].id
                                );
                                keyword_status_out[idx] = Some("failed");
                                record_enrichment_failure(self, &items[idx], "keywords", &error);
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "[enrichment-batcher] keyword enrichment failed for {}: {e}",
                        items[idx].id
                    );
                    keyword_status_out[idx] = Some("failed");
                    record_enrichment_failure(self, &items[idx], "keywords", &e);
                }
            }
        }

        // Persist skipped status for items that never entered the LLM path.
        for (i, status) in keyword_status_out.iter().enumerate() {
            if active[i] && *status == Some("skipped") {
                write_keyword_enrichment_status(self, &items[i], "skipped");
            }
        }

        // 3. Batch embedding for items that need it
        let embed_indices: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(i, item)| active[*i] && item.needs_embedding)
            .map(|(i, _)| i)
            .collect();

        let embed_texts: Vec<String> = embed_indices
            .iter()
            .map(|&i| {
                let generated_summary = summaries[i].as_deref();
                let mut item = items[i].clone();
                if let Some(kws) = keywords_out[i].as_ref() {
                    item.keywords = kws.clone();
                }
                if let Some(ents) = entities_out[i].as_ref() {
                    item.entities = ents.clone();
                }
                embedding_input_for_item(&item, generated_summary)
            })
            .collect();

        let mut embed_results: Vec<Option<Vec<f32>>> = vec![None; items.len()];

        let (admitted_embeddings, regular_embeddings): (Vec<_>, Vec<_>) = embed_indices
            .into_iter()
            .zip(embed_texts)
            .partition(|(item_idx, _)| items[*item_idx].admitted_intent.is_some());

        if !regular_embeddings.is_empty() {
            let regular_texts: Vec<_> = regular_embeddings
                .iter()
                .map(|(_, text)| crate::memory_search_ops::scrub_secrets(text).0)
                .collect();
            match self
                .llm
                .embed_voyage_batch(&regular_texts, "document")
                .await
            {
                Ok(vecs) => {
                    for (vec_idx, (item_idx, _)) in regular_embeddings.iter().enumerate() {
                        if vec_idx < vecs.len() {
                            embed_results[*item_idx] = Some(vecs[vec_idx].clone());
                        }
                    }
                    tracing::info!(
                        "[enrichment-batcher] batch embedded {} texts in 1 API call",
                        regular_texts.len()
                    );
                }
                Err(error) => {
                    tracing::warn!("[enrichment-batcher] batch embedding failed: {error}");
                    for (item_idx, _) in &regular_embeddings {
                        record_enrichment_failure(self, &items[*item_idx], "embedding", &error);
                    }
                }
            }
        }

        // An admitted provider call must be independently cancellable: mixing
        // multiple durable owners (or ordinary, non-durable items) into one
        // provider future would let one lost lease discard the whole batch.
        for (item_idx, text) in admitted_embeddings {
            let texts = vec![crate::memory_search_ops::scrub_secrets(&text).0];
            let result = self
                .run_admitted_enrichment_egress(
                    &items[item_idx],
                    self.llm.embed_voyage_batch(&texts, "document"),
                )
                .await;
            match result {
                None => active[item_idx] = false,
                Some(Ok(vecs)) => {
                    if let Some(vector) = vecs.into_iter().next() {
                        embed_results[item_idx] = Some(vector);
                    }
                }
                Some(Err(error)) => {
                    tracing::warn!(
                        "[enrichment-batcher] admitted embedding failed for {}: {error}",
                        items[item_idx].id
                    );
                    record_enrichment_failure(self, &items[item_idx], "embedding", &error);
                }
            }
        }

        // 4. Write results back to DB
        //
        // tachi#1435 slice 4 / #2059 codex round 2 (BUG fix): enrichment can
        // add a vector/keywords/summary to an entry that was already
        // searchable when it was first saved — the FTS/keyword content a
        // subsequent search reads changes here, so a stale cached search
        // result must not survive this flush. Tracked once per BATCH (not
        // per item — see `any_field_write` below) and invalidated once after
        // the whole loop, so a fully no-op batch (e.g. every item's keyword
        // stage was `skipped` with no other field write) never pays for a
        // DELETE against a table that could not possibly be stale.
        let mut any_field_write = false;
        for (i, item) in items.iter().enumerate() {
            if !active[i] || !self.admitted_enrichment_dispatch_is_active(item) {
                continue;
            }
            let new_vec = embed_results[i].as_deref();
            let new_summary = summaries[i].as_deref();
            let new_keywords = keywords_out[i].as_deref();
            let new_entities = entities_out[i].as_deref();
            let keyword_status = keyword_status_out[i];
            let receipts = EnrichmentInvocationReceipts {
                summary: summary_receipts[i].as_ref(),
                metadata: metadata_receipts[i].as_ref(),
                keywords: keyword_receipts[i].as_ref(),
            };

            if new_vec.is_some()
                || new_summary.is_some()
                || new_keywords.is_some()
                || new_entities.is_some()
            {
                let update_action = |store: &mut MemoryStore| {
                    if let Some(intent) = item.admitted_intent.as_ref() {
                        if !item
                            .admitted_lease
                            .as_ref()
                            .is_some_and(|control| control.is_active())
                            || !Self::renew_admitted_enrichment_owner(store, intent)?
                        {
                            return Ok(false);
                        }
                    }
                    let updated = store
                        .update_enrichment_fields_with_receipts(
                            &item.id,
                            new_summary,
                            new_vec,
                            new_keywords,
                            new_entities,
                            item.revision,
                            receipts,
                        )
                        .map_err(|e| format!("Failed to update enriched entry: {e}"))?;
                    if updated && new_vec.is_some() {
                        if let Some(entry) = store
                            .get(&item.id)
                            .map_err(|e| format!("load enriched entry: {e}"))?
                        {
                            if let Err(err) =
                                crate::memory_search_ops::apply_confidence_reinforcement_links(
                                    store, &entry,
                                )
                            {
                                tracing::warn!(
                                    "[enrichment-batcher] confidence reinforcement failed for {}: {err}",
                                    item.id
                                );
                            }
                        }
                    }
                    Ok(updated)
                };

                let res = if let Some(ref project_name) = item.named_project {
                    self.with_named_project_store(project_name, update_action)
                } else if let Some(ref db_path) = item.db_path {
                    self.with_path_store(db_path, update_action)
                } else {
                    self.with_store_for_scope(item.target_db, update_action)
                };

                match res {
                    Ok(true) => {
                        // This item's enriched fields (vector/summary/keywords/
                        // entities) actually landed — searchable content
                        // changed, so this batch must invalidate below.
                        any_field_write = true;
                        // Re-apply keyword status after field update: success path
                        // clears failure metadata, so keyword stage outcome must
                        // land after that write (including failed keyword stage
                        // when other stages still succeeded).
                        if let Some(status) = keyword_status {
                            if status != "skipped" {
                                write_keyword_enrichment_status(self, item, status);
                            }
                        }
                        if new_vec.is_some() {
                            if item.admitted_intent.is_none() {
                                let contradiction_server = self.clone();
                                let contradiction_id = item.id.clone();
                                let contradiction_db = item.target_db;
                                let contradiction_project = item.named_project.clone();
                                let contradiction_path = item.db_path.clone();
                                tokio::spawn(async move {
                                    if let Err(err) =
                                        crate::memory_search_ops::apply_auto_contradiction_detection(
                                            &contradiction_server,
                                            &contradiction_id,
                                            contradiction_db,
                                            contradiction_project.as_deref(),
                                            contradiction_path.as_ref(),
                                        )
                                        .await
                                    {
                                        tracing::warn!(
                                            "[enrichment-batcher] auto contradiction detection failed for {contradiction_id}: {err}"
                                        );
                                    }
                                });

                                let agent_id_owned = item
                                    .foundry_agent_id
                                    .clone()
                                    .or_else(|| {
                                        let guard = self.agent_runtime_read();
                                        guard.agent_profile.as_ref().map(|p| p.agent_id.clone())
                                    })
                                    .unwrap_or_else(|| "system".to_string());

                                let path_prefix_owned = match item.foundry_path_prefix.clone() {
                                    Some(p) => p,
                                    None => derive_path_prefix(self, item)
                                        .unwrap_or_else(|| "/".to_string()),
                                };

                                if let Err(err) = enqueue_foundry_capture_maintenance(
                                    self,
                                    item.target_db,
                                    item.named_project.clone(),
                                    item.db_path.clone(),
                                    &agent_id_owned,
                                    &path_prefix_owned,
                                    &[item.id.clone()],
                                ) {
                                    tracing::warn!(
                                        "[enrichment-batcher] failed to enqueue foundry maintenance for {}: {err}",
                                        item.id
                                    );
                                }
                            }
                        }
                    }
                    Ok(false) => tracing::debug!(
                        "[enrichment-batcher] discarded {} (revision changed)",
                        item.id
                    ),
                    Err(e) => {
                        tracing::warn!(
                            "[enrichment-batcher] DB update failed for {}: {e}",
                            item.id
                        );
                        record_enrichment_failure(self, item, "db_update", &e);
                    }
                }
            } else if let Some(status) = keyword_status {
                // Keyword-only outcome with no field writes (failed already
                // recorded; enriched/skipped still need status visibility when
                // expand returned nothing new or pure skip).
                if status == "enriched" || status == "failed" {
                    write_keyword_enrichment_status(self, item, status);
                }
            }
        }

        for (i, item) in items.iter().enumerate() {
            if !active[i]
                || !metadata_attempted_out[i]
                || !self.admitted_enrichment_dispatch_is_active(item)
            {
                continue;
            }
            let mark = |store: &mut MemoryStore| {
                store
                    .mark_metadata_extraction_attempted(&item.id, item.revision)
                    .map_err(|e| format!("mark metadata extraction attempted: {e}"))
            };
            let res = if let Some(ref project_name) = item.named_project {
                self.with_named_project_store(project_name, mark)
            } else if let Some(ref db_path) = item.db_path {
                self.with_path_store(db_path, mark)
            } else {
                self.with_store_for_scope(item.target_db, mark)
            };
            if let Err(err) = res {
                tracing::warn!("[enrichment-batcher] {err} for {}", item.id);
            }
        }

        // Admitted auto-ingest remains pending until the revision-checked
        // enrichment write above has produced terminal entry metadata. A
        // failed/no-op stage clears only this runtime's dispatch marker so the
        // durable intent can be retried; a successful write retires the intent.
        for (i, item) in items.iter().enumerate() {
            if active[i] && self.reconcile_admitted_enrichment_intent(item) {
                self.wake_admitted_auto_ingest_completion(item);
            }
        }

        // Batch-granularity recall-cache bust (not per item — see the
        // comment at this loop's start). Shares the same choke point +
        // epoch bump as `save_memory`'s and `contradiction`'s invalidation.
        if any_field_write {
            crate::memory_search_ops::invalidate_recall_cache_after_write(self, "enrichment_flush");
        }

        tracing::info!("[enrichment-batcher] batch of {batch_size} complete");
    }
}

fn write_keyword_enrichment_status(server: &MemoryServer, item: &EnrichmentItem, status: &str) {
    if !server.admitted_enrichment_dispatch_is_active(item) {
        return;
    }
    let action = |store: &mut MemoryStore| {
        if let Some(intent) = item.admitted_intent.as_ref() {
            if !MemoryServer::renew_admitted_enrichment_owner(store, intent)? {
                return Ok(());
            }
        }
        store
            .set_keyword_enrichment_status(&item.id, status)
            .map_err(|e| format!("set keyword enrichment status: {e}"))
    };
    let res = if let Some(ref project_name) = item.named_project {
        server.with_named_project_store(project_name, action)
    } else if let Some(ref db_path) = item.db_path {
        server.with_path_store(db_path, action)
    } else {
        server.with_store_for_scope(item.target_db, action)
    };
    if let Err(err) = res {
        tracing::warn!(
            "[enrichment-batcher] failed to set keywords_status={status} for {}: {err}",
            item.id
        );
    }
}

fn record_enrichment_failure(
    server: &MemoryServer,
    item: &EnrichmentItem,
    stage: &str,
    error: &str,
) {
    if !server.admitted_enrichment_dispatch_is_active(item) {
        return;
    }
    if should_defer_enrichment_failure(stage, error) {
        tracing::warn!(
            "[enrichment-batcher] deferring transient enrichment failure for {} at stage={stage}: {error}",
            item.id
        );
        // Transient defer skips durable aggregate failure, but write-side keyword
        // enrichment must still leave an operator-visible keywords_status so the
        // attempt is not silent (#943).
        if stage == "keywords" {
            write_keyword_enrichment_status(server, item, "failed");
        }
        return;
    }

    let action = |store: &mut MemoryStore| {
        if let Some(intent) = item.admitted_intent.as_ref() {
            if !MemoryServer::renew_admitted_enrichment_owner(store, intent)? {
                return Ok(());
            }
        }
        store
            .record_enrichment_failure(&item.id, stage, error)
            .map_err(|e| format!("record enrichment failure: {e}"))
    };
    let res = if let Some(ref project_name) = item.named_project {
        server.with_named_project_store(project_name, action)
    } else if let Some(ref db_path) = item.db_path {
        server.with_path_store(db_path, action)
    } else {
        server.with_store_for_scope(item.target_db, action)
    };
    if let Err(err) = res {
        tracing::warn!(
            "[enrichment-batcher] failed to record enrichment failure for {}: {err}",
            item.id
        );
    }
}

fn should_defer_enrichment_failure(stage: &str, error: &str) -> bool {
    if stage == "db_update" {
        return false;
    }

    // Vault lock states (incl. auto-lock, previously missed) are transient and
    // deferrable; classification comes from the shared resolver. NotAuthorized/
    // Missing are intentionally NOT deferred here (auth fails closed; a missing
    // secret will not become present by retrying).
    let lower = error.to_ascii_lowercase();
    matches!(
        crate::vault_ops::classify_vault_read_error(error),
        crate::vault_ops::VaultReadState::Locked | crate::vault_ops::VaultReadState::AutoLocked
    ) || lower.contains("secret materialization failed")
        || lower.contains("temporarily unavailable")
        || lower.contains("retry after")
        || lower.contains("missing api key")
}

/// Look up the saved memory's `path` and return its parent directory as a
/// foundry path prefix. Used by the always-on enrichment fallback when the
/// caller did not pass an explicit `foundry_path_prefix`. Returns None if
/// the entry cannot be loaded; the caller falls back to "/".
fn derive_path_prefix(server: &MemoryServer, item: &EnrichmentItem) -> Option<String> {
    let lookup = |store: &mut MemoryStore| {
        store
            .get(&item.id)
            .map_err(|e| format!("derive path prefix get: {e}"))
    };
    let entry = if let Some(name) = item.named_project.as_deref() {
        server.with_named_project_store_read(name, lookup).ok()?
    } else if let Some(db_path) = item.db_path.as_ref() {
        server.with_path_store_read(db_path, lookup).ok()?
    } else {
        server
            .with_store_for_scope_read(item.target_db, lookup)
            .ok()?
    }?;
    let path = entry.path;
    if path.is_empty() {
        return Some("/".to_string());
    }

    let parent = std::path::Path::new(&path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("");
    if parent.is_empty() || parent == "." {
        Some("/".to_string())
    } else {
        Some(parent.to_string())
    }
}

#[cfg(test)]
mod tests;
