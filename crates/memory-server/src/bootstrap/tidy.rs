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
    app_home: &std::path::Path,
    roots: Vec<PathBuf>,
    git_root: Option<&PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let report = build_tidy_report(&roots, git_root)?;
    let apply_summary = if apply {
        Some(execute_tidy_apply(app_home, &report)?)
    } else {
        None
    };

    if json_output {
        if let Some(summary) = apply_summary.as_ref() {
            print_pretty_json(&json!({
                "report": report,
                "apply_summary": summary,
            }))
        } else {
            print_pretty_json(&serde_json::to_value(&report)?)
        }
    } else {
        println!("{}", render_tidy_report(&report));
        if let Some(summary) = apply_summary.as_ref() {
            println!();
            println!("{}", render_tidy_apply_summary(summary));
        }
        Ok(())
    }
}
