use super::*;

pub(super) fn classify_tidy_scope(path: &std::path::Path, git_root: Option<&PathBuf>) -> String {
    if let Some(root) = git_root {
        if path.starts_with(root) {
            return "project".to_string();
        }
    }

    let normalized = path.to_string_lossy().replace('\\', "/");
    let extract_agent = |marker: &str| -> Option<String> {
        let (_, rest) = normalized.split_once(marker)?;
        Some(rest.split('/').next().unwrap_or("unknown").to_string())
    };
    let extract_backup_agent = || -> Option<String> {
        let (_, rest) = normalized.split_once("/.openclaw/backups/")?;
        let (_, rest) = rest.split_once("/data/agents/")?;
        Some(rest.split('/').next().unwrap_or("unknown").to_string())
    };

    if normalized.contains("/.tachi/global/")
        || normalized.ends_with("/.tachi/global/memory.db")
        || normalized.contains("/.sigil/global/")
        || normalized.ends_with("/.sigil/global/memory.db")
    {
        "global".to_string()
    } else if let Some(agent) = extract_agent("/.openclaw/extensions/tachi/data/agents/") {
        format!("openclaw-plugin-agent:{agent}")
    } else if let Some(agent) = extract_agent("/.openclaw/core/extensions/tachi/data/agents/") {
        format!("openclaw-core-agent:{agent}")
    } else if let Some(agent) =
        extract_agent("/.openclaw/core/extensions/memory-hybrid-bridge/data/agents/")
    {
        format!("openclaw-legacy-agent:{agent}")
    } else if let Some(agent) = extract_backup_agent() {
        format!("openclaw-backup-agent:{agent}")
    } else if normalized.contains("/.openclaw/backups/") {
        "openclaw-backup".to_string()
    } else if let Some(agent) = extract_agent("/.openclaw/agents/") {
        format!("openclaw-agent-local:{agent}")
    } else if normalized.contains("/.openclaw/") {
        "openclaw-review".to_string()
    } else if let Some((_, rest)) = normalized.split_once("/.tachi/projects/") {
        let project_name = rest.split('/').next().unwrap_or("unknown");
        format!("project:{project_name}")
    } else if normalized.contains("/.gemini/") {
        "global".to_string()
    } else if normalized.contains("/.sigil/") || normalized.contains("/.tachi/") {
        "review".to_string()
    } else {
        "archive".to_string()
    }
}

pub(super) fn tidy_group_key(scope_suggestion: &str) -> String {
    scope_suggestion
        .split(':')
        .next()
        .unwrap_or(scope_suggestion)
        .to_string()
}

pub(super) fn tidy_group_priority(group: &str) -> usize {
    match group {
        "openclaw-plugin-agent" => 0,
        "project" => 1,
        "global" => 2,
        "openclaw-agent-local" => 3,
        "openclaw-core-agent" => 4,
        "openclaw-legacy-agent" => 5,
        "openclaw-backup-agent" => 6,
        "openclaw-backup" => 7,
        "openclaw-review" => 8,
        "review" => 9,
        "archive" => 10,
        _ => 99,
    }
}

pub(super) fn tidy_recommended_action(scope_suggestion: &str, status: &str) -> String {
    if status != "ok" {
        return "repair_before_any_move".to_string();
    }

    match tidy_group_key(scope_suggestion).as_str() {
        "openclaw-plugin-agent" | "openclaw-agent-local" => "keep_separate_agent_db".to_string(),
        "project" => "keep_project_db".to_string(),
        "global" => "keep_global_db".to_string(),
        "openclaw-core-agent" | "openclaw-legacy-agent" => {
            "review_for_legacy_migration".to_string()
        }
        "openclaw-backup-agent" | "openclaw-backup" => "archive_or_delete_after_review".to_string(),
        "openclaw-review" | "review" | "archive" => "manual_review".to_string(),
        _ => "manual_review".to_string(),
    }
}

pub(super) fn tidy_target_label(scope_suggestion: &str, action: &str) -> String {
    match action {
        "keep_separate_agent_db" | "keep_project_db" | "keep_global_db" => {
            scope_suggestion.to_string()
        }
        "review_for_legacy_migration" => format!("review->{scope_suggestion}"),
        "archive_or_delete_after_review" => "archive".to_string(),
        "repair_before_any_move" => "repair".to_string(),
        _ => "manual-review".to_string(),
    }
}

pub(super) fn tidy_rationale(scope_suggestion: &str, action: &str) -> String {
    match action {
        "keep_separate_agent_db" => format!(
            "{scope_suggestion} looks like an active agent-local OpenClaw database and should stay separate."
        ),
        "keep_project_db" => "This database already matches the current project-scoped layout.".to_string(),
        "keep_global_db" => "This database already matches the global/shared layout.".to_string(),
        "review_for_legacy_migration" => format!(
            "{scope_suggestion} appears to be legacy OpenClaw state and needs manual migration review."
        ),
        "archive_or_delete_after_review" => format!(
            "{scope_suggestion} appears to be backup state that should not be merged blindly."
        ),
        "repair_before_any_move" => "The DB could not be opened/read cleanly; repair it before planning migration.".to_string(),
        _ => format!("{scope_suggestion} needs manual review before deciding a destination."),
    }
}

pub(crate) fn build_tidy_report(
    roots: &[PathBuf],
    git_root: Option<&PathBuf>,
) -> Result<TidyReport, Box<dyn std::error::Error>> {
    let mut discovered = Vec::new();
    for root in roots {
        collect_memory_db_files(root, &mut discovered, 10);
    }

    discovered.sort();
    discovered.dedup();

    let mut databases = Vec::new();
    let mut total_memories = 0usize;

    for path in discovered {
        let scope_suggestion = classify_tidy_scope(&path, git_root);
        let mut status = "ok".to_string();
        let mut entry_count = None;
        let mut vec_available = false;

        match open_cli_store(&path) {
            Ok(store) => {
                vec_available = store.vec_available;
                match store.stats(false) {
                    Ok(stats) => {
                        entry_count = Some(stats.total as usize);
                        total_memories += stats.total as usize;
                    }
                    Err(_) => {
                        status = "stats_error".to_string();
                    }
                }
            }
            Err(_) => {
                status = "open_error".to_string();
            }
        }

        databases.push(TidyFinding {
            path: path.display().to_string(),
            entry_count,
            vec_available,
            recommended_action: tidy_recommended_action(&scope_suggestion, &status),
            scope_suggestion,
            status,
        });
    }

    databases.sort_by(|a, b| {
        tidy_group_priority(&tidy_group_key(&a.scope_suggestion))
            .cmp(&tidy_group_priority(&tidy_group_key(&b.scope_suggestion)))
            .then_with(|| a.path.cmp(&b.path))
    });

    let mut groups_map = std::collections::BTreeMap::<String, (usize, usize)>::new();
    for db in &databases {
        let key = tidy_group_key(&db.scope_suggestion);
        let entry = groups_map.entry(key).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += db.entry_count.unwrap_or(0);
    }
    let groups = groups_map
        .into_iter()
        .map(|(group, (database_count, memory_count))| TidyGroupSummary {
            group,
            database_count,
            memory_count,
        })
        .collect::<Vec<_>>();
    let mut groups = groups;
    groups.sort_by(|a, b| {
        tidy_group_priority(&a.group)
            .cmp(&tidy_group_priority(&b.group))
            .then_with(|| a.group.cmp(&b.group))
    });

    let mut plan_map = std::collections::BTreeMap::<(usize, String, String), Vec<String>>::new();
    for db in &databases {
        let key = (
            tidy_group_priority(&tidy_group_key(&db.scope_suggestion)),
            db.scope_suggestion.clone(),
            db.recommended_action.clone(),
        );
        plan_map.entry(key).or_default().push(db.path.clone());
    }
    let dry_run_plan = plan_map
        .into_iter()
        .enumerate()
        .map(|(index, ((_, scope, action), source_paths))| TidyPlanStep {
            order: index + 1,
            target_label: tidy_target_label(&scope, &action),
            rationale: tidy_rationale(&scope, &action),
            scope,
            action,
            source_paths,
        })
        .collect::<Vec<_>>();

    let mut next_steps = Vec::new();
    if databases.len() > 1 {
        next_steps.push(
            "Review scope_suggestion for each DB before adding any migration step".to_string(),
        );
    }
    if databases.iter().any(|db| db.status != "ok") {
        next_steps.push(
            "Repair or inspect DBs with open_error/stats_error before consolidation".to_string(),
        );
    }
    if databases.is_empty() {
        next_steps.push(
            "No memory.db files found in scanned roots; add more roots or initialize a project DB"
                .to_string(),
        );
    }

    Ok(TidyReport {
        scanned_roots: roots
            .iter()
            .map(|root| root.display().to_string())
            .collect(),
        groups,
        dry_run_plan,
        total_databases: databases.len(),
        total_memories,
        databases,
        next_steps,
    })
}

pub(super) fn render_tidy_report(report: &TidyReport) -> String {
    let mut lines = vec![
        "tachi tidy".to_string(),
        format!("scanned roots: {}", report.scanned_roots.join(", ")),
        format!(
            "found {} databases, {} memories",
            report.total_databases, report.total_memories
        ),
        String::new(),
    ];

    for db in &report.databases {
        let emoji = if db.status == "ok" { "✅" } else { "⚠️" };
        let count = db
            .entry_count
            .map(|count| count.to_string())
            .unwrap_or_else(|| "?".to_string());
        lines.push(format!(
            "{emoji} {} — entries: {count}, suggest: {}, action: {}, vectors: {}, status: {}",
            db.path, db.scope_suggestion, db.recommended_action, db.vec_available, db.status
        ));
    }

    if !report.groups.is_empty() {
        lines.push(String::new());
        lines.push("Groups:".to_string());
        for group in &report.groups {
            lines.push(format!(
                "  - {}: {} DBs, {} memories",
                group.group, group.database_count, group.memory_count
            ));
        }
    }

    if !report.dry_run_plan.is_empty() {
        lines.push(String::new());
        lines.push("Dry-run plan:".to_string());
        for step in &report.dry_run_plan {
            lines.push(format!(
                "  {}. [{}] {} -> {}",
                step.order, step.action, step.scope, step.target_label
            ));
            lines.push(format!("     rationale: {}", step.rationale));
            for source in &step.source_paths {
                lines.push(format!("     source: {source}"));
            }
        }
    }

    if !report.next_steps.is_empty() {
        lines.push(String::new());
        lines.push("Next steps:".to_string());
        for step in &report.next_steps {
            lines.push(format!("  - {step}"));
        }
    }

    lines.join("\n")
}

pub(super) fn render_tidy_apply_summary(summary: &TidyApplySummary) -> String {
    let mut lines = vec![
        "Apply summary:".to_string(),
        format!("  report: {}", summary.report_path),
        format!(
            "  applied: {} | skipped: {}",
            summary.applied_count, summary.skipped_count
        ),
    ];

    for step in &summary.applied_steps {
        lines.push(format!(
            "  {}. [{}] {} -> {}",
            step.order, step.outcome, step.scope, step.action
        ));
        lines.push(format!("     {}", step.note));
    }

    lines.join("\n")
}

pub(crate) fn execute_tidy_apply(
    app_home: &std::path::Path,
    report: &TidyReport,
) -> Result<TidyApplySummary, Box<dyn std::error::Error>> {
    let tidy_dir = app_home.join("tidy");
    std::fs::create_dir_all(&tidy_dir)?;
    let report_path = tidy_dir.join("last-apply.json");

    let mut applied_steps = Vec::new();
    let mut applied_count = 0usize;
    let mut skipped_count = 0usize;

    for step in &report.dry_run_plan {
        let (outcome, note) = match step.action.as_str() {
            "keep_separate_agent_db" => (
                "confirmed".to_string(),
                "No file move required; keeping the agent-scoped DB in place.".to_string(),
            ),
            "keep_project_db" => (
                "confirmed".to_string(),
                "No file move required; project DB already matches the intended layout."
                    .to_string(),
            ),
            "keep_global_db" => (
                "confirmed".to_string(),
                "No file move required; global DB already matches the intended layout.".to_string(),
            ),
            "archive_or_delete_after_review" => (
                "skipped".to_string(),
                "Backup/legacy DBs still require explicit review before any delete/archive action."
                    .to_string(),
            ),
            "review_for_legacy_migration" => (
                "skipped".to_string(),
                "Legacy DBs require provenance-aware migration rules before apply.".to_string(),
            ),
            "repair_before_any_move" => (
                "skipped".to_string(),
                "DB must be repaired and re-scanned before apply.".to_string(),
            ),
            _ => (
                "skipped".to_string(),
                "This action remains manual-review only in the conservative apply path."
                    .to_string(),
            ),
        };

        if outcome == "confirmed" {
            applied_count += 1;
        } else {
            skipped_count += 1;
        }

        applied_steps.push(TidyAppliedStep {
            order: step.order,
            scope: step.scope.clone(),
            action: step.action.clone(),
            outcome,
            note,
        });
    }

    let summary = TidyApplySummary {
        report_path: report_path.display().to_string(),
        applied_steps,
        applied_count,
        skipped_count,
    };

    let payload = json!({
        "report": report,
        "apply_summary": &summary,
    });
    std::fs::write(&report_path, serde_json::to_string_pretty(&payload)?)?;

    Ok(summary)
}

pub(super) async fn run_tidy_command(
    json_output: bool,
    apply: bool,
    dry_run: bool,
    execute: bool,
    yes: bool,
    target_db_override: Option<PathBuf>,
    home: &std::path::Path,
    app_home: &std::path::Path,
    roots: Vec<PathBuf>,
    git_root: Option<&PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let report = build_tidy_report(&roots, git_root)?;

    // --execute: fragment-DB consolidation pipeline.
    if execute {
        // Acquire and hold the daemon lock for the entire migration to prevent
        // concurrent writes from a live daemon, which would corrupt the DB.
        let lock_path = app_home.join("daemon.lock");
        let _lock = match crate::daemon_lock::DaemonLock::acquire(&lock_path) {
            Err(crate::daemon_lock::DaemonLockError::AlreadyRunning { pid }) => {
                return Err(format!(
                    "refusing to run tachi tidy --execute while tachi daemon is running (pid {pid}); stop it first"
                )
                .into());
            }
            Err(e) => {
                return Err(format!("daemon lock probe failed: {e}").into());
            }
            Ok(lock) => lock,
        };

        let target_db = target_db_override
            
            .unwrap_or_else(|| app_home.join("global").join("memory.db"));
        let archive_root = app_home
            .join("archive")
            .join(chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string());
        let plan = build_migration_plan(&report, &target_db, &archive_root, home);

        let cfg = MigrationConfig {
            target_db,
            archive_root,
            manifest_path: crate::manifest::Manifest::default_path(home),
            yes,
            dry_run: false,
            interactive: !yes && atty_stdout(),
        };

        let summary = execute_tidy_migrations(&plan, &cfg)?;
        drop(_lock); // explicitly release after migrations complete

        if json_output {
            return print_pretty_json(&json!({
                "report": report,
                "execute_summary": summary,
            }));
        }
        println!("{}", render_tidy_report(&report));
        println!();
        println!("{}", render_tidy_execute_summary(&summary));
        return Ok(());
    }

    // Legacy --apply path: conservative confirm-only summary.
    let apply_summary = if apply {
        Some(execute_tidy_apply(app_home, &report)?)
    } else {
        None
    };

    // --dry-run (or default): also include the migration plan preview when
    // there are any migration candidates, but make no writes.
    let target_db_preview = target_db_override
        
        .unwrap_or_else(|| app_home.join("global").join("memory.db"));
    let archive_preview = app_home.join("archive").join("<timestamp>");
    let plan_preview = build_migration_plan(&report, &target_db_preview, &archive_preview, home);

    if json_output {
        if let Some(summary) = apply_summary.as_ref() {
            return print_pretty_json(&json!({
                "report": report,
                "apply_summary": summary,
                "migration_plan": plan_preview,
                "dry_run": dry_run || !apply,
            }));
        }
        return print_pretty_json(&json!({
            "report": report,
            "migration_plan": plan_preview,
            "dry_run": true,
        }));
    }

    println!("{}", render_tidy_report(&report));
    if !plan_preview.is_empty() {
        println!();
        println!("{}", render_migration_plan(&plan_preview));
    }
    if let Some(summary) = apply_summary.as_ref() {
        println!();
        println!("{}", render_tidy_apply_summary(summary));
    }
    if !execute {
        println!();
        println!(
            "(no writes performed — pass --execute to migrate, or --apply for the conservative path)"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Fragment-DB migration: planner + executor.
// ---------------------------------------------------------------------------

/// Configuration controlling how migrations are executed.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct MigrationConfig {
    pub target_db: PathBuf,
    pub archive_root: PathBuf,
    pub manifest_path: PathBuf,
    /// Skip per-DB y/n prompts.
    pub yes: bool,
    /// When true, do not perform any write. Used by integration tests and
    /// equivalent to `--dry-run --execute` (which is currently disallowed at
    /// the CLI but useful for tests).
    pub dry_run: bool,
    /// True when prompts are appropriate (TTY + !yes).
    pub interactive: bool,
}

/// Build the list of source DBs that are candidates for fragment-consolidation
/// migration into a single target DB. Pure function — no I/O.
///
/// We migrate DBs whose recommended action implies that the rows should be
/// merged into the canonical store. We deliberately do NOT migrate
/// `keep_*_db` (already in the right place) or `repair_before_any_move`
/// (unsafe). `archive_or_delete_after_review` is included only when the
/// caller explicitly opts in via `--yes`; the planner records it with action
/// label so the executor can decide.
pub(crate) fn build_migration_plan(
    report: &TidyReport,
    target_db: &std::path::Path,
    archive_root: &std::path::Path,
    home: &std::path::Path,
) -> Vec<TidyMigration> {
    let mut plan = Vec::new();
    let target_str = target_db.to_string_lossy().to_string();

    for db in &report.databases {
        if db.status != "ok" {
            continue;
        }
        // Never migrate the target onto itself.
        if db.path == target_str {
            continue;
        }
        let should_migrate = matches!(
            db.recommended_action.as_str(),
            "review_for_legacy_migration"
        );
        if !should_migrate {
            continue;
        }

        let source = PathBuf::from(&db.path);
        let archive_path = archive_root.join(archive_relative_path(&source, home));

        plan.push(TidyMigration {
            source_path: db.path.clone(),
            target_path: target_str.clone(),
            archive_path: archive_path.display().to_string(),
            scope_suggestion: db.scope_suggestion.clone(),
            action: db.recommended_action.clone(),
            source_row_count: db.entry_count.unwrap_or(0),
            reason: tidy_rationale(&db.scope_suggestion, &db.recommended_action),
        });
    }

    plan
}

/// Compute a stable, collision-free relative path used inside the archive
/// timestamp directory. We prefer the source path relative to `$HOME` so the
/// archive layout mirrors the user's tree; falls back to the file name when
/// the source is outside `$HOME`.
fn archive_relative_path(source: &std::path::Path, home: &std::path::Path) -> PathBuf {
    if let Ok(rel) = source.strip_prefix(home) {
        return rel.to_path_buf();
    }
    PathBuf::from(
        source
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "memory.db".to_string()),
    )
}

pub(crate) fn execute_tidy_migrations(
    plan: &[TidyMigration],
    cfg: &MigrationConfig,
) -> Result<TidyExecuteSummary, Box<dyn std::error::Error>> {
    let mut outcomes = Vec::new();
    let mut migrated = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    if plan.is_empty() {
        return Ok(TidyExecuteSummary {
            target_db: cfg.target_db.display().to_string(),
            planned: plan.to_vec(),
            outcomes,
            migrated_count: 0,
            skipped_count: 0,
            failed_count: 0,
            dry_run: cfg.dry_run,
        });
    }

    // Ensure target parent exists (needed for both real run and creating a
    // fresh empty target DB).
    if let Some(parent) = cfg.target_db.parent() {
        if !cfg.dry_run {
            std::fs::create_dir_all(parent)?;
        }
    }

    for migration in plan {
        // Interactive confirm.
        if cfg.interactive {
            let prompt = format!(
                "Migrate {} ({} rows) into {} and archive source? [y/N]",
                migration.source_path, migration.source_row_count, migration.target_path
            );
            let confirmed = dialoguer::Confirm::new()
                .with_prompt(prompt)
                .default(false)
                .interact()
                .unwrap_or(false);
            if !confirmed {
                outcomes.push(TidyMigrationOutcome {
                    source_path: migration.source_path.clone(),
                    target_path: migration.target_path.clone(),
                    archive_path: None,
                    status: "skipped".to_string(),
                    rows_before_target: 0,
                    rows_after_target: 0,
                    rows_copied: 0,
                    message: "skipped by interactive prompt".to_string(),
                });
                skipped += 1;
                continue;
            }
        }

        match migrate_single_db(migration, cfg) {
            Ok(outcome) => {
                if outcome.status == "migrated" {
                    migrated += 1;
                } else if outcome.status == "failed" {
                    failed += 1;
                } else {
                    skipped += 1;
                }
                outcomes.push(outcome);
            }
            Err(err) => {
                failed += 1;
                outcomes.push(TidyMigrationOutcome {
                    source_path: migration.source_path.clone(),
                    target_path: migration.target_path.clone(),
                    archive_path: None,
                    status: "failed".to_string(),
                    rows_before_target: 0,
                    rows_after_target: 0,
                    rows_copied: 0,
                    message: format!("migration error: {err}"),
                });
            }
        }
    }

    // Update manifest (drop migrated source entries, ensure target entry).
    if !cfg.dry_run {
        if let Err(err) = update_manifest_after_migration(cfg, &outcomes) {
            eprintln!(
                "[tidy] WARN: manifest update failed after migration: {err}; rows were migrated successfully"
            );
        }
    }

    Ok(TidyExecuteSummary {
        target_db: cfg.target_db.display().to_string(),
        planned: plan.to_vec(),
        outcomes,
        migrated_count: migrated,
        skipped_count: skipped,
        failed_count: failed,
        dry_run: cfg.dry_run,
    })
}

fn migrate_single_db(
    migration: &TidyMigration,
    cfg: &MigrationConfig,
) -> Result<TidyMigrationOutcome, Box<dyn std::error::Error>> {
    use std::collections::HashSet;

    let source_path = PathBuf::from(&migration.source_path);
    let target_path = cfg.target_db.clone();

    let source_store = open_cli_store_read_only(&source_path)?;
    let source_count: usize =
        source_store
            .connection()
            .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;

    if cfg.dry_run {
        return Ok(TidyMigrationOutcome {
            source_path: migration.source_path.clone(),
            target_path: migration.target_path.clone(),
            archive_path: Some(migration.archive_path.clone()),
            status: "dry_run".to_string(),
            rows_before_target: 0,
            rows_after_target: 0,
            rows_copied: source_count,
            message: format!("would migrate {source_count} rows"),
        });
    }

    let mut target_store = open_cli_store(&target_path)?;
    let rows_before = target_store
        .stats(true)
        .map(|s| s.total as usize)
        .unwrap_or(0);

    let existing_target_ids: HashSet<String> = {
        let conn = target_store.connection();
        let mut stmt = conn.prepare("SELECT id FROM memories")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.filter_map(Result::ok).collect()
    };

    let mut copied = 0usize;
    let mut newly_inserted_ids: Vec<String> = Vec::new();
    let mut copy_err: Option<Box<dyn std::error::Error>> = None;

    {
        let conn = source_store.connection();
        let mut stmt = conn.prepare(
            "SELECT id,path,summary,text,importance,timestamp,category,topic,keywords,'[]' AS persons,entities,location,source,scope,archived,access_count,last_access,revision,metadata,retention_policy,domain
             FROM memories",
        )?;
        let rows = stmt.query_map([], memory_core::row_to_entry)?;
        for row in rows {
            let entry = row?;
            let existed_before = existing_target_ids.contains(&entry.id);
            match target_store.upsert(&entry) {
                Ok(()) => {
                    if !existed_before {
                        newly_inserted_ids.push(entry.id.clone());
                    }
                    copied += 1;
                }
                Err(e) => {
                    copy_err = Some(Box::new(e));
                    break;
                }
            }
        }
    }

    if let Some(err) = copy_err {
        // Best-effort rollback: delete rows we newly inserted in this run.
        for id in &newly_inserted_ids {
            let _ = target_store.delete(id);
        }
        return Ok(TidyMigrationOutcome {
            source_path: migration.source_path.clone(),
            target_path: migration.target_path.clone(),
            archive_path: None,
            status: "failed".to_string(),
            rows_before_target: rows_before,
            rows_after_target: rows_before,
            rows_copied: 0,
            message: format!(
                "rolled back after {copied}/{source_count} rows ({} reverted): {err}",
                newly_inserted_ids.len()
            ),
        });
    }

    let rows_after = target_store
        .stats(true)
        .map(|s| s.total as usize)
        .unwrap_or(rows_before);
    drop(target_store);

    // Archive the source DB file. Move (rename) when possible; fall back to
    // copy + remove across filesystems.
    let archive_path = PathBuf::from(&migration.archive_path);
    if let Some(parent) = archive_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::rename(&source_path, &archive_path) {
        Ok(()) => {}
        Err(_) => {
            std::fs::copy(&source_path, &archive_path)?;
            std::fs::remove_file(&source_path)?;
        }
    }
    // Also move sidecar WAL/SHM files if present.
    for ext in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{ext}", source_path.display()));
        if sidecar.exists() {
            let dst = PathBuf::from(format!("{}{ext}", archive_path.display()));
            if let Err(e) = std::fs::rename(&sidecar, &dst).or_else(|_| {
                std::fs::copy(&sidecar, &dst)
                    .map(|_| ())
                    .and_then(|_| std::fs::remove_file(&sidecar))
            }) {
                tracing::warn!("tidy: failed to move sidecar {}: {e}", sidecar.display());
            }
        }
    }

    Ok(TidyMigrationOutcome {
        source_path: migration.source_path.clone(),
        target_path: migration.target_path.clone(),
        archive_path: Some(archive_path.display().to_string()),
        status: "migrated".to_string(),
        rows_before_target: rows_before,
        rows_after_target: rows_after,
        rows_copied: copied,
        message: format!("migrated {copied} rows ({rows_before} -> {rows_after} on target)"),
    })
}

/// Drop migrated source entries from the manifest and ensure the target entry
/// exists. We deliberately only touch entries we actually migrated; the rest
/// of the manifest is left as-is. Pure with respect to `outcomes` — file I/O
/// is wrapped in `Manifest::load_or_empty` / `save`.
pub(crate) fn update_manifest_after_migration(
    cfg: &MigrationConfig,
    outcomes: &[TidyMigrationOutcome],
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::manifest::{DbEntry, DbRole, Manifest};

    let mut manifest = Manifest::load_or_empty(&cfg.manifest_path);

    let migrated_canons: std::collections::HashSet<String> = outcomes
        .iter()
        .filter(|o| o.status == "migrated")
        .map(|o| {
            crate::manifest::canonicalize_db_path(std::path::Path::new(&o.source_path))
                .display()
                .to_string()
        })
        .collect();
    if migrated_canons.is_empty() {
        return Ok(());
    }

    let target_canonical = crate::manifest::canonicalize_db_path(&cfg.target_db)
        .display()
        .to_string();

    manifest.dbs.retain(|e| !migrated_canons.contains(&e.path));

    if !manifest
        .dbs
        .iter()
        .any(|e| e.path == target_canonical || e.path == cfg.target_db.display().to_string())
    {
        manifest.dbs.push(DbEntry {
            path: target_canonical,
            role: DbRole::Global,
            owner: "tachi".to_string(),
            schema_kind: "tachi-memory-v1".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: chrono::Utc::now().to_rfc3339(),
            last_classification: "healthy".to_string(),
            scope_hint: "global".to_string(),
            notes: "registered by `tachi tidy --execute`".to_string(),
        });
    }
    manifest.generated_at = chrono::Utc::now().to_rfc3339();
    manifest.save(&cfg.manifest_path)?;
    Ok(())
}

fn render_migration_plan(plan: &[TidyMigration]) -> String {
    let mut lines = vec!["Migration plan (fragment-DB consolidation):".to_string()];
    for (idx, m) in plan.iter().enumerate() {
        lines.push(format!(
            "  {}. {} ({} rows) -> {}",
            idx + 1,
            m.source_path,
            m.source_row_count,
            m.target_path
        ));
        lines.push(format!("     archive: {}", m.archive_path));
        lines.push(format!("     reason:  {}", m.reason));
    }
    lines.join("\n")
}

fn render_tidy_execute_summary(summary: &TidyExecuteSummary) -> String {
    let mut lines = vec![
        "Execute summary:".to_string(),
        format!("  target: {}", summary.target_db),
        format!(
            "  migrated: {} | skipped: {} | failed: {}{}",
            summary.migrated_count,
            summary.skipped_count,
            summary.failed_count,
            if summary.dry_run { " (dry-run)" } else { "" }
        ),
    ];
    for o in &summary.outcomes {
        lines.push(format!(
            "  - [{}] {} -> {}",
            o.status, o.source_path, o.target_path
        ));
        lines.push(format!(
            "     rows: copied={}, target {} -> {}",
            o.rows_copied, o.rows_before_target, o.rows_after_target
        ));
        if let Some(arch) = &o.archive_path {
            lines.push(format!("     archive: {arch}"));
        }
        lines.push(format!("     {}", o.message));
    }
    lines.join("\n")
}
