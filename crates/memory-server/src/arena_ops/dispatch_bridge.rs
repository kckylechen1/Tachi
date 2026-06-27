use crate::{TachiArenaParams, TachiDispatchParams};
use serde_json::{json, Value};
use std::path::Path;

use super::lane::HarnessLane;

fn default_opencode_model(role: Option<&str>) -> &'static str {
    match role.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "explore" | "search" | "librarian" => "deepseek/deepseek-v4-flash",
        "critic" | "review" | "reviewer" => "deepseek/deepseek-v4-pro",
        _ => "zhipuai-coding-plan/glm-5.1",
    }
}

fn opencode_binary() -> String {
    #[cfg(test)]
    {
        if let Ok(path) = std::env::var("TACHI_TEST_OPENCODE_BIN") {
            return path;
        }
    }
    "opencode".to_string()
}

pub(super) fn dispatch_params_for_mission(
    params: &TachiArenaParams,
    lane: &HarnessLane,
    tracked_prompt: &str,
) -> Option<TachiDispatchParams> {
    let agent = match lane.id {
        "opencode" => "custom",
        "claude" => "claude",
        _ => return None,
    };
    let mut command = Vec::new();
    if lane.id == "opencode" && params.profile.is_none() {
        let model = params
            .model
            .as_deref()
            .unwrap_or_else(|| default_opencode_model(params.role.as_deref()));
        command = vec![
            opencode_binary(),
            "--pure".to_string(),
            "run".to_string(),
            "--model".to_string(),
            model.to_string(),
        ];
    }

    Some(TachiDispatchParams {
        agent: Some(agent.to_string()),
        profile: params.profile.clone(),
        credential_profiles: params.credential_profiles.clone(),
        task: tracked_prompt.to_string(),
        cwd: params.cwd.clone(),
        skills: params.skills.clone(),
        context_query: None,
        model: params.model.clone(),
        timeout_secs: params.timeout_secs.unwrap_or(600),
        permission_profile: params.permission_profile.clone(),
        allowed_tools: Vec::new(),
        max_turns: None,
        sandbox: params.sandbox.clone(),
        inject_tachi_mcp: None,
        inject_hub_mcps: None,
        command,
        harness_transport: None,
        harness_server_url: None,
        project: params.project.clone(),
        stage: params.role.clone(),
        issue_ref: params.issue_ref.clone(),
        pr_ref: params.pr_ref.clone(),
        flow_id: params.flow_id.clone(),
        tool_profile: params.tool_profile.clone(),
        auto_capability_bundle: params.auto_capability_bundle,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
    })
}

pub(super) fn completion_draft_for_mission(status: &Value, result_path: &Path) -> Value {
    let task = status
        .get("task")
        .and_then(Value::as_str)
        .unwrap_or("tracked arena mission");
    let agent = status
        .get("dispatch_agent")
        .and_then(Value::as_str)
        .or_else(|| status.get("harness").and_then(Value::as_str))
        .unwrap_or("arena-worker");
    json!({
        "tool": "tachi_task",
        "arguments": {
            "action": "complete",
            "dispatch_id": status.get("dispatch_id").cloned().unwrap_or(Value::Null),
            "task": task,
            "agent": agent,
            "outcome": "success|failure|partial|aborted",
            "profile": status.get("dispatch_profile_name").cloned().unwrap_or(Value::Null),
            "flow_id": status.get("flow_id").cloned().unwrap_or(Value::Null),
            "issue_ref": status.get("issue_ref").cloned().unwrap_or(Value::Null),
            "pr_ref": status.get("pr_ref").cloned().unwrap_or(Value::Null),
            "evidence_refs": [result_path.to_string_lossy().to_string()],
            "tests_run": [],
            "diff_present": null,
        }
    })
}
