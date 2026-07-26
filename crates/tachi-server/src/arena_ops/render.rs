use crate::TachiArenaParams;
use serde_json::Value;

use super::lane::harness_lane;
use super::state::{read_mission_result, ArenaArtifactRead};

pub(super) fn render_arena_md(arena_id: &str, title: &str, objective: &str) -> String {
    format!(
        "# {title}\n\n\
         Arena: `{arena_id}`\n\n\
         ## Objective\n\n{objective}\n\n\
         ## Contract\n\n\
         Arena owns run documents. Memory owns distilled knowledge.\n\n\
         Workers must write `plan.md` before substantive work and `result.md` before completion.\n"
    )
}

pub(super) fn render_prompt_md(
    params: &TachiArenaParams,
    arena_id: &str,
    mission_id: &str,
    feedback_rules_section: Option<&str>,
) -> String {
    let prompt = params.prompt.as_deref().unwrap_or("");
    let role = params.role.as_deref().unwrap_or("worker");
    let requested_harness = params.harness.as_deref().unwrap_or("manual");
    let lane = harness_lane(params.harness.as_deref());
    format!(
        "# Arena Mission\n\n\
         Arena: `{arena_id}`\n\
         Mission: `{mission_id}`\n\
         Requested harness: `{requested_harness}`\n\
         Harness lane: `{}` ({})\n\
         Launch mode: `{}`\n\
         Role: `{role}`\n\n\
         ## Lane Guidance\n\n\
         - Command hint: `{}`\n\
         - MCP support: `{}`\n\
         - Artifact contract: {}\n\
{}\n\
         ## Task\n\n{prompt}\n\n\
{}\n\
         ## Skills\n\n{}\n\n\
         ## Scope\n\n{}\n\n\
         ## Permissions\n\n{}\n\n\
         ## Worker Report Contract\n\n\
         Write `plan.md` before substantive work. Write `result.md` when finished, blocked, or partially complete.\n\
         Include Summary, Files changed, Commands run, Verification performed, and Remaining risks or blockers.\n",
        lane.id,
        lane.label,
        lane.launch_mode,
        lane.command_hint,
        lane.mcp_support,
        lane.artifact_contract,
        list_lines(
            &lane
                .notes
                .iter()
                .map(|note| note.to_string())
                .collect::<Vec<_>>()
        ),
        feedback_rules_section.unwrap_or(""),
        list_lines(&params.skills),
        list_lines(&params.scope),
        list_lines(&params.permissions)
    )
}

fn list_lines(items: &[String]) -> String {
    if items.is_empty() {
        "- none\n".to_string()
    } else {
        items
            .iter()
            .map(|item| format!("- {item}\n"))
            .collect::<String>()
    }
}

fn mission_result_preview(arena_id: &str, mission_id: &str) -> String {
    let raw = match read_mission_result(arena_id, mission_id) {
        ArenaArtifactRead::Present(raw) => raw,
        ArenaArtifactRead::Missing => String::new(),
        ArenaArtifactRead::Error(_) => return "result unreadable".to_string(),
    };
    raw.lines()
        .find(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .unwrap_or("")
        .trim()
        .chars()
        .take(180)
        .collect()
}

pub(super) fn render_summary_md(arena_id: &str, missions: &[Value]) -> String {
    let mut out = format!(
        "# Arena Summary\n\nArena: `{arena_id}`\n\nMissions: {}\n\nState: closed\n\n## Mission Results\n\n",
        missions.len()
    );
    if missions.is_empty() {
        out.push_str("- none\n");
        return out;
    }
    for mission in missions {
        let mission_id = mission
            .get("mission_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let harness = mission
            .get("harness")
            .and_then(Value::as_str)
            .unwrap_or("manual");
        let role = mission
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("worker");
        let state = mission
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let preview = mission_result_preview(arena_id, mission_id);
        out.push_str(&format!(
            "- `{mission_id}` [{harness}/{role}] {state}: {}\n",
            if preview.is_empty() {
                "no result preview"
            } else {
                preview.as_str()
            }
        ));
    }
    out
}
