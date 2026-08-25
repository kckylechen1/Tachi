use super::*;

pub(in crate::copilot_ops) fn tokenize_task(input: &str) -> Vec<String> {
    input
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .map(|token| token.trim().to_lowercase())
        .filter(|token| is_meaningful_skill_token(token))
        .collect()
}

pub(in crate::copilot_ops) fn is_meaningful_skill_token(token: &str) -> bool {
    if token.chars().count() < 3 {
        return false;
    }
    const STOPWORDS: &[&str] = &[
        "fix", "fixed", "fixing", "repair", "resolve", "bug", "bugs", "issue", "issues", "problem",
        "problems", "error", "errors", "failed", "failure", "task", "work", "use", "using", "add",
        "update", "change", "修复", "问题", "错误", "失败", "任务",
    ];
    !STOPWORDS.contains(&token)
}

pub(in crate::copilot_ops) fn tokenize_skill_text(input: &str) -> HashSet<String> {
    tokenize_task(input).into_iter().collect()
}

// #1690 C3 slice A: the "second model brain" is retired. The copilot
// briefing path no longer runs hub scoring / pattern-bridge / telemetry
// signals over the capability registry (`recommend_skills_light` is deleted).
// The only skill list a brief still carries is a STATIC projection of its own
// intent routing: `selected_sops` is built from the frozen intent→native-skill
// map in `task_routing` (itself sourced from the static native skill registry
// in `skill_policy`), and `native_skill_suggestions` just re-shapes those rows
// into the `recommended_skills` output field — no server access, no scoring,
// no pattern/telemetry signals. Rows whose id is a `workflow:` SOP or a
// non-native skill drop out.
pub(in crate::copilot_ops) fn native_skill_suggestions(
    selected_sops: &[Value],
    limit: usize,
) -> Vec<Value> {
    selected_sops
        .iter()
        .filter_map(|sop| {
            let id = sop.get("id").and_then(Value::as_str)?;
            if !id.starts_with("skill:") || !crate::skill_policy::is_native_skill(id) {
                return None;
            }
            Some(json!({
                "id": id,
                "name": sop.get("name").and_then(Value::as_str).unwrap_or(id),
                "reason": sop.get("reason").and_then(Value::as_str).unwrap_or("Native skill routed by the static task-brief intent map."),
            }))
        })
        .take(limit.max(1))
        .collect()
}
