//! Recovery of historical REM Wiki draft operations.
//!
//! Weekly clustering, model synthesis, and new draft production are retired.
//! This maintenance path validates existing operation/source ledgers, completes
//! current source groups, or compensates and archives stale pending operations.
//! Existing drafts retain their review lifecycle and remain available to readers.

use serde_json::Value;

use crate::server_state::MemoryServer;
use memcore::types::MemoryEntry;
use memcore::MemoryError;

#[cfg(test)]
static FAIL_REM_WIKI_COMPLETION_ONCE: std::sync::Mutex<Option<String>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn take_rem_wiki_completion_failure(draft_id: &str) -> bool {
    let mut target = FAIL_REM_WIKI_COMPLETION_ONCE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if target.as_deref() == Some(draft_id) {
        target.take();
        true
    } else {
        false
    }
}

// ─── Constants ────────────────────────────────────────────────────────────────

const REM_WIKI_SOURCE_SET_CONTRACT: &str = "rem-wiki-source-set-v1";
const REM_WIKI_SOURCE_CLAIM_CONTRACT: &str = "rem-wiki-source-claim-v1";

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize)]
struct RemSourceStore {
    identity: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemSourceRole {
    Global,
    Project,
}

#[derive(Clone, Debug)]
struct RuntimeRemSourceStores {
    global: RemSourceStore,
    project: Option<RemSourceStore>,
    shared_route: RemSourceRole,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize)]
struct RemSourceRef {
    store: RemSourceStore,
    id: String,
    revision: i64,
}

type RemSourceRevision = (String, i64);
type RoutedRemSources = (Vec<RemSourceRevision>, Vec<RemSourceRevision>);

#[derive(serde::Serialize)]
struct RemSourceSetIdentityV1<'a> {
    contract: &'static str,
    producer_version: &'a str,
    sources: &'a [RemSourceRef],
}

#[derive(serde::Serialize)]
struct RemSourceClaimIdentityV1<'a> {
    contract: &'static str,
    source: &'a RemSourceRef,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RemRecoveryReport {
    pub completed: usize,
    pub aborted_stale: usize,
    pub foreign_pending_skipped: usize,
}

// ─── Source-store identity ───────────────────────────────────────────────────

fn rem_source_store_identities(
    global_path: &std::path::Path,
    project_path: Option<&std::path::Path>,
) -> Result<RuntimeRemSourceStores, String> {
    let mut paths = vec![global_path.to_path_buf()];
    if let Some(project_path) = project_path {
        paths.push(project_path.to_path_buf());
    }
    let bindings = crate::physical_db_identity::physical_db_bindings_for_paths(&paths)?;
    let global = RemSourceStore {
        identity: bindings[0].physical_id.clone(),
    };
    let project = project_path.map(|_| RemSourceStore {
        identity: bindings[1].physical_id.clone(),
    });
    let shared_route = if project.as_ref() == Some(&global)
        && bindings[1].is_primary_alias
        && !bindings[0].is_primary_alias
    {
        RemSourceRole::Project
    } else {
        RemSourceRole::Global
    };
    Ok(RuntimeRemSourceStores {
        global,
        project,
        shared_route,
    })
}

fn runtime_rem_source_stores(server: &MemoryServer) -> Result<RuntimeRemSourceStores, String> {
    let global_path = server.global_db_path_buf();
    let project_path = server.project_db_path_buf();
    let runtime_stores = rem_source_store_identities(&global_path, project_path.as_deref())?;
    let store_identity = |store: &mut memcore::MemoryStore, role: &str| {
        store
            .opened_physical_db_identity()
            .map(str::to_string)
            .ok_or_else(|| format!("REM {role} store has no opened physical identity"))
    };
    let global_write_identity =
        server.with_global_store(|store| store_identity(store, "global"))?;
    let global_read_identity =
        server.with_global_store_read(|store| store_identity(store, "global read-pool"))?;
    if global_write_identity != runtime_stores.global.identity
        || global_read_identity != runtime_stores.global.identity
    {
        return Err(format!(
            "REM global store path identity changed after open: opened write={global_write_identity}, read={global_read_identity}, current={}",
            runtime_stores.global.identity
        ));
    }
    if let Some(project_store) = runtime_stores.project.as_ref() {
        let project_write_identity =
            server.with_project_store(|store| store_identity(store, "project"))?;
        let project_read_identity =
            server.with_project_store_read(|store| store_identity(store, "project read-pool"))?;
        if project_write_identity != project_store.identity
            || project_read_identity != project_store.identity
        {
            return Err(format!(
                "REM project store path identity changed after open: opened write={project_write_identity}, read={project_read_identity}, current={}",
                project_store.identity
            ));
        }
    }
    Ok(runtime_stores)
}

fn run_checked_rem_source_action<T>(
    store: &mut memcore::MemoryStore,
    path: &std::path::Path,
    expected: &RemSourceStore,
    role: &str,
    action: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    let verify = |store: &memcore::MemoryStore, phase: &str| {
        store
            .verify_opened_physical_db_identity(path)
            .map_err(|error| format!("REM {role} store identity check failed {phase}: {error}"))?;
        let opened = store
            .opened_physical_db_identity()
            .ok_or_else(|| format!("REM {role} store has no opened physical identity"))?;
        if opened != expected.identity {
            return Err(format!(
                "REM {role} store does not match the operation source identity {phase}"
            ));
        }
        Ok(())
    };
    verify(store, "before operation")?;
    let result = action(store);
    let post = verify(store, "after operation");
    match (result, post) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(identity_error)) => Err(format!(
            "{error}; additionally, the REM physical identity invariant failed after the operation: {identity_error}"
        )),
    }
}

fn with_rem_global_store<T>(
    server: &MemoryServer,
    expected: &RemSourceStore,
    action: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    let path = server.global_db_path_buf();
    server.with_global_store(|store| {
        run_checked_rem_source_action(store, &path, expected, "global", action)
    })
}

fn with_rem_global_store_read<T>(
    server: &MemoryServer,
    expected: &RemSourceStore,
    action: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    let path = server.global_db_path_buf();
    server.with_global_store_read(|store| {
        run_checked_rem_source_action(store, &path, expected, "global read", action)
    })
}

fn with_rem_project_store<T>(
    server: &MemoryServer,
    expected: &RemSourceStore,
    action: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    let path = server
        .project_db_path_buf()
        .ok_or_else(|| "REM project store is unavailable".to_string())?;
    server.with_project_store(|store| {
        run_checked_rem_source_action(store, &path, expected, "project", action)
    })
}

fn with_rem_project_store_read<T>(
    server: &MemoryServer,
    expected: &RemSourceStore,
    action: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    let path = server
        .project_db_path_buf()
        .ok_or_else(|| "REM project store is unavailable".to_string())?;
    server.with_project_store_read(|store| {
        run_checked_rem_source_action(store, &path, expected, "project read", action)
    })
}

fn stable_rem_draft_id_for_version(sources: &[RemSourceRef], producer_version: &str) -> String {
    let payload = serde_json::to_vec(&RemSourceSetIdentityV1 {
        contract: REM_WIKI_SOURCE_SET_CONTRACT,
        producer_version,
        sources,
    })
    .expect("serializing the REM source-set identity cannot fail");
    format!(
        "wiki-rem:{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, &payload)
    )
}

fn stable_rem_source_claim(source: &RemSourceRef) -> (String, String) {
    let identity = serde_json::to_string(&RemSourceClaimIdentityV1 {
        contract: REM_WIKI_SOURCE_CLAIM_CONTRACT,
        source,
    })
    .expect("serializing a REM source claim cannot fail");
    let key = format!(
        "rem-source:{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, identity.as_bytes())
    );
    (key, identity)
}

fn rem_identity_conflict(draft_id: &str, reason: &str) -> MemoryError {
    MemoryError::InvalidArg(format!(
        "rem_wiki_identity_conflict: source-set identity collision for {draft_id}: {reason}"
    ))
}

fn stored_rem_sources(entry: &MemoryEntry) -> Result<Vec<RemSourceRef>, MemoryError> {
    let sources: Vec<RemSourceRef> = serde_json::from_value(
        entry
            .metadata
            .pointer("/rem/sources")
            .cloned()
            .ok_or_else(|| rem_identity_conflict(&entry.id, "sources are missing"))?,
    )
    .map_err(|_| rem_identity_conflict(&entry.id, "sources are malformed"))?;
    if sources.is_empty() {
        return Err(rem_identity_conflict(&entry.id, "source set is empty"));
    }
    Ok(sources)
}

fn validate_existing_rem_draft(
    existing: &MemoryEntry,
    draft_id: &str,
    sources: &[RemSourceRef],
) -> Result<(), MemoryError> {
    let rem_string = |key: &str| {
        existing
            .metadata
            .pointer(&format!("/rem/{key}"))
            .and_then(Value::as_str)
    };
    let require = |condition: bool, reason: &str| {
        condition
            .then_some(())
            .ok_or_else(|| rem_identity_conflict(draft_id, reason))
    };
    require(existing.id == draft_id, "row id is not canonical")?;
    require(existing.source == "wiki", "row source is not wiki")?;
    require(
        existing.path.starts_with("/wiki/drafts/"),
        "row is outside the draft namespace",
    )?;
    require(
        existing
            .metadata
            .get("artifact_kind")
            .and_then(Value::as_str)
            == Some("draft"),
        "artifact_kind is not draft",
    )?;
    require(
        rem_string("producer") == Some("weekly_wiki_evolver"),
        "producer is mismatched",
    )?;
    require(
        rem_string("source_set_contract") == Some(REM_WIKI_SOURCE_SET_CONTRACT),
        "source-set contract is mismatched",
    )?;
    let producer_version = rem_string("producer_version")
        .filter(|version| !version.trim().is_empty() && version.len() <= 128)
        .ok_or_else(|| rem_identity_conflict(draft_id, "producer version is invalid"))?;
    require(
        rem_string("operation_id") == Some(draft_id),
        "operation id is mismatched",
    )?;
    require(
        matches!(
            rem_string("operation_status"),
            Some("pending_sources" | "complete")
        ),
        "operation status is invalid",
    )?;
    require(
        existing
            .metadata
            .get("review_status")
            .and_then(Value::as_str)
            == Some("pending"),
        "review status is not pending",
    )?;
    require(
        crate::provenance::trusted_existing_model_invocation(&existing.metadata).is_some(),
        "typed model invocation receipt is missing",
    )?;
    require(
        stored_rem_sources(existing)? == sources,
        "stored source set is mismatched",
    )?;
    require(
        stable_rem_draft_id_for_version(sources, producer_version) == draft_id,
        "canonical fields do not recompute to the occupied id",
    )?;
    if existing.archived {
        return Err(rem_identity_conflict(draft_id, "occupied row is archived"));
    }
    Ok(())
}

fn ensure_rem_draft_winner(
    server: &MemoryServer,
    draft_id: &str,
    sources: &[RemSourceRef],
) -> Result<(), String> {
    let expected_claims = sources
        .iter()
        .map(stable_rem_source_claim)
        .collect::<Vec<_>>();
    server.with_named_project_store_identity_checked("wiki", |store| {
        store
            .with_immutable_supersession_transaction(|operation| {
                let existing = operation
                    .get_memory(draft_id)?
                    .ok_or_else(|| rem_identity_conflict(draft_id, "operation row is missing"))?;
                validate_existing_rem_draft(&existing, draft_id, sources)?;
                operation.validate_rem_source_claims_for_draft(draft_id, &expected_claims)?;
                if !operation.memory_is_active_unsuperseded(draft_id)? {
                    return Err(rem_identity_conflict(
                        draft_id,
                        "occupied row is archived or superseded",
                    ));
                }
                Ok(())
            })
            .map_err(|error| format!("validate REM draft operation: {error}"))
    })
}

fn complete_rem_operation(
    server: &MemoryServer,
    draft_id: &str,
    sources: &[RemSourceRef],
) -> Result<bool, String> {
    ensure_rem_draft_winner(server, draft_id, sources)?;
    let now = memcore::time_compat::rfc3339_compat(jiff::Timestamp::now());
    let runtime_stores = runtime_rem_source_stores(server)?;
    let (global_sources, project_sources) = route_rem_sources(sources, &runtime_stores, draft_id)?;

    if !project_sources.is_empty() && !server.has_project_db() {
        return Err(format!(
            "complete REM project source group: bound project store is unavailable for {draft_id}"
        ));
    }

    if !global_sources.is_empty() {
        if let Err(error) = with_rem_global_store(server, &runtime_stores.global, |store| {
            store
                .mark_rem_processed_for_draft_at_revisions(&global_sources, &now, draft_id)
                .map_err(|error| format!("mark global REM sources: {error}"))
        })
        .map_err(|error| format!("complete REM global source group: {error}"))
        {
            return fail_after_rem_source_completion(
                server,
                &runtime_stores,
                draft_id,
                &global_sources,
                &project_sources,
                error,
            )
            .map(|()| false);
        }
    }
    if !project_sources.is_empty() {
        let project_store = runtime_stores.project.as_ref().ok_or_else(|| {
            format!("complete REM project source group: project identity missing for {draft_id}")
        })?;
        if let Err(error) = with_rem_project_store(server, project_store, |store| {
            store
                .mark_rem_processed_for_draft_at_revisions(&project_sources, &now, draft_id)
                .map_err(|error| format!("mark project REM sources: {error}"))
        })
        .map_err(|error| format!("complete REM project source group: {error}"))
        {
            return fail_after_rem_source_completion(
                server,
                &runtime_stores,
                draft_id,
                &global_sources,
                &project_sources,
                error,
            )
            .map(|()| false);
        }
    }
    #[cfg(test)]
    if take_rem_wiki_completion_failure(draft_id) {
        return fail_after_rem_source_completion(
            server,
            &runtime_stores,
            draft_id,
            &global_sources,
            &project_sources,
            "injected REM Wiki completion failure".to_string(),
        )
        .map(|()| false);
    }
    let completed_now = match server.with_named_project_store_identity_checked("wiki", |store| {
        store
            .complete_rem_wiki_operation(draft_id, &now)
            .map_err(|error| format!("complete REM draft receipt: {error}"))
    }) {
        Ok(completed_now) => completed_now,
        Err(error) => {
            return fail_after_rem_source_completion(
                server,
                &runtime_stores,
                draft_id,
                &global_sources,
                &project_sources,
                error,
            )
            .map(|()| false);
        }
    };
    Ok(completed_now)
}

fn fail_after_rem_source_completion(
    server: &MemoryServer,
    runtime_stores: &RuntimeRemSourceStores,
    draft_id: &str,
    global_sources: &[RemSourceRevision],
    project_sources: &[RemSourceRevision],
    primary_error: String,
) -> Result<(), String> {
    match compensate_rem_source_markers(
        server,
        runtime_stores,
        draft_id,
        global_sources,
        project_sources,
    ) {
        Ok(()) => Err(format!(
            "{primary_error}; REM source markers were compensated for pending operation {draft_id}"
        )),
        Err(error) => Err(format!(
            "{primary_error}; REM source marker compensation failed for pending operation {draft_id}: {error}"
        )),
    }
}

fn compensate_rem_source_markers(
    server: &MemoryServer,
    runtime_stores: &RuntimeRemSourceStores,
    draft_id: &str,
    global_sources: &[RemSourceRevision],
    project_sources: &[RemSourceRevision],
) -> Result<(), String> {
    let mut rollback_errors = Vec::new();
    if !project_sources.is_empty() {
        let rollback = runtime_stores
            .project
            .as_ref()
            .ok_or_else(|| "REM project identity is unavailable during compensation".to_string())
            .and_then(|project_store| {
                with_rem_project_store(server, project_store, |store| {
                    store
                        .rollback_rem_processed_for_draft_at_revisions(project_sources, draft_id)
                        .map_err(|error| format!("rollback project REM sources: {error}"))
                })
            })
            .map_err(|error| format!("compensate REM project source group: {error}"));
        if let Err(error) = rollback {
            rollback_errors.push(error);
        }
    }
    if !global_sources.is_empty() {
        if let Err(error) = with_rem_global_store(server, &runtime_stores.global, |store| {
            store
                .rollback_rem_processed_for_draft_at_revisions(global_sources, draft_id)
                .map_err(|error| format!("rollback global REM sources: {error}"))
        })
        .map_err(|error| format!("compensate REM global source group: {error}"))
        {
            rollback_errors.push(error);
        }
    }
    if rollback_errors.is_empty() {
        Ok(())
    } else {
        Err(rollback_errors.join("; "))
    }
}

fn rem_sources_belong_to_runtime_stores(
    sources: &[RemSourceRef],
    runtime_stores: &RuntimeRemSourceStores,
) -> bool {
    !sources.is_empty()
        && sources.iter().all(|source| {
            source.store == runtime_stores.global
                || runtime_stores.project.as_ref() == Some(&source.store)
        })
}

fn rem_sources_owned_by_runtime_count(
    sources: &[RemSourceRef],
    runtime_stores: &RuntimeRemSourceStores,
) -> usize {
    sources
        .iter()
        .filter(|source| {
            source.store == runtime_stores.global
                || runtime_stores.project.as_ref() == Some(&source.store)
        })
        .count()
}

fn route_rem_sources(
    sources: &[RemSourceRef],
    runtime_stores: &RuntimeRemSourceStores,
    draft_id: &str,
) -> Result<RoutedRemSources, String> {
    let mut global_sources = Vec::new();
    let mut project_sources = Vec::new();
    let shared_physical_store = runtime_stores.project.as_ref() == Some(&runtime_stores.global);
    for source in sources {
        let marker = (source.id.clone(), source.revision);
        if source.store == runtime_stores.global {
            if shared_physical_store && runtime_stores.shared_route == RemSourceRole::Project {
                project_sources.push(marker);
            } else {
                global_sources.push(marker);
            }
        } else if runtime_stores.project.as_ref() == Some(&source.store) {
            project_sources.push(marker);
        } else {
            return Err(format!(
                "REM source store identity mismatch for {draft_id}: {}",
                source.store.identity
            ));
        }
    }
    Ok((global_sources, project_sources))
}

fn rem_source_revisions_are_current(
    server: &MemoryServer,
    runtime_stores: &RuntimeRemSourceStores,
    global_sources: &[RemSourceRevision],
    project_sources: &[RemSourceRevision],
) -> Result<bool, String> {
    let group_is_current = |store: &mut memcore::MemoryStore,
                            sources: &[RemSourceRevision],
                            role: &str|
     -> Result<bool, String> {
        for (id, expected_revision) in sources {
            let Some(source) = store
                .get(id)
                .map_err(|error| format!("read {role} REM source {id}: {error}"))?
            else {
                return Ok(false);
            };
            if source.archived || source.revision != *expected_revision {
                return Ok(false);
            }
        }
        Ok(true)
    };
    if !global_sources.is_empty()
        && !with_rem_global_store_read(server, &runtime_stores.global, |store| {
            group_is_current(store, global_sources, "global")
        })?
    {
        return Ok(false);
    }
    if !project_sources.is_empty() {
        let project_store = runtime_stores
            .project
            .as_ref()
            .ok_or_else(|| "REM project source identity is unavailable".to_string())?;
        if !with_rem_project_store_read(server, project_store, |store| {
            group_is_current(store, project_sources, "project")
        })? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn rem_source_markers_exist(
    server: &MemoryServer,
    runtime_stores: &RuntimeRemSourceStores,
) -> Result<bool, String> {
    if with_rem_global_store_read(server, &runtime_stores.global, |store| {
        store
            .rem_processed_source_markers()
            .map(|markers| !markers.is_empty())
            .map_err(|error| format!("scan global REM source markers: {error}"))
    })? {
        return Ok(true);
    }
    if server.has_project_db() && runtime_stores.project.as_ref() != Some(&runtime_stores.global) {
        let project_store = runtime_stores
            .project
            .as_ref()
            .ok_or_else(|| "REM project source identity is unavailable".to_string())?;
        if with_rem_project_store_read(server, project_store, |store| {
            store
                .rem_processed_source_markers()
                .map(|markers| !markers.is_empty())
                .map_err(|error| format!("scan project REM source markers: {error}"))
        })? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn rem_source_markers(
    server: &MemoryServer,
    runtime_stores: &RuntimeRemSourceStores,
) -> Result<Vec<(RemSourceStore, String, String, i64)>, String> {
    let global_markers = with_rem_global_store_read(server, &runtime_stores.global, |store| {
        store
            .rem_processed_source_markers()
            .map_err(|error| format!("scan global REM source markers: {error}"))
    })?;
    let mut markers = global_markers
        .into_iter()
        .map(|(source_id, draft_id, revision)| {
            (runtime_stores.global.clone(), source_id, draft_id, revision)
        })
        .collect::<Vec<_>>();
    if server.has_project_db() && runtime_stores.project.as_ref() != Some(&runtime_stores.global) {
        let project_store = runtime_stores
            .project
            .as_ref()
            .ok_or_else(|| "REM project source identity is unavailable".to_string())?;
        let project_markers = with_rem_project_store_read(server, project_store, |store| {
            store
                .rem_processed_source_markers()
                .map_err(|error| format!("scan project REM source markers: {error}"))
        })?;
        markers.extend(
            project_markers
                .into_iter()
                .map(|(source_id, draft_id, revision)| {
                    (project_store.clone(), source_id, draft_id, revision)
                }),
        );
    }
    Ok(markers)
}

fn validate_rem_source_marker_ledgers(
    server: &MemoryServer,
    runtime_stores: &RuntimeRemSourceStores,
) -> Result<(), String> {
    let markers = rem_source_markers(server, runtime_stores)?;
    if markers.is_empty() {
        return Ok(());
    }
    server.with_named_project_store_read_identity_checked("wiki", |wiki_store| {
        for (source_store, source_id, draft_id, source_revision) in &markers {
            let operation = wiki_store
                .get_with_options(draft_id, true)
                .map_err(|error| format!("read REM source-marker ledger {draft_id}: {error}"))?
                .ok_or_else(|| {
                    format!(
                        "REM source marker {source_id} names missing Wiki operation ledger {draft_id}"
                    )
                })?;
            let sources = stored_rem_sources(&operation)
                .map_err(|error| format!("validate REM source-marker ledger {draft_id}: {error}"))?;
            validate_existing_rem_draft(&operation, draft_id, &sources)
                .map_err(|error| format!("validate REM source-marker ledger {draft_id}: {error}"))?;
            if !sources
                .iter()
                .any(|source| {
                    source.store == *source_store
                        && source.id == *source_id
                        && source.revision == *source_revision
                })
            {
                return Err(format!(
                    "REM source marker {source_id} revision {source_revision} is absent from Wiki operation ledger {draft_id}"
                ));
            }
        }
        Ok(())
    })
}

/// Finish or compensate persisted REM operations without producing new drafts.
///
/// The manifest daemon retains its weekly maintenance schedule until historical
/// cross-store source claims have been reconciled on their owning runtimes.
pub(crate) fn recover_pending_rem_operations(
    server: &MemoryServer,
) -> Result<RemRecoveryReport, String> {
    let wiki_path = MemoryServer::resolve_existing_named_project_db_path_in_home(
        "wiki",
        &server.tachi_home_dir(),
    )
    .map_err(|error| format!("resolve REM Wiki operation store: {error}"))?;
    let runtime_stores = runtime_rem_source_stores(server)?;
    if wiki_path.is_none() {
        if rem_source_markers_exist(server, &runtime_stores)? {
            return Err(
                "REM Wiki operation store is unavailable but REM source markers exist; cannot establish the pending-operation ledger"
                    .to_string(),
            );
        }
        return Ok(RemRecoveryReport::default());
    }
    validate_rem_source_marker_ledgers(server, &runtime_stores)?;
    let mut report = RemRecoveryReport::default();
    let mut after: Option<(String, String)> = None;
    const PAGE_SIZE: usize = 500;
    loop {
        let pending = server.with_named_project_store_read_identity_checked("wiki", |store| {
            store
                .pending_rem_wiki_operations_after(
                    after
                        .as_ref()
                        .map(|(timestamp, id)| (timestamp.as_str(), id.as_str())),
                    PAGE_SIZE,
                )
                .map_err(|error| format!("list pending REM draft operations: {error}"))
        })?;
        if pending.is_empty() {
            return Ok(report);
        }
        let page_len = pending.len();
        let last = pending
            .last()
            .map(|entry| (entry.timestamp.clone(), entry.id.clone()))
            .expect("a non-empty REM operation page has a final row");
        for entry in pending {
            let sources = stored_rem_sources(&entry)
                .map_err(|error| format!("recover REM draft {} source set: {error}", entry.id))?;
            if !rem_sources_belong_to_runtime_stores(&sources, &runtime_stores) {
                let owned_sources = rem_sources_owned_by_runtime_count(&sources, &runtime_stores);
                if owned_sources == 0 {
                    report.foreign_pending_skipped += 1;
                    continue;
                }
                return Err(format!(
                    "recover REM draft {}: source set mixes current and foreign physical stores",
                    entry.id
                ));
            }
            validate_existing_rem_draft(&entry, &entry.id, &sources)
                .map_err(|error| format!("recover REM draft {}: {error}", entry.id))?;
            let (global_sources, project_sources) =
                route_rem_sources(&sources, &runtime_stores, &entry.id)?;
            if !rem_source_revisions_are_current(
                server,
                &runtime_stores,
                &global_sources,
                &project_sources,
            )? {
                compensate_rem_source_markers(
                    server,
                    &runtime_stores,
                    &entry.id,
                    &global_sources,
                    &project_sources,
                )
                .map_err(|error| {
                    format!(
                        "recover REM draft {}: stale source marker compensation failed; keeping operation and source claims pending: {error}",
                        entry.id
                    )
                })?;
                let aborted_at = memcore::time_compat::rfc3339_compat(jiff::Timestamp::now());
                server.with_named_project_store_identity_checked("wiki", |store| {
                    store
                        .abort_stale_rem_wiki_operation(&entry.id, &aborted_at)
                        .map_err(|error| {
                            format!("abort stale REM draft operation {}: {error}", entry.id)
                        })
                })?;
                eprintln!(
                    "[wiki_evolver] warn: archived stale pending REM operation {} and released its source claims",
                    entry.id
                );
                report.aborted_stale += 1;
                continue;
            }
            let completed_now = complete_rem_operation(server, &entry.id, &sources)?;
            if completed_now {
                crate::wiki_ops::append_wiki_log(
                    server,
                    "write",
                    &format!(
                        "{} | recovered weekly REM draft completion from {} source(s)",
                        entry.path,
                        sources.len()
                    ),
                );
            }
            report.completed += 1;
        }
        if page_len < PAGE_SIZE {
            return Ok(report);
        }
        after = Some(last);
    }
}

#[cfg(test)]
mod rem_identity_tests;
