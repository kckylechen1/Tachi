use super::enrichment::enqueue_save_enrichment;
use super::entry::build_save_entry;
use super::persist::{
    find_exact_path_text_duplicate, lookup_existing_entry, mark_save_target_used,
    spawn_save_contradiction_detection, upsert_idless_save_entry, upsert_save_entry,
    upsert_wiki_projection_entry, AtomicReferenceWrite,
};
use super::response::{build_duplicate_save_response, build_save_response};
use super::validation::{validate_save_text, SaveTextValidation};
use super::write_affinity::{apply_write_affinity, AffinityNote};
use crate::memory_search_ops::auto_link::{is_training_seed, spawn_auto_linking};
use crate::memory_search_ops::text_scrub::{scrub_secrets, scrub_think_tags};
use crate::tool_params::{build_evidence_refs_v1, SaveMemoryParams};
use crate::{DbScope, MemoryServer};
use blake2::{Blake2s256, Digest};
use chrono::Utc;
use serde_json::json;

pub(super) struct AuthorizedReferenceMutations(Vec<memcore::db::ValidatedReferenceMutation>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SaveMetadataAuthority {
    Public,
    ServerVerified,
}

/// Who asked for this save — tachi#1446 signal D.
///
/// Orthogonal to [`SaveMetadataAuthority`], which answers "may this payload
/// write reserved reference metadata"; this answers "was there a caller on the
/// other end". They do not coincide: `dispatch_ops::kanban_helpers` re-saves a
/// kanban row through the `Public` metadata authority and is still the system
/// writing its own bookkeeping.
///
/// Only [`SaveInitiator::Caller`] can mark a save's target memory used, so the
/// polarity is chosen to fail safe: a call site that forgets to classify itself
/// gets `System` and under-records the signal, rather than feeding the exposure
/// loop this issue exists to cut.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SaveInitiator {
    /// An agent-facing tool call (`tachi_save`, `tachi_memory action=save`,
    /// `save_memory`) — the id in `params.id` came from outside the process.
    Caller,
    /// An in-process writer re-saving a row it owns.
    System,
}

impl AuthorizedReferenceMutations {
    pub(super) fn empty() -> Self {
        Self(Vec::new())
    }

    pub(super) fn from_authorized(mutations: Vec<memcore::db::ValidatedReferenceMutation>) -> Self {
        Self(mutations)
    }

    fn validate(references: &[String]) -> Result<Self, String> {
        crate::wiki_ops::validate_references(references)?;
        let captured_at = Utc::now().to_rfc3339();
        build_evidence_refs_v1(references, &captured_at)
            .into_iter()
            .map(|reference| {
                let target_kind = reference
                    .target_kind
                    .map(serde_json::to_value)
                    .transpose()
                    .map_err(|error| format!("serialize trusted evidence target kind: {error}"))?
                    .and_then(|value| value.as_str().map(str::to_string));
                memcore::db::ValidatedReferenceMutation::evidence(
                    reference.target_ref,
                    reference.captured_at,
                    target_kind,
                )
                .map_err(|error| format!("construct trusted evidence ref: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }
}

#[cfg(test)]
struct PreUpsertBarrier {
    entry_id: String,
    barrier: std::sync::Arc<std::sync::Barrier>,
}

#[cfg(test)]
struct PreUpsertIdentityBarrier {
    identity: String,
    barrier: std::sync::Arc<std::sync::Barrier>,
}

#[cfg(test)]
struct PreUpsertPathBarrier {
    path: String,
    barrier: std::sync::Arc<std::sync::Barrier>,
}

#[cfg(test)]
static PRE_UPSERT_BARRIER: std::sync::OnceLock<std::sync::Mutex<Option<PreUpsertBarrier>>> =
    std::sync::OnceLock::new();

#[cfg(test)]
static PRE_UPSERT_IDENTITY_BARRIER: std::sync::OnceLock<
    std::sync::Mutex<Option<PreUpsertIdentityBarrier>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
static PRE_UPSERT_PATH_BARRIER: std::sync::OnceLock<
    std::sync::Mutex<Option<PreUpsertPathBarrier>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
struct PreUpsertPause {
    entry_id: String,
    pause_trusted_append: bool,
    arrived: std::sync::Arc<std::sync::Barrier>,
    release: std::sync::Arc<std::sync::Barrier>,
}

#[cfg(test)]
static PRE_UPSERT_PAUSE: std::sync::OnceLock<std::sync::Mutex<Option<PreUpsertPause>>> =
    std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) struct PreUpsertBarrierGuard;

#[cfg(test)]
pub(crate) struct PreUpsertIdentityBarrierGuard;

#[cfg(test)]
pub(crate) struct PreUpsertPathBarrierGuard;

#[cfg(test)]
pub(crate) struct PreUpsertPauseGuard;

#[cfg(test)]
pub(crate) fn install_pre_upsert_barrier(
    entry_id: &str,
    barrier: std::sync::Arc<std::sync::Barrier>,
) -> PreUpsertBarrierGuard {
    let slot = PRE_UPSERT_BARRIER.get_or_init(|| std::sync::Mutex::new(None));
    *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(PreUpsertBarrier {
        entry_id: entry_id.to_string(),
        barrier,
    });
    PreUpsertBarrierGuard
}

#[cfg(test)]
impl Drop for PreUpsertBarrierGuard {
    fn drop(&mut self) {
        if let Some(slot) = PRE_UPSERT_BARRIER.get() {
            *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        }
    }
}

#[cfg(test)]
pub(crate) fn install_pre_upsert_identity_barrier(
    path: &str,
    text: &str,
    barrier: std::sync::Arc<std::sync::Barrier>,
) -> PreUpsertIdentityBarrierGuard {
    let slot = PRE_UPSERT_IDENTITY_BARRIER.get_or_init(|| std::sync::Mutex::new(None));
    *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
        Some(PreUpsertIdentityBarrier {
            identity: idless_save_identity(path, text),
            barrier,
        });
    PreUpsertIdentityBarrierGuard
}

#[cfg(test)]
impl Drop for PreUpsertIdentityBarrierGuard {
    fn drop(&mut self) {
        if let Some(slot) = PRE_UPSERT_IDENTITY_BARRIER.get() {
            *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        }
    }
}

#[cfg(test)]
pub(crate) fn install_pre_upsert_path_barrier(
    path: &str,
    barrier: std::sync::Arc<std::sync::Barrier>,
) -> PreUpsertPathBarrierGuard {
    let slot = PRE_UPSERT_PATH_BARRIER.get_or_init(|| std::sync::Mutex::new(None));
    *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(PreUpsertPathBarrier {
        path: path.to_string(),
        barrier,
    });
    PreUpsertPathBarrierGuard
}

#[cfg(test)]
impl Drop for PreUpsertPathBarrierGuard {
    fn drop(&mut self) {
        if let Some(slot) = PRE_UPSERT_PATH_BARRIER.get() {
            *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        }
    }
}

#[cfg(test)]
pub(crate) fn install_pre_upsert_pause(
    entry_id: &str,
    pause_trusted_append: bool,
    arrived: std::sync::Arc<std::sync::Barrier>,
    release: std::sync::Arc<std::sync::Barrier>,
) -> PreUpsertPauseGuard {
    let slot = PRE_UPSERT_PAUSE.get_or_init(|| std::sync::Mutex::new(None));
    *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(PreUpsertPause {
        entry_id: entry_id.to_string(),
        pause_trusted_append,
        arrived,
        release,
    });
    PreUpsertPauseGuard
}

#[cfg(test)]
impl Drop for PreUpsertPauseGuard {
    fn drop(&mut self) {
        if let Some(slot) = PRE_UPSERT_PAUSE.get() {
            *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        }
    }
}

#[cfg(test)]
fn wait_at_pre_upsert_barrier(entry_id: &str) {
    let barrier = PRE_UPSERT_BARRIER.get().and_then(|slot| {
        slot.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .filter(|configured| configured.entry_id == entry_id)
            .map(|configured| std::sync::Arc::clone(&configured.barrier))
    });
    if let Some(barrier) = barrier {
        barrier.wait();
    }
}

#[cfg(test)]
fn wait_at_pre_upsert_identity_barrier(identity: Option<&str>) {
    let barrier = identity.and_then(|identity| {
        PRE_UPSERT_IDENTITY_BARRIER.get().and_then(|slot| {
            slot.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
                .filter(|configured| configured.identity == identity)
                .map(|configured| std::sync::Arc::clone(&configured.barrier))
        })
    });
    if let Some(barrier) = barrier {
        barrier.wait();
    }
}

#[cfg(test)]
fn wait_at_pre_upsert_path_barrier(path: &str) {
    let barrier = PRE_UPSERT_PATH_BARRIER.get().and_then(|slot| {
        slot.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .filter(|configured| configured.path == path)
            .map(|configured| std::sync::Arc::clone(&configured.barrier))
    });
    if let Some(barrier) = barrier {
        barrier.wait();
    }
}

#[cfg(test)]
fn wait_at_pre_upsert_pause(entry_id: &str, trusted_append: bool) {
    let pause = PRE_UPSERT_PAUSE.get().and_then(|slot| {
        slot.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .filter(|configured| {
                configured.entry_id == entry_id && configured.pause_trusted_append == trusted_append
            })
            .map(|configured| {
                (
                    std::sync::Arc::clone(&configured.arrived),
                    std::sync::Arc::clone(&configured.release),
                )
            })
    });
    if let Some((arrived, release)) = pause {
        arrived.wait();
        release.wait();
    }
}

fn atomic_evidence_metadata_patch(
    existing: Option<&serde_json::Value>,
    final_metadata: &serde_json::Value,
    explicit_keys: &[String],
) -> serde_json::Map<String, serde_json::Value> {
    let Some(final_object) = final_metadata.as_object() else {
        return serde_json::Map::new();
    };
    let existing_object = existing.and_then(serde_json::Value::as_object);
    let mut patch = serde_json::Map::new();
    for key in explicit_keys {
        if !matches!(key.as_str(), "evidence_refs_v1" | "source_refs") {
            if let Some(value) = final_object.get(key) {
                patch.insert(key.clone(), value.clone());
            }
        }
    }
    for (key, value) in final_object {
        if matches!(key.as_str(), "evidence_refs_v1" | "source_refs") {
            continue;
        }
        if existing_object.and_then(|object| object.get(key)) != Some(value) {
            patch.insert(key.clone(), value.clone());
        }
    }
    patch
}

fn strip_reserved_public_metadata(metadata: &mut Option<serde_json::Value>) {
    if let Some(serde_json::Value::Object(object)) = metadata {
        object.remove("evidence_refs_v1");
        object.remove("source_refs");
        // REM receipts are owned by the evolver and source-marker seams.
        // Public/system save metadata must neither mint a pending operation
        // nor reset a source's processed marker.
        object.remove("rem");
        // Wiki operation-log ownership is established only by the internal
        // log writer, never by caller metadata on an ordinary memory row.
        object.remove("wiki_log");
        // Wiki projection lineage is derived from the active row selected in
        // the projection transaction. Public metadata cannot assert it.
        object.remove("wiki_update_of");
        object.remove("wiki_previous_revision");
    }
}

fn is_wiki_namespace(path: &str) -> bool {
    let normalized = memcore::path_router::normalize_path(path);
    normalized == "/wiki"
        || normalized.starts_with("/wiki/")
        || normalized == "/guide"
        || normalized.starts_with("/guide/")
}

const PUBLIC_WIKI_AUTHORITY_KEYS: [&str; 3] =
    ["review_receipt", "source_bundle_hash", "source_ref"];

fn is_wiki_classified(entry: &memcore::MemoryEntry) -> bool {
    is_wiki_namespace(&entry.path)
        || matches!(
            entry.category.trim().to_ascii_lowercase().as_str(),
            "wiki" | "guide"
        )
        || entry
            .domain
            .as_deref()
            .is_some_and(|domain| domain.trim().eq_ignore_ascii_case("wiki"))
        || entry
            .metadata
            .get("wiki")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
}

fn constrain_public_wiki_metadata(
    metadata: &mut serde_json::Value,
    path: &str,
    scope: &str,
) -> Result<(), String> {
    let candidate =
        crate::tool_params::build_candidate_knowledge_artifact_fields(path, scope, metadata);
    let object = metadata
        .as_object_mut()
        .ok_or_else(|| "metadata must be a JSON object for wiki-classified saves".to_string())?;

    for key in PUBLIC_WIKI_AUTHORITY_KEYS {
        object.remove(key);
    }
    object.insert("lifecycle".to_string(), json!("pending_review"));
    object.insert("status".to_string(), json!("pending_review"));
    object.insert("review_status".to_string(), json!("pending"));
    object.insert("authority".to_string(), json!("advisory"));
    if let Some(candidate) = candidate.as_object() {
        for (key, value) in candidate {
            object.insert(key.clone(), value.clone());
        }
    }
    // Generic public saves never establish playbook authority. The dedicated
    // Wiki/guide facade may create a pending playbook candidate, but this
    // lower-level public seam remains advisory-only.
    object.insert("authority".to_string(), json!("advisory"));
    Ok(())
}

fn idless_save_identity(path: &str, text: &str) -> String {
    let path = memcore::path_router::normalize_path(path);
    let mut hasher = Blake2s256::new();
    hasher.update(b"tachi:idless-memory:v1\0");
    hasher.update((path.len() as u64).to_be_bytes());
    hasher.update(path.as_bytes());
    hasher.update((text.len() as u64).to_be_bytes());
    hasher.update(text.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Render a #1041 S1 write-affinity note into the compact JSON shape
/// surfaced on the save response (`domain_affinity`), never blocking the
/// caller — a hard mismatch with no eligible store is a loud `Err` from
/// `apply_write_affinity` before a response is ever built, not a note.
fn domain_affinity_note_json(note: &AffinityNote) -> serde_json::Value {
    match note {
        AffinityNote::Unregistered { domain } => json!({
            "status": "unregistered",
            "domain": domain,
        }),
        AffinityNote::Rerouted { domain, project } => json!({
            "status": "rerouted",
            "domain": domain,
            "project": project,
        }),
    }
}

/// In-process save. tachi#1446: [`SaveInitiator::System`], because most call
/// sites of this entry point are in-process writers (`kanban_helpers`,
/// `flow_link`, `complete_ops::lessons`, dispatch eval persistence). The one
/// agent-facing caller — the `save_memory` tool wrapper — uses
/// [`handle_save_memory_from_caller`] instead.
pub(crate) async fn handle_save_memory(
    server: &MemoryServer,
    params: SaveMemoryParams,
) -> Result<String, String> {
    handle_save_memory_impl(
        server,
        params,
        AuthorizedReferenceMutations::empty(),
        SaveMetadataAuthority::Public,
        SaveInitiator::System,
        None,
        false,
    )
    .await
}

/// [`handle_save_memory`] for the agent-facing `save_memory` tool: identical
/// except that a `params.id` naming an existing memory marks that memory used
/// (tachi#1446 signal D — see `persist::mark_save_target_used`).
pub(crate) async fn handle_save_memory_from_caller(
    server: &MemoryServer,
    params: SaveMemoryParams,
) -> Result<String, String> {
    handle_save_memory_impl(
        server,
        params,
        AuthorizedReferenceMutations::empty(),
        SaveMetadataAuthority::Public,
        SaveInitiator::Caller,
        None,
        false,
    )
    .await
}

/// The `tachi_save` facade's memory arm — always a caller-initiated save
/// (`facade_save_ops::handle_tachi_save` is reached only from the `tachi_save`
/// tool, `tachi_memory action=save`, and `tachi_memory action=checkpoint`).
pub(crate) async fn handle_save_memory_with_references(
    server: &MemoryServer,
    params: SaveMemoryParams,
    references: Vec<String>,
) -> Result<String, String> {
    let evidence_refs = AuthorizedReferenceMutations::validate(&references)?;
    handle_save_memory_impl(
        server,
        params,
        evidence_refs,
        SaveMetadataAuthority::Public,
        SaveInitiator::Caller,
        None,
        false,
    )
    .await
}

pub(crate) async fn handle_save_memory_with_authorized_reference_mutations(
    server: &MemoryServer,
    params: SaveMemoryParams,
    mutations: Vec<memcore::db::ValidatedReferenceMutation>,
) -> Result<String, String> {
    handle_save_memory_impl(
        server,
        params,
        AuthorizedReferenceMutations::from_authorized(mutations),
        SaveMetadataAuthority::ServerVerified,
        SaveInitiator::System,
        None,
        false,
    )
    .await
}

/// Server-internal first-write seam for durable model-derived artifacts.
/// The receipt is typed and never passes through public JSON metadata.
#[allow(dead_code)] // retained as the text-save sibling promised by the receipt API contract
pub(crate) async fn handle_save_memory_with_authorized_reference_mutations_and_invocation(
    server: &MemoryServer,
    params: SaveMemoryParams,
    mutations: Vec<memcore::db::ValidatedReferenceMutation>,
    invocation: tachi_llm::PersistedModelInvocationReceiptV1,
) -> Result<String, String> {
    handle_save_memory_impl(
        server,
        params,
        AuthorizedReferenceMutations::from_authorized(mutations),
        SaveMetadataAuthority::ServerVerified,
        SaveInitiator::System,
        Some(invocation),
        false,
    )
    .await
}

/// Server-internal Wiki/Guide projection save. The canonical row, duplicate
/// supersession claims, and supersedes edges share one transaction.
pub(crate) async fn handle_save_memory_with_wiki_projection(
    server: &MemoryServer,
    params: SaveMemoryParams,
    mutations: Vec<memcore::db::ValidatedReferenceMutation>,
    model_invocation: Option<tachi_llm::PersistedModelInvocationReceiptV1>,
) -> Result<String, String> {
    handle_save_memory_impl(
        server,
        params,
        AuthorizedReferenceMutations::from_authorized(mutations),
        SaveMetadataAuthority::ServerVerified,
        SaveInitiator::System,
        model_invocation,
        true,
    )
    .await
}

async fn handle_save_memory_impl(
    server: &MemoryServer,
    mut params: SaveMemoryParams,
    evidence_refs: AuthorizedReferenceMutations,
    metadata_authority: SaveMetadataAuthority,
    initiator: SaveInitiator,
    model_invocation: Option<tachi_llm::PersistedModelInvocationReceiptV1>,
    wiki_projection: bool,
) -> Result<String, String> {
    strip_reserved_public_metadata(&mut params.metadata);
    params.text = scrub_think_tags(&params.text);
    params.summary = scrub_think_tags(&params.summary);
    let (safe_text, secret_redactions) = scrub_secrets(&params.text);
    let gate_warnings = match validate_save_text(&params, &safe_text)? {
        SaveTextValidation::Accepted(warnings) => warnings,
        SaveTextValidation::Rejected(body) => return Ok(body),
    };
    let requested_id = params.id.clone();
    let idless_identity = requested_id
        .is_none()
        .then(|| idless_save_identity(&params.path, &safe_text));
    let id = requested_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let timestamp = params
        .timestamp
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    let valid_from = params
        .valid_from
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| timestamp.clone());
    let requested_scope = params.scope.clone();
    let named_project = params.project.clone();
    if let Some(project) = named_project.as_deref() {
        server.prepare_named_project_store_for_write(project)?;
    }
    let (target_db, warning) = if named_project.is_some() {
        (DbScope::Project, None) // Will use named project below
    } else {
        server.resolve_write_scope(&requested_scope)
    };
    // #925: `resolve_write_scope` already detects a silent scope downgrade
    // (requested != "global" but no project DB, so it falls back to global)
    // via `warning`, but that generic `warning` string gets stripped out of
    // the compact save/checkpoint receipt (`save_receipt_value`). Surface a
    // dedicated, stable `scope`/`scope_warning` pair so the fallback stays
    // loud through every response shape.
    //
    // #1176: the actionable half of that warning depends on whether THIS
    // session is bound to a project (`session_project()` — the same signal
    // `reject_unbound_cross_project_write` gates the -32602 on). Suggesting
    // `project=<name>` to an unbound session is a circular instruction: the
    // very next call with that param gets hard-rejected.
    let session_bound = server.session_project().is_some();
    let scope_warning = warning.as_ref().map(|_| {
        crate::memory_search_ops::scope_downgrade_warning(
            &requested_scope,
            target_db.as_str(),
            session_bound,
        )
    });

    // #1041 F2: resolve whether a caller-supplied `id` already exists at the
    // PRE-gate target (see `write_affinity` module doc's F2 note — `id` is
    // an update key, not placement authority, so an id that doesn't resolve
    // here is really a new row and gets the same domain-routing scrutiny as
    // an id-less save). When the gate later determines this genuinely IS a
    // patch (skips unchanged), this same lookup is reused below instead of
    // querying twice.
    let pre_gate_existing_entry = lookup_existing_entry(
        server,
        &id,
        requested_id.is_some(),
        target_db,
        named_project.as_deref(),
    )?;
    let id_resolves_at_target = pre_gate_existing_entry.is_some();

    // #1041 S1: domain-store write affinity gate. Only acts on the ambiguous
    // default path (no CALLER-explicit project=, no id resolving to an
    // existing row at the target, resolved to the bound project store) —
    // reroutes to the domain's registered store when one is mounted,
    // refuses loudly when it isn't, or passes through unchanged when the
    // domain has no registered route at all (uncertain, fail-safe
    // permissive). See `write_affinity` module docs.
    // #1041 B2: capture the PRE-gate target before `apply_write_affinity`
    // shadows `target_db`/`named_project` with the (possibly rerouted)
    // POST-gate values, so the duplicate-lookup avoidance below can tell
    // whether the gate actually changed the target or just passed through.
    let pre_gate_target_db = target_db;
    let pre_gate_named_project = named_project.clone();
    let affinity = apply_write_affinity(
        server,
        &params,
        target_db,
        named_project.as_deref(),
        id_resolves_at_target,
    )?;
    let target_db = affinity.target_db;
    let named_project = affinity.named_project;
    let affinity_note = affinity.note;

    // #1041 S3: dedup must fire for every id-less save, `force` or not.
    // `force` bypasses the *content-quality* gates (noise filter / capture
    // gate) above — it was never meant to also waive "did I already save
    // this exact row", but the prior `!params.force &&` guard coupled the
    // two. That coupling is exactly the "sync pipe has no write-side
    // idempotency" defect: a periodic writer that passes `force=true` (to
    // get past the noise filter on short factual content) minted a fresh
    // random id on every retry because this check was skipped outright.
    // Path+text identity is unaffected by `force` from here on; a caller
    // that truly wants a second, distinct row can still pass its own `id`.
    //
    // This lookup preserves the existing fast duplicate response. It is not
    // the race boundary: the v19 id-less identity constraint below is the
    // authoritative single-winner decision when concurrent callers both miss
    // this read.
    if params.id.is_none() && !wiki_projection {
        if let Some(existing_id) = find_exact_path_text_duplicate(
            server,
            &params.path,
            &safe_text,
            target_db,
            named_project.as_deref(),
        )? {
            let response = build_duplicate_save_response(&existing_id, &params.path, target_db);
            return serde_json::to_string(&serde_json::Value::Object(response))
                .map_err(|e| format!("Failed to serialize response: {}", e));
        }
    }
    // #1041 F2/B2: when the id resolved at the PRE-gate target, the gate
    // necessarily passed through unchanged (that's exactly the skip
    // condition) — target_db/named_project are identical to what
    // `pre_gate_existing_entry` was already looked up against, so reuse it
    // instead of querying the same store twice. When it did NOT resolve
    // there, only re-query if the gate actually changed the target (a real
    // reroute, which is now never mixed with a caller-supplied id — see
    // `write_affinity`'s B2 refusal): a passthrough (same store, same
    // project) already got its answer from the pre-gate lookup (`None`,
    // since `id_resolves_at_target` is false here), so re-querying the
    // identical store for the identical id would just be a wasted duplicate
    // read.
    let existing_entry = if id_resolves_at_target {
        pre_gate_existing_entry
    } else if target_db == pre_gate_target_db && named_project == pre_gate_named_project {
        None
    } else {
        lookup_existing_entry(
            server,
            &id,
            requested_id.is_some(),
            target_db,
            named_project.as_deref(),
        )?
    };
    let enrichment_revision = existing_entry
        .as_ref()
        .map(|entry| entry.revision)
        .unwrap_or(0)
        + 1;

    let needs_summary = params.summary.is_empty();
    let needs_embedding = params.vector.is_none();
    let auto_link = params.auto_link;
    let emit_continuity = params.emit_continuity;
    let explicit_metadata_keys = params
        .metadata
        .as_ref()
        .and_then(serde_json::Value::as_object)
        .map(|object| object.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut entry = build_save_entry(
        server,
        params,
        safe_text,
        id.clone(),
        timestamp.clone(),
        valid_from,
        target_db,
        existing_entry.as_ref(),
        model_invocation.as_ref(),
    )?;

    // #1041 F3/C5: `build_save_entry` -> `inject_provenance` resolves
    // `provenance.db_path` from `target_db` alone, which only ever knows
    // this daemon's OWN bound project path — it has no visibility into ANY
    // named-project target, whether that name came from a write-affinity
    // reroute, a caller's own explicit `project=`, or simply passed through
    // unchanged. So this runs for every named-project save, not only a
    // rerouted one (see `correct_provenance_db_path_for_named_project`'s doc
    // for why narrowing this to "only reroutes" would resurrect an older,
    // broader pre-#1041 bug instead of closing it) — the invariant is
    // `provenance.db_path` always matches where the row actually landed.
    if let Some(project_name) = named_project.as_deref() {
        if let Ok(named_project_path) = server.resolve_server_named_project_db_path(project_name) {
            entry.metadata = crate::provenance::correct_provenance_db_path_for_named_project(
                entry.metadata,
                &named_project_path,
            );
        }
    }

    // Public metadata is caller-controlled. Decide from the completed row,
    // after patch inheritance and domain resolution, and retain the existing
    // row's classification as a one-way authority constraint even when a
    // public update tries to declassify the candidate.
    let metadata_removals = if metadata_authority == SaveMetadataAuthority::Public
        && (is_wiki_classified(&entry) || existing_entry.as_ref().is_some_and(is_wiki_classified))
    {
        constrain_public_wiki_metadata(&mut entry.metadata, &entry.path, &entry.scope)?;
        PUBLIC_WIKI_AUTHORITY_KEYS.to_vec()
    } else {
        Vec::new()
    };

    #[cfg(test)]
    let trusted_append = !evidence_refs.0.is_empty();
    let evidence_write = AtomicReferenceWrite {
        metadata_patch: atomic_evidence_metadata_patch(
            existing_entry.as_ref().map(|existing| &existing.metadata),
            &entry.metadata,
            &explicit_metadata_keys,
        ),
        metadata_removals,
        mutations: evidence_refs.0,
    };

    #[cfg(test)]
    wait_at_pre_upsert_barrier(&entry.id);
    #[cfg(test)]
    wait_at_pre_upsert_identity_barrier(idless_identity.as_deref());
    #[cfg(test)]
    wait_at_pre_upsert_path_barrier(&entry.path);
    #[cfg(test)]
    wait_at_pre_upsert_pause(&entry.id, trusted_append);

    let mut wiki_duplicates_superseded = None;
    let mut wiki_previous_revision = None;
    if wiki_projection {
        let result = upsert_wiki_projection_entry(
            server,
            &mut entry,
            idless_identity.as_deref(),
            target_db,
            named_project.as_deref(),
            &evidence_write,
        )?;
        wiki_duplicates_superseded = Some(result.duplicates_superseded);
        wiki_previous_revision = result.previous_revision;
        if let memcore::db::IdlessUpsertResult::Duplicate { id } = result.upsert {
            let response = build_duplicate_save_response(&id, &entry.path, target_db);
            return serde_json::to_string(&serde_json::Value::Object(response))
                .map_err(|error| format!("Failed to serialize response: {error}"));
        }
    } else if let Some(identity) = idless_identity.as_deref() {
        match upsert_idless_save_entry(
            server,
            &mut entry,
            identity,
            target_db,
            named_project.as_deref(),
            &evidence_write,
        )? {
            memcore::db::IdlessUpsertResult::Saved => {}
            memcore::db::IdlessUpsertResult::Duplicate { id } => {
                let response = build_duplicate_save_response(&id, &entry.path, target_db);
                return serde_json::to_string(&serde_json::Value::Object(response))
                    .map_err(|error| format!("Failed to serialize response: {error}"));
            }
        }
    } else {
        upsert_save_entry(
            server,
            &mut entry,
            target_db,
            named_project.as_deref(),
            &evidence_write,
        )?;
    }

    // tachi#1446 signal D. The save has committed; if a caller named an
    // existing memory's id, that memory was used. Deliberately AFTER the
    // upsert (a save that failed is not a use) and deliberately outside it (a
    // use event must never be able to fail a save — see
    // `mark_save_target_used`). `idless_identity` is `Some` only when the
    // caller supplied no id at all, so that branch can never qualify.
    if initiator == SaveInitiator::Caller && requested_id.is_some() && existing_entry.is_some() {
        if let Err(error) =
            mark_save_target_used(server, &entry.id, target_db, named_project.as_deref())
        {
            eprintln!(
                "warning: tachi#1446 use-provenance mark failed for memory {} (save itself \
                 succeeded): {error}",
                entry.id
            );
        }
    }

    // #1435 slice 3 / #2059: write-side recall-cache bust, shared with the
    // enrichment-flush and contradiction-supersede paths (see
    // `search_memory::cache::invalidate_recall_cache_after_write`'s doc for
    // the epoch guard + cross-process boundary). Both dedupe short-circuits
    // above (`find_exact_path_text_duplicate` and
    // `IdlessUpsertResult::Duplicate`) already returned before this point,
    // so an exact-duplicate no-write correctly never invalidates.
    let recall_fence =
        crate::memory_search_ops::invalidate_recall_cache_after_write(server, "save");

    let continuity_event = if emit_continuity {
        Some(crate::continuity_ops::emit_memory_saved_event(
            server,
            &entry,
            target_db,
            named_project.as_deref(),
        ))
    } else {
        None
    };

    if !needs_embedding && entry.vector.is_some() {
        spawn_save_contradiction_detection(server, id, target_db, named_project.clone());
    }

    let enrichment_enqueued = enqueue_save_enrichment(
        server,
        &entry,
        needs_embedding,
        needs_summary,
        target_db,
        named_project.clone(),
        enrichment_revision,
    );
    let mut response = build_save_response(
        &entry,
        &timestamp,
        target_db,
        named_project.as_deref(),
        enrichment_enqueued,
        needs_embedding,
        needs_summary,
        warning,
        gate_warnings,
        secret_redactions,
        &requested_scope,
        scope_warning,
        recall_fence,
    );
    if let Some(event) = continuity_event {
        response.insert("continuity_event".into(), event);
    }

    if let Some(note) = affinity_note {
        response.insert("domain_affinity".into(), domain_affinity_note_json(&note));
    }

    if let Some(count) = wiki_duplicates_superseded {
        response.insert("wiki_duplicates_superseded".into(), json!(count));
    }
    if let Some(revision) = wiki_previous_revision {
        response.insert("wiki_previous_revision".into(), json!(revision));
    }

    if auto_link && !entry.entities.is_empty() && !is_training_seed(&entry) {
        spawn_auto_linking(server, &entry, target_db, named_project);
        response.insert("auto_link".into(), json!("pending"));
    } else if auto_link && !entry.entities.is_empty() && is_training_seed(&entry) {
        response.insert("auto_link".into(), json!("skipped_training_seed"));
    }

    serde_json::to_string(&serde_json::Value::Object(response))
        .map_err(|e| format!("Failed to serialize response: {}", e))
}
