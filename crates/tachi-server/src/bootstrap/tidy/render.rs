use super::super::{TidyApplySummary, TidyExecuteSummary, TidyMigration, TidyReport};

pub(super) fn render_tidy_report(report: &TidyReport) -> String {
    let mut lines = vec![
        "tachi tidy".to_string(),
        format!("scanned roots: {}", report.scanned_roots.join(", ")),
        format!(
            "found {} physical databases, {} path aliases, {} memories",
            report.total_databases, report.total_aliases, report.total_memories
        ),
        String::new(),
    ];

    if !report.physical_stores.is_empty() {
        lines.push("Physical stores:".to_string());
        for store in &report.physical_stores {
            lines.push(format!(
                "  - {} [{}] aliases={} open_path={} basis={}",
                store.canonical_path,
                store.physical_id,
                store.aliases.len(),
                store.open_path,
                store.open_path_basis.as_str()
            ));
            for sidecar_path in &store.sidecar_paths {
                lines.push(format!("      sidecar-visible: {sidecar_path}"));
            }
        }
        lines.push(String::new());
    }

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
        if let Some(physical_id) = &db.physical_id {
            lines.push(format!(
                "   physical_id={physical_id} primary_alias={} canonical={}",
                db.is_primary_alias,
                db.canonical_path.as_deref().unwrap_or("<unknown>")
            ));
            if let Some(open_path) = &db.inventory_open_path {
                lines.push(format!("   inventory_open_path={open_path}"));
            }
        }
        if let Some(kind) = db.open_failure_kind {
            lines.push(format!("   inventory_failure={}", kind.as_str()));
        }
        if db.is_symlink {
            let target = db
                .symlink_target
                .as_deref()
                .unwrap_or("<unreadable symlink target>");
            let exists = db
                .target_exists
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            lines.push(format!("   symlink -> {target} (target_exists={exists})"));
        }
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

pub(super) fn render_migration_plan(plan: &[TidyMigration]) -> String {
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

pub(super) fn render_tidy_execute_summary(summary: &TidyExecuteSummary) -> String {
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
