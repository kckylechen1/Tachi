use super::{load_manifest_targets, DailyStageReport, ManifestDbTarget, TruthMaintenanceRoute};
use crate::server_state::{DbScope, MemoryServer};
use memcore::MemoryStore;
use serde_json::{json, Value};

struct TruthMaintenanceScope {
    runnable: Vec<ManifestDbTarget>,
    exclusions: Vec<TruthMaintenanceExclusion>,
}

struct TruthMaintenanceExclusion {
    target: String,
    label: String,
    path: std::path::PathBuf,
}

impl TruthMaintenanceExclusion {
    fn as_json(&self) -> Value {
        json!({
            "outcome": "expected_exclusion",
            "reason": "manifest_allow_write_false",
            "target": self.target,
            "label": self.label,
            "path": self.path,
        })
    }
}

impl TruthMaintenanceScope {
    fn examined(&self) -> usize {
        self.runnable.len() + self.exclusions.len()
    }

    fn exclusion_details(&self) -> Vec<Value> {
        self.exclusions
            .iter()
            .map(TruthMaintenanceExclusion::as_json)
            .collect()
    }
}

fn account_truth_maintenance_scope(targets: Vec<ManifestDbTarget>) -> TruthMaintenanceScope {
    let mut runnable = Vec::new();
    let mut exclusions = Vec::new();
    for target in targets {
        if target.allow_write {
            runnable.push(target);
        } else {
            exclusions.push(TruthMaintenanceExclusion {
                target: target.name,
                label: target.label,
                path: target.path,
            });
        }
    }
    TruthMaintenanceScope {
        runnable,
        exclusions,
    }
}

fn truth_maintenance_stage(
    status: &str,
    summary: String,
    examined: usize,
    progressed: Vec<Value>,
    exclusions: Vec<Value>,
    incomplete: Vec<Value>,
) -> DailyStageReport {
    DailyStageReport {
        status: status.to_string(),
        summary,
        details: json!({
            "scope_accounting": {
                "examined": examined,
                "progressed": progressed,
                "expected_exclusions": exclusions,
                "incomplete": incomplete,
            }
        }),
    }
}

pub(crate) async fn run_truth_maintenance_stage(
    server: &MemoryServer,
    app_home: &std::path::Path,
) -> DailyStageReport {
    let targets = match load_manifest_targets(server, &app_home.join("manifest.json")) {
        Ok(targets) => targets,
        Err(error) => {
            return truth_maintenance_stage(
                "failed",
                format!("truth maintenance could not load manifest targets: {error}"),
                0,
                Vec::new(),
                Vec::new(),
                vec![json!({
                    "outcome": "incomplete",
                    "reason": "manifest_load_failed",
                    "error": error,
                })],
            );
        }
    };
    let scope = account_truth_maintenance_scope(targets);
    let examined = scope.examined();
    let exclusions = scope.exclusion_details();
    let excluded = exclusions.len();
    let mut progressed = Vec::new();
    let mut runnable = scope.runnable.into_iter();
    while let Some(target) = runnable.next() {
        let route = resolve_truth_maintenance_route(server, &target);
        let target_name = target.name.clone();
        let target_label = target.label.clone();
        let target_path = target.path.display().to_string();
        if let Err(error) = run_truth_maintenance_for_target(server, target, route).await {
            let mut incomplete = vec![json!({
                "outcome": "incomplete",
                "reason": "maintenance_error",
                "target": target_name,
                "label": target_label,
                "path": target_path,
                "error": error,
            })];
            incomplete.extend(runnable.map(|remaining| {
                json!({
                    "outcome": "incomplete",
                    "reason": "not_started_after_peer_error",
                    "target": remaining.name,
                    "label": remaining.label,
                    "path": remaining.path,
                })
            }));
            return truth_maintenance_stage(
                "failed",
                "truth maintenance stopped after an incomplete target; see scope accounting"
                    .to_string(),
                examined,
                progressed,
                exclusions,
                incomplete,
            );
        }
        progressed.push(json!({
            "outcome": "progressed",
            "target": target_name,
            "label": target_label,
            "path": target_path,
        }));
    }
    truth_maintenance_stage(
        "completed",
        format!(
            "truth maintenance accounted for {examined} target(s): {} progressed, {excluded} expected exclusion(s)",
            progressed.len()
        ),
        examined,
        progressed,
        exclusions,
        Vec::new(),
    )
}

pub(crate) fn resolve_truth_maintenance_route(
    server: &MemoryServer,
    target: &ManifestDbTarget,
) -> TruthMaintenanceRoute {
    resolve_truth_maintenance_route_for_paths_in_home(
        &server.tachi_home_dir(),
        &server.global_db_path_buf(),
        server.project_db_path_buf().as_deref(),
        target,
    )
}

pub(crate) fn resolve_truth_maintenance_route_for_paths_in_home(
    tachi_home: &std::path::Path,
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

    if let Some(project_name) =
        crate::path_utils::named_project_for_db_path_in_home(&target.path, tachi_home)
    {
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
    // tachi#1585 D5: this store is opened outside the server's startup funnels,
    // so it must be handed the server's already-resolved `KernelPolicy`
    // explicitly — `memcore` reads no env of its own any more, and the
    // embedding scan below (`entries_missing_vectors`) is gated on
    // `policy.embed.raw_tier_enabled` (`TACHI_EMBED_RAW_TIER`). Reusing the
    // startup resolution rather than re-resolving keeps one answer per process.
    let store = MemoryStore::open_with_label(db_path, &target.label)
        .map_err(|e| format!("open maintenance DB {}: {e}", target.label))?
        .with_kernel_policy(server.db.kernel_policy.clone());
    let recall_config = memcore::RecallConfig::get();

    store
        .archive_stale_low_value_memories_with_config(recall_config)
        .map_err(|e| format!("truth maintenance prune {}: {e}", target.label))?;

    // ── Self-healing: promote raw → consolidated when DB health ratio is low ──
    let counts = store
        .tier_health_counts()
        .map_err(|e| format!("count tier health {}: {e}", target.label))?;
    let health_ratio = if counts.total_active > 0 {
        counts.consolidated as f64 / counts.total_active as f64
    } else {
        1.0
    };
    if health_ratio < 0.35 && counts.total_active > 0 {
        let promoted = store
            .promote_diversely_recalled_raw_memories(recall_config)
            .map_err(|e| format!("self-heal promote raw memories {}: {e}", target.label))?;
        if promoted > 0 {
            eprintln!(
                "[daily_pipeline] self-heal {}: promoted {promoted} raw → consolidated (ratio was {health_ratio:.2})",
                target.label
            );
        }
    }

    // ── Post-distillation embedding: enqueue non-raw entries without vectors ──
    let needs_embed = store
        .entries_missing_vectors(50)
        .map_err(|e| format!("scan embedding candidates {}: {e}", target.label))?;
    for entry in &needs_embed {
        let _ =
            server
                .enrichment_lock()
                .enrich_tx
                .try_send(crate::enrichment::build_enrichment_item(
                    entry,
                    true,  // needs_embedding
                    false, // needs_summary
                    route.target_db,
                    route.named_project.clone(),
                    route.db_path.clone(),
                    None,
                    None,
                    entry.revision,
                ));
    }

    // tachi#1446 lever 6. `promote_memory_to_durable` below is irreversible, so
    // what feeds its score must not be the system's own display action. Read
    // once outside the loop: the arm is a property of the configuration, not of
    // the candidate, and re-resolving it per entry would let a mid-run config
    // reload promote two candidates under two different rules.
    let promotion_candidates = store
        .promotion_candidate_entries_for_config(200, recall_config)
        .map_err(|e| format!("scan promotion candidates {}: {e}", target.label))?;
    for entry in &promotion_candidates {
        let access_days = store
            .distinct_promotion_days(&entry.id, recall_config)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn target(name: &str, allow_write: bool) -> ManifestDbTarget {
        ManifestDbTarget {
            name: name.to_string(),
            label: format!("{name}-label"),
            path: std::path::PathBuf::from(format!("/{name}.db")),
            role: "project".to_string(),
            owner: "tachi".to_string(),
            schema_kind: "tachi".to_string(),
            allow_write,
            last_classification: "healthy".to_string(),
        }
    }

    #[test]
    fn excluded_target_is_reported_while_writable_peer_remains_runnable() {
        let scope = account_truth_maintenance_scope(vec![
            target("read-only", false),
            target("writable", true),
        ]);

        assert_eq!(scope.examined(), 2);
        assert_eq!(scope.runnable.len(), 1);
        assert_eq!(scope.runnable[0].name, "writable");
        assert_eq!(scope.exclusions.len(), 1);
        assert_eq!(scope.exclusions[0].target, "read-only");
        assert_eq!(scope.exclusions[0].label, "read-only-label");
    }
}
