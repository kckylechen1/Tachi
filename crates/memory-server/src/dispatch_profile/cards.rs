use super::*;

mod evidence;
mod loadout;
mod render;

use evidence::profile_projected_weak_against;
use loadout::load_profile_overlay;

pub(in crate::dispatch_profile) use evidence::profile_demotion_targets;
#[cfg(test)]
pub(crate) use evidence::profile_evidence_required;
pub(crate) use evidence::{
    profile_evidence_contract_json_for_server, profile_evidence_required_for_server,
    profile_weak_against_for_server,
};
pub(in crate::dispatch_profile) use loadout::{
    profile_demotion_targets_from_overlay, profile_projected_evidence_required_from_overlay,
    profile_projected_passive_traits_from_overlay, profile_projected_signature_skills_from_overlay,
    profile_projected_weak_against_from_overlay,
};
pub(crate) use loadout::{
    profile_required_skill_ids, profile_required_skill_ids_for_server,
    profile_skill_loadout_json_for_server,
};
pub(crate) use render::{profile_json, profile_json_for_server};

pub(crate) fn profile_eval_feedback_json(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
    limit: usize,
) -> Result<Value, String> {
    let limit = limit.max(1);
    let rows = load_live_eval_rows(server, limit)?;
    let performance_matrix = aggregate_performance_matrix(&rows);
    Ok(tachi_dispatch::policy::profile_eval_feedback_json(
        profile,
        rows.len(),
        &performance_matrix,
        limit,
    ))
}
