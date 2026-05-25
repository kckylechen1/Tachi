//! Dispatch V2 — two-stage Plan → Execute pipeline.
//!
//! Opt-in via `DISPATCH_V2_ENABLED=true`, `DISPATCH_V2_PLAN_REVIEW=true`,
//! `DISPATCH_V2_PLAN_TIMEOUT_SECS=<u64>`, or by passing
//! `stage = "plan_execute"` in the dispatch params.
//!
//! Stage 1 (plan): invoke `server.claude_pool` with a planning system
//! prompt and write the result to `<run_dir>/plan.md`.
//!
//! Stage 1.5 (optional review gate): when `DISPATCH_V2_PLAN_REVIEW=true`,
//! either prompt the operator interactively (TTY) or park the kanban row
//! in `TASK_STATE_PENDING_REVIEW` and return early (non-TTY).
//!
//! Stage 2 (execute): hand the generated `plan.md` plus the
//! `skill:implement-plan` reference to the chosen agent backend
//! (`claude`, `codex`, `custom`) and run it like the legacy V1 path.
//!
//! Default behaviour (env unset, `stage` not `plan_execute`) is the
//! legacy V1 single-stage flow — V2 must never silently activate.

use std::collections::HashMap;

use crate::TachiDispatchParams;

/// System prompt fed to the planning LLM. Must produce a markdown plan
/// with the four sections enforced by `parse_plan`.
pub(super) const PLAN_SYSTEM_PROMPT: &str = "You are a planning engine for Tachi dispatch. Given a task, output a markdown plan with:\n## Goal\n## Steps (numbered)\n## Files\n- list of files likely touched\n## Validation\n- shell commands to verify success\nOnly output the plan, no preamble.";

/// Skill id injected into the Stage 2 execute prompt. We deliberately
/// reuse the existing Hub-registered `superpowers-executing-plans`
/// capability when `skill:implement-plan` is not registered, so V2 works
/// out of the box in environments that have not seeded a dedicated
/// implement-plan capability yet.
pub(super) const IMPLEMENT_PLAN_SKILL: &str = "skill:implement-plan";
pub(super) const IMPLEMENT_PLAN_FALLBACK_SKILL: &str = "skill:superpowers-executing-plans";

/// Default Stage 1 wall-clock budget (seconds). Overridden by
/// `DISPATCH_V2_PLAN_TIMEOUT_SECS`.
pub(super) const DEFAULT_PLAN_TIMEOUT_SECS: u64 = 180;

/// Decide whether to take the V2 two-stage path.
///
/// V2 is opt-in. It activates when either:
///   * `DISPATCH_V2_ENABLED=true` (case-insensitive), or
///   * the caller explicitly sets `stage = "plan_execute"`.
///
/// `env` is passed in as a map rather than read from `std::env` so the
/// decision is unit-testable.
pub(super) fn v2_enabled(params: &TachiDispatchParams, env: &HashMap<String, String>) -> bool {
    if matches!(
        params
            .stage
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("plan_execute")
    ) {
        return true;
    }
    env.get("DISPATCH_V2_ENABLED")
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// Read `DISPATCH_V2_PLAN_REVIEW` from env. Defaults to false.
pub(super) fn plan_review_enabled(env: &HashMap<String, String>) -> bool {
    env.get("DISPATCH_V2_PLAN_REVIEW")
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// Read `DISPATCH_V2_PLAN_TIMEOUT_SECS` from env. Defaults to
/// `DEFAULT_PLAN_TIMEOUT_SECS`.
pub(super) fn plan_timeout_secs(env: &HashMap<String, String>) -> u64 {
    env.get("DISPATCH_V2_PLAN_TIMEOUT_SECS")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_PLAN_TIMEOUT_SECS)
}

/// Build the Stage 2 execute prompt by stitching the generated plan
/// together with the original task and an explicit implement-plan skill
/// reference. Kept pure so tests can assert the exact contract.
pub(super) fn build_execute_prompt(plan: &str, task: &str) -> String {
    format!(
        "You are executing a plan.\nSkill: implement-plan\n\nPlan:\n{}\n\nTask:\n{}\n\nExecute the plan step by step.",
        plan.trim(),
        task.trim()
    )
}

/// Parsed plan sections. Empty strings mean the section was missing.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ParsedPlan {
    pub goal: String,
    pub steps: String,
    pub files: String,
    pub validation: String,
}

impl ParsedPlan {
    /// All four sections present and non-empty.
    pub(super) fn is_complete(&self) -> bool {
        !self.goal.trim().is_empty()
            && !self.steps.trim().is_empty()
            && !self.files.trim().is_empty()
            && !self.validation.trim().is_empty()
    }
}

/// Parse a markdown plan into its four canonical sections.
///
/// Section headers are matched case-insensitively against the leading
/// word (e.g. `## Goal`, `## Steps (numbered)`, `## Files`,
/// `## Validation`). Content for a section is everything from the line
/// after the header up to the next `##` header or EOF.
pub(super) fn parse_plan(markdown: &str) -> ParsedPlan {
    let mut out = ParsedPlan::default();
    let mut current: Option<&str> = None;
    let mut buffer: Vec<&str> = Vec::new();

    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("## ") {
            // Flush previous section.
            if let Some(name) = current.take() {
                let body = buffer.join("\n").trim().to_string();
                assign_section(&mut out, name, body);
                buffer.clear();
            }
            let head = rest
                .split(|c: char| c == '(' || c.is_whitespace())
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            current = match head.as_str() {
                "goal" => Some("goal"),
                "steps" => Some("steps"),
                "files" => Some("files"),
                "validation" => Some("validation"),
                _ => None,
            };
        } else if current.is_some() {
            buffer.push(line);
        }
    }
    if let Some(name) = current.take() {
        let body = buffer.join("\n").trim().to_string();
        assign_section(&mut out, name, body);
    }
    out
}

fn assign_section(out: &mut ParsedPlan, name: &str, body: String) {
    match name {
        "goal" => out.goal = body,
        "steps" => out.steps = body,
        "files" => out.files = body,
        "validation" => out.validation = body,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(stage: Option<&str>) -> TachiDispatchParams {
        TachiDispatchParams {
            agent: "claude".to_string(),
            task: "do thing".to_string(),
            cwd: None,
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 600,
            permission_profile: None,
            allowed_tools: Vec::new(),
            max_turns: None,
            sandbox: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            command: Vec::new(),
            project: None,
            stage: stage.map(str::to_string),
        }
    }

    fn env_with(kv: &[(&str, &str)]) -> HashMap<String, String> {
        kv.iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn v2_disabled_by_default() {
        assert!(!v2_enabled(&params(None), &env_with(&[])));
        assert!(!v2_enabled(&params(Some("plan")), &env_with(&[])));
        assert!(!v2_enabled(&params(Some("execute")), &env_with(&[])));
    }

    #[test]
    fn v2_enabled_via_env() {
        assert!(v2_enabled(
            &params(None),
            &env_with(&[("DISPATCH_V2_ENABLED", "true")])
        ));
        assert!(v2_enabled(
            &params(None),
            &env_with(&[("DISPATCH_V2_ENABLED", "1")])
        ));
        assert!(v2_enabled(
            &params(None),
            &env_with(&[("DISPATCH_V2_ENABLED", "YES")])
        ));
        assert!(!v2_enabled(
            &params(None),
            &env_with(&[("DISPATCH_V2_ENABLED", "false")])
        ));
        assert!(!v2_enabled(
            &params(None),
            &env_with(&[("DISPATCH_V2_ENABLED", "0")])
        ));
    }

    #[test]
    fn v2_enabled_via_stage_param() {
        assert!(v2_enabled(&params(Some("plan_execute")), &env_with(&[])));
        // Stage param wins regardless of env.
        assert!(v2_enabled(
            &params(Some("plan_execute")),
            &env_with(&[("DISPATCH_V2_ENABLED", "false")])
        ));
    }

    #[test]
    fn plan_review_env_parsing() {
        assert!(!plan_review_enabled(&env_with(&[])));
        assert!(plan_review_enabled(&env_with(&[(
            "DISPATCH_V2_PLAN_REVIEW",
            "true"
        )])));
        assert!(!plan_review_enabled(&env_with(&[(
            "DISPATCH_V2_PLAN_REVIEW",
            "no"
        )])));
    }

    #[test]
    fn plan_timeout_env_parsing() {
        assert_eq!(plan_timeout_secs(&env_with(&[])), DEFAULT_PLAN_TIMEOUT_SECS);
        assert_eq!(
            plan_timeout_secs(&env_with(&[("DISPATCH_V2_PLAN_TIMEOUT_SECS", "42")])),
            42
        );
        // Garbage falls back to default.
        assert_eq!(
            plan_timeout_secs(&env_with(&[("DISPATCH_V2_PLAN_TIMEOUT_SECS", "nope")])),
            DEFAULT_PLAN_TIMEOUT_SECS
        );
    }

    #[test]
    fn parse_plan_extracts_all_sections() {
        let md = "## Goal\nDo the thing.\n\n## Steps (numbered)\n1. one\n2. two\n\n## Files\n- src/lib.rs\n\n## Validation\n- cargo test\n";
        let parsed = parse_plan(md);
        assert_eq!(parsed.goal, "Do the thing.");
        assert!(parsed.steps.contains("1. one"));
        assert!(parsed.steps.contains("2. two"));
        assert!(parsed.files.contains("src/lib.rs"));
        assert!(parsed.validation.contains("cargo test"));
        assert!(parsed.is_complete());
    }

    #[test]
    fn parse_plan_incomplete_when_section_missing() {
        let md = "## Goal\nx\n\n## Steps\n1. y\n";
        let parsed = parse_plan(md);
        assert!(!parsed.is_complete());
        assert!(parsed.files.is_empty());
        assert!(parsed.validation.is_empty());
    }

    #[test]
    fn parse_plan_handles_unknown_sections() {
        let md = "Preamble we ignore\n## Goal\nG\n## Notes\nignored\n## Steps\nS\n## Files\nF\n## Validation\nV\n";
        let parsed = parse_plan(md);
        assert_eq!(parsed.goal, "G");
        assert_eq!(parsed.steps, "S");
        assert_eq!(parsed.files, "F");
        assert_eq!(parsed.validation, "V");
    }

    #[test]
    fn build_execute_prompt_includes_plan_task_and_skill() {
        let out = build_execute_prompt("PLAN BODY", "TASK BODY");
        assert!(out.contains("Skill: implement-plan"));
        assert!(out.contains("PLAN BODY"));
        assert!(out.contains("TASK BODY"));
        assert!(out.contains("Execute the plan step by step."));
    }
}
