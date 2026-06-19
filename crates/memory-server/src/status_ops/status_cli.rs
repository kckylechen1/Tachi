use std::path::{Path, PathBuf};

use serde_json::json;

use memory_core::{get_foundry_config, set_foundry_config, MemoryStore, PerDbConfig};

use crate::cli::{DaemonAction, FoundryAction, WatcherAction};
use crate::manifest::Manifest;

pub(crate) async fn run_status(
    watch: bool,
    json_out: bool,
    hide_orphans: bool,
    probe_keys: bool,
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !watch {
        return render_one(
            json_out,
            hide_orphans,
            probe_keys,
            app_home,
            global_db_path,
            project_db_path,
        )
        .await;
    }

    if json_out {
        return render_one(
            true,
            hide_orphans,
            probe_keys,
            app_home,
            global_db_path,
            project_db_path,
        )
        .await;
    }

    loop {
        print!("\x1b[2J\x1b[H");
        if let Err(e) = render_one(
            false,
            hide_orphans,
            probe_keys,
            app_home,
            global_db_path,
            project_db_path,
        )
        .await
        {
            eprintln!("[!] status render failed: {e}");
        }
        tokio::time::sleep(crate::status_ops::WATCH_INTERVAL).await;
    }
}

async fn render_one(
    json_out: bool,
    hide_orphans: bool,
    probe_keys: bool,
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let provider_probe_report = if probe_keys {
        let report = super::status_health::run_provider_probe_report(global_db_path).await;
        if let Err(err) =
            super::status_health::write_provider_probe_cache_report(app_home, report.clone())
        {
            eprintln!("[!] provider probe cache write failed: {err}");
        }
        report
    } else {
        super::status_health::ProviderProbeReport {
            probes: Vec::new(),
            rotation_groups: Vec::new(),
        }
    };
    let snapshot = if probe_keys {
        crate::status_ops::collect_snapshot_with_provider_value_compare(
            app_home,
            global_db_path,
            project_db_path,
        )
    } else {
        crate::status_ops::collect_snapshot(app_home, global_db_path, project_db_path)
    };

    if json_out {
        let mut v = serde_json::to_value(&snapshot)?;
        if let Some(obj) = v.as_object_mut() {
            obj.insert(
                "provider_probes".to_string(),
                serde_json::to_value(&provider_probe_report.probes)?,
            );
            obj.insert(
                "provider_rotation_groups".to_string(),
                serde_json::to_value(&provider_probe_report.rotation_groups)?,
            );
        }
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }

    println!("tachi status @ {}", chrono::Utc::now().to_rfc3339());
    println!("  app_home: {}", app_home.display());
    println!();

    println!("Daemon");
    match &snapshot.daemon {
        crate::status_ops::DaemonStatus::Running { pid, lock_path } => {
            println!("  [OK] running pid={pid} lock={}", lock_path.display());
        }
        crate::status_ops::DaemonStatus::Foreign {
            pid,
            lock_path,
            reason,
            version,
            port,
            global_db,
        } => {
            println!(
                "  [!] foreign daemon pid={pid} lock={} reason={reason}",
                lock_path.display()
            );
            println!(
                "      version={} port={} global_db={}",
                version.as_deref().unwrap_or("unknown"),
                port.map(|p| p.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                global_db.as_deref().unwrap_or("unknown")
            );
        }
        crate::status_ops::DaemonStatus::StalePid { pid, lock_path } => {
            println!(
                "  [!] stale pid file pid={pid} (process not alive); lock at {}",
                lock_path.display()
            );
        }
        crate::status_ops::DaemonStatus::None => {
            println!("  [OK] no daemon running (single-process mode)");
        }
    }
    println!();

    let visible_dbs: Vec<&crate::status_ops::DbStatus> = snapshot
        .dbs
        .iter()
        .filter(|db| !(hide_orphans && db.orphan))
        .collect();
    let hidden_orphans = snapshot.dbs.len() - visible_dbs.len();

    println!("Manifest ({} dbs)", snapshot.dbs.len());
    if snapshot.dbs.is_empty() {
        println!("  [!] manifest empty or missing — run `tachi doctor` to populate it");
    }
    if hidden_orphans > 0 {
        println!(
            "  [i] {hidden_orphans} orphan db{plural} hidden by --hide-orphans (still counted in Summary)",
            plural = if hidden_orphans == 1 { "" } else { "s" },
        );
    }
    for db in &visible_dbs {
        let stuck_marker = if db.stuck_in_progress > 0 {
            format!(" [!] {} stuck running", db.stuck_in_progress)
        } else {
            String::new()
        };
        let orphan_marker = if db.orphan {
            " [i] orphan (no scheduler route on this host — informational, not an error)"
        } else {
            ""
        };
        println!(
            "  [OK] {label:<20} active={active:<4} pending={pending:<4} running={running:<3} failed={failed:<3} history={terminal:<5} gc_eligible={gc:<4}{orphan}{stuck}",
            label = crate::status_ops::truncate(&db.label, 20),
            active = db.active_jobs,
            pending = db.pending,
            running = db.running,
            failed = db.failed,
            terminal = db.terminal_jobs,
            gc = db.gc_eligible,
            orphan = orphan_marker,
            stuck = stuck_marker,
        );
        if db.memory_total > 0 {
            let pct = db.vector_coverage * 100.0;
            let marker = if crate::status_ops::vector_dimension_mismatch(db)
                || (db.memory_total > 0 && db.vector_coverage < 0.9)
                || db.enrichment_failed_recent > 0
                || db.vector_orphans > 0
            {
                "[!]"
            } else if db.vector_missing > 0 {
                "[i]"
            } else {
                "[OK]"
            };
            let failures = if db.enrichment_failed_recent > 0 {
                format!(" enrichment_failed={}", db.enrichment_failed_recent)
            } else {
                String::new()
            };
            let pending_enrichment = if db.pending_enrichment > 0 {
                format!(" pending_enrichment={}", db.pending_enrichment)
            } else {
                String::new()
            };
            let orphans = if db.vector_orphans > 0 {
                format!(" orphans={}", db.vector_orphans)
            } else {
                String::new()
            };
            let dim = db
                .vector_dimension
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            println!(
                "       {marker} vectors={}/{} missing={} coverage={pct:.1}% dim={}{}{}{}",
                db.vector_count,
                db.memory_total,
                db.vector_missing,
                dim,
                pending_enrichment,
                failures,
                orphans
            );
        }
        if let Some(job) = &db.latest_active_job {
            println!(
                "       [i] latest_active kind={} status={} at={}",
                job.kind,
                job.status,
                job.updated_at.as_deref().unwrap_or("unknown")
            );
        }
        if db.active_jobs == 0 {
            if let Some(job) = &db.latest_terminal_job {
                println!(
                    "       [i] last_terminal kind={} status={} at={}",
                    job.kind,
                    job.status,
                    job.updated_at.as_deref().unwrap_or("unknown")
                );
            }
        }
        if let Some(job) = &db.latest_failed_job {
            let inferred = job
                .inferred_invalid_provider
                .as_deref()
                .map(|provider| format!(" provider={provider}"))
                .unwrap_or_default();
            println!(
                "       [X] latest_failed kind={} lane={} at={}{} reason={}",
                job.kind,
                job.lane.as_deref().unwrap_or("unknown"),
                job.updated_at.as_deref().unwrap_or("unknown"),
                inferred,
                crate::status_ops::truncate(job.reason.as_deref().unwrap_or("unknown"), 96)
            );
        }
        if let Some(err) = &db.error {
            println!("       [X] {err}");
        }
    }
    println!();

    println!("Dispatch Ledger (recent, not background worker queue)");
    if snapshot.dispatches.is_empty() {
        println!("  (none)");
    } else {
        for d in &snapshot.dispatches {
            let icon = match d.outcome.as_str() {
                "completed" | "success" if d.reviewed => "[OK]",
                "completed" | "success" => "[!] ",
                "in_progress" => "[..]",
                "stale_working" => "[!] ",
                _ => "[X] ",
            };
            let review_tag = if !d.reviewed && matches!(d.outcome.as_str(), "completed" | "success")
            {
                " unreviewed"
            } else {
                ""
            };
            println!(
                "  {icon} {id:<12} {agent:<14} \"{task}\"  {outcome} {elapsed}{review}",
                id = crate::status_ops::truncate(&d.dispatch_id, 12),
                agent = crate::status_ops::truncate(&d.agent, 14),
                task = crate::status_ops::truncate(&d.task, 40),
                outcome = d.outcome,
                elapsed = d.elapsed,
                review = review_tag,
            );
        }
    }
    println!();

    println!("Recent Evals");
    if snapshot.recent_evals.is_empty() {
        println!("  (none)");
    } else {
        for e in &snapshot.recent_evals {
            let icon = match e.outcome.as_str() {
                "success" => "[OK]",
                "partial" => "[~] ",
                _ => "[X] ",
            };
            let quality = e
                .quality_score
                .map(|q| format!("  quality={q:.2}"))
                .unwrap_or_default();
            println!(
                "  {icon} {id:<24} {agent:<14} {outcome}{quality}",
                id = crate::status_ops::truncate(&e.task_id, 24),
                agent = crate::status_ops::truncate(&e.agent, 14),
                outcome = e.outcome,
            );
        }
    }
    println!();

    println!("Daily Pipeline");
    match &snapshot.last_daily_report {
        Some(p) => println!("  [OK] last report: {p}"),
        None => println!("  (no daily reports yet)"),
    }
    match &snapshot.distill_marker {
        Some(marker) => {
            let icon = if marker.is_stale { "[!]" } else { "[OK]" };
            println!(
                "  {icon} last distill: {} ({}) marker={}",
                marker.last_run_at, marker.age, marker.path
            );
        }
        None => println!("  [!] last distill marker missing"),
    }
    println!();

    println!("Provider Keys");
    for key in &snapshot.api_keys {
        let marker = match key.status.as_str() {
            "configured" => "[OK]",
            "drift" => "[i]",
            "deprecated-unset" => "[i]",
            _ if key.required => "[!]",
            _ => "[i]",
        };
        println!(
            "  {marker} {name:<24} {status:<16} source={source} ({label})",
            name = key.name,
            status = key.status,
            source = key.source,
            label = key.label
        );
        if let Some(provider) = &key.inferred_invalid_provider {
            println!("       [X] inferred invalid provider/key from failed jobs: {provider}");
        }
        if let Some(warning) = &key.drift_warning {
            println!("       [i] {warning}");
        }
        if let Some(hint) = &key.cleanup_hint {
            println!("       [i] {hint}");
        }
        if let Some(rotation) = &key.rotation {
            println!(
                "       [i] rotation: total={} configured={} current={} strategy={} healthy={} rate_limited={} auth_failed={}",
                rotation.total_keys,
                rotation.configured_keys,
                rotation.current_index,
                rotation.strategy,
                rotation
                    .healthy_keys
                    .map(|count| count.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                rotation.rate_limited_keys,
                rotation.auth_failed_keys,
            );
            for member in &rotation.members {
                println!(
                    "           - {}: {}{}",
                    member.name,
                    member.status,
                    member
                        .message
                        .as_ref()
                        .map(|msg| format!(" ({})", crate::status_ops::truncate(msg, 72)))
                        .unwrap_or_default()
                );
            }
        }
    }
    if probe_keys {
        println!("  live probes:");
        for probe in &provider_probe_report.probes {
            println!(
                "    {}: {}{}",
                probe.name,
                probe.status,
                probe
                    .message
                    .as_ref()
                    .map(|msg| format!(" ({})", crate::status_ops::truncate(msg, 96)))
                    .unwrap_or_default()
            );
        }
        render_rotation_group_probes(&provider_probe_report.rotation_groups);
    } else if let Some(cache) = &snapshot.provider_probe_cache {
        let marker = if cache.is_stale() { "[!]" } else { "[OK]" };
        println!(
            "  {marker} cached probes: last={} ttl={}h",
            cache.last_probe_at,
            cache.ttl_seconds / 3600
        );
        for probe in &cache.probes {
            println!(
                "    {}: {}{}",
                probe.name,
                probe.status,
                probe
                    .message
                    .as_ref()
                    .map(|msg| format!(" ({})", crate::status_ops::truncate(msg, 96)))
                    .unwrap_or_default()
            );
        }
        render_rotation_group_probes(&cache.rotation_groups);
    } else {
        println!("  live probes skipped (pass --probe-keys to test providers)");
    }
    println!();

    if !snapshot.project_warnings.is_empty() {
        println!("Project Warnings");
        for warning in &snapshot.project_warnings {
            println!("  [!] {warning}");
        }
        println!();
    }

    let total_pending: usize = snapshot.dbs.iter().map(|d| d.pending).sum();
    let total_orphan = snapshot.dbs.iter().filter(|d| d.orphan).count();
    let total_stuck: usize = snapshot.dbs.iter().map(|d| d.stuck_in_progress).sum();
    println!(
        "Summary: health_score={score}/100, {n} dbs, {pending} total pending, {orphan} orphan (informational), {stuck} stuck running",
        score = snapshot.health_score,
        n = snapshot.dbs.len(),
        pending = total_pending,
        orphan = total_orphan,
        stuck = total_stuck
    );

    Ok(())
}

fn render_rotation_group_probes(groups: &[super::status_health::ProviderRotationGroupProbe]) {
    if groups.is_empty() {
        return;
    }
    println!("  rotation groups:");
    for group in groups {
        println!(
            "    {}: total={} configured={} healthy={} rate_limited={} auth_failed={} current={} strategy={}",
            group.logical_name,
            group.total_keys,
            group.configured_keys,
            group.healthy_keys,
            group.rate_limited_keys,
            group.auth_failed_keys,
            group.current_index,
            group.strategy,
        );
        for key in &group.keys {
            println!(
                "      - {}: {}{}",
                key.name,
                key.status,
                key.message
                    .as_ref()
                    .map(|msg| format!(" ({})", crate::status_ops::truncate(msg, 72)))
                    .unwrap_or_default()
            );
        }
    }
}

pub(crate) async fn run_daemon(
    action: DaemonAction,
    app_home: &Path,
    global_db_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        DaemonAction::Status { json: json_out } => {
            let daemon = crate::status_ops::collect_daemon_status(app_home, global_db_path);
            if json_out {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::to_value(&daemon)?)?
                );
            } else {
                match daemon {
                    crate::status_ops::DaemonStatus::Running { pid, lock_path } => {
                        println!("[OK] daemon running pid={pid} lock={}", lock_path.display())
                    }
                    crate::status_ops::DaemonStatus::Foreign {
                        pid,
                        lock_path,
                        reason,
                        version,
                        port,
                        global_db,
                    } => {
                        println!(
                            "[!] foreign daemon pid={pid} lock={} reason={reason}",
                            lock_path.display()
                        );
                        println!(
                            "    version={} port={} global_db={}",
                            version.as_deref().unwrap_or("unknown"),
                            port.map(|p| p.to_string())
                                .unwrap_or_else(|| "unknown".to_string()),
                            global_db.as_deref().unwrap_or("unknown")
                        );
                    }
                    crate::status_ops::DaemonStatus::StalePid { pid, lock_path } => {
                        println!(
                            "[!] stale pid file pid={pid} at {} (process not alive)",
                            lock_path.display()
                        )
                    }
                    crate::status_ops::DaemonStatus::None => println!("[OK] no daemon running"),
                }
            }
            Ok(())
        }
        DaemonAction::Kill { force } => {
            match crate::status_ops::collect_daemon_status(app_home, global_db_path) {
                crate::status_ops::DaemonStatus::Running { pid, .. } => {
                    #[cfg(unix)]
                    {
                        let r = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                        if r == 0 {
                            println!("[OK] sent SIGTERM to daemon pid={pid}");
                        } else {
                            let err = std::io::Error::last_os_error();
                            return Err(format!("kill({pid}) failed: {err}").into());
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        return Err("daemon kill is only implemented on unix".into());
                    }
                }
                crate::status_ops::DaemonStatus::StalePid { pid, lock_path } => {
                    if force {
                        let _ = std::fs::remove_file(&lock_path);
                        println!(
                            "[OK] removed stale lock {} (pid {pid} was not alive)",
                            lock_path.display()
                        );
                    } else {
                        println!(
                            "[!] pid {pid} in {} is not alive; rerun with --force to unlink the stale lock",
                            lock_path.display()
                        );
                    }
                }
                crate::status_ops::DaemonStatus::Foreign { reason, .. } => {
                    println!("[!] refusing to kill foreign daemon for current DB scope: {reason}");
                }
                crate::status_ops::DaemonStatus::None => {
                    println!("[OK] no daemon to kill for current DB scope");
                }
            }
            Ok(())
        }
    }
}

pub(crate) async fn run_watcher(
    action: WatcherAction,
    global_db_path: &Path,
    project_db_path: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let server = crate::MemoryServer::new(global_db_path.to_path_buf(), project_db_path)?;
    match action {
        WatcherAction::Status { json: json_out } => {
            let watcher = crate::facade_memory_ops::claude_jsonl_passive_watcher_status();
            if json_out {
                println!("{}", serde_json::to_string_pretty(&watcher)?);
            } else {
                println!(
                    "passive watcher: {}",
                    watcher["status"].as_str().unwrap_or("unknown")
                );
                if let Some(path) = watcher.get("latest_jsonl").and_then(|v| v.as_str()) {
                    println!("  latest_jsonl: {path}");
                }
                if let Some(note) = watcher.get("note").and_then(|v| v.as_str()) {
                    println!("  note: {note}");
                }
            }
            Ok(())
        }
        WatcherAction::CaptureLatest { json: json_out } => {
            let captured =
                crate::facade_memory_ops::capture_latest_claude_jsonl_checkpoint(&server)
                    .await?
                    .unwrap_or_else(|| json!({"status":"not_detected"}));
            if json_out {
                println!("{}", serde_json::to_string_pretty(&captured)?);
            } else {
                println!(
                    "passive watcher capture: {}",
                    captured["status"].as_str().unwrap_or("unknown")
                );
                if let Some(path) = captured.get("path").and_then(|v| v.as_str()) {
                    println!("  source: {path}");
                }
            }
            Ok(())
        }
    }
}

pub(crate) async fn run_foundry(
    action: FoundryAction,
    app_home: &Path,
    global_db_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        FoundryAction::ConfigGet { db } => {
            let target = db.unwrap_or_else(|| global_db_path.to_path_buf());
            let cfg = read_per_db_config(&target)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "db": target.display().to_string(),
                    "config": cfg,
                }))?
            );
            Ok(())
        }
        FoundryAction::ConfigSet {
            db,
            enabled,
            max_jobs_per_minute,
            distill_concurrency,
            enrichment_concurrency,
            llm_provider_override,
        } => {
            let target = db.unwrap_or_else(|| global_db_path.to_path_buf());
            let path_str = target
                .to_str()
                .ok_or_else(|| format!("non-utf8 path: {}", target.display()))?;
            let store = MemoryStore::open_with_label(path_str, "tachi-foundry-config")
                .map_err(|e| format!("open {}: {e}", target.display()))?;
            let mut cfg = get_foundry_config(store.connection())
                .map_err(|e| format!("get_foundry_config: {e}"))?;
            if let Some(v) = enabled {
                cfg.enabled = v;
            }
            if let Some(v) = max_jobs_per_minute {
                cfg.max_jobs_per_minute = v;
            }
            if let Some(v) = distill_concurrency {
                cfg.distill_concurrency = v;
            }
            if let Some(v) = enrichment_concurrency {
                cfg.enrichment_concurrency = v;
            }
            if let Some(v) = llm_provider_override {
                cfg.llm_provider_override = if v.is_empty() { None } else { Some(v) };
            }
            set_foundry_config(store.connection(), &cfg, "tachi-cli")
                .map_err(|e| format!("set_foundry_config: {e}"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "db": target.display().to_string(),
                    "config": cfg,
                    "updated": true,
                }))?
            );
            Ok(())
        }
        FoundryAction::ConfigList { json: json_out } => {
            let manifest_path = app_home.join("manifest.json");
            let manifest = Manifest::load(&manifest_path).unwrap_or_else(|_| Manifest::empty());
            let mut entries: Vec<serde_json::Value> = Vec::new();
            for entry in &manifest.dbs {
                if is_checkpoint_fixture_path(&entry.path) {
                    continue;
                }
                let p = PathBuf::from(&entry.path);
                if !p.exists() {
                    entries.push(json!({
                        "db": entry.path,
                        "label": entry.scope_hint,
                        "config": null,
                        "error": "missing on disk",
                    }));
                    continue;
                }
                match read_per_db_config(&p) {
                    Ok(cfg) => entries.push(json!({
                        "db": entry.path,
                        "label": entry.scope_hint,
                        "config": cfg,
                    })),
                    Err(e) => entries.push(json!({
                        "db": entry.path,
                        "label": entry.scope_hint,
                        "config": null,
                        "error": e.to_string(),
                    })),
                }
            }
            if json_out {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else {
                for e in &entries {
                    let db = e.get("db").and_then(|v| v.as_str()).unwrap_or("?");
                    let label = e.get("label").and_then(|v| v.as_str()).unwrap_or("?");
                    if let Some(err) = e.get("error").and_then(|v| v.as_str()) {
                        eprintln!("[X] {label:<20} {db}  ({err})");
                    } else {
                        let cfg = e.get("config").cloned().unwrap_or(serde_json::Value::Null);
                        let enabled = cfg.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                        let max_jpm = cfg
                            .get("max_jobs_per_minute")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let dc = cfg
                            .get("distill_concurrency")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let ec = cfg
                            .get("enrichment_concurrency")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        println!(
                            "[OK] {label:<20} enabled={enabled} max_jpm={max_jpm:<3} distill={dc} enrich={ec}  {db}"
                        );
                    }
                }
            }
            Ok(())
        }
    }
}

pub(crate) fn is_checkpoint_fixture_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let file = lower
        .rsplit(std::path::MAIN_SEPARATOR)
        .next()
        .unwrap_or(&lower);
    file.contains(".checkpointed.") && file.ends_with(".db")
}

fn read_per_db_config(path: &Path) -> Result<PerDbConfig, Box<dyn std::error::Error>> {
    let path_str = path
        .to_str()
        .ok_or_else(|| format!("non-utf8 path: {}", path.display()))?;
    let store = MemoryStore::open_with_label(path_str, "tachi-foundry-config")
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    let cfg =
        get_foundry_config(store.connection()).map_err(|e| format!("get_foundry_config: {e}"))?;
    Ok(cfg)
}
