use crate::TachiArenaParams;

use super::lane::harness_lane;

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
