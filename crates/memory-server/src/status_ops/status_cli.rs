use std::path::{Path, PathBuf};

#[cfg(unix)]
use serde::Serialize;
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
                || crate::status_ops::low_vector_coverage(db)
                || crate::status_ops::has_enrichment_failures(db)
                || crate::status_ops::has_vector_orphans(db)
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
        if db.namespace.recall_cache_rows > 0
            || db.namespace.wiki_rows > 0
            || db.namespace.graph_edges > 0
            || db.namespace.derived_items > 0
        {
            println!(
                "       [i] namespace cache={} wiki={} wiki_non_source={} derived_items={} graph_edges={} graph_orphans={}",
                db.namespace.recall_cache_rows,
                db.namespace.wiki_rows,
                db.namespace.wiki_non_source_rows,
                db.namespace.derived_items,
                db.namespace.graph_edges,
                db.namespace.graph_orphan_edges,
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
    if !snapshot.plan_c_split_brain.is_empty() {
        println!("Plan C Warnings");
        for issue in &snapshot.plan_c_split_brain {
            println!("  [!] {}", issue.warning_message());
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
        DaemonAction::Reap { apply, json } => reap_stale_processes(app_home, apply, json),
    }
}

/// True when `pid` names a live process this user can see. `EPERM` means the
/// process exists but is owned by someone else (still "alive").
#[cfg(unix)]
fn process_alive(pid: i64) -> bool {
    if pid <= 0 {
        return false;
    }
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ReapProcessFinding {
    pid: i64,
    ppid: i64,
    kind: &'static str,
    reap: bool,
    reason: String,
    global_db: Option<String>,
    project_db: Option<String>,
    no_project_db: bool,
    profile: Option<String>,
    command: String,
}

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct StaleDaemonFileFinding {
    file: String,
    path: String,
    pid: Option<i64>,
    reap: bool,
    reason: String,
    lock_file: String,
}

/// Extract the token following `flag` from a ps command line.
fn flag_value(command: &str, flag: &str) -> Option<String> {
    let mut it = command.split_whitespace();
    while let Some(tok) = it.next() {
        if tok == flag {
            return it.next().map(|s| s.to_string());
        }
    }
    None
}

#[cfg(unix)]
fn flag_present(command: &str, flag: &str) -> bool {
    command.split_whitespace().any(|tok| tok == flag)
}

#[cfg(unix)]
fn parse_ps_line(line: &str) -> Option<(i64, i64, String)> {
    let mut rest = line.trim_start();
    let pid_end = rest.find(char::is_whitespace)?;
    let pid = rest[..pid_end].parse::<i64>().ok()?;
    rest = rest[pid_end..].trim_start();

    let ppid_end = rest.find(char::is_whitespace)?;
    let ppid = rest[..ppid_end].parse::<i64>().ok()?;
    rest = rest[ppid_end..].trim_start();
    if rest.is_empty() {
        return None;
    }

    Some((pid, ppid, rest.to_string()))
}

#[cfg(unix)]
fn classify_reap_process_line(line: &str, self_pid: i64) -> Option<ReapProcessFinding> {
    let (pid, ppid, command) = parse_ps_line(line)?;
    if pid == self_pid {
        return None;
    }

    let argv0 = command.split_whitespace().next()?;
    let base = argv0.rsplit('/').next().unwrap_or(argv0);
    if base != "tachi" && base != "memory-server" {
        return None;
    }

    let global_db = flag_value(&command, "--global-db");
    let project_db = flag_value(&command, "--project-db");
    let no_project_db = flag_present(&command, "--no-project-db");
    let profile = flag_value(&command, "--profile");
    let is_daemon = flag_present(&command, "--daemon");

    let (kind, reap, reason) = if ppid == 1 && !is_daemon {
        (
            "orphan-stdio",
            true,
            "stdio adapter is reparented to pid 1; launching host likely exited".to_string(),
        )
    } else if is_daemon {
        match global_db.as_deref() {
            Some(db) if !Path::new(db).exists() => (
                "dead-db-daemon",
                true,
                format!("daemon global DB does not exist: {db}"),
            ),
            Some(db) => ("daemon", false, format!("daemon global DB exists: {db}")),
            None => (
                "daemon",
                false,
                "daemon has no --global-db flag; cannot prove DB is dead".to_string(),
            ),
        }
    } else {
        (
            "stdio",
            false,
            format!("stdio adapter still has live parent pid {ppid}"),
        )
    };

    Some(ReapProcessFinding {
        pid,
        ppid,
        kind,
        reap,
        reason,
        global_db,
        project_db,
        no_project_db,
        profile,
        command,
    })
}

#[cfg(unix)]
fn read_daemon_discovery_pid(path: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(path).ok()?;
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
        if let Some(pid) = value.get("pid").and_then(|p| p.as_i64()) {
            return Some(pid);
        }
        if let Some(pid) = value.as_i64() {
            return Some(pid);
        }
    }
    text.trim().parse::<i64>().ok()
}

/// Machine-wide sweep for stale tachi processes. Only ever reaps the
/// unambiguously dead: stdio servers whose launching host died (reparented to
/// pid 1) and daemons whose backing global DB no longer exists. Healthy live
/// daemons/servers and this very process are left alone. Dry-run by default.
#[cfg(unix)]
fn reap_stale_processes(
    app_home: &Path,
    apply: bool,
    json_out: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let self_pid = std::process::id() as i64;
    let out = std::process::Command::new("ps")
        .args(["-ax", "-o", "pid=,ppid=,command="])
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);

    let mut findings: Vec<ReapProcessFinding> = Vec::new();
    let mut reaped = 0usize;
    let mut kept = 0usize;

    for line in text.lines() {
        let Some(finding) = classify_reap_process_line(line, self_pid) else {
            continue;
        };

        if finding.reap {
            reaped += 1;
            if apply {
                unsafe { libc::kill(finding.pid as libc::pid_t, libc::SIGTERM) };
            }
        } else {
            kept += 1;
        }
        findings.push(finding);
    }

    // Stale daemon discovery files (pid recorded but no longer alive).
    let mut stale_files: Vec<String> = Vec::new();
    let mut stale_daemon_files: Vec<StaleDaemonFileFinding> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(app_home) {
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            if !(name.starts_with("daemon") && name.ends_with(".pid")) {
                continue;
            }
            let path = ent.path();
            let pid = read_daemon_discovery_pid(&path);
            let alive = pid.map(process_alive).unwrap_or(false);
            if !alive {
                let lock_file = path.with_extension("lock");
                let reason = pid
                    .map(|pid| format!("recorded daemon pid {pid} is not alive"))
                    .unwrap_or_else(|| "daemon pid file does not contain a valid pid".to_string());
                stale_files.push(name);
                stale_daemon_files.push(StaleDaemonFileFinding {
                    file: ent.file_name().to_string_lossy().to_string(),
                    path: path.display().to_string(),
                    pid,
                    reap: true,
                    reason,
                    lock_file: lock_file.display().to_string(),
                });
                if apply {
                    let _ = std::fs::remove_file(&path);
                    let _ = std::fs::remove_file(lock_file);
                }
            }
        }
    }

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "applied": apply,
                "reaped": reaped,
                "kept": kept,
                "stale_files": stale_files,
                "stale_daemon_files": stale_daemon_files,
                "processes": findings,
            }))?
        );
        return Ok(());
    }

    let verb = if apply { "reaped" } else { "would reap" };
    println!(
        "tachi process sweep: {verb} {reaped}, kept {kept}{}",
        if apply {
            ""
        } else {
            " (dry-run — pass --apply to act)"
        }
    );
    for f in &findings {
        let mark = if f.reap { "KILL" } else { "keep" };
        let mut scope = Vec::new();
        if let Some(global_db) = &f.global_db {
            scope.push(format!("global_db={global_db}"));
        }
        if let Some(project_db) = &f.project_db {
            scope.push(format!("project_db={project_db}"));
        } else if f.no_project_db {
            scope.push("project_db=<disabled>".to_string());
        }
        if let Some(profile) = &f.profile {
            scope.push(format!("profile={profile}"));
        }
        let scope = if scope.is_empty() {
            String::new()
        } else {
            format!(" ({})", scope.join(" "))
        };
        println!(
            "  [{mark}] pid={} ppid={} {}{}",
            f.pid, f.ppid, f.kind, scope
        );
        println!("       reason: {}", f.reason);
        println!("      command: {}", f.command);
    }
    if !stale_daemon_files.is_empty() {
        let fverb = if apply { "removed" } else { "stale" };
        println!("  {fverb} lock/pid files:");
        for f in &stale_daemon_files {
            println!(
                "    - file={} pid={} reason={} lock={}",
                f.file,
                f.pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                f.reason,
                f.lock_file
            );
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn reap_stale_processes(
    _app_home: &Path,
    _apply: bool,
    _json_out: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    Err("daemon reap is only implemented on unix".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn classify_reap_process_line_keeps_scoped_daemon_with_scope() {
        let dir = tempfile::tempdir().expect("temp dir");
        let global = dir.path().join("global.db");
        let project = dir.path().join("project.db");
        std::fs::write(&global, "").expect("global db");

        let line = format!(
            "  123  45 /opt/bin/memory-server --daemon --global-db {} --project-db {} --profile openclaw",
            global.display(),
            project.display()
        );
        let finding = classify_reap_process_line(&line, 999).expect("tachi process");

        assert_eq!(finding.kind, "daemon");
        assert!(!finding.reap);
        assert_eq!(finding.global_db.as_deref(), Some(global.to_str().unwrap()));
        assert_eq!(
            finding.project_db.as_deref(),
            Some(project.to_str().unwrap())
        );
        assert_eq!(finding.profile.as_deref(), Some("openclaw"));
        assert!(finding.reason.contains("exists"));
    }

    #[test]
    fn classify_reap_process_line_reaps_orphan_stdio() {
        let line = "321 1 /usr/local/bin/tachi --global-db /tmp/tachi.db --no-project-db";
        let finding = classify_reap_process_line(line, 999).expect("tachi process");

        assert_eq!(finding.kind, "orphan-stdio");
        assert!(finding.reap);
        assert!(finding.no_project_db);
        assert!(finding.reason.contains("pid 1"));
    }

    #[test]
    fn classify_reap_process_line_reaps_daemon_with_missing_global_db() {
        let line =
            "456 12 /usr/local/bin/memory-server --daemon --global-db /tmp/tachi-missing-global.db";
        let finding = classify_reap_process_line(line, 999).expect("tachi process");

        assert_eq!(finding.kind, "dead-db-daemon");
        assert!(finding.reap);
        assert!(finding.reason.contains("does not exist"));
    }

    #[test]
    fn classify_reap_process_line_ignores_current_and_non_tachi_processes() {
        assert!(classify_reap_process_line("777 1 /usr/local/bin/memory-server", 777).is_none());
        assert!(classify_reap_process_line("778 1 /usr/local/bin/node server.js", 777).is_none());
    }

    #[test]
    fn read_daemon_discovery_pid_accepts_json_and_plain_pid() {
        let dir = tempfile::tempdir().expect("temp dir");
        let json_path = dir.path().join("daemon-a.pid");
        let plain_path = dir.path().join("daemon-b.pid");
        std::fs::write(&json_path, serde_json::json!({ "pid": 42 }).to_string()).expect("json pid");
        std::fs::write(&plain_path, "43\n").expect("plain pid");

        assert_eq!(read_daemon_discovery_pid(&json_path), Some(42));
        assert_eq!(read_daemon_discovery_pid(&plain_path), Some(43));
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
