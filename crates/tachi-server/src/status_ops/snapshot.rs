use super::*;
use crate::manifest::{DbEntry, DbRole};

pub(crate) fn collect_snapshot(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> StatusSnapshot {
    collect_snapshot_scoped(app_home, global_db_path, project_db_path, false)
}

pub(crate) fn collect_snapshot_with_provider_value_compare(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> StatusSnapshot {
    collect_snapshot_inner(app_home, global_db_path, project_db_path, true, true)
}

/// Default-scoped `collect_snapshot`, with the coldpath fleet-probe gate
/// (#coldpath perf pack item 5) explicit at the call site: `all_dbs = false`
/// probes only the global DB + current-project DB (the two paths every
/// `tachi status` invocation actually has open); `all_dbs = true` restores
/// the full manifest fleet view. See `collect_snapshot_inner` for what
/// "probe" means and why skipping it is cheap.
pub(crate) fn collect_snapshot_scoped(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    all_dbs: bool,
) -> StatusSnapshot {
    collect_snapshot_inner(app_home, global_db_path, project_db_path, false, all_dbs)
}

fn unavailable_db_status(entry: &DbEntry, label: String, orphan: bool, error: String) -> DbStatus {
    DbStatus {
        path: entry.path.clone(),
        label,
        orphan,
        memory_total: 0,
        vector_count: 0,
        vector_missing: 0,
        vector_orphans: 0,
        vector_coverage: 0.0,
        vector_dimension: None,
        vector_sweep: None,
        vector_sweep_error: None,
        namespace: NamespaceHealth::default(),
        continuity: memcore::ContinuityMetrics::default(),
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
        error: Some(error),
    }
}

fn collect_snapshot_inner(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    compare_provider_values: bool,
    all_dbs: bool,
) -> StatusSnapshot {
    let daemon = collect_daemon_status(app_home, global_db_path);
    let daemon_inventory = collect_daemon_inventory(app_home, global_db_path);
    let manifest_path = app_home.join("manifest.json");

    let manifest = Manifest::load(&manifest_path).unwrap_or_else(|_| Manifest::empty());

    // Coldpath scoping (perf pack item 5): probing every manifest DB
    // read-only (job histogram, vector health, namespace counts, continuity
    // metrics — several queries each) is the dominant cost of `tachi
    // status` on hosts with many registered DBs (~220ms -> ~60ms measured
    // going from a 7-DB fleet down to 2). The default view only opens the
    // global DB and the current-project DB — the two paths this
    // invocation's caller actually cares about — and `--all-dbs` restores
    // the full fleet. This does NOT change what's IN the manifest, only
    // which entries get probed for this render; entries skipped this way
    // are simply absent from `dbs` (and therefore from `Summary`'s "N dbs"
    // count) rather than shown with a placeholder, since unlike a missing
    // DB file this isn't an error condition worth flagging.
    let fallback_global_entry: Option<DbEntry> = if !manifest
        .dbs
        .iter()
        .any(|entry| paths_equal(&PathBuf::from(&entry.path), global_db_path))
        && global_db_path.exists()
    {
        let canon =
            std::fs::canonicalize(global_db_path).unwrap_or_else(|_| global_db_path.to_path_buf());
        Some(DbEntry {
            path: canon.display().to_string(),
            role: DbRole::Global,
            owner: "tachi".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: String::new(),
            last_classification: "healthy".to_string(),
            scope_hint: "global".to_string(),
            notes: "implicit global store fallback".to_string(),
        })
    } else {
        None
    };

    let scoped_entries: Vec<&DbEntry> = if all_dbs {
        let mut entries: Vec<&DbEntry> = manifest.dbs.iter().collect();
        if let Some(ref fallback) = fallback_global_entry {
            entries.insert(0, fallback);
        }
        entries
    } else {
        let mut entries: Vec<&DbEntry> = manifest
            .dbs
            .iter()
            .filter(|entry| {
                let path = PathBuf::from(&entry.path);
                paths_equal(&path, global_db_path)
                    || project_db_path.is_some_and(|p| paths_equal(&path, p))
            })
            .collect();
        if let Some(ref fallback) = fallback_global_entry {
            entries.insert(0, fallback);
        }
        entries
    };

    let mut dbs: Vec<DbStatus> = Vec::with_capacity(scoped_entries.len());
    for entry in scoped_entries {
        let path = PathBuf::from(&entry.path);
        let label = db_status_label(entry, &path, app_home);
        let orphan =
            is_orphan_entry_in_home(entry, &path, global_db_path, project_db_path, app_home);
        let leaf_exists = match crate::path_utils::manifest_db_leaf_exists(entry) {
            Ok(exists) => exists,
            Err(error) => {
                dbs.push(unavailable_db_status(entry, label, orphan, error));
                continue;
            }
        };
        if !leaf_exists {
            dbs.push(unavailable_db_status(
                entry,
                label,
                orphan,
                "missing on disk".to_string(),
            ));
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
            )) => {
                let vector_sweep_result =
                    crate::vector_backfill::read_vector_sweep_state_for_status(&path);
                let (vector_sweep, vector_sweep_error) = match vector_sweep_result {
                    Ok(state) => (state, None),
                    Err(err) => (None, Some(err)),
                };
                dbs.push(DbStatus {
                    path: entry.path.clone(),
                    label,
                    orphan,
                    memory_total: vector.total,
                    vector_count: vector.with_vec,
                    vector_missing: vector.missing,
                    vector_orphans: vector.orphans,
                    vector_coverage: vector.coverage,
                    vector_dimension: vector.dimension,
                    vector_sweep,
                    vector_sweep_error,
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
                });
            }
            Err(error) => dbs.push(unavailable_db_status(entry, label, orphan, error)),
        }
    }

    let dispatches = collect_dispatches(global_db_path, project_db_path);
    let recent_evals = collect_recent_evals(global_db_path, project_db_path);
    let last_daily_report = find_last_daily_report(app_home);
    let distill_marker = read_distill_marker(app_home);
    let provider_probe_cache = status_health::read_provider_probe_cache(app_home, global_db_path);
    let fresh_provider_probe_cache = provider_probe_cache
        .as_ref()
        .filter(|cache| !cache.is_stale());
    let mut api_keys = if compare_provider_values {
        status_health::collect_api_key_status_with_probe_cache(
            global_db_path,
            fresh_provider_probe_cache,
            true,
            Some(app_home),
        )
    } else {
        status_health::collect_api_key_status_with_probe_cache(
            global_db_path,
            fresh_provider_probe_cache,
            false,
            Some(app_home),
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
    let (plan_c_split_brain, plan_c_alias_integrity) = match project_db_path {
        Some(path) => {
            match crate::path_utils::inspect_plan_c_alias_for_local_db_in_home(path, app_home) {
                crate::path_utils::PlanCAliasInspection::SplitBrain(issue) => {
                    (vec![issue], Vec::new())
                }
                crate::path_utils::PlanCAliasInspection::Integrity(issue) => {
                    (Vec::new(), vec![issue])
                }
                crate::path_utils::PlanCAliasInspection::Absent
                | crate::path_utils::PlanCAliasInspection::MatchingSymlink => {
                    (Vec::new(), Vec::new())
                }
            }
        }
        None => (Vec::new(), Vec::new()),
    };
    let health_deductions = status_health::calculate_health_deductions(
        &daemon,
        &dbs,
        distill_marker.as_ref(),
        &api_keys,
        cached_probe_results,
        fresh_provider_probe_cache.map(|cache| cache.rotation_groups.as_slice()),
    );
    let health_score = status_health::health_score_from_deductions(&health_deductions);
    let disk = disk::collect_disk_status(global_db_path);

    StatusSnapshot {
        daemon,
        daemon_inventory,
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
        plan_c_alias_integrity,
        health_deductions,
        health_score,
        disk,
    }
}

pub(crate) fn list_recent_checkpoint_entries(
    server: &crate::MemoryServer,
    limit: usize,
) -> Vec<serde_json::Value> {
    list_recent_entries_by_path(server, "/agent/checkpoints/", limit)
}

pub(crate) fn list_recent_checkpoint_entries_for_project(
    server: &crate::MemoryServer,
    project_name: &str,
    limit: usize,
) -> Vec<serde_json::Value> {
    let Ok(db_path) = server.resolve_server_named_project_db_path(project_name) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    collect_entries_for_status(
        db_path.as_path(),
        "/agent/checkpoints/",
        limit,
        "project",
        &mut rows,
    );
    rows.sort_by(|a, b| {
        let aa = a.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        let bb = b.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        bb.cmp(aa)
    });
    rows.truncate(limit.max(1).min(50));
    rows
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
    // Recency-first: `list_by_path` orders `path ASC, timestamp DESC`, so a
    // `LIMIT` here can truncate before ever reaching a genuinely newer entry
    // that happens to sort under a lexicographically later path prefix
    // (e.g. `/agent/checkpoints/2026-07-09` sorts after
    // `/agent/checkpoints/2026-05-30`). `list_by_path_recent` orders purely
    // by `timestamp DESC` so the newest rows always survive the cutoff.
    let entries: Vec<MemoryEntry> = match store.list_by_path_recent(path_prefix, limit, false) {
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

fn db_status_label(entry: &DbEntry, path: &Path, app_home: &Path) -> String {
    if matches!(entry.role, DbRole::Global) {
        return "global".to_string();
    }
    let scope_hint = entry.scope_hint.trim();
    if !is_placeholder_scope_hint(scope_hint) {
        return scope_hint.to_string();
    }
    if let Some(label) = label_for_tachi_run_db(path) {
        return label;
    }
    if let Some(name) = crate::path_utils::named_project_for_db_path_in_home(path, app_home) {
        return format!("project:{name}");
    }
    if matches!(entry.role, DbRole::Project) {
        return path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .map(|name| format!("project:{name}"))
            .unwrap_or_else(|| "project".to_string());
    }
    if !entry.owner.trim().is_empty() && entry.owner != "tachi" {
        return entry.owner.clone();
    }
    let file = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(memcore::MEMORY_DB_FILENAME);
    let parent = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("?");
    format!("{parent}/{file}")
}

fn is_placeholder_scope_hint(scope_hint: &str) -> bool {
    scope_hint.is_empty()
        || scope_hint.eq_ignore_ascii_case("unknown")
        || scope_hint.eq_ignore_ascii_case("tachi-other")
}

fn label_for_tachi_run_db(path: &Path) -> Option<String> {
    let components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    let run_idx = components
        .iter()
        .position(|component| component == "runs")?;
    let run_name = components.get(run_idx + 1)?;
    let db_role = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())?;
    if db_role != "project" && db_role != "global" {
        return None;
    }
    Some(format!("run:{run_name}:{db_role}"))
}

#[cfg(test)]
pub(crate) fn db_status_label_for_tests(entry: &DbEntry, path: &Path) -> String {
    db_status_label(entry, path, &crate::path_utils::tachi_home())
}

#[cfg(test)]
mod env_drift_tests {
    use super::*;
    use crate::test_support::EnvRestore;

    #[test]
    fn snapshot_alias_health_stays_bound_after_environment_drift() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let project_root = crate::utils::find_project_git_root().expect("test project root");
        let project_name = crate::path_utils::plan_c_dir_name_from_root(&project_root)
            .expect("test project identity");
        let (server, project_db) = crate::tests::make_server_with_project_fixture(&project_name);
        let app_home = server.tachi_home_dir();
        let global_db = server.global_db_path_buf();
        let ambient_home = tempfile::tempdir().expect("ambient home");
        crate::tests::create_split_brain_alias(ambient_home.path(), &project_db);
        let _ambient_home = EnvRestore::set_path("TACHI_HOME", ambient_home.path());

        let snapshot = collect_snapshot(&app_home, &global_db, Some(&project_db));

        assert!(snapshot.plan_c_split_brain.is_empty());
        assert!(snapshot.plan_c_alias_integrity.is_empty());
    }

    #[test]
    fn snapshot_synthesizes_global_db_fallback_when_absent_from_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        let app_home = temp.path();
        let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(global_db.parent().unwrap()).expect("create global dir");
        let _store =
            memcore::MemoryStore::open(global_db.to_str().unwrap()).expect("open global store");

        // Manifest is completely empty / absent
        let snapshot_default = collect_snapshot(app_home, &global_db, None);
        assert_eq!(
            snapshot_default.dbs.len(),
            1,
            "default snapshot must synthesize global DB fallback"
        );
        assert_eq!(snapshot_default.dbs[0].label, "global");

        let snapshot_all = collect_snapshot_inner(app_home, &global_db, None, false, true);
        assert_eq!(
            snapshot_all.dbs.len(),
            1,
            "all_dbs snapshot must synthesize global DB fallback"
        );
        assert_eq!(snapshot_all.dbs[0].label, "global");
    }
}
