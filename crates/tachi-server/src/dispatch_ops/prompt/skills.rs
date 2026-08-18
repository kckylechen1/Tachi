use crate::tool_params::TachiDispatchParams;

/// Resolve the effective skills list. Per #1690 C3 S1, skills come ONLY from
/// the explicit `params.skills` param — which already includes the profile's
/// STATIC reviewed skill list (`common_skills` + `signature_skills`,
/// materialized by `resolve_and_apply_dispatch_profile` when the caller passed
/// none). There is NO automatic derivation from the stage or from task-selected
/// SOPs: stage defaults and task-SOP promotion are retired.
///
/// The stage key is an explicit caller-chosen param, so its advisory
/// instruction (e.g. the "plan first" line for stage=auto) is still rendered:
/// that is a projection of the explicit stage choice, not skill selection.
pub(in crate::dispatch_ops) fn resolve_effective_skills(
    params: &TachiDispatchParams,
) -> (Vec<String>, Option<String>) {
    let stage_key = crate::skill_policy::dispatch_stage_key(params.stage.as_deref());
    let auto_instruction = crate::skill_policy::dispatch_stage_instruction(&stage_key);
    (params.skills.clone(), auto_instruction)
}

pub(super) fn render_skill_invocation_contract(
    skill_id: &str,
    cap: &memcore::HubCapability,
    def: &serde_json::Value,
) -> String {
    let source_path = def
        .get("source_path")
        .and_then(|value| value.as_str())
        .or_else(|| def.get("skill_path").and_then(|value| value.as_str()))
        .unwrap_or("unknown");
    let prompt = def
        .get("prompt")
        .and_then(|value| value.as_str())
        .map(|value| compact_skill_text(value, 700));
    let content = def
        .get("content")
        .and_then(|value| value.as_str())
        .map(|value| compact_skill_text(strip_frontmatter(value), 1400));

    let mut lines = vec![
        format!("### {skill_id}"),
        format!("- name: {}", cap.name),
        format!("- source_path: {source_path}"),
        format!("- why: {}", cap.description),
        format!(
            "- invocation: `tachi_skill(action='run', skill_id='{skill_id}', args={{\"task\": \"<task>\", \"context\": \"<context>\"}})` when available"
        ),
    ];
    if let Some(prompt) = prompt {
        lines.push(format!("- activation_prompt: {prompt}"));
    }
    if let Some(content) = content {
        lines.push(format!("- embedded_contract: {content}"));
    }
    lines.join("\n")
}

fn strip_frontmatter(text: &str) -> &str {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("---") {
        return trimmed;
    }
    let rest = &trimmed[3..];
    if let Some(end) = rest.find("\n---") {
        return rest[end + 4..].trim_start();
    }
    trimmed
}

fn compact_skill_text(text: &str, max_chars: usize) -> String {
    let mut out = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if out.chars().count() > max_chars {
        out = out.chars().take(max_chars).collect::<String>();
        out.push_str("...");
    }
    out
}
