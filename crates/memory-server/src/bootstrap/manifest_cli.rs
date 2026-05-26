use super::*;

pub(super) async fn run_doctor_command(
    json_output: bool,
    scan_only: bool,
    roots_override: Vec<PathBuf>,
    jobs_report: bool,
    probe_keys: bool,
    home: &std::path::Path,
    app_home: &std::path::Path,
    git_root: Option<&PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let roots: Vec<PathBuf> = if !roots_override.is_empty() {
        roots_override
    } else {
        crate::doctor::default_scan_roots(home, git_root.map(|p| p.as_path()))
    };
    let quarantine_dir = app_home.join("quarantine");
    let opts = crate::doctor::ScanOptions {
        auto_fix: !scan_only,
        max_depth: 10,
    };
    let report = crate::doctor::scan(&roots, &quarantine_dir, opts);

    // Always update the manifest after a doctor run (idempotent; preserves notes).
    let manifest_path = crate::manifest::Manifest::default_path(home);
    let mut m = crate::manifest::Manifest::load_or_empty(&manifest_path);
    m.populate_from_doctor(&report);
    if let Err(e) = m.save(&manifest_path) {
        eprintln!(
            "[doctor] warning: failed to update manifest at {}: {e}",
            manifest_path.display()
        );
    }

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
                serde_json::to_value(crate::status_ops::model_lanes_json())?,
            );
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
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
struct ProviderKeyReport {
    keys: Vec<ProviderKeyStatus>,
    probes: Vec<crate::status_ops::ProviderProbeResult>,
}

#[derive(Debug, serde::Serialize)]
struct ProviderKeyStatus {
    name: String,
    label: String,
    required: bool,
    deprecated: bool,
    status: String,
    source: String,
}

// Use the canonical key definitions from status_ops to avoid divergence.
use crate::status_ops::API_KEY_DEFS;

async fn collect_provider_key_report(
    global_db_path: &std::path::Path,
    probe_keys: bool,
) -> ProviderKeyReport {
    let keys = collect_provider_key_status(global_db_path);
    let probes = if probe_keys {
        crate::status_ops::run_provider_probes(global_db_path).await
    } else {
        Vec::new()
    };
    ProviderKeyReport { keys, probes }
}

fn collect_provider_key_status(global_db_path: &std::path::Path) -> Vec<ProviderKeyStatus> {
    let statuses = crate::status_ops::provider_key_status_json(global_db_path);
    statuses
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| {
            let def = API_KEY_DEFS.iter().find(|def| {
                value
                    .get("name")
                    .and_then(|v| v.as_str())
                    .is_some_and(|name| name == def.key)
            })?;
            Some(ProviderKeyStatus {
                name: def.key.to_string(),
                label: def.label.to_string(),
                required: def.required,
                deprecated: def.deprecated,
                status: value
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                source: value
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
            })
        })
        .collect()
}

#[derive(Debug, serde::Serialize)]
struct DbJobReport {
    path: String,
    histogram: memory_core::JobStatusHistogram,
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
                    histogram: memory_core::JobStatusHistogram::default(),
                    error: format!("open failed: {e}"),
                });
                continue;
            }
        };
        let hist = match memory_core::job_status_histogram(&conn, 30) {
            Ok(h) => h,
            Err(e) => {
                out.push(DbJobReport {
                    path: entry.path.clone(),
                    histogram: memory_core::JobStatusHistogram::default(),
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
    home: &std::path::Path,
    app_home: &std::path::Path,
    git_root: Option<&PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let manifest_path = crate::manifest::Manifest::default_path(home);

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
            let roots = crate::doctor::default_scan_roots(home, git_root.map(|p| p.as_path()));
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
            let roots = crate::doctor::default_scan_roots(home, git_root.map(|p| p.as_path()));
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
    }
}
