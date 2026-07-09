use super::*;

#[derive(Debug, Default)]
pub(super) struct DispatchCompleteDefaults {
    pub(super) agent: Option<String>,
    pub(super) profile: Option<String>,
    pub(super) task: Option<String>,
}

pub(super) fn read_dispatch_defaults_for_complete(
    dispatch_id: &str,
) -> Option<DispatchCompleteDefaults> {
    let mut defaults = DispatchCompleteDefaults::default();
    merge_dispatch_defaults_from_path(
        &mut defaults,
        &tachi_home_for_tools()
            .join("runs")
            .join(dispatch_id)
            .join("status.json"),
    );
    if defaults.agent.is_some() && defaults.task.is_some() && defaults.profile.is_some() {
        return Some(defaults);
    }
    Some(defaults).filter(|defaults| {
        defaults.agent.is_some() || defaults.task.is_some() || defaults.profile.is_some()
    })
}

pub(super) fn read_dispatch_defaults_for_complete_with_flow(
    flow_id: Option<&str>,
    dispatch_id: &str,
) -> Option<DispatchCompleteDefaults> {
    let mut defaults = read_dispatch_defaults_for_complete(dispatch_id).unwrap_or_default();
    if let Some(flow_id) = flow_id {
        if let Ok(run_dir) = crate::shell_ops::run_dir_for_flow_id(flow_id) {
            merge_dispatch_defaults_from_path(
                &mut defaults,
                &run_dir
                    .join("artifacts")
                    .join(format!("dispatch-{dispatch_id}.json")),
            );
        }
    }
    Some(defaults).filter(|defaults| {
        defaults.agent.is_some() || defaults.task.is_some() || defaults.profile.is_some()
    })
}

pub(super) fn merge_dispatch_defaults_from_path(
    defaults: &mut DispatchCompleteDefaults,
    path: &Path,
) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return;
    };
    if defaults.agent.is_none() {
        defaults.agent = value
            .get("agent")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
    if defaults.profile.is_none() {
        defaults.profile = value
            .get("profile")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
    if defaults.task.is_none() {
        defaults.task = value
            .get("task")
            .and_then(Value::as_str)
            .or_else(|| value.get("summary").and_then(Value::as_str))
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
}

pub(super) fn tachi_home_for_tools() -> PathBuf {
    if let Ok(home) = std::env::var("TACHI_HOME") {
        PathBuf::from(home)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".tachi")
    } else {
        std::env::temp_dir().join("tachi")
    }
}
