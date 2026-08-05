use crate::kanban::{gc_expired_kanban_cards, DEFAULT_KANBAN_GC_MAX_AGE_DAYS};
use crate::shared_defs::{slim_entry, slim_entry_with_enrichment};
use crate::tool_params::{
    ArchiveMemoryParams, DeleteMemoryParams, GetMemoryParams, ListMemoriesParams,
};
use crate::{DbScope, MemoryServer};
use memcore::{GcConfig, MemoryEntry, MemoryStore};
use serde_json::json;
use std::collections::HashMap;

/// tachi#1561 (L1/L3): `get` is an id-addressed read surface and carried no
/// namespace filter at all — any internal bookkeeping row (wiki `_log`,
/// reserved `wiki-rem:` operation drafts, recall-cache rows, anchors) came
/// back with its full body as long as the caller knew the id.
///
/// The rejection reuses [`memcore::is_internal_only_row`] — deliberately
/// **not** [`memcore::is_namespace_search_noise`], which `search` filters
/// candidates through. That predicate is scoped to "should this row surface
/// in an unaddressed listing/search", so at `path_prefix = None` it also
/// drops kanban cards, handoff notes, and continuity-projection rows — the
/// owner's own content, not internal bookkeeping — because an id-addressed
/// `get` has no `path_prefix` to opt back in with. Applying it here would
/// make "fetch the kanban card whose id I already have" indistinguishable
/// from a leak. `is_internal_only_row` covers only rows the store's own
/// storage layer produced (Wiki REM drafts, the Wiki operation log, the
/// recall-rerank cache, anchor plumbing) — never something a caller who
/// already holds the id should be denied.
///
/// A rejected row is dropped, **not** reported: the caller sees the ordinary
/// per-store miss and, at the end of the chain, the existing
/// `{"error": "Memory not found"}` shape. No new error kind, and no way to
/// tell "absent" from "present but withheld".
fn readable_entry(entry: Option<MemoryEntry>) -> Option<MemoryEntry> {
    entry.filter(|entry| !memcore::is_internal_only_row(entry))
}

/// tachi#1561 (L2/L7): server-side list surfaces (`list_memories` here,
/// `sync_memories` in `pipeline_ops::sync`) return raw `list_by_path` rows.
/// They filter through the same namespace predicate search uses, passing the
/// caller's `path_prefix` so every existing explicit opt-in survives:
/// `/recall-cache*` (see `memcore::path_prefix_opts_into_recall_cache`, and
/// its server-side mirror `recall_cache_recall_opted_in`), `/kanban*`,
/// `/handoff*`, and the continuity-projection prefixes. Scoped browsing keeps
/// working; unscoped listing stops dumping bookkeeping rows.
///
/// The `retain` call stays at each site (the surfaces differ in how they
/// obtain the prefix and in what they wrap the entry in); only the predicate
/// is shared, so there is still exactly one definition of "listable".
///
/// tachi#1569 made this the **second** line, not the only one: the list
/// routes in `memcore` now exclude the Wiki store's internal rows in SQL,
/// keyed on store identity, so those rows no longer consume the caller's
/// `LIMIT` before this filter runs. This filter is deliberately kept — it is
/// wider (kanban, handoff and continuity projections are noise on a listing
/// surface but are not Wiki-corpus bookkeeping), and it is the only line for
/// stores that are not the Wiki corpus.
///
/// tachi#1561 residual: also drops a Wiki/guide-classified row (`/wiki`
/// path, `category`/`domain` "wiki"/"guide", or `metadata.wiki` — see
/// `memcore::namespace::Surface::Docs`) whose derived lifecycle is not
/// default-retrievable (drafts, etc — see
/// `memcore::is_non_default_retrievable_wiki_row`). Unlike the Wiki search
/// leg (`memcore::SearchOptions::bypass_wiki_lifecycle_gate`), `list_memories`
/// has no `requested_lifecycle` escape hatch of its own, so this check is
/// unconditional here — there is no wiki_ops caller of this function that
/// needs to see drafts (wiki_ops's own listing goes through
/// `list_user_facing_wiki_entries`, not this route).
pub(crate) fn is_listable_row(entry: &MemoryEntry, path_prefix: Option<&str>) -> bool {
    !memcore::is_namespace_search_noise(entry, path_prefix)
        && !memcore::is_non_default_retrievable_wiki_row(entry)
}

pub(crate) async fn handle_get_memory(
    server: &MemoryServer,
    params: GetMemoryParams,
) -> Result<String, String> {
    // Check named project DB first, then default project DB
    if let Some(ref project_name) = params.project {
        let project_entry = server.with_named_project_store_read(project_name, |store| {
            store
                .get_with_options(&params.id, params.include_archived)
                .map_err(|e| {
                    format!(
                        "Failed to get memory from project '{}': {}",
                        project_name, e
                    )
                })
        })?;

        if let Some(entry) = readable_entry(project_entry) {
            return serde_json::to_string(&slim_entry_with_enrichment(
                server,
                &entry,
                DbScope::Project,
                Some(project_name.as_str()),
            ))
            .map_err(|e| format!("Failed to serialize: {}", e));
        }
    } else if server.has_project_db() {
        let project_entry = server.with_project_store_read(|store| {
            store
                .get_with_options(&params.id, params.include_archived)
                .map_err(|e| format!("Failed to get memory from project DB: {}", e))
        })?;

        if let Some(entry) = readable_entry(project_entry) {
            return serde_json::to_string(&slim_entry_with_enrichment(
                server,
                &entry,
                DbScope::Project,
                None,
            ))
            .map_err(|e| format!("Failed to serialize: {}", e));
        }
    }

    if params.project.is_none() {
        if let Some(project_name) = crate::memory_search_ops::resolve_workspace_named_project() {
            if crate::memory_search_ops::named_project_db_exists(server, &project_name) {
                let project_entry =
                    server.with_named_project_store_read(&project_name, |store| {
                        store
                            .get_with_options(&params.id, params.include_archived)
                            .map_err(|e| {
                                format!(
                                    "Failed to get memory from named project '{}': {}",
                                    project_name, e
                                )
                            })
                    })?;

                if let Some(entry) = readable_entry(project_entry) {
                    return serde_json::to_string(&slim_entry_with_enrichment(
                        server,
                        &entry,
                        DbScope::Project,
                        Some(project_name.as_str()),
                    ))
                    .map_err(|e| format!("Failed to serialize: {}", e));
                }
            }
        }
    }

    let global_entry = server.with_global_store_read(|store| {
        store
            .get_with_options(&params.id, params.include_archived)
            .map_err(|e| format!("Failed to get memory from global DB: {}", e))
    })?;

    match readable_entry(global_entry) {
        Some(entry) => serde_json::to_string(&slim_entry_with_enrichment(
            server,
            &entry,
            DbScope::Global,
            None,
        ))
        .map_err(|e| format!("Failed to serialize: {}", e)),
        None => serde_json::to_string(&json!({
            "error": "Memory not found"
        }))
        .map_err(|e| format!("Failed to serialize: {}", e)),
    }
}

pub(crate) async fn handle_list_memories(
    server: &MemoryServer,
    params: ListMemoriesParams,
) -> Result<String, String> {
    let mut combined_entries: Vec<(MemoryEntry, DbScope)> = Vec::new();

    if let Some(ref project_name) = params.project {
        let global_entries = server.with_global_store_read(|store| {
            store
                .list_by_path(&params.path_prefix, params.limit, params.include_archived)
                .map_err(|e| format!("Failed to list memories from global DB: {}", e))
        })?;
        combined_entries.extend(global_entries.into_iter().map(|e| (e, DbScope::Global)));

        let project_entries = server.with_named_project_store_read(project_name, |store| {
            store
                .list_by_path(&params.path_prefix, params.limit, params.include_archived)
                .map_err(|e| {
                    format!(
                        "Failed to list memories from project '{}': {}",
                        project_name, e
                    )
                })
        })?;
        combined_entries.extend(project_entries.into_iter().map(|e| (e, DbScope::Project)));
        // tachi#1561 (L2): drop internal rows *before* the limit, so a prefix
        // dense in bookkeeping rows cannot starve the caller's budget.
        // tachi#1569 moved the Wiki-corpus half of that job into SQL (the
        // named-project store above is where `project = "wiki"` lands); this
        // remains as the wider second line — see `is_listable_row`.
        combined_entries
            .retain(|(entry, _)| is_listable_row(entry, Some(params.path_prefix.as_str())));
        combined_entries.sort_by(|a, b| b.0.timestamp.cmp(&a.0.timestamp));
        combined_entries.truncate(params.limit);
        let slim: Vec<serde_json::Value> = combined_entries
            .iter()
            .map(|(e, db_scope)| slim_entry(e, *db_scope))
            .collect();
        return serde_json::to_string(&slim).map_err(|e| format!("Failed to serialize: {}", e));
    }

    let global_entries = server.with_global_store_read(|store| {
        store
            .list_by_path(&params.path_prefix, params.limit, params.include_archived)
            .map_err(|e| format!("Failed to list memories from global DB: {}", e))
    })?;
    combined_entries.extend(global_entries.into_iter().map(|e| (e, DbScope::Global)));

    if server.has_project_db() {
        let project_entries = server.with_project_store_read(|store| {
            store
                .list_by_path(&params.path_prefix, params.limit, params.include_archived)
                .map_err(|e| format!("Failed to list memories from project DB: {}", e))
        })?;
        combined_entries.extend(project_entries.into_iter().map(|e| (e, DbScope::Project)));
    }

    // tachi#1561 (L2): see the note in the named-project branch above.
    combined_entries.retain(|(entry, _)| is_listable_row(entry, Some(params.path_prefix.as_str())));
    combined_entries.sort_by(|a, b| b.0.timestamp.cmp(&a.0.timestamp));
    combined_entries.truncate(params.limit);

    let slim: Vec<serde_json::Value> = combined_entries
        .iter()
        .map(|(e, db_scope)| slim_entry(e, *db_scope))
        .collect();
    serde_json::to_string(&slim).map_err(|e| format!("Failed to serialize: {}", e))
}

pub(crate) async fn handle_memory_stats(server: &MemoryServer) -> Result<String, String> {
    let global_stats = server.with_global_store_read(|store| {
        store
            .stats(false)
            .map_err(|e| format!("Failed to get global stats: {}", e))
    })?;

    let project_stats = if server.has_project_db() {
        Some(server.with_project_store_read(|store| {
            store
                .stats(false)
                .map_err(|e| format!("Failed to get project stats: {}", e))
        })?)
    } else {
        None
    };

    let mut total = global_stats.total;
    let mut by_scope: HashMap<String, u64> = global_stats.by_scope.clone();
    let mut by_category: HashMap<String, u64> = global_stats.by_category.clone();
    let mut by_root_path: HashMap<String, u64> = global_stats.by_root_path.clone();

    if let Some(ref project_stats) = project_stats {
        total += project_stats.total;

        for (k, v) in &project_stats.by_scope {
            *by_scope.entry(k.clone()).or_insert(0) += v;
        }
        for (k, v) in &project_stats.by_category {
            *by_category.entry(k.clone()).or_insert(0) += v;
        }
        for (k, v) in &project_stats.by_root_path {
            *by_root_path.entry(k.clone()).or_insert(0) += v;
        }
    }

    let mut databases = serde_json::Map::new();
    let global_db_path = server.global_db_path_buf();
    let project_db_path = server.project_db_path_buf();
    databases.insert(
        "global".into(),
        json!({
            "path": global_db_path.display().to_string(),
            "vec_available": server.global_vec_available(),
            "total": global_stats.total,
            "by_scope": global_stats.by_scope,
            "by_category": global_stats.by_category,
        }),
    );
    if let Some(ref ps) = project_stats {
        databases.insert(
            "project".into(),
            json!({
                "path": project_db_path.as_ref().map(|p| p.display().to_string()),
                "vec_available": server.project_vec_available(),
                "total": ps.total,
                "by_scope": ps.by_scope,
                "by_category": ps.by_category,
            }),
        );
    }

    serde_json::to_string(&json!({
        "total": total,
        "by_scope": by_scope,
        "by_category": by_category,
        "by_root_path": by_root_path,
        "databases": databases,
    }))
    .map_err(|e| format!("Failed to serialize: {}", e))
}

pub(crate) async fn handle_runtime_info(server: &MemoryServer) -> Result<String, String> {
    let tool_profile = server
        .active_tool_profile()
        .map(|profile| profile.as_str())
        .unwrap_or_else(|| tachi_hub::default_tool_profile().as_str());
    let requested_profile = std::env::var("TACHI_PROFILE").ok();
    let derivative_identity = std::env::var("TACHI_DERIVATIVE_IDENTITY")
        .ok()
        .or_else(|| std::env::var("TACHI_PRODUCT").ok())
        .unwrap_or_else(|| "tachi".to_string());
    let binary = std::env::current_exe()
        .ok()
        .map(|path| path.display().to_string());
    let app_home = server.tachi_home_dir();
    let global_db_path = server.global_db_path_buf();
    let project_db_path = server.project_db_path_buf();
    let session_client = server.session_client();
    let session_project = server.session_project();
    let daemon = crate::status_ops::collect_daemon_status(&app_home, &global_db_path);
    let process =
        crate::status_ops::runtime_observability_json(server, &app_home, Some(&daemon), true);

    let project = project_db_path.as_ref().map(|path| {
        json!({
            "path": path.display().to_string(),
            "vec_available": server.project_vec_available(),
        })
    });
    let (plan_c_split_brain, plan_c_alias_integrity) = match project_db_path.as_deref() {
        Some(path) => {
            match crate::path_utils::inspect_plan_c_alias_for_local_db_in_home(path, &app_home) {
                crate::path_utils::PlanCAliasInspection::SplitBrain(issue) => (Some(issue), None),
                crate::path_utils::PlanCAliasInspection::Integrity(issue) => (None, Some(issue)),
                crate::path_utils::PlanCAliasInspection::Absent
                | crate::path_utils::PlanCAliasInspection::MatchingSymlink => (None, None),
            }
        }
        None => (None, None),
    };
    let binding = crate::memory_search_ops::library_binding_receipt(server, None);

    serde_json::to_string(&json!({
        "runtime": {
            "name": "tachi",
            "version": env!("CARGO_PKG_VERSION"),
            "binary": binary,
            "pid": std::process::id(),
            "tool_profile": tool_profile,
            "requested_profile": requested_profile,
            "derivative_identity": derivative_identity,
            "session_client": session_client,
            "session_project": session_project,
        },
        "process": process,
        "databases": {
            "global": {
                "path": global_db_path.display().to_string(),
                "vec_available": server.global_vec_available(),
            },
            "project": project,
            "single_db_mode": !server.has_project_db(),
            "plan_c_split_brain": plan_c_split_brain,
            "plan_c_alias_integrity": plan_c_alias_integrity,
        },
        "binding": binding,
        "env": {
            "TACHI_HOME": std::env::var("TACHI_HOME").ok(),
            "SIGIL_HOME": std::env::var("SIGIL_HOME").ok(),
            "MEMORY_DB_PATH": std::env::var("MEMORY_DB_PATH").ok(),
            "TACHI_PROFILE": std::env::var("TACHI_PROFILE").ok(),
            "TACHI_DERIVATIVE_IDENTITY": std::env::var("TACHI_DERIVATIVE_IDENTITY").ok(),
            "TACHI_PRODUCT": std::env::var("TACHI_PRODUCT").ok(),
        },
    }))
    .map_err(|e| format!("Failed to serialize runtime_info: {e}"))
}

pub(crate) async fn handle_delete_memory(
    server: &MemoryServer,
    params: DeleteMemoryParams,
) -> Result<String, String> {
    if let Some(ref project_name) = params.project {
        let project_deleted = server.with_named_project_store(project_name, |store| {
            store
                .delete(&params.id)
                .map_err(|e| format!("Delete failed in project '{}': {}", project_name, e))
        })?;
        if project_deleted {
            // #1413 concern 1: a delete changes what a subsequent search
            // surfaces; bust the shared (global) recall cache AFTER the store
            // commit returned. `invalidate_recall_cache_after_write` re-takes
            // the global write gate via `with_global_store`; calling it from
            // inside the `with_named_project_store` closure above would nest
            // that gate inside the named-project gate (or recurse on it when
            // the delete targets the global store), so it must run here.
            let _ = crate::memory_search_ops::invalidate_recall_cache_after_write(
                server,
                "delete_memory",
            );
            return serde_json::to_string(&json!({
                "deleted": true,
                "db": "project",
                "project": project_name,
                "id": params.id,
            }))
            .map_err(|e| format!("Failed to serialize: {}", e));
        }

        let global_deleted = server.with_global_store(|store| {
            store
                .delete(&params.id)
                .map_err(|e| format!("Delete failed in global DB: {}", e))
        })?;
        if global_deleted {
            // #1413 concern 1: invalidate after the global store commit
            // (never inside the closure above — see the note above).
            let _ = crate::memory_search_ops::invalidate_recall_cache_after_write(
                server,
                "delete_memory",
            );
        }
        return serde_json::to_string(&json!({
            "deleted": global_deleted,
            "db": if global_deleted { "global" } else { "not_found" },
            "project": project_name,
            "id": params.id,
        }))
        .map_err(|e| format!("Failed to serialize: {}", e));
    }

    if server.has_project_db() {
        let deleted = server.with_project_store(|store| {
            store
                .delete(&params.id)
                .map_err(|e| format!("Delete failed: {}", e))
        })?;
        if deleted {
            // #1413 concern 1: invalidate after the project-store commit
            // (never inside the `with_project_store` closure above).
            let _ = crate::memory_search_ops::invalidate_recall_cache_after_write(
                server,
                "delete_memory",
            );
            return serde_json::to_string(
                &json!({ "deleted": true, "db": "project", "id": params.id }),
            )
            .map_err(|e| format!("Failed to serialize: {}", e));
        }
    }

    let deleted = server.with_global_store(|store| {
        store
            .delete(&params.id)
            .map_err(|e| format!("Delete failed: {}", e))
    })?;
    if deleted {
        // #1413 concern 1: invalidate after the global store commit (never
        // inside the closure above — it would recurse on `global_rw_gate`).
        let _ =
            crate::memory_search_ops::invalidate_recall_cache_after_write(server, "delete_memory");
    }

    serde_json::to_string(&json!({
        "deleted": deleted,
        "db": if deleted { "global" } else { "not_found" },
        "id": params.id,
    }))
    .map_err(|e| format!("Failed to serialize: {}", e))
}

pub(crate) async fn handle_archive_memory(
    server: &MemoryServer,
    params: ArchiveMemoryParams,
) -> Result<String, String> {
    if let Some(ref project_name) = params.project {
        let project_archived = server.with_named_project_store(project_name, |store| {
            store
                .archive_memory(&params.id)
                .map_err(|e| format!("Archive failed in project '{}': {}", project_name, e))
        })?;
        if project_archived {
            // #1413 concern 1: an archive changes what a subsequent (default
            // non-archived) search surfaces; bust the shared (global) recall
            // cache AFTER the store commit returned — never inside the
            // `with_named_project_store` closure above (the invalidator
            // re-takes the global write gate via `with_global_store`).
            let _ = crate::memory_search_ops::invalidate_recall_cache_after_write(
                server,
                "archive_memory",
            );
            return serde_json::to_string(&json!({
                "archived": true,
                "db": "project",
                "project": project_name,
                "id": params.id,
            }))
            .map_err(|e| format!("Failed to serialize: {}", e));
        }

        let global_archived = server.with_global_store(|store| {
            store
                .archive_memory(&params.id)
                .map_err(|e| format!("Archive failed in global DB: {}", e))
        })?;
        if global_archived {
            // #1413 concern 1: invalidate after the global store commit
            // (never inside the closure above).
            let _ = crate::memory_search_ops::invalidate_recall_cache_after_write(
                server,
                "archive_memory",
            );
        }
        return serde_json::to_string(&json!({
            "archived": global_archived,
            "db": if global_archived { "global" } else { "not_found" },
            "project": project_name,
            "id": params.id,
        }))
        .map_err(|e| format!("Failed to serialize: {}", e));
    }

    if server.has_project_db() {
        let archived = server.with_project_store(|store| {
            store
                .archive_memory(&params.id)
                .map_err(|e| format!("Archive failed: {}", e))
        })?;
        if archived {
            // #1413 concern 1: invalidate after the project-store commit
            // (never inside the `with_project_store` closure above).
            let _ = crate::memory_search_ops::invalidate_recall_cache_after_write(
                server,
                "archive_memory",
            );
            return serde_json::to_string(
                &json!({ "archived": true, "db": "project", "id": params.id }),
            )
            .map_err(|e| format!("Failed to serialize: {}", e));
        }
    }

    let archived = server.with_global_store(|store| {
        store
            .archive_memory(&params.id)
            .map_err(|e| format!("Archive failed: {}", e))
    })?;
    if archived {
        // #1413 concern 1: invalidate after the global store commit (never
        // inside the closure above — it would recurse on `global_rw_gate`).
        let _ =
            crate::memory_search_ops::invalidate_recall_cache_after_write(server, "archive_memory");
    }

    serde_json::to_string(&json!({
        "archived": archived,
        "db": if archived { "global" } else { "not_found" },
        "id": params.id,
    }))
    .map_err(|e| format!("Failed to serialize: {}", e))
}

/// The GC participants shared by global and project stores. Keep their exact
/// order: `gc_tables` observes pre-kanban rows for its diversity reconciliation.
fn gc_common_store(store: &mut MemoryStore, db_label: &str) -> Result<serde_json::Value, String> {
    let mut gc = store
        .gc_tables(&GcConfig::default())
        .map_err(|error| format!("GC failed on {db_label} DB: {error}"))?;
    let kanban_deleted = gc_expired_kanban_cards(store, DEFAULT_KANBAN_GC_MAX_AGE_DAYS)?;
    let sticky_expired = crate::sticky_ops::gc_expired_sticky_memories(store)?;
    // Branch #5: GC foundry jobs in terminal state >= 30 days old
    // (was 7d, see project owner's lifecycle spec).
    let foundry_deleted = memcore::gc_foundry_jobs(store.connection(), 30).unwrap_or(0);
    if let Some(object) = gc.as_object_mut() {
        object.insert("kanban_cards_pruned".into(), json!(kanban_deleted));
        object.insert("foundry_jobs_pruned".into(), json!(foundry_deleted));
        object.insert("sticky_memories_expired".into(), json!(sticky_expired));
    }
    // #1099: `handoff_memories_pruned` (ex-`gc_expired_handoff_memories`)
    // retired along with handoff_ops's write path — see handoff_ops.rs's
    // module doc for the legacy-row data policy (retain read-only, no
    // longer specially swept; not silently orphaned, still fully readable).
    Ok(gc)
}

pub(crate) async fn handle_memory_gc(server: &MemoryServer) -> Result<String, String> {
    let mut results = serde_json::Map::new();

    let global_gc = server.with_global_store(|store| {
        let mut gc = gc_common_store(store, "global")?;
        // #1001 follow-up (R2 review of #1007 CONCERN, #1029 lesson):
        // `session_claims` shipped with no reaper — `released` rows were
        // retained forever and a dead `active` heartbeat (crashed/killed
        // session that never called release) was invisible to *readers*
        // (`list_active_claims`'s lazy TTL filter) but stayed in storage
        // forever. `session_claims` is global-store-only (`claims_ops`
        // always writes via `with_global_store`), so this sweep only runs
        // here, not in the project-DB arm below.
        let claims_gc = memcore::gc_session_claims(store.connection(), chrono::Utc::now(), 7, 30)
            .map_err(|e| format!("GC session_claims failed on global DB: {e}"))?;
        if let Some(object) = gc.as_object_mut() {
            object.insert(
                "session_claims_released_pruned".into(),
                json!(claims_gc.released_pruned),
            );
            object.insert(
                "session_claims_active_orphaned".into(),
                json!(claims_gc.active_orphaned),
            );
        }
        Ok(gc)
    })?;
    results.insert("global".into(), global_gc);
    // #1413 concern 1: bust the shared (global) recall cache right after the
    // GLOBAL gc closure returns — never inside it (the invalidator re-takes the
    // global write gate via `with_global_store`, which would recurse on the
    // non-reentrant `global_rw_gate`). Invalidating per-closure (not once at
    // the end) closes the partial-success gap: if the project gc arm below
    // returns Err, the global content change above is still reflected in the
    // cache rather than left stale behind a now-aborted batch.
    let _ = crate::memory_search_ops::invalidate_recall_cache_after_write(server, "memory_gc");

    if server.has_project_db() {
        let project_gc = server.with_project_store(|store| gc_common_store(store, "project"))?;
        results.insert("project".into(), project_gc);
        // #1413 concern 1: bust again after the PROJECT gc closure returns.
        // Project writes can stale the shared (global) recall cache too (a
        // project-scoped search caches its rows in the global recall_cache
        // table, keyed by project), so this closure's commit needs its own
        // bust — never inside the closure.
        let _ = crate::memory_search_ops::invalidate_recall_cache_after_write(server, "memory_gc");
    }

    serde_json::to_string(&results).map_err(|e| format!("Failed to serialize: {}", e))
}

#[cfg(test)]
mod env_drift_tests {
    use super::*;
    use crate::test_support::EnvRestore;

    #[tokio::test]
    // The process-global TACHI_HOME override must remain serialized through
    // the awaited runtime-info operation.
    #[allow(clippy::await_holding_lock)]
    async fn runtime_info_alias_health_stays_bound_after_environment_drift() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let project_root = crate::utils::find_project_git_root().expect("test project root");
        let project_name = crate::path_utils::plan_c_dir_name_from_root(&project_root)
            .expect("test project identity");
        let (server, project_db) = crate::tests::make_server_with_project_fixture(&project_name);
        let ambient_home = tempfile::tempdir().expect("ambient home");
        crate::tests::create_split_brain_alias(ambient_home.path(), &project_db);
        let _ambient_home = EnvRestore::set_path("TACHI_HOME", ambient_home.path());

        let output = handle_runtime_info(&server)
            .await
            .expect("runtime info after environment drift");
        let value: serde_json::Value = serde_json::from_str(&output).expect("runtime info JSON");

        assert!(value["databases"]["plan_c_split_brain"].is_null());
        assert!(value["databases"]["plan_c_alias_integrity"].is_null());
    }
}
