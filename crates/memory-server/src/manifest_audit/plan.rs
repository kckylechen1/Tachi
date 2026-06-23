use super::classify::classify_project_db;
use super::types::{ProjectDbInput, RelocationPlan};

/// Build a [`RelocationPlan`] from a set of pre-classified inputs. Pure.
pub fn build_plan(inputs: &[ProjectDbInput]) -> RelocationPlan {
    let mut plan = RelocationPlan::default();
    for inp in inputs {
        let item = classify_project_db(inp);
        plan.tally(&item);
        plan.items.push(item);
    }
    plan
}
