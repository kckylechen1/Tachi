use super::*;

pub(crate) fn collect_snapshot(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> StatusSnapshot {
    collect_snapshot_inner(app_home, global_db_path, project_db_path, false)
}

pub(crate) fn collect_snapshot_with_provider_value_compare(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> StatusSnapshot {
    collect_snapshot_inner(app_home, global_db_path, project_db_path, true)
}

fn collect_snapshot_inner(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    compare_provider_values: bool,
) -> StatusSnapshot {
    let daemon = collect_daemon_status(app_home, global_db_path);
    let manifest_path = app_home.join("manifest.json");

    let manifest = Manifest::load(&manifest_path).unwrap_or_else(|_| Manifest::empty());

    let mut dbs: Vec<DbStatus> = Vec::with_capacity(manifest.dbs.len());
    for entry in &manifest.dbs {
        let path = PathBuf::from(&entry.path);
        let label = if entry.scope_hint.is_empty() {
            path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("?.db")
                .to_string()
        } else {
            entry.scope_hint.clone()
        };
        let orphan = is_orphan_entry(entry, &path, global_db_path, project_db_path);
        if !path.exists() {
            dbs.push(DbStatus {
                path: entry.path.clone(),
                label,
                orphan,
                memory_total: 0,
                vector_count: 0,
                vector_missing: 0,
                vector_orphans: 0,
                vector_coverage: 0.0,
                vector_dimension: None,
                namespace: NamespaceHealth::default(),
                continuity: memory_core::ContinuityMetrics::default(),
                pending_enrichment: 0,
                enrichment_failed_recent: 0,
                enrichment_failures: Vec::new(),
                pending: 0,
                running: 0,
                active_jobs: 0,
                completed: 0,
                failed: 0,
                dead_lettered: 0,
                skipped: 0,
                terminal_jobs: 0,
                gc_eligible: 0,
                stuck_in_progress: 0,
                latest_active_job: None,
                latest_terminal_job: None,
                latest_job: None,
                latest_failed_job: None,
                error: Some("missing on disk".to_string()),
            });
            continue;
        }
        match probe_db(&path) {
            Ok((
                hist,
                stuck,
                vector,
                namespace,
                continuity,
                latest_active_job,
                latest_terminal_job,
                latest_job,
                latest_failed_job,
            )) => dbs.push(DbStatus {
                path: entry.path.clone(),
                label,
                orphan,
                memory_total: vector.total,
                vector_count: vector.with_vec,
                vector_missing: vector.missing,
                vector_orphans: vector.orphans,
                vector_coverage: vector.coverage,
                vector_dimension: vector.dimension,
                namespace,
                continuity,
                pending_enrichment: vector.pending_enrichment,
                enrichment_failed_recent: vector.enrichment_failed_recent,
                enrichment_failures: vector.enrichment_failures,
                pending: hist.queued + hist.planned,
                running: hist.running,
                active_jobs: hist.planned + hist.queued + hist.running,
                completed: hist.completed,
                failed: hist.failed,
                dead_lettered: hist.dead_lettered,
                skipped: hist.skipped,
                terminal_jobs: hist.completed + hist.failed + hist.skipped,
                gc_eligible: hist.gc_eligible,
                stuck_in_progress: stuck,
                latest_active_job,
                latest_terminal_job,
                latest_job,
                latest_failed_job,
                error: None,
            }),
            Err(e) => dbs.push(DbStatus {
                path: entry.path.clone(),
                label,
                orphan,
                memory_total: 0,
                vector_count: 0,
                vector_missing: 0,
                vector_orphans: 0,
                vector_coverage: 0.0,
                vector_dimension: None,
                namespace: NamespaceHealth::default(),
                continuity: memory_core::ContinuityMetrics::default(),
                pending_enrichment: 0,
                enrichment_failed_recent: 0,
                enrichment_failures: Vec::new(),
                pending: 0,
                running: 0,
                active_jobs: 0,
                completed: 0,
                failed: 0,
                dead_lettered: 0,
                skipped: 0,
                terminal_jobs: 0,
                gc_eligible: 0,
                stuck_in_progress: 0,
                latest_active_job: None,
                latest_terminal_job: None,
                latest_job: None,
                latest_failed_job: None,
                error: Some(e),
            }),
        }
    }

    let dispatches = collect_dispatches(global_db_path, project_db_path);
    let recent_evals = collect_recent_evals(global_db_path, project_db_path);
    let last_daily_report = find_last_daily_report(app_home);
    let distill_marker = read_distill_marker(app_home);
    let provider_probe_cache = status_health::read_provider_probe_cache(app_home);
    let fresh_provider_probe_cache = provider_probe_cache
        .as_ref()
        .filter(|cache| !cache.is_stale());
    let mut api_keys = if compare_provider_values {
        status_health::collect_api_key_status_with_probe_cache(
            global_db_path,
            fresh_provider_probe_cache,
            true,
        )
    } else {
        status_health::collect_api_key_status_with_probe_cache(
            global_db_path,
            fresh_provider_probe_cache,
            false,
        )
    };
    status_health::apply_inferred_provider_failures(&mut api_keys, &dbs);
    let cached_probe_results = provider_probe_cache
        .as_ref()
        .filter(|cache| !cache.is_stale())
        .map(|cache| cache.probes.as_slice());
    let git_root = crate::utils::find_project_git_root();
    let project_warnings = crate::doctor::project_secret_file_warnings(git_root.as_deref())
        .into_iter()
        .map(|warning| warning.message)
        .collect();
    let plan_c_split_brain = project_db_path
        .and_then(crate::path_utils::plan_c_split_brain_for_local_db)
        .into_iter()
        .collect();
    let health_score = status_health::calculate_health_score(
        &daemon,
        &dbs,
        distill_marker.as_ref(),
        &api_keys,
        cached_probe_results,
        fresh_provider_probe_cache.map(|cache| cache.rotation_groups.as_slice()),
    );

    StatusSnapshot {
        daemon,
        dbs,
        manifest_path: manifest_path.display().to_string(),
        dispatches,
        recent_evals,
        last_daily_report,
        distill_marker,
        api_keys,
        provider_probe_cache,
        project_warnings,
        plan_c_split_brain,
        health_score,
    }
}

pub(crate) fn list_recent_checkpoint_entries(
    server: &crate::MemoryServer,
    limit: usize,
) -> Vec<serde_json::Value> {
    list_recent_entries_by_path(server, "/agent/checkpoints/", limit)
}

pub(crate) fn list_recent_kanban_entries(
    server: &crate::MemoryServer,
    limit: usize,
) -> Vec<serde_json::Value> {
    list_recent_entries_by_path(server, "/kanban/tasks/", limit)
}

fn list_recent_entries_by_path(
    server: &crate::MemoryServer,
    path_prefix: &str,
    limit: usize,
) -> Vec<serde_json::Value> {
    let limit = limit.max(1).min(50);
    let mut rows = Vec::new();
    collect_entries_for_status(
        server.global_db_path_buf().as_path(),
        path_prefix,
        limit,
        "global",
        &mut rows,
    );
    if let Some(path) = server.project_db_path_buf() {
        collect_entries_for_status(path.as_path(), path_prefix, limit, "project", &mut rows);
    }
    rows.sort_by(|a, b| {
        let aa = a.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        let bb = b.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        bb.cmp(aa)
    });
    rows.truncate(limit);
    rows
}

fn collect_entries_for_status(
    db_path: &Path,
    path_prefix: &str,
    limit: usize,
    db_label: &str,
    out: &mut Vec<serde_json::Value>,
) {
    let Some(path_str) = db_path.to_str() else {
        return;
    };
    let Ok(store) = MemoryStore::open_read_only(path_str) else {
        return;
    };
    let entries: Vec<MemoryEntry> = match store.list_by_path(path_prefix, limit, false) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries {
        out.push(json!({
            "id": entry.id,
            "path": entry.path,
            "summary": entry.summary,
            "updated_at": entry.timestamp,
            "topic": entry.topic,
            "category": entry.category,
            "db": db_label,
        }));
    }
}

pub(super) fn paths_equal(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}
