use super::print_pretty_json;
use std::error::Error;
use std::path::{Path, PathBuf};
use tachi_bootstrap::cli::ManifestAction;

fn manifest_path(app_home: &Path) -> PathBuf {
    app_home.join("manifest.json")
}

fn default_scan_roots(home: &Path, app_home: &Path, git_root: Option<&PathBuf>) -> Vec<PathBuf> {
    let mut roots = crate::doctor::default_scan_roots(home, git_root.map(|p| p.as_path()));
    if app_home.exists() && !roots.iter().any(|root| root == app_home) {
        roots.push(app_home.to_path_buf());
    }
    roots
}

pub(super) async fn run_doctor_command(
    json_output: bool,
    fix: bool,
    roots_override: Vec<PathBuf>,
    jobs_report: bool,
    probe_keys: bool,
    run_daily: bool,
    home: &Path,
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    git_root: Option<&PathBuf>,
) -> Result<(), Box<dyn Error>> {
    let roots: Vec<PathBuf> = if !roots_override.is_empty() {
        roots_override
    } else {
        default_scan_roots(home, app_home, git_root)
    };
    let quarantine_dir = app_home.join("quarantine");
    let opts = crate::doctor::ScanOptions {
        auto_fix: fix,
        max_depth: 10,
    };
    let mut report = crate::doctor::scan(&roots, &quarantine_dir, opts);
    report
        .warnings
        .extend(crate::doctor::project_secret_file_warnings(
            git_root.map(|p| p.as_path()),
        ));

    // Always update the manifest after a doctor run (idempotent; preserves notes).
    let manifest_path = manifest_path(app_home);
    let mut m = crate::manifest::Manifest::load_or_empty(&manifest_path);
    m.populate_from_doctor(&report);
    if let Err(e) = m.save(&manifest_path) {
        eprintln!(
            "[doctor] warning: failed to update manifest at {}: {e}",
            manifest_path.display()
        );
    }

    // --run-daily: unified remediation verb for stale distill marker + probe cache.
    // Runs the provider probe refresh and the daily distill batch, then writes the
    // marker so the next `tachi status` sees a fresh (or failure-detailed) marker
    // instead of a bare stale warning.
    let daily_remediation = if run_daily {
        Some(run_daily_pipeline_remediation(app_home, global_db_path, project_db_path).await)
    } else {
        None
    };

    // Branch #5: optional foundry job-status histogram per manifest DB.
    let jobs_section = if jobs_report {
        Some(collect_job_histograms(&m))
    } else {
        None
    };
    let provider_section =
        collect_provider_key_report(&app_home.join("global").join("memory.db"), probe_keys).await;

    if json_output {
        let mut full = serde_json::to_value(&report)?;
        if let Some(jobs) = &jobs_section {
            if let Some(obj) = full.as_object_mut() {
                obj.insert("foundry_jobs".into(), serde_json::to_value(jobs)?);
            }
        }
        if let Some(obj) = full.as_object_mut() {
            obj.insert(
                "provider_keys".into(),
                serde_json::to_value(&provider_section)?,
            );
            obj.insert(
                "models".into(),
                serde_json::to_value(crate::status_ops::status_health::model_lanes_json())?,
            );
            if let Some(remediation) = &daily_remediation {
                obj.insert(
                    "daily_remediation".into(),
                    serde_json::Value::String(remediation.clone()),
                );
            }
        }
        print_pretty_json(&full)
    } else {
        println!("{}", crate::doctor::render_report(&report));
        println!();
        println!(
            "manifest: {} ({} dbs recorded)",
            manifest_path.display(),
            m.dbs.len()
        );
        if let Some(jobs) = &jobs_section {
            println!("\n=== foundry jobs ===");
            for entry in jobs {
                let h = &entry.histogram;
                println!(
                    "  {} planned={} queued={} running={} completed={} failed={} skipped={} other={} gc_eligible(>=30d)={} total={}",
                    entry.path,
                    h.planned, h.queued, h.running, h.completed, h.failed, h.skipped,
                    h.other.iter().map(|(_, n)| n).sum::<usize>(),
                    h.gc_eligible, h.total
                );
                if !entry.error.is_empty() {
                    println!("    error: {}", entry.error);
                }
            }
        }
        println!("\n=== provider keys ===");
        for key in &provider_section.keys {
            println!(
                "  {} status={} source={} required={} deprecated={}",
                key.name, key.status, key.source, key.required, key.deprecated
            );
        }
        if probe_keys {
            for probe in &provider_section.probes {
                println!(
                    "  probe {}: {}{}",
                    probe.name,
                    probe.status,
                    probe
                        .message
                        .as_ref()
                        .map(|msg| format!(" ({msg})"))
                        .unwrap_or_default()
                );
            }
        } else {
            println!("  live probes skipped (pass --probe-keys to test providers)");
        }
        println!("\n=== model lanes ===");
        println!("  embedding: voyage-4 (1024d), key=VOYAGE_API_KEY");
        println!("  rerank: rerank-2.5, keys=VOYAGE_RERANK_API_KEY or VOYAGE_API_KEY");
        println!("  extract/summary: Qwen/Qwen3.5-27B via SiliconFlow-compatible chat");
        println!("  distill/reasoning: Claude CLI first, chat fallback lanes");
        if let Some(remediation) = &daily_remediation {
            println!("\n=== daily pipeline remediation ===");
            println!("{remediation}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_path_lives_directly_under_app_home() {
        let app_home = std::path::Path::new("/tmp/tachi-app-home");
        assert_eq!(manifest_path(app_home), app_home.join("manifest.json"));
    }

    #[test]
    fn default_scan_roots_include_custom_app_home_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let app_home = temp.path().join("custom-tachi");
        std::fs::create_dir_all(&app_home).expect("create custom app home");

        let roots = default_scan_roots(&home, &app_home, None);
        assert!(
            roots.iter().any(|root| root == &app_home),
            "custom app home should be scanned: {roots:?}"
        );

        let default_app_home = app_home.join(".tachi");
        std::fs::create_dir_all(&default_app_home).expect("create default app home");
        let roots = default_scan_roots(&app_home, &default_app_home, None);
        assert_eq!(
            roots
                .iter()
                .filter(|root| *root == &default_app_home)
                .count(),
            1,
            "app home should not be duplicated: {roots:?}"
        );
    }
}

#[derive(Debug, serde::Serialize)]
struct ProviderKeyReport {
    keys: Vec<ProviderKeyStatus>,
    probes: Vec<crate::status_ops::status_health::ProviderProbeResult>,
}

#[derive(Debug, serde::Serialize)]
struct ProviderKeyStatus {
    name: String,
    label: String,
    required: bool,
    deprecated: bool,
    canonical_name: String,
    alias_names: Vec<String>,
    cleanup_hint: Option<String>,
    status: String,
    source: String,
}

impl From<crate::status_ops::ApiKeyStatus> for ProviderKeyStatus {
    fn from(status: crate::status_ops::ApiKeyStatus) -> Self {
        Self {
            name: status.name,
            label: status.label,
            required: status.required,
            deprecated: status.deprecated,
            canonical_name: status.canonical_name,
            alias_names: status.alias_names,
            cleanup_hint: status.cleanup_hint,
            status: status.status,
            source: status.source,
        }
    }
}

/// Runs the daily pipeline remediation when `--run-daily` is passed:
/// 1. Refreshes the provider key probe cache.
/// 2. Runs the daily distill batch and writes the marker.
///
/// Returns a human-readable summary line for the doctor report.
async fn run_daily_pipeline_remediation(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> String {
    // Step 1: refresh provider probe cache.
    let probe_result =
        crate::status_ops::status_health::refresh_doctor_probe_cache(app_home, global_db_path)
            .await;
    let probe_summary = match &probe_result {
        Ok(cache) => {
            let failed = cache.probes.iter().filter(|p| p.status != "ok").count();
            format!(
                "probe cache refreshed ({} probes, {failed} failed)",
                cache.probes.len()
            )
        }
        Err(e) => format!("probe cache refresh failed: {e}"),
    };

    // Step 2: run distill batch (requires a project DB).
    let distill_summary = match project_db_path {
        Some(_) => {
            let marker_path = app_home.join("foundry-runs").join(".last_distill_run");
            match crate::MemoryServer::new(
                global_db_path.to_path_buf(),
                project_db_path.map(|p| p.to_path_buf()),
            ) {
                Ok(server) => {
                    match crate::foundry_runtime_ops::run_daily_batch_distill(&server).await {
                        Ok(report) => {
                            // Write success marker (same shape as the scheduler).
                            if let Some(parent) = marker_path.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            let marker_body = serde_json::json!({
                                "ts": chrono::Utc::now().to_rfc3339(),
                                "groups_distilled": report.groups_distilled,
                                "groups_skipped": report.groups_skipped,
                                "fallback_used": report.fallback_used,
                                "errors": report.errors.len(),
                            })
                            .to_string();
                            let _ = std::fs::write(&marker_path, marker_body);
                            format!(
                                "distill: dispatched={} distilled={} skipped={} fallback={} errors={}",
                                report.batches_dispatched,
                                report.groups_distilled,
                                report.groups_skipped,
                                report.fallback_used,
                                report.errors.len()
                            )
                        }
                        Err(e) => {
                            // Write failure marker so status surfaces the reason.
                            if let Some(parent) = marker_path.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            let marker_body = serde_json::json!({
                                "ts": chrono::Utc::now().to_rfc3339(),
                                "error": e.to_string(),
                                "groups_distilled": 0,
                                "groups_skipped": 0,
                                "fallback_used": 0,
                                "errors": 0,
                            })
                            .to_string();
                            let _ = std::fs::write(&marker_path, marker_body);
                            format!("distill batch failed: {e}")
                        }
                    }
                }
                Err(e) => format!("distill skipped (server init failed): {e}"),
            }
        }
        None => "distill skipped (no project DB)".to_string(),
    };

    format!("  {probe_summary}\n  {distill_summary}")
}

async fn collect_provider_key_report(global_db_path: &Path, probe_keys: bool) -> ProviderKeyReport {
    let (keys, probes) = crate::status_ops::status_health::collect_doctor_provider_key_report(
        global_db_path,
        probe_keys,
    )
    .await;
    ProviderKeyReport {
        keys: keys.into_iter().map(ProviderKeyStatus::from).collect(),
        probes,
    }
}

#[derive(Debug, serde::Serialize)]
struct DbJobReport {
    path: String,
    histogram: memcore::JobStatusHistogram,
    error: String,
}

fn collect_job_histograms(manifest: &crate::manifest::Manifest) -> Vec<DbJobReport> {
    let mut out = Vec::new();
    for entry in &manifest.dbs {
        // Read-only: open the connection directly without going through MemoryStore::open
        // which would try to write schema. We use rusqlite OpenFlags to be paranoid.
        let conn = match rusqlite::Connection::open_with_flags(
            &entry.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(c) => c,
            Err(e) => {
                out.push(DbJobReport {
                    path: entry.path.clone(),
                    histogram: memcore::JobStatusHistogram::default(),
                    error: format!("open failed: {e}"),
                });
                continue;
            }
        };
        let hist = match memcore::job_status_histogram(&conn, 30) {
            Ok(h) => h,
            Err(e) => {
                out.push(DbJobReport {
                    path: entry.path.clone(),
                    histogram: memcore::JobStatusHistogram::default(),
                    error: format!("histogram failed: {e}"),
                });
                continue;
            }
        };
        // Skip silent zero-job DBs to keep the report focused.
        if hist.total > 0 {
            out.push(DbJobReport {
                path: entry.path.clone(),
                histogram: hist,
                error: String::new(),
            });
        }
    }
    out
}

pub(super) async fn run_manifest_command(
    action: ManifestAction,
    home: &Path,
    app_home: &Path,
    git_root: Option<&PathBuf>,
) -> Result<(), Box<dyn Error>> {
    let manifest_path = manifest_path(app_home);

    match action {
        ManifestAction::Show { json } => {
            let m = crate::manifest::Manifest::load_or_empty(&manifest_path);
            if json {
                print_pretty_json(&serde_json::to_value(&m)?)
            } else {
                println!("{}", crate::manifest::render_manifest(&m));
                println!("(stored at {})", manifest_path.display());
                Ok(())
            }
        }
        ManifestAction::Init | ManifestAction::Refresh => {
            let roots = default_scan_roots(home, app_home, git_root);
            let quarantine_dir = app_home.join("quarantine");
            let opts = crate::doctor::ScanOptions {
                auto_fix: false,
                max_depth: 10,
            };
            let report = crate::doctor::scan(&roots, &quarantine_dir, opts);
            let mut m = crate::manifest::Manifest::load_or_empty(&manifest_path);
            m.populate_from_doctor(&report);
            m.save(&manifest_path)?;
            println!(
                "manifest written: {} ({} dbs)",
                manifest_path.display(),
                m.dbs.len()
            );
            Ok(())
        }
        ManifestAction::Resolve { target } => {
            let m = crate::manifest::Manifest::load_or_empty(&manifest_path);
            // Try exact path first.
            if let Some(e) = m.lookup(&target) {
                println!(
                    "[exact] {} role={:?} owner={} writable={} schema={} last={}",
                    e.path, e.role, e.owner, e.allow_write, e.schema_kind, e.last_classification
                );
                return Ok(());
            }
            // Else interpret as a scope hint.
            let matches: Vec<_> = m
                .dbs
                .iter()
                .filter(|e| e.scope_hint == target || e.owner == target)
                .collect();
            if matches.is_empty() {
                println!(
                    "no manifest entry matches '{target}' (try `tachi manifest show` to list, or `tachi doctor` to refresh)"
                );
            } else {
                for e in matches {
                    println!(
                        "[scope] {} role={:?} owner={} writable={} schema={} last={}",
                        e.path,
                        e.role,
                        e.owner,
                        e.allow_write,
                        e.schema_kind,
                        e.last_classification
                    );
                }
            }
            Ok(())
        }
        ManifestAction::Sweep { apply, json } => {
            let roots = default_scan_roots(home, app_home, git_root);
            let quarantine_dir = app_home.join("quarantine");
            let opts = crate::doctor::ScanOptions {
                auto_fix: false,
                max_depth: 10,
            };
            let report = crate::doctor::scan(&roots, &quarantine_dir, opts);
            let m = crate::manifest::Manifest::load_or_empty(&manifest_path);
            let mut plan = crate::manifest::plan_sweep(&report, &m, &quarantine_dir);
            if apply {
                plan = crate::manifest::apply_sweep(plan, &quarantine_dir);
            }
            if json {
                print_pretty_json(&serde_json::to_value(&plan)?)
            } else {
                println!(
                    "sweep {}: planned={} applied={} skipped={}",
                    if apply { "applied" } else { "dry-run" },
                    plan.planned.len(),
                    plan.applied.len(),
                    plan.skipped.len()
                );
                for a in &plan.planned {
                    println!(
                        "  [plan]   {} → {}  ({})",
                        a.path,
                        a.quarantine_to.as_deref().unwrap_or("-"),
                        a.reason
                    );
                }
                for a in &plan.applied {
                    println!(
                        "  [moved]  {} → {}  ({})",
                        a.path,
                        a.quarantine_to.as_deref().unwrap_or("-"),
                        a.reason
                    );
                }
                for a in &plan.skipped {
                    println!("  [skip]   {}  ({})", a.path, a.note);
                }
                if !apply {
                    println!("\n(dry-run; re-run with --apply to move files)");
                }
                Ok(())
            }
        }
        ManifestAction::Gc { json } => {
            let report = crate::manifest::gc_manifest(&manifest_path)?;
            if json {
                print_pretty_json(&serde_json::to_value(&report)?)
            } else {
                println!(
                    "manifest gc: before={} after={} canonicalized={} removed_missing={} removed_fixture={} schema_kind_fixed={} dedup_collapsed={}{}",
                    report.entries_before,
                    report.entries_after,
                    report.canonicalized,
                    report.removed_missing,
                    report.removed_fixture,
                    report.schema_kind_fixed,
                    report.dedup_collapsed,
                    if report.aborted {
                        format!(" ABORTED: {}", report.abort_reason.as_deref().unwrap_or("?"))
                    } else {
                        String::new()
                    }
                );
                println!("(manifest at {})", manifest_path.display());
                Ok(())
            }
        }
        ManifestAction::AuditProjects { json, apply } => {
            run_audit_projects(apply, json, app_home, git_root, &manifest_path)
        }
    }
}

/// DRY-RUN project-DB relocation audit. Enumerates
/// `<app_home>/projects/*/memory.db`, classifies each, and prints a relocation
/// plan. Moves/deletes NOTHING. `--apply` is intentionally refused for now —
/// any real mutation must take a backup and refuse on ambiguity, which is out of
/// scope for this read-only audit surface.
fn run_audit_projects(
    apply: bool,
    json: bool,
    app_home: &Path,
    git_root: Option<&PathBuf>,
    manifest_path: &Path,
) -> Result<(), Box<dyn Error>> {
    use memory_server_manifest_audit::{build_plan, gather_project_inputs, render_plan};

    let projects_dir = app_home.join("projects");

    // (scope_hint, path) pairs for project-role entries, used to resolve owning
    // repos from repo-local `.tachi/memory.db` paths already in the manifest.
    let manifest = crate::manifest::Manifest::load_or_empty(manifest_path);
    let manifest_project_paths: Vec<(String, String)> = manifest
        .dbs
        .iter()
        .filter(|e| matches!(e.role, crate::manifest::DbRole::Project))
        .map(|e| (e.scope_hint.clone(), e.path.clone()))
        .collect();

    // Candidate git roots: the current invocation's git root (if any). Kept
    // minimal and read-only; the manifest path resolution covers most cases.
    let candidate_git_roots: Vec<PathBuf> = git_root.into_iter().cloned().collect();

    let inputs =
        gather_project_inputs(&projects_dir, &manifest_project_paths, &candidate_git_roots)?;
    let plan = build_plan(&inputs);

    if json {
        print_pretty_json(&serde_json::to_value(&plan)?)?;
    } else {
        print!("{}", render_plan(&plan, &projects_dir));
        println!(
            "\nsummary: {} relocatable, {} home-resident, {} symlink-alias, {} broken-symlink, {} garbage",
            plan.n_relocatable,
            plan.n_home_resident,
            plan.n_symlink_alias,
            plan.n_symlink_broken,
            plan.n_garbage,
        );
        println!("(DRY-RUN: nothing was moved or deleted)");
    }

    if apply {
        // Explicit refusal: this audit surface is plan-only by design. A real
        // mutation pass must back up first and refuse on ambiguity; until that
        // is built and reviewed, --apply must not touch real per-project data.
        return Err(
            "--apply is not yet implemented for `manifest audit-projects`: this command is \
             plan-only and refuses to move or delete real project data. Review the printed plan \
             and relocate manually, or wait for the guarded --apply pass."
                .into(),
        );
    }

    Ok(())
}
