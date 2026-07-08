use super::types::RescuePlan;

pub fn render_plan(plan: &RescuePlan) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "rescue plan for {}\n  source rows (non-archived): {}\n  per-target routing:\n",
        plan.source_path, plan.source_total
    ));
    for (target, n) in &plan.per_target {
        out.push_str(&format!("    {:>16} <- {}\n", target, n));
    }
    out
}
