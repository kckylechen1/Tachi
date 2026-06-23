use super::classify::classify;
use super::source::read_source_rows;
use super::types::RescuePlan;
use std::path::Path;

pub fn plan_rescue(source: &Path) -> Result<RescuePlan, String> {
    let rows = read_source_rows(source)?;
    let mut plan = RescuePlan {
        source_path: source.display().to_string(),
        source_total: rows.len(),
        ..Default::default()
    };
    for row in &rows {
        let a = classify(row);
        *plan.per_target.entry(a.target.clone()).or_insert(0) += 1;
        plan.assignments.push(a);
    }
    Ok(plan)
}
