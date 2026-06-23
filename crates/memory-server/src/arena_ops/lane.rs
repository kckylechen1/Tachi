use std::path::Path;

#[derive(Debug, Clone)]
pub(super) struct HarnessLane {
    pub(super) id: &'static str,
    pub(super) label: &'static str,
    pub(super) kind: &'static str,
    pub(super) launch_mode: &'static str,
    pub(super) command_hint: &'static str,
    pub(super) mcp_support: &'static str,
    pub(super) artifact_contract: &'static str,
    pub(super) notes: &'static [&'static str],
}

pub(super) fn harness_lane(requested: Option<&str>) -> HarnessLane {
    let normalized = requested
        .unwrap_or("manual")
        .trim()
        .to_ascii_lowercase()
        .replace('_', "-");
    match normalized.as_str() {
        "" | "manual" | "tracked-document" | "document" => HarnessLane {
            id: "manual",
            label: "Manual tracked document",
            kind: "manual",
            launch_mode: "tracked_document",
            command_hint: "Give tracked_prompt to any worker and require plan.md/result.md writes.",
            mcp_support: "external",
            artifact_contract: "Worker writes plan.md and result.md in the mission directory.",
            notes: &[
                "Fallback lane for harnesses Tachi does not launch natively.",
                "Leader owns process start, permissions, and completion review.",
            ],
        },
        "opencode" | "omo" => HarnessLane {
            id: "opencode",
            label: "OpenCode worker",
            kind: "worker",
            launch_mode: "opencode_worker",
            command_hint: "opencode --pure run --model <provider/model> \"<tracked_prompt>\"",
            mcp_support: "profile/config dependent",
            artifact_contract: "OpenCode must write mission plan.md and result.md; stdout is advisory.",
            notes: &[
                "Default execution lane for external subagents.",
                "Prefer for explore, implementation drafts, critic passes, and verifier work.",
            ],
        },
        "claude" | "claude-code" => HarnessLane {
            id: "claude",
            label: "Claude Code worker",
            kind: "worker",
            launch_mode: "claude_worker",
            command_hint: "claude --print \"<tracked_prompt>\" --mcp-config <config.json>",
            mcp_support: "json mcp config",
            artifact_contract: "Claude must write mission plan.md and result.md; use MCP when granted.",
            notes: &[
                "Use when the mission needs strong MCP/tool execution.",
                "Good fallback when OpenCode provider routing is unavailable.",
            ],
        },
        "gemini" | "gemini-advisor" | "ask-gemini" => HarnessLane {
            id: "gemini-advisor",
            label: "Gemini advisor",
            kind: "advisor",
            launch_mode: "advisor_artifact",
            command_hint: "gemini -p \"<advisor_prompt>\"; save output as .omx/artifacts/gemini-<slug>-<timestamp>.md",
            mcp_support: "not required",
            artifact_contract: "Advisor output is captured as an artifact and linked back into result.md; Gemini is not expected to edit arena files directly.",
            notes: &[
                "Brainstorm, design feedback, process critique, and second opinions only.",
                "Do not treat this lane as a normal worker harness.",
            ],
        },
        _ => HarnessLane {
            id: "manual",
            label: "Manual tracked document",
            kind: "manual",
            launch_mode: "tracked_document",
            command_hint: "Unsupported harness hint; use tracked_prompt manually or choose opencode, claude, gemini-advisor, or manual.",
            mcp_support: "external",
            artifact_contract: "Worker writes plan.md and result.md in the mission directory.",
            notes: &[
                "Unknown harness hints are intentionally treated as manual document missions.",
                "Tachi keeps the mission contract stable instead of launching arbitrary adapters.",
            ],
        },
    }
}

pub(super) fn tracked_worker_prompt(
    lane: &HarnessLane,
    prompt_path: &Path,
    plan_path: &Path,
    result_path: &Path,
) -> String {
    format!(
        "You are executing a tracked Tachi Arena mission.\n\n\
         Harness lane: {} ({})\n\
         Launch mode: {}\n\
         Command hint: {}\n\
\n\
         Read the mission prompt from:\n{}\n\n\
         Before substantive work, write a concise plan to:\n{}\n\n\
         When finished, blocked, or partially complete, write a completion report to:\n{}\n\n\
         Artifact contract: {}\n\n\
         The completion report must include:\n\
         - Summary\n\
         - Files changed\n\
         - Commands run\n\
         - Verification performed\n\
         - Remaining risks or blockers\n\n\
         Return a concise summary and mention the report path.",
        lane.id,
        lane.kind,
        lane.launch_mode,
        lane.command_hint,
        prompt_path.display(),
        plan_path.display(),
        result_path.display(),
        lane.artifact_contract
    )
}
