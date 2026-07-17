use crate::{TachiArenaParams, TachiDispatchParams};
use serde_json::{json, Value};
use std::path::Path;

use super::lane::HarnessLane;

fn default_opencode_model(role: Option<&str>) -> &'static str {
    match role.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "explore" | "search" | "librarian" => "deepseek/deepseek-v4-flash",
        "critic" | "review" | "reviewer" => "deepseek/deepseek-v4-pro",
        _ => tachi_dispatch::GLM_CODING_DEFAULT_MODEL,
    }
}

fn default_opencode_model_for_command(role: Option<&str>) -> String {
    match role.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "explore" | "search" | "librarian" => default_opencode_model(role).to_string(),
        "critic" | "review" | "reviewer" => default_opencode_model(role).to_string(),
        _ => tachi_dispatch::resolve_dispatch_model(tachi_dispatch::GLM_CODING_MODEL_ALIAS)
            .unwrap_or_else(|| tachi_dispatch::GLM_CODING_DEFAULT_MODEL.to_string()),
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
        "opencode" => "opencode",
        "claude" => "claude",
        _ => return None,
    };
    let mut command = Vec::new();
    if lane.id == "opencode" && params.profile.is_none() {
        let model = params
            .model
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| default_opencode_model_for_command(params.role.as_deref()));
        command = vec![
            opencode_binary(),
            "--pure".to_string(),
            "run".to_string(),
            "--model".to_string(),
            model,
        ];
    }

    Some(TachiDispatchParams {
        agent: Some(agent.to_string()),
        profile: params.profile.clone(),
        credential_profiles: params.credential_profiles.clone(),
        task: tracked_prompt.to_string(),
        execution_level: None,
        cwd: params.cwd.clone(),
        // Arena spawn bridges a bare cwd into dispatch; declare it unmanaged for
        // the fail-safe env gate (#894 S1 §1.3 escape hatch).
        env_id: None,
        unmanaged_cwd: Some(true),
        skills: params.skills.clone(),
        context_query: None,
        model: params.model.clone(),
        timeout_secs: params.timeout_secs.unwrap_or(600),
        permission_profile: params.permission_profile.clone(),
        allowed_tools: Vec::new(),
        completion_predicate: None,
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
        verbose: None,
        inject_card: None,
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
    let inferred_outcome = infer_completion_outcome(result_path);
    let mut arguments = serde_json::Map::new();
    arguments.insert("action".into(), json!("complete"));
    arguments.insert(
        "dispatch_id".into(),
        status.get("dispatch_id").cloned().unwrap_or(Value::Null),
    );
    arguments.insert("task".into(), json!(task));
    arguments.insert("agent".into(), json!(agent));
    if let Some(outcome) = inferred_outcome {
        arguments.insert("outcome".into(), json!(outcome));
    } else {
        arguments.insert("outcome".into(), Value::Null);
    }
    arguments.insert(
        "profile".into(),
        status
            .get("dispatch_profile_name")
            .cloned()
            .unwrap_or(Value::Null),
    );
    arguments.insert(
        "flow_id".into(),
        status.get("flow_id").cloned().unwrap_or(Value::Null),
    );
    arguments.insert(
        "issue_ref".into(),
        status.get("issue_ref").cloned().unwrap_or(Value::Null),
    );
    arguments.insert(
        "pr_ref".into(),
        status.get("pr_ref").cloned().unwrap_or(Value::Null),
    );
    arguments.insert(
        "evidence_refs".into(),
        json!([result_path.to_string_lossy().to_string()]),
    );
    arguments.insert("tests_run".into(), json!([]));
    arguments.insert("diff_present".into(), Value::Null);
    let mut draft = serde_json::Map::new();
    draft.insert("tool".into(), json!("tachi_task"));
    draft.insert("arguments".into(), Value::Object(arguments));
    if inferred_outcome.is_none() {
        draft.insert("required_edits".into(), json!(["outcome"]));
    }
    Value::Object(draft)
}

pub(super) fn infer_completion_outcome(result_path: &Path) -> Option<&'static str> {
    let raw = std::fs::read_to_string(result_path).ok()?;
    let lower = raw.to_ascii_lowercase();
    if lower.contains("exit_code: 0") || lower.contains("status: success") {
        Some("success")
    } else if lower.contains("exit_code:") && !lower.contains("exit_code: 0") {
        Some("failure")
    } else if lower.contains("aborted") {
        Some("aborted")
    } else if lower.contains("partial") {
        Some("partial")
    } else {
        None
    }
}
