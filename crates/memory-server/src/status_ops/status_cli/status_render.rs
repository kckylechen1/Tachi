use std::path::Path;

use crate::status_ops::status_health;

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
        let report = status_health::run_provider_probe_report(global_db_path).await;
        if let Err(err) = status_health::write_provider_probe_cache_report(app_home, report.clone())
        {
            eprintln!("[!] provider probe cache write failed: {err}");
        }
        report
    } else {
        status_health::ProviderProbeReport {
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
        let outcomes = &db.continuity.session_outcomes;
        if outcomes.outcome_events > 0 {
            let rate = outcomes
                .challenge_rate
                .map(|value| format!("{:.1}%", value * 100.0))
                .unwrap_or_else(|| "n/a".to_string());
            println!(
                "       [i] continuity outcome_events={} eligible={} ai_corrected={} challenge_rate={} (read-only signal)",
                outcomes.outcome_events,
                outcomes.eligible_outcomes,
                outcomes.ai_corrected,
                rate
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

fn render_rotation_group_probes(groups: &[status_health::ProviderRotationGroupProbe]) {
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
