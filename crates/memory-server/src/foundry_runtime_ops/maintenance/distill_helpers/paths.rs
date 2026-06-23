use crate::foundry_runtime_ops::helpers::build_foundry_agent_root;
use crate::utils::sanitize_safe_path_name;

pub(in crate::foundry_runtime_ops) fn build_foundry_distill_root(agent_id: &str) -> String {
    format!("{}/distilled", build_foundry_agent_root(agent_id))
}

pub(in crate::foundry_runtime_ops::maintenance) fn build_guide_distill_path(
    agent_id: &str,
    guide_type: &str,
    timestamp_segment: &str,
) -> String {
    format!(
        "/guide/{}/{}/{}",
        guide_type,
        sanitize_safe_path_name(agent_id),
        timestamp_segment
    )
}
