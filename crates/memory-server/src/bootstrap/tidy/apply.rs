use std::path::PathBuf;

use serde_json::json;

use super::super::{TidyAppliedStep, TidyApplySummary, TidyReport};

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
            "remove_broken_symlink" => {
                let mut removed = 0usize;
                let mut failures = Vec::new();
                for source in &step.source_paths {
                    let path = PathBuf::from(source);
                    match std::fs::symlink_metadata(&path) {
                        Ok(meta) if meta.file_type().is_symlink() && !path.exists() => {
                            match std::fs::remove_file(&path) {
                                Ok(()) => {
                                    removed += 1;
                                    if let Some(parent) = path.parent() {
                                        let _ = std::fs::remove_dir(parent);
                                    }
                                }
                                Err(err) => failures.push(format!("{source}: {err}")),
                            }
                        }
                        Ok(_) => failures.push(format!("{source}: no longer a broken symlink")),
                        Err(err) => failures.push(format!("{source}: {err}")),
                    }
                }

                if failures.is_empty() {
                    (
                        "cleaned".to_string(),
                        format!("Removed {removed} broken memory.db symlink(s)."),
                    )
                } else {
                    (
                        "skipped".to_string(),
                        format!(
                            "Removed {removed} broken memory.db symlink(s); failures: {}",
                            failures.join("; ")
                        ),
                    )
                }
            }
            _ => (
                "skipped".to_string(),
                "This action remains manual-review only in the conservative apply path."
                    .to_string(),
            ),
        };

        if outcome == "confirmed" || outcome == "cleaned" {
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
