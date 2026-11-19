use super::types::RelocationPlan;
use std::path::Path;

/// Render the plan as human-readable text.
pub fn render_plan(plan: &RelocationPlan, projects_dir: &Path) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "project-DB relocation plan (DRY-RUN)  dir={}  entries={}",
        projects_dir.display(),
        plan.items.len()
    );
    let _ = writeln!(
        out,
        "  symlink-alias={}  symlink-broken={}  relocatable={}  home-resident={}  garbage={}",
        plan.n_symlink_alias,
        plan.n_symlink_broken,
        plan.n_relocatable,
        plan.n_home_resident,
        plan.n_garbage,
    );
    for item in &plan.items {
        let _ = writeln!(
            out,
            "  [{}] {}  action={}",
            item.class.as_str(),
            item.project_name,
            item.action.as_str(),
        );
        let _ = writeln!(out, "      path: {}", item.db_path);
        if let Some(t) = &item.symlink_target {
            let _ = writeln!(out, "      -> target: {t}");
        }
        if let Some(dest) = &item.relocate_to {
            let _ = writeln!(out, "      => relocate to: {dest}");
        }
        let _ = writeln!(out, "      note: {}", item.note);
    }
    out
}
