use crate::tool_params::{TachiEventParams, TachiProfileParams};
use crate::MemoryServer;
use memory_core::{AgentProfilePack, AgentProfileRule};
use serde_json::Value;

use super::pack::identity_name;

pub(super) fn render_profile_context(
    server: &MemoryServer,
    pack: &AgentProfilePack,
    params: &TachiProfileParams,
) -> String {
    let session_kind = params
        .session_kind
        .as_deref()
        .unwrap_or("main")
        .to_ascii_lowercase();
    let private_session = matches!(session_kind.as_str(), "subagent" | "group" | "external");
    let include_user = params.include_private_user && !private_session;

    let mut lines = vec![
        "## Tachi Profile Context".to_string(),
        String::new(),
        format!("- agent_id: {}", pack.agent_id),
        format!("- name: {}", identity_name(pack)),
    ];
    if let Some(role) = &params.role {
        lines.push(format!("- requested_role: {role}"));
    }
    if let Some(project) = &params.project {
        lines.push(format!("- project: {project}"));
    }
    lines.push(String::new());

    append_rules_limited(&mut lines, "Voice", &pack.voice, 4);
    append_rules_limited(
        &mut lines,
        "Operating Contract",
        &pack.operating_contract,
        6,
    );
    append_rules_limited(&mut lines, "Quality Bar", &pack.quality_bar, 4);
    append_rules_limited(&mut lines, "Tool Policy", &pack.tool_policy, 4);
    append_rules_limited(&mut lines, "Memory Policy", &pack.memory_policy, 4);
    append_rules_limited(&mut lines, "Role Hats", &pack.role_hats, 4);
    if include_user {
        append_rules_limited(&mut lines, "User Model", &pack.user_model, 4);
    } else if !pack.user_model.is_empty() {
        lines.push("## User Model".to_string());
        lines.push(String::new());
        lines.push("- suppressed for this session kind; request include_private_user=true only in trusted main sessions.".to_string());
        lines.push(String::new());
    }
    if params.include_continuity {
        append_continuity_context(server, params, &mut lines);
    }

    let mut context = lines.join("\n").trim_end().to_string() + "\n";
    truncate_context_to_max_chars(&mut context, params.max_chars);
    context
}

fn append_continuity_context(
    server: &MemoryServer,
    params: &TachiProfileParams,
    lines: &mut Vec<String>,
) {
    let event_params = TachiEventParams {
        action: "context".to_string(),
        format: None,
        id: None,
        source_repo: None,
        adapter: None,
        project: params.project.clone(),
        domain: None,
        session_id: None,
        actor: None,
        event_type: None,
        authority: None,
        effects: Vec::new(),
        projection_hints: Vec::new(),
        payload: None,
        provenance: None,
        created_at: None,
        limit: 8,
        path_prefix: None,
        dry_run: false,
    };
    let context = match crate::continuity_ops::build_continuity_context(server, &event_params) {
        Ok(context) => context,
        Err(error) => {
            lines.push("## Continuity Read Model".to_string());
            lines.push(String::new());
            lines.push(format!("- unavailable: {error}"));
            lines.push(String::new());
            return;
        }
    };

    lines.push("## Continuity Read Model".to_string());
    lines.push(String::new());
    let memory_count = context
        .get("memory_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let event_count = context
        .get("event_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    lines.push(format!(
        "- projected_memories: {memory_count}; recent_events: {event_count}"
    ));
    if let Some(rate) = context
        .pointer("/metrics/session_outcomes/challenge_rate")
        .and_then(Value::as_f64)
    {
        lines.push(format!("- challenge_rate: {rate:.2}"));
    }
    lines.push("- guardrail: continuity context is guidance; facts, tests, and user corrections still dominate.".to_string());
    lines.push("- affect, when present, is tone/reminder only; never scoring, execution, routing, or fact mutation.".to_string());
    append_context_items(lines, "Patterns", context.get("patterns"), 5);
    append_context_items(lines, "Lorebook", context.get("lorebook"), 3);
    append_context_items(lines, "Affect", context.get("affect"), 3);
    lines.push(String::new());
}

fn append_context_items(lines: &mut Vec<String>, title: &str, value: Option<&Value>, limit: usize) {
    let Some(items) = value.and_then(Value::as_array) else {
        return;
    };
    if items.is_empty() {
        return;
    }
    lines.push(format!("### {title}"));
    lines.push(String::new());
    for item in items.iter().take(limit) {
        let summary = item
            .get("summary")
            .or_else(|| item.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("continuity item")
            .trim();
        let path = item
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if path.is_empty() {
            lines.push(format!("- {summary}"));
        } else {
            lines.push(format!("- {summary} ({path})"));
        }
    }
    lines.push(String::new());
}

fn truncate_context_to_max_chars(context: &mut String, max_chars: usize) {
    if context.len() <= max_chars {
        return;
    }
    if max_chars == 0 {
        context.clear();
        return;
    }

    let suffix = "\n[truncated by tachi_profile max_chars]\n";
    if max_chars <= suffix.len() {
        context.truncate(floor_char_boundary(context, max_chars));
        return;
    }

    let cut_at = floor_char_boundary(context, max_chars - suffix.len());
    context.truncate(cut_at);
    context.push_str(suffix);
}

fn floor_char_boundary(value: &str, idx: usize) -> usize {
    let mut boundary = idx.min(value.len());
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

fn append_rules_limited(
    lines: &mut Vec<String>,
    title: &str,
    rules: &[AgentProfileRule],
    limit: usize,
) {
    if rules.is_empty() {
        return;
    }
    lines.push(format!("## {title}"));
    lines.push(String::new());
    for rule in rules.iter().take(limit) {
        lines.push(format!("- {}", rule.text));
    }
    lines.push(String::new());
}
