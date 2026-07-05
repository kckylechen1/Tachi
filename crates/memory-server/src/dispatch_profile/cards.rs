use super::*;

mod evidence;
mod feedback;
mod loadout;
mod matrix;
mod render;

use evidence::profile_projected_weak_against;
use loadout::load_profile_overlay;

pub(in crate::dispatch_profile) use evidence::profile_demotion_targets;
#[cfg(test)]
pub(crate) use evidence::profile_evidence_required;
pub(crate) use evidence::{
    profile_evidence_contract_json, profile_evidence_contract_json_for_server,
    profile_evidence_required_for_server, profile_weak_against_for_server,
};
pub(crate) use feedback::profile_eval_feedback_json;
pub(in crate::dispatch_profile) use loadout::{
    profile_demotion_targets_from_overlay, profile_projected_evidence_required_from_overlay,
    profile_projected_passive_traits_from_overlay, profile_projected_signature_skills_from_overlay,
    profile_projected_weak_against_from_overlay,
};
pub(crate) use loadout::{
    profile_required_skill_ids, profile_required_skill_ids_for_server, profile_skill_loadout_json,
    profile_skill_loadout_json_for_server,
};
pub(in crate::dispatch_profile) use matrix::{
    sum_matrix_failures, sum_matrix_samples, summarize_matrix_rows, weighted_matrix_rate,
};
pub(crate) use render::{profile_json, profile_json_for_server};
pub(in crate::dispatch_profile) use tachi_dispatch::profile_role_matches;
