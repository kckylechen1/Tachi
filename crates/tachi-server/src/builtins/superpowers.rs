use super::helpers::{make_skill_capability, resolve_skill_content_source, SkillContentSource};
use super::*;
use tachi_dispatch::{
    SUPERPOWER_BRAINSTORMING, SUPERPOWER_EXECUTING_PLANS, SUPERPOWER_FINISHING_BRANCH,
    SUPERPOWER_REQUESTING_CODE_REVIEW, SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT,
    SUPERPOWER_VERIFICATION_BEFORE_COMPLETION, SUPERPOWER_WRITING_PLANS,
};

pub(super) fn builtin_superpowers_skills() -> Result<Vec<HubCapability>, String> {
    const SUPERPOWERS_SKILLS: &[(&str, &str, &str, SkillContentSource)] = &[
        (
            SUPERPOWER_BRAINSTORMING,
            "brainstorming",
            "Superpowers workflow gate for turning rough ideas into approved designs before implementation.",
            SkillContentSource::Vendored("skill/superpowers/skills/brainstorming/SKILL.md"),
        ),
        (
            SUPERPOWER_WRITING_PLANS,
            "writing-plans",
            "Superpowers workflow gate for writing bite-sized implementation plans before touching code.",
            SkillContentSource::Vendored("skill/superpowers/skills/writing-plans/SKILL.md"),
        ),
        (
            SUPERPOWER_EXECUTING_PLANS,
            "executing-plans",
            "Superpowers workflow gate for executing written plans with review checkpoints.",
            SkillContentSource::Vendored("skill/superpowers/skills/executing-plans/SKILL.md"),
        ),
        (
            SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT,
            "subagent-driven-development",
            "Superpowers workflow gate for running implementation through bounded worker slices with reviewer callbacks.",
            SkillContentSource::Native(
                r#"# Subagent-Driven Development

## Native Tachi Contract

Use this workflow when a plan or task can be split into independent worker slices.

1. Parse the plan once. Extract worker tasks with full context, write scope, forbidden files, validation commands, and done condition.
2. Dispatch only bounded, verifiable worker slices. Do not dispatch one-search or one-file edits that the leader can finish directly.
3. Keep same-file edits, plan-before-code sequences, and chained transforms serial.
4. For each worker, include required skills, allowed scope, validation, and an explicit report-back contract.
5. Require two review gates for meaningful implementation: spec compliance first, then code quality.
6. The leader owns integration, conflict resolution, final verification, and user-facing completion.

## Required Worker Output

Start with `Using skills: <ids>`. Finish with changed files, verification run, blockers, and recommended handoff. Do not mark the parent task complete.
"#,
            ),
        ),
        (
            SUPERPOWER_REQUESTING_CODE_REVIEW,
            "requesting-code-review",
            "Superpowers workflow gate for dispatching focused code review before merge or after major work.",
            SkillContentSource::Vendored("skill/superpowers/skills/requesting-code-review/SKILL.md"),
        ),
        (
            SUPERPOWER_VERIFICATION_BEFORE_COMPLETION,
            "verification-before-completion",
            "Superpowers workflow gate for proving completion before handoff, ship, or merge.",
            SkillContentSource::Native(
                r#"# Verification Before Completion

## Native Tachi Contract

Before claiming completion:

1. Restate what proves the task is done.
2. Run the smallest reliable verification commands for the touched behavior.
3. Read the output and fix failures instead of reporting partial success.
4. Check current git status and ensure no unrelated changes are claimed.
5. Record remaining risks or not-tested gaps honestly.
6. Save meaningful decisions or milestones through Tachi memory when available.

## Hard Stop

Do not say the task is complete if required verification is missing, failed, or unread.
"#,
            ),
        ),
        (
            SUPERPOWER_FINISHING_BRANCH,
            "finishing-a-development-branch",
            "Superpowers workflow gate for verifying and completing a development branch.",
            SkillContentSource::Vendored(
                "skill/superpowers/skills/finishing-a-development-branch/SKILL.md",
            ),
        ),
    ];

    SUPERPOWERS_SKILLS
        .iter()
        .map(|(id, name, description, source)| {
            let (content, _resolved_path, content_hash, source_path) =
                resolve_skill_content_source(name, source);
            make_skill_capability(
                id,
                &format!("superpowers/{name}"),
                description,
                json!({
                    "execution": "document",
                    "system": "You are applying a Superpowers workflow gate. Follow the embedded SKILL.md contract, respect hard gates, and keep stage transitions explicit.",
                    "prompt": "Apply the Superpowers workflow skill `superpowers/{{skill_name}}` to the task below.\n\nTask:\n{{task}}\n\nContext:\n{{context}}\n\nReturn the stage-appropriate result and name the next workflow gate when applicable.",
                    "content": content,
                    "content_hash": content_hash,
                    "policy": { "visibility": "discoverable" },
                    "tags": ["superpowers", "workflow", "builtin", "skill-set"],
                    "source_path": source_path,
                    "skill_path": format!("/skills/superpowers/{name}"),
                    "retention_policy": "permanent",
                    "inputSchema": {
                        "type": "object",
                        "required": ["task"],
                        "properties": {
                            "task": {"type": "string"},
                            "context": {"type": "string"},
                            "skill_name": {"type": "string", "default": name}
                        }
                    }
                }),
            )
        })
        .collect()
}
