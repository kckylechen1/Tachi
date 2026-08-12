use super::super::*;

pub(super) fn format_feature_briefing_markdown(value: &Value) -> String {
    let mut out = Vec::new();
    out.push("# Feature Briefing".to_string());
    out.push(markdown_section(
        "A2A Responses",
        value.get("a2a_responses").and_then(Value::as_array),
        "No pending A2A responses.",
    ));
    out.push(format!(
        "\n## Objective\n{}",
        value
            .get("objective")
            .and_then(Value::as_str)
            .unwrap_or("(unspecified)")
    ));
    out.push(format!(
        "\n## Current Stage\n{}",
        value
            .get("current_stage")
            .and_then(Value::as_str)
            .unwrap_or("intake")
    ));
    out.push(markdown_section(
        "Project Work Record",
        value.get("project_work_record").and_then(Value::as_array),
        "No GitHub issue/PR reference attached.",
    ));
    out.push(markdown_section(
        "Canonical Docs / Specs",
        value.get("canonical_docs").and_then(Value::as_array),
        "No canonical docs/specs attached.",
    ));
    out.push(markdown_section(
        "Run Artifacts",
        value.get("run_artifacts").and_then(Value::as_array),
        "No flow run artifacts attached.",
    ));
    out.push(markdown_section(
        "Board State",
        value
            .get("board_state")
            .and_then(|board| board.get("tasks"))
            .and_then(Value::as_array),
        "No matching board tasks.",
    ));
    let mut guide_rows = Vec::new();
    if let Some(hits) = value.get("guide_hits").and_then(Value::as_array) {
        guide_rows.extend(hits.iter().cloned());
    }
    if let Some(sops) = value
        .get("guide_sop")
        .and_then(|guide| guide.get("selected_sops"))
        .and_then(Value::as_array)
    {
        guide_rows.extend(sops.iter().cloned());
    }
    out.push(markdown_section(
        "Guide / SOP",
        if guide_rows.is_empty() {
            None
        } else {
            Some(&guide_rows)
        },
        "No SOP selected.",
    ));
    out.push(markdown_section(
        "Feedback Rules",
        value
            .get("feedback_rules")
            .and_then(|rules| rules.get("rules"))
            .and_then(Value::as_array),
        "No applicable feedback rules.",
    ));
    out.push(markdown_dispatch_recommendation(value));
    out.push(markdown_section(
        "Relevant Skills / Profiles",
        value.get("relevant_profiles").and_then(Value::as_array),
        "No dispatch profiles ranked.",
    ));
    out.push(markdown_section(
        "Wiki Decisions / Lessons",
        value.get("wiki_hits").and_then(Value::as_array),
        "No wiki hits.",
    ));
    out.push(markdown_section(
        "Memory Fragments / Checkpoints",
        value.get("memory_fragments").and_then(Value::as_array),
        "No project-scoped memory fragments.",
    ));
    out.push(markdown_section(
        "Eval Evidence",
        value.get("eval_evidence").and_then(Value::as_array),
        "No matching eval evidence.",
    ));
    if let Some(loops) = value
        .get("open_loops")
        .and_then(Value::as_array)
        .filter(|loops| !loops.is_empty())
    {
        out.push("\n## ⚠️ Open Loops (closure debt)".to_string());
        for item in loops {
            let detail = item.get("detail").and_then(Value::as_str).unwrap_or("");
            let action = item.get("action").and_then(Value::as_str).unwrap_or("");
            out.push(format!("- {detail} → `{action}`"));
        }
    }
    // #1000 round-3 codex review finding 5: the JSON response has carried
    // `issue_freshness` since #1000 shipped, but this markdown renderer
    // never rendered it — `tachi_memory`'s compatibility briefing did (via
    // `agent_markdown::format_briefing`), `tachi_task`'s feature briefing
    // markdown did not. Shared helper keeps the two renderers' wording (and
    // the finding-7 wording fix) from drifting apart.
    if let Some(section) = value
        .get("issue_freshness")
        .and_then(crate::agent_markdown::render_issue_freshness_section)
    {
        out.push(section);
    }
    out.push(markdown_presence_section(value));
    out.push(format!(
        "\n## Next Action\n{}",
        value
            .get("next_action")
            .and_then(Value::as_str)
            .unwrap_or("Continue from the canonical docs/specs.")
    ));
    out.join("\n")
}

/// Presence 工位表 (#1001 round 2 item 4): the task-facing `feature_briefing`
/// markdown used to render every section EXCEPT presence — the JSON payload
/// carried `value["presence"]` (board + advisory collision warnings) but the
/// text a harness actually reads never showed it, silently dropping the
/// "who else is touching this" signal for any caller using the markdown
/// format. Mirrors `agent_markdown::briefing::format_briefing`'s presence
/// rendering (same row shape, same advisory framing) so the two briefing
/// surfaces agree. Returns an empty string (no section, no stray heading)
/// when there is nothing to show — both board and warnings empty.
pub(super) fn markdown_presence_section(value: &Value) -> String {
    let presence = value.get("presence").unwrap_or(&Value::Null);
    let board = presence.get("board");
    let items = board
        .and_then(|b| b.get("items"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // #527: `briefing_claims_board` already caps `items` at
    // `PRESENCE_BOARD_DISPLAY_CAP` and reports how many live claims were cut
    // off in `overflow` — render that as a "+N more" note (#1004 convention)
    // rather than silently showing a partial list with no indication
    // anything was truncated.
    let overflow = board
        .and_then(|b| b.get("overflow"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let warnings = presence
        .get("warnings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if items.is_empty() && warnings.is_empty() {
        return String::new();
    }
    let mut out = vec!["\n## Presence 工位表 (who's working what)".to_string()];
    out.push(
        "_Advisory only — never a lock. TTL-expired claims disappear on their own._".to_string(),
    );
    for row in &items {
        let session = row
            .get("session_client")
            .and_then(Value::as_str)
            .unwrap_or("?");
        let issue_ref = row.get("issue_ref").and_then(Value::as_str);
        let flow_id = row.get("flow_id").and_then(Value::as_str);
        let heartbeat = row
            .get("heartbeat_at")
            .and_then(Value::as_str)
            .unwrap_or("");
        let target = issue_ref.or(flow_id).unwrap_or("(no issue/flow declared)");
        out.push(format!(
            "- **{session}** → {target} (heartbeat {heartbeat})"
        ));
    }
    if overflow > 0 {
        out.push(format!("- _+{overflow} more (showing {})_", items.len()));
    }
    for warning in &warnings {
        if let Some(text) = warning.as_str() {
            out.push(format!("- ⚠️ {text}"));
        }
    }
    out.join("\n")
}

pub(super) fn markdown_dispatch_recommendation(value: &Value) -> String {
    let mut out = vec!["\n## Native Subagent Handoff".to_string()];
    let recommendation = value.get("route_recommendation").unwrap_or(&Value::Null);
    let suggested = value.get("suggested_handoff").unwrap_or(&Value::Null);
    let Some(profile) = recommendation
        .get("recommended_profile")
        .and_then(Value::as_str)
    else {
        out.push("- No advisory profile recommendation available.".to_string());
        return out.join("\n");
    };
    let agent = recommendation
        .get("recommended_agent")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let risk = recommendation
        .get("risk")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    out.push(format!(
        "- Profile: `{profile}` via `{agent}` (risk={risk})"
    ));
    if let Some(reason) = recommendation.get("reason").and_then(Value::as_array) {
        let reason = reason
            .iter()
            .take(3)
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        if !reason.is_empty() {
            out.push(format!("- Why: {}", reason.join("; ")));
        }
    }
    if let Some(context) = suggested.get("context") {
        out.push(format!("- Handoff context: `{}`", compact_json(context)));
    }
    out.push("- Use the host harness's native subagent; this recommendation does not authorize Tachi worker dispatch.".to_string());
    out.join("\n")
}

pub(super) fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

pub(super) fn markdown_section(title: &str, rows: Option<&Vec<Value>>, empty: &str) -> String {
    let mut out = vec![format!("\n## {title}")];
    let Some(rows) = rows.filter(|rows| !rows.is_empty()) else {
        out.push(format!("- {empty}"));
        return out.join("\n");
    };
    for row in rows.iter().take(8) {
        out.push(format!("- {}", compact_value_line(row)));
    }
    out.join("\n")
}

pub(super) fn compact_value_line(value: &Value) -> String {
    if let Some(path) = value.get("path").and_then(Value::as_str) {
        let summary = value
            .get("summary")
            .or_else(|| value.get("kind"))
            .or_else(|| value.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if summary.is_empty() {
            return format!("`{path}`");
        }
        return format!("`{path}` - {summary}");
    }
    if let Some(summary) = value.get("summary").and_then(Value::as_str) {
        return summary.to_string();
    }
    if let Some(id) = value.get("dispatch_id").and_then(Value::as_str) {
        let state = value
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let summary = value.get("summary").and_then(Value::as_str).unwrap_or("");
        return format!("`{id}` [{state}] {summary}");
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1000 round-3 codex review finding 5: `issue_freshness` is present on
    /// the JSON response (`handlers.rs` always inserts it, even when both
    /// queues are empty), but the markdown renderer used to drop it
    /// entirely — no "Issue freshness" section appeared anywhere in
    /// `tachi_task`'s markdown output, unlike `tachi_memory`'s briefing.
    #[test]
    fn feature_briefing_markdown_renders_issue_freshness_section_when_nonempty() {
        let value = json!({
            "objective": "test",
            "current_stage": "intake",
            "issue_freshness": {
                "zombies": {
                    "count": 1,
                    "items": [{ "issue_ref": "owner/repo#979" }],
                    "overflow": 0,
                },
                "stale_candidates": {
                    "count": 0,
                    "items": [],
                    "overflow": 0,
                },
            },
        });
        let markdown = format_feature_briefing_markdown(&value);
        assert!(
            markdown.contains("Issue freshness"),
            "expected an Issue freshness section, got: {markdown}"
        );
        assert!(
            markdown.contains("owner/repo#979"),
            "expected the zombie issue_ref surfaced, got: {markdown}"
        );
    }

    /// Empty-queue case must not emit an empty/misleading section — same
    /// convention as every other `markdown_section` call in this renderer.
    #[test]
    fn feature_briefing_markdown_omits_issue_freshness_section_when_empty() {
        let value = json!({
            "objective": "test",
            "current_stage": "intake",
            "issue_freshness": {
                "zombies": { "count": 0, "items": [], "overflow": 0 },
                "stale_candidates": { "count": 0, "items": [], "overflow": 0 },
            },
        });
        let markdown = format_feature_briefing_markdown(&value);
        assert!(
            !markdown.contains("Issue freshness"),
            "expected no Issue freshness section when both queues are empty, got: {markdown}"
        );
    }

    /// Missing `issue_freshness` field entirely (defensive — shouldn't
    /// happen since `handlers.rs` always inserts it, but the renderer must
    /// not panic) must degrade to omitting the section, not erroring.
    #[test]
    fn feature_briefing_markdown_handles_missing_issue_freshness_field() {
        let value = json!({
            "objective": "test",
            "current_stage": "intake",
        });
        let markdown = format_feature_briefing_markdown(&value);
        assert!(!markdown.contains("Issue freshness"));
    }
}
