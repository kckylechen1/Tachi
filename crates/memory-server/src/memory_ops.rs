use crate::kanban::{gc_expired_kanban_cards, DEFAULT_KANBAN_GC_MAX_AGE_DAYS};
use crate::shared_defs::{slim_entry, slim_entry_with_enrichment};
use crate::tool_params::{
    ArchiveMemoryParams, DeleteDomainParams, DeleteMemoryParams, GetDomainParams, GetMemoryParams,
    ListMemoriesParams, RegisterDomainParams,
};
use crate::{DbScope, MemoryServer};
use memory_core::{GcConfig, MemoryEntry};
use serde_json::json;
use std::collections::HashMap;

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

        if let Some(entry) = project_entry {
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

        if let Some(entry) = project_entry {
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
            if crate::memory_search_ops::named_project_db_exists(&project_name) {
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

                if let Some(entry) = project_entry {
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

    match global_entry {
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
    let app_home = crate::status_ops::resolve_app_home();
    let global_db_path = server.global_db_path_buf();
    let project_db_path = server.project_db_path_buf();
    let daemon = crate::status_ops::collect_daemon_status(&app_home, &global_db_path);
    let process =
        crate::status_ops::runtime_observability_json(server, &app_home, Some(&daemon), true);

    let project = project_db_path.as_ref().map(|path| {
        json!({
            "path": path.display().to_string(),
            "vec_available": server.project_vec_available(),
        })
    });
    let plan_c_split_brain = project_db_path
        .as_ref()
        .and_then(|path| crate::path_utils::plan_c_split_brain_for_local_db(path.as_path()));

    serde_json::to_string(&json!({
        "runtime": {
            "name": "tachi",
            "version": env!("CARGO_PKG_VERSION"),
            "binary": binary,
            "pid": std::process::id(),
            "tool_profile": tool_profile,
            "requested_profile": requested_profile,
            "derivative_identity": derivative_identity,
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
        },
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
    if server.has_project_db() {
        let deleted = server.with_project_store(|store| {
            store
                .delete(&params.id)
                .map_err(|e| format!("Delete failed: {}", e))
        })?;
        if deleted {
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
    if server.has_project_db() {
        let archived = server.with_project_store(|store| {
            store
                .archive_memory(&params.id)
                .map_err(|e| format!("Archive failed: {}", e))
        })?;
        if archived {
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

    serde_json::to_string(&json!({
        "archived": archived,
        "db": if archived { "global" } else { "not_found" },
        "id": params.id,
    }))
    .map_err(|e| format!("Failed to serialize: {}", e))
}

// ─── Domain CRUD ────────────────────────────────────────────────────────────

pub(crate) async fn handle_register_domain(
    server: &MemoryServer,
    params: RegisterDomainParams,
) -> Result<String, String> {
    let domain = memory_core::DomainConfig {
        name: params.name.clone(),
        description: params.description.unwrap_or_default(),
        gc_threshold_days: params.gc_threshold_days,
        default_retention: params.default_retention,
        default_path_prefix: params.default_path_prefix,
        metadata: params.metadata.unwrap_or(json!({})),
        created_at: chrono::Utc::now().to_rfc3339(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };

    server.with_global_store(|store| {
        store
            .register_domain(&domain)
            .map_err(|e| format!("Failed to register domain: {}", e))
    })?;

    serde_json::to_string(&json!({
        "registered": true,
        "domain": params.name,
    }))
    .map_err(|e| format!("Failed to serialize: {}", e))
}

pub(crate) async fn handle_get_domain(
    server: &MemoryServer,
    params: GetDomainParams,
) -> Result<String, String> {
    let domain = server.with_global_store_read(|store| {
        store
            .get_domain(&params.name)
            .map_err(|e| format!("Failed to get domain: {}", e))
    })?;

    match domain {
        Some(d) => serde_json::to_string(&d).map_err(|e| format!("Failed to serialize: {}", e)),
        None => serde_json::to_string(&json!({ "error": "Domain not found", "name": params.name }))
            .map_err(|e| format!("Failed to serialize: {}", e)),
    }
}

pub(crate) async fn handle_list_domains(server: &MemoryServer) -> Result<String, String> {
    let domains = server.with_global_store_read(|store| {
        store
            .list_domains()
            .map_err(|e| format!("Failed to list domains: {}", e))
    })?;

    serde_json::to_string(&json!({
        "count": domains.len(),
        "domains": domains,
    }))
    .map_err(|e| format!("Failed to serialize: {}", e))
}

pub(crate) async fn handle_delete_domain(
    server: &MemoryServer,
    params: DeleteDomainParams,
) -> Result<String, String> {
    let deleted = server.with_global_store(|store| {
        store
            .delete_domain(&params.name)
            .map_err(|e| format!("Failed to delete domain: {}", e))
    })?;

    serde_json::to_string(&json!({
        "deleted": deleted,
        "domain": params.name,
    }))
    .map_err(|e| format!("Failed to serialize: {}", e))
}

pub(crate) async fn handle_memory_gc(server: &MemoryServer) -> Result<String, String> {
    let mut results = serde_json::Map::new();

    let global_gc = server.with_global_store(|store| {
        let mut gc = store
            .gc_tables(&GcConfig::default())
            .map_err(|e| format!("GC failed on global DB: {}", e))?;
        let kanban_deleted = gc_expired_kanban_cards(store, DEFAULT_KANBAN_GC_MAX_AGE_DAYS)?;
        let handoff_deleted = crate::handoff_ops::gc_expired_handoff_memories(store, 30)?;
        // Branch #5: GC foundry jobs in terminal state >= 30 days old
        // (was 7d, see project owner's lifecycle spec).
        let foundry_deleted = memory_core::gc_foundry_jobs(store.connection(), 30).unwrap_or(0);
        if let Some(object) = gc.as_object_mut() {
            object.insert("kanban_cards_pruned".into(), json!(kanban_deleted));
            object.insert("foundry_jobs_pruned".into(), json!(foundry_deleted));
            object.insert("handoff_memories_pruned".into(), json!(handoff_deleted));
        }
        Ok(gc)
    })?;
    results.insert("global".into(), global_gc);

    if server.has_project_db() {
        let project_gc = server.with_project_store(|store| {
            let mut gc = store
                .gc_tables(&GcConfig::default())
                .map_err(|e| format!("GC failed on project DB: {}", e))?;
            let kanban_deleted = gc_expired_kanban_cards(store, DEFAULT_KANBAN_GC_MAX_AGE_DAYS)?;
            let handoff_deleted = crate::handoff_ops::gc_expired_handoff_memories(store, 30)?;
            let foundry_deleted = memory_core::gc_foundry_jobs(store.connection(), 30).unwrap_or(0);
            if let Some(object) = gc.as_object_mut() {
                object.insert("kanban_cards_pruned".into(), json!(kanban_deleted));
                object.insert("foundry_jobs_pruned".into(), json!(foundry_deleted));
                object.insert("handoff_memories_pruned".into(), json!(handoff_deleted));
            }
            Ok(gc)
        })?;
        results.insert("project".into(), project_gc);
    }

    serde_json::to_string(&results).map_err(|e| format!("Failed to serialize: {}", e))
}
