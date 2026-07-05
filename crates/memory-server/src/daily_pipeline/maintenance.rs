use super::{load_manifest_targets, ManifestDbTarget, TruthMaintenanceRoute};
use crate::server_state::{DbScope, MemoryServer};
use memory_core::MemoryStore;

pub(crate) async fn run_truth_maintenance_stage(
    server: &MemoryServer,
    app_home: &std::path::Path,
) -> Result<(), String> {
    let targets = load_manifest_targets(server, &app_home.join("manifest.json"))?;
    for target in targets.into_iter().filter(|target| target.allow_write) {
        let route = resolve_truth_maintenance_route(server, &target);
        run_truth_maintenance_for_target(server, target, route).await?;
    }
    Ok(())
}

pub(crate) fn resolve_truth_maintenance_route(
    server: &MemoryServer,
    target: &ManifestDbTarget,
) -> TruthMaintenanceRoute {
    resolve_truth_maintenance_route_for_paths(
        &server.global_db_path_buf(),
        server.project_db_path_buf().as_deref(),
        target,
    )
}

pub(crate) fn resolve_truth_maintenance_route_for_paths(
    global_db_path: &std::path::Path,
    project_db_path: Option<&std::path::Path>,
    target: &ManifestDbTarget,
) -> TruthMaintenanceRoute {
    let role_is_global = target.role == "global";
    let target_db = if role_is_global {
        DbScope::Global
    } else {
        DbScope::Project
    };

    if role_is_global {
        return TruthMaintenanceRoute {
            target_db,
            named_project: None,
            db_path: (!same_db_path(global_db_path, &target.path)).then(|| target.path.clone()),
        };
    }

    if project_db_path.is_some_and(|path| same_db_path(path, &target.path)) {
        return TruthMaintenanceRoute {
            target_db,
            named_project: None,
            db_path: None,
        };
    }

    if let Some(project_name) = crate::path_utils::named_project_for_db_path(&target.path) {
        return TruthMaintenanceRoute {
            target_db,
            named_project: Some(project_name),
            db_path: None,
        };
    }

    TruthMaintenanceRoute {
        target_db,
        named_project: None,
        db_path: Some(target.path.clone()),
    }
}

fn same_db_path(left: &std::path::Path, right: &std::path::Path) -> bool {
    crate::manifest::canonicalize_db_path(left) == crate::manifest::canonicalize_db_path(right)
}

async fn run_truth_maintenance_for_target(
    server: &MemoryServer,
    target: ManifestDbTarget,
    route: TruthMaintenanceRoute,
) -> Result<(), String> {
    let Some(db_path) = target.path.to_str() else {
        return Ok(());
    };
    let store = MemoryStore::open_with_label(db_path, &target.label)
        .map_err(|e| format!("open maintenance DB {}: {e}", target.label))?;

    store
        .truth_maintenance_prune_stale()
        .map_err(|e| format!("truth maintenance prune {}: {e}", target.label))?;

    // ── Self-healing: promote raw → consolidated when DB health ratio is low ──
    let total_active = store
        .count_active_memories()
        .map_err(|e| format!("count active memories {}: {e}", target.label))?;
    let consolidated_count = store
        .count_consolidated_active_memories()
        .map_err(|e| format!("count consolidated memories {}: {e}", target.label))?;
    let health_ratio = if total_active > 0 {
        consolidated_count as f64 / total_active as f64
    } else {
        1.0
    };
    if health_ratio < 0.35 && total_active > 0 {
        // Self-healing may only apply the same promotion gate as record_access:
        // repeated exact recall from diverse queries. Do not promote merely
        // because a raw note was accessed often.
        let promoted = store
            .truth_maintenance_self_heal_promote_raw()
            .map_err(|e| format!("self-heal promote raw memories {}: {e}", target.label))?;
        if promoted > 0 {
            eprintln!(
                "[daily_pipeline] self-heal {}: promoted {promoted} raw → consolidated (ratio was {health_ratio:.2})",
                target.label
            );
        }
    }

    // ── Post-distillation embedding: enqueue non-raw entries without vectors ──
    let needs_embed_ids = store
        .list_memory_ids_needing_embedding(50)
        .map_err(|e| format!("list embedding candidates {}: {e}", target.label))?;
    if !needs_embed_ids.is_empty() {
        let candidates = memory_core::db::fetch_by_ids(store.connection(), &needs_embed_ids, false)
            .map_err(|e| format!("fetch embedding candidates {}: {e}", target.label))?;
        for entry in candidates.values() {
            let _ = server.enrichment_lock().enrich_tx.try_send(
                crate::enrichment::build_enrichment_item(
                    entry,
                    true,  // needs_embedding
                    false, // needs_summary
                    route.target_db,
                    route.named_project.clone(),
                    route.db_path.clone(),
                    None,
                    None,
                    entry.revision,
                ),
            );
        }
    }

    let ids = store
        .list_promotion_candidate_ids(200)
        .map_err(|e| format!("list promotion candidates {}: {e}", target.label))?;
    let entries = memory_core::db::fetch_by_ids(store.connection(), &ids, false)
        .map_err(|e| format!("fetch promotion candidates {}: {e}", target.label))?;

    for entry in entries.values() {
        let access_days = store
            .count_distinct_access_days(&entry.id)
            .map_err(|e| format!("count access days for {}: {e}", entry.id))?;
        if crate::pipeline_ops::calculate_promotion_score(entry, access_days) < 0.60 {
            continue;
        }
        store
            .promote_memory_to_durable(&entry.id)
            .map_err(|e| format!("promote memory {}: {e}", entry.id))?;
        let _ =
            server
                .enrichment_lock()
                .enrich_tx
                .try_send(crate::enrichment::build_enrichment_item(
                    entry,
                    true,
                    false,
                    route.target_db,
                    route.named_project.clone(),
                    route.db_path.clone(),
                    None,
                    None,
                    entry.revision,
                ));
    }

    Ok(())
}
