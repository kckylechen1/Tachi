use std::path::Path;

use crate::status_ops::status_health;

pub(crate) async fn run_status(
    watch: bool,
    json_out: bool,
    hide_orphans: bool,
    probe_keys: bool,
    all_dbs: bool,
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !watch {
        return render_one(
            json_out,
            hide_orphans,
            probe_keys,
            all_dbs,
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
            all_dbs,
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
            all_dbs,
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
    all_dbs: bool,
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let provider_probe_report = if probe_keys {
        let report = status_health::run_provider_probe_report(global_db_path).await;
        if let Err(err) = status_health::write_provider_probe_cache_report(
            app_home,
            global_db_path,
            report.clone(),
        ) {
            eprintln!("[!] provider probe cache write failed: {err}");
        }
        report
    } else {
        status_health::ProviderProbeReport {
            probes: Vec::new(),
            rotation_groups: Vec::new(),
        }
    };
    // --probe-keys implies the full provider-value-compare snapshot path
    // regardless of --all-dbs (live key probing is opt-in and rare, so it
    // isn't worth a second scoping axis); otherwise honor --all-dbs to pick
    // between the default global+project-only probe and the full fleet.
    let snapshot = if probe_keys {
        crate::status_ops::collect_snapshot_with_provider_value_compare(
            app_home,
            global_db_path,
            project_db_path,
        )
    } else {
        crate::status_ops::collect_snapshot_scoped(
            app_home,
            global_db_path,
            project_db_path,
            all_dbs,
        )
    };

    if json_out {
        let mut v = serde_json::to_value(&snapshot)?;
        if let Some(obj) = v.as_object_mut() {
            obj.insert(
                "host_profile".to_string(),
                crate::host_profile::runtime_json(),
            );
            obj.insert(
                "provider_probes".to_string(),
                serde_json::to_value(&provider_probe_report.probes)?,
            );
            obj.insert(
                "provider_rotation_groups".to_string(),
                serde_json::to_value(&provider_probe_report.rotation_groups)?,
            );
            // Machine consumers need to know `dbs` was scoped (perf pack item
            // 5) rather than silently reading a shorter fleet as the whole
            // manifest.
            obj.insert(
                "dbs_scoped_to_global_and_project".to_string(),
                (!all_dbs).into(),
            );
        }
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }

    println!("tachi status @ {}", chrono::Utc::now().to_rfc3339());
    println!("  app_home: {}", app_home.display());
    let host_profile = crate::host_profile::runtime_json();
    if let Some(error) = host_profile
        .get("configuration_error")
        .and_then(serde_json::Value::as_str)
    {
        println!("  host_profile: [X] {error}");
    } else {
        println!(
            "  host_profile: {} (max {})",
            host_profile["profile"].as_str().unwrap_or("unknown"),
            host_profile["max_execution_level"]
                .as_str()
                .unwrap_or("unknown")
        );
    }
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
            println!(
                "  [!] no daemon running (single-process/stdio mode; background tasks paused)"
            );
        }
    }
    let other_running_daemons: Vec<_> = snapshot
        .daemon_inventory
        .iter()
        .filter(|daemon| daemon.process_running && !daemon.authoritative_for_current_global)
        .collect();
    if !other_running_daemons.is_empty() {
        println!("  [i] other daemon scopes running:");
        for daemon in other_running_daemons {
            println!(
                "      pid={} state={} global_db={} project_db={}",
                daemon
                    .pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                daemon.state,
                daemon.global_db.as_deref().unwrap_or("unknown"),
                daemon.project_db.as_deref().unwrap_or("none"),
            );
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
    if !all_dbs {
        println!("  [i] scoped to global + current-project db; pass --all-dbs for the full fleet");
    }
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
        if let Some(sweep) = &db.vector_sweep {
            let state = if sweep.enabled { "enabled" } else { "disabled" };
            let reason = sweep
                .disabled_reason
                .as_deref()
                .or(sweep.last_error.as_deref())
                .map(|value| format!(" reason={}", crate::status_ops::truncate(value, 96)))
                .unwrap_or_default();
            let next = sweep
                .next_run_after
                .as_deref()
                .map(|value| format!(" next={value}"))
                .or_else(|| {
                    sweep
                        .interval_secs
                        .map(|secs| format!(" interval_secs={secs}"))
                })
                .unwrap_or_default();
            println!(
                "       [i] vector_sweep={} last_run={} embedded={} failed={} current_pending={} threshold={} needed={} skip_cache={}{}{}",
                state,
                sweep.last_run_at,
                sweep.embedded_count,
                sweep.failed_count,
                sweep.current_pending_count,
                sweep.current_pending_threshold,
                sweep.current_backfill_needed,
                sweep.skip_recall_cache,
                next,
                reason
            );
        }
        if let Some(err) = &db.vector_sweep_error {
            println!(
                "       [!] vector_sweep_error={}",
                crate::status_ops::truncate(err, 160)
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

    println!("Disk (#484)");
    render_disk_volume(&snapshot.disk.worktrees_root);
    render_disk_volume(&snapshot.disk.shared_target_dir);
    render_top_consumers(&snapshot.disk.top_consumers);
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

    if !snapshot.health_deductions.is_empty() {
        println!("Health Deductions");
        for deduction in &snapshot.health_deductions {
            println!(
                "  -{} {:<28} {}",
                deduction.points, deduction.label, deduction.detail
            );
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

/// Top byte consumers from the exec_env resource ledger (#894 S2b) — purely
/// informational, next to the free-space numbers: "you have N% free, and THIS
/// is what is eating it". Silent when the ledger has nothing measured.
fn render_top_consumers(consumers: &[crate::status_ops::disk::ResourceConsumer]) {
    if consumers.is_empty() {
        return;
    }
    let summary = consumers
        .iter()
        .map(|consumer| {
            format!(
                "{} ({:.1} GB, {})",
                consumer.path,
                consumer.bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                consumer.kind
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    println!("  top consumers: {summary}");
}

fn render_disk_volume(volume: &crate::status_ops::disk::DiskVolumeStatus) {
    if let Some(err) = &volume.error {
        println!("  [!] {}: {} ({err})", volume.label, volume.path);
        return;
    }
    let marker = if volume.warning.is_some() {
        "[!]"
    } else {
        "[OK]"
    };
    let free_gb = volume
        .free_bytes
        .map(|b| b as f64 / (1024.0 * 1024.0 * 1024.0));
    println!(
        "  {marker} {}: {} — {} free ({})",
        volume.label,
        volume.path,
        free_gb
            .map(|gb| format!("{gb:.1} GB"))
            .unwrap_or_else(|| "? GB".to_string()),
        volume
            .free_percent
            .map(|p| format!("{p:.1}%"))
            .unwrap_or_else(|| "?%".to_string()),
    );
    if let Some(warning) = &volume.warning {
        println!("      {warning}");
    }
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
