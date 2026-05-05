use super::*;

pub(super) async fn run_rescue_command(
    action: RescueAction,
    home: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        RescueAction::Antigravity {
            source,
            targets_root,
            apply,
            json,
        } => {
            let source_path: PathBuf = source
                .unwrap_or_else(|| home.join(".gemini").join("antigravity").join("memory.db"));
            let targets_root_path: PathBuf =
                targets_root.unwrap_or_else(|| home.join(".tachi").join("projects"));

            if !source_path.exists() {
                return Err(
                    format!("rescue source DB not found: {}", source_path.display()).into(),
                );
            }
            if !targets_root_path.exists() {
                return Err(format!(
                    "rescue targets root not found: {}",
                    targets_root_path.display()
                )
                .into());
            }

            let plan = crate::rescue::plan_rescue(&source_path)
                .map_err(|e| format!("plan_rescue: {e}"))?;

            if !apply {
                if json {
                    print_pretty_json(&serde_json::to_value(&plan)?)
                } else {
                    println!("{}", crate::rescue::render_plan(&plan));
                    println!(
                        "\n(dry-run; re-run with --apply to write into target DBs at {})",
                        targets_root_path.display()
                    );
                    Ok(())
                }
            } else {
                let report = crate::rescue::apply_rescue(&source_path, &targets_root_path, plan)
                    .map_err(|e| format!("apply_rescue: {e}"))?;
                if json {
                    print_pretty_json(&serde_json::to_value(&report)?)
                } else {
                    println!("{}", crate::rescue::render_plan(&report.plan));
                    println!("\n=== apply summary ===");
                    let mut total_written = 0usize;
                    for (target, n) in &report.written_per_target {
                        println!("  written {:>16} <- {}", target, n);
                        total_written += n;
                    }
                    println!("  total written: {}", total_written);
                    println!("  skipped (already present): {}", report.skipped_existing);
                    if let Some(bk) = &report.source_backed_up_to {
                        println!("  source backed up to: {}", bk);
                    }
                    if !report.errors.is_empty() {
                        println!("\n=== errors ({}) ===", report.errors.len());
                        for e in &report.errors {
                            println!("  - {}", e);
                        }
                    }
                    Ok(())
                }
            }
        }
    }
}
