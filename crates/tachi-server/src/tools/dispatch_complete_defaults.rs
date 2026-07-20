use super::*;

#[derive(Debug, Default)]
pub(super) struct DispatchCompleteDefaults {
    pub(super) agent: Option<String>,
    pub(super) profile: Option<String>,
    pub(super) task: Option<String>,
}

/// tachi#1173 k2 fix: `dispatch_id` here is caller-supplied (via
/// `TachiTaskParams::dispatch_id` on `tachi_task(action='complete')`) and
/// was joined directly onto `home.join("runs")` with no validation -- the
/// same path-traversal shape tachi#1173's board autopsy review closed in
/// `board::runs::collect_run_task_by_id` (eb473fd0). Gated the same way.
pub(super) fn read_dispatch_defaults_for_complete(
    home: &Path,
    dispatch_id: &str,
) -> Option<DispatchCompleteDefaults> {
    if !crate::dispatch_ops::is_valid_dispatch_id(dispatch_id) {
        return None;
    }
    let runs_dir = home.join("runs");
    let run_dir = runs_dir.join(dispatch_id);
    if !run_dir.is_dir() || !crate::dispatch_ops::canonical_dir_is_within(&run_dir, &runs_dir) {
        return None;
    }
    let mut defaults = DispatchCompleteDefaults::default();
    merge_dispatch_defaults_from_path(&mut defaults, &run_dir.join("status.json"));
    if defaults.agent.is_some() && defaults.task.is_some() && defaults.profile.is_some() {
        return Some(defaults);
    }
    Some(defaults).filter(|defaults| {
        defaults.agent.is_some() || defaults.task.is_some() || defaults.profile.is_some()
    })
}

pub(super) fn read_dispatch_defaults_for_complete_with_flow(
    home: &Path,
    flow_id: Option<&str>,
    dispatch_id: &str,
) -> Option<DispatchCompleteDefaults> {
    // Gate up front: the flow-scoped merge below embeds `dispatch_id` into a
    // filename (`dispatch-{dispatch_id}.json`) rather than joining it as a
    // bare path component, so a `/`-free malicious id wouldn't traverse --
    // but an id containing `/` still splits into path components at join
    // time, so it gets the same allowlist rather than relying on that
    // incidental defanging.
    if !crate::dispatch_ops::is_valid_dispatch_id(dispatch_id) {
        return None;
    }
    let mut defaults = read_dispatch_defaults_for_complete(home, dispatch_id).unwrap_or_default();
    if let Some(flow_id) = flow_id {
        if let Ok(run_dir) = crate::task_lifecycle::run_dir_for_flow_id(flow_id) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// tachi#1173 k2 fix discriminator: a caller-supplied `dispatch_id`
    /// containing a path-traversal or absolute-path payload must be
    /// rejected fail-closed -- and must never surface fields merged in from
    /// a decoy `status.json` planted outside `home/runs` that a successful
    /// escape would have read.
    #[test]
    fn read_dispatch_defaults_for_complete_rejects_path_traversal_dispatch_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        let runs_dir = home.join("runs");
        std::fs::create_dir_all(&runs_dir).expect("create runs dir");

        let decoy_dir = home.join("decoy");
        std::fs::create_dir_all(&decoy_dir).expect("create decoy dir");
        std::fs::write(
            decoy_dir.join("status.json"),
            serde_json::json!({
                "agent": "decoy-should-never-be-read",
                "task": "leaked-via-traversal",
                "profile": "leaked-via-traversal",
            })
            .to_string(),
        )
        .expect("write decoy status.json");

        for malicious in [
            "../decoy",
            "../../decoy",
            "..",
            "",
            "/etc/passwd",
            "a/../../decoy",
        ] {
            let result = read_dispatch_defaults_for_complete(home, malicious);
            assert!(
                result.is_none(),
                "dispatch_id {malicious:?} must be rejected fail-closed (no defaults \
                 resolved outside home/runs); got: {result:?}"
            );
        }
    }

    /// The gate must not break the ordinary path: a dispatch_id shaped like
    /// a real one, with a real status.json under it, still resolves.
    #[test]
    fn read_dispatch_defaults_for_complete_still_resolves_legit_dispatch_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        let dispatch_id = "20260718T101010Z-claude-abc12345";
        let run_dir = home.join("runs").join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("create run dir");
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::json!({"agent": "claude", "task": "do the thing", "profile": "p1"})
                .to_string(),
        )
        .expect("write status.json");

        let defaults = read_dispatch_defaults_for_complete(home, dispatch_id)
            .expect("defaults for legit dispatch_id");
        assert_eq!(defaults.agent.as_deref(), Some("claude"));
        assert_eq!(defaults.task.as_deref(), Some("do the thing"));
        assert_eq!(defaults.profile.as_deref(), Some("p1"));
    }
}
