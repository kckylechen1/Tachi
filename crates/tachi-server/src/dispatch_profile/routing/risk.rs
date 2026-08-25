use super::*;

pub(crate) fn classify_dispatch_risk(
    task: &str,
    risk_override: Option<&str>,
    file_paths: &[String],
) -> DispatchRisk {
    let route = crate::copilot_ops::build_task_brief_routing(task);
    let task_type = route.intent.to_string();
    tachi_dispatch::classify_dispatch_risk(task, &task_type, risk_override, file_paths)
}
