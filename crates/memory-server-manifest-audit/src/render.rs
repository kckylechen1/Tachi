use super::types::RelocationPlan;
use std::path::Path;

/// Render the plan as human-readable text.
pub fn render_plan(plan: &RelocationPlan, projects_dir: &Path) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if let Err(error) = writeln!(
        out,
        "project-DB relocation plan (DRY-RUN)  dir={}  entries={}",
        projects_dir.display(),
        plan.items.len()
    ) {
        eprintln!("failed to render relocation plan header: {error}");
    }
    if let Err(error) = writeln!(
        out,
        "  symlink-alias={}  symlink-broken={}  relocatable={}  home-resident={}  garbage={}",
        plan.n_symlink_alias,
        plan.n_symlink_broken,
        plan.n_relocatable,
        plan.n_home_resident,
        plan.n_garbage,
    ) {
        eprintln!("failed to render relocation plan statistics: {error}");
    }
    for item in &plan.items {
        if let Err(error) = writeln!(
            out,
            "  [{}] {}  action={}",
            item.class.as_str(),
            item.project_name,
            item.action.as_str(),
        ) {
            eprintln!("failed to render relocation plan item header: {error}");
        }
        if let Err(error) = writeln!(out, "      path: {}", item.db_path) {
            eprintln!("failed to render relocation plan path: {error}");
        }
        if let Some(t) = &item.symlink_target {
            if let Err(error) = writeln!(out, "      -> target: {t}") {
                eprintln!("failed to render symlink target: {error}");
            }
        }
        if let Some(dest) = &item.relocate_to {
            if let Err(error) = writeln!(out, "      => relocate to: {dest}") {
                eprintln!("failed to render relocate target: {error}");
            }
        }
        if let Err(error) = writeln!(out, "      note: {}", item.note) {
            eprintln!("failed to render item note: {error}");
        }
    }
    out
}
