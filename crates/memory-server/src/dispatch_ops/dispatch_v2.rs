//! Dispatch V2 — Two-stage Plan -> Execute (Phase 6).
//!
//! This module layers a real LLM-generated plan in front of the existing
//! single-stage dispatch. It is **opt-in**: the default behaviour stays V1
//! (see `dispatch.rs`) until operators flip the env var or pass a
//! `stage="auto" | "plan_execute"`.
//!
//! Stage 1 (Plan):
//!   * builds a planning prompt = `PLAN_SYSTEM_PROMPT` + task body
//!   * invokes the shared `ClaudePool` (from Phase 1) — bounded concurrency,
//!     wall-clock timeout, audit dir under `~/.tachi/foundry-runs/`.
//!   * writes the LLM output to `<run_dir>/plan.md`.
//!   * appends a `plan_generated` event to `trajectory.jsonl`.
//!   * an empty / blank plan is treated as a hard error (no silent fallback
//!     to V1 single-stage; the dispatcher is supposed to fail loudly so the
//!     operator notices the planner regression).
//!
//! Stage 2 (Execute):
//!   * the original V1 execute path is reused — see `dispatch.rs`. V2 just
//!     enriches the prompt with the Stage-1 plan and the
//!     `skill:implement-plan` skill body (inline fallback below; the
//!     capability registry version, if present, is layered in by
//!     `prompt::assemble_prompt`).
//!
//! Review gate:
//!   * `DISPATCH_V2_PLAN_REVIEW=true` makes the dispatcher pause after
//!     Stage 1 — for non-interactive callers this means returning a
//!     `pending_review` response without executing. The MVP does NOT block
//!     the async path on a TTY confirm; review is auditable via plan.md
//!     and status.json.

use serde_json::Value;
use std::time::Instant;

/// System prompt prepended to the Stage-1 task body. Kept verbatim so the
/// LLM produces a deterministic, parseable plan layout.
pub(super) const PLAN_SYSTEM_PROMPT: &str = r#"You are a planning engine for Tachi dispatch.
Given a task, output a markdown plan with EXACTLY these top-level sections, in order:

## Goal
One sentence describing the desired outcome.

## Steps
Numbered list. Each step must be concrete and independently verifiable.

## Files
- bullet list of files (relative paths) likely to be created or modified.

## Validation
- shell commands that prove the change works (e.g. `cargo test -p memory-server`).

Output ONLY the plan markdown. No preamble, no postscript, no code-fence wrapping
the whole document."#;

/// Inline fallback for the implement-plan skill when the capability is not
/// registered in the running server. Keeps V2 functional on fresh boxes.
pub(super) const IMPLEMENT_PLAN_SKILL_FALLBACK: &str = r#"## Skill: implement-plan
Follow the attached plan step by step. Do not redesign it. After each step run
the relevant Validation command. Stop and report blockers instead of improvising
outside the plan. When done, call tachi_task(action="complete") with the dispatch_id."#;

/// Default wall-clock budget for Stage 1 (`claude_pool.call`). Independent of
/// the broader dispatch timeout so a slow planner can't burn the execute
/// budget.
pub(super) const DEFAULT_PLAN_TIMEOUT_SECS: u64 = 180;

/// Result of evaluating whether V2 should kick in for a given dispatch call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum V2Decision {
    Enabled,
    Disabled,
}

/// V2 is enabled when either:
///   * `DISPATCH_V2_ENABLED=true|1|on|yes` is exported, OR
///   * the per-call `stage` is one of `"auto"` / `"plan_execute"`.
///
/// Anything else (including the existing `"plan"` / `"execute"` single-stage
/// hints used by the V1 prompt assembler) leaves V1 untouched.
pub(super) fn v2_enabled(env_val: Option<&str>, stage: Option<&str>) -> V2Decision {
    let env_on = env_val
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    let stage_on = stage
        .map(|s| {
            matches!(
                s.trim().to_ascii_lowercase().as_str(),
                "auto" | "plan_execute"
            )
        })
        .unwrap_or(false);
    if env_on || stage_on {
        V2Decision::Enabled
    } else {
        V2Decision::Disabled
    }
}

/// Convenience reader that consults the live environment + stage param.
pub(super) fn v2_enabled_from_env(stage: Option<&str>) -> V2Decision {
    let env_val = std::env::var("DISPATCH_V2_ENABLED").ok();
    v2_enabled(env_val.as_deref(), stage)
}

/// Whether a non-interactive review gate should pause execution after the
/// plan is produced. Default false — the plan is still persisted so a human
/// can audit it after the fact.
pub(super) fn plan_review_required() -> bool {
    std::env::var("DISPATCH_V2_PLAN_REVIEW")
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

pub(super) fn plan_timeout_secs() -> u64 {
    std::env::var("DISPATCH_V2_PLAN_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_PLAN_TIMEOUT_SECS)
}

/// Outcome of Stage 1.
pub(super) struct PlanOutcome {
    pub plan_md: String,
    pub duration_ms: u64,
}

/// Run Stage 1. Returns the plan body and elapsed time, or a descriptive
/// error suitable for surfacing to the caller AND for writing to status.json.
pub(super) async fn run_plan_stage(
    server: &crate::MemoryServer,
    task: &str,
    label: &str,
) -> Result<PlanOutcome, String> {
    if task.trim().is_empty() {
        return Err("dispatch v2: task is empty; cannot plan".to_string());
    }

    let composed = format!("{}\n\n# Task\n{}", PLAN_SYSTEM_PROMPT, task.trim());

    let started = Instant::now();
    let outcome = server
        .claude_pool
        .call(label, &composed)
        .await
        .map_err(|e| format!("dispatch v2 stage1 (plan) failed: {e}"))?;
    let duration_ms = started.elapsed().as_millis() as u64;

    let plan_md = outcome.text.trim().to_string();
    if plan_md.is_empty() {
        return Err("dispatch v2 stage1 (plan) returned empty plan.md".to_string());
    }

    Ok(PlanOutcome {
        plan_md,
        duration_ms,
    })
}

/// Append a single JSON event line to `<run_dir>/trajectory.jsonl`. Best-effort.
pub(super) fn append_trajectory_event(trajectory_path: &std::path::Path, event: Value) {
    use std::io::Write;
    let Ok(line) = serde_json::to_string(&event) else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(trajectory_path)
    {
        let _ = writeln!(f, "{}", line);
    }
    if let Some(run_dir) = trajectory_path.parent() {
        let progress_path = run_dir.join("progress.jsonl");
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(progress_path)
        {
            let _ = writeln!(f, "{}", line);
        }
    }
}

/// Write (or overwrite) `<run_dir>/status.json` with the V2 audit fields.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_status_json(
    run_dir: &std::path::Path,
    dispatch_id: &str,
    v2: bool,
    plan_generated_at: Option<&str>,
    executed_at: Option<&str>,
    plan_review_status: &str,
    exit_code: Option<i32>,
    duration_ms_plan: Option<u64>,
    duration_ms_execute: Option<u64>,
    total_duration_ms: Option<u64>,
    extra: Option<Value>,
) {
    let mut obj = serde_json::Map::new();
    obj.insert("dispatch_id".into(), Value::String(dispatch_id.to_string()));
    obj.insert("v2".into(), Value::Bool(v2));
    if let Some(s) = plan_generated_at {
        obj.insert("plan_generated_at".into(), Value::String(s.to_string()));
    }
    if let Some(s) = executed_at {
        obj.insert("executed_at".into(), Value::String(s.to_string()));
    }
    obj.insert(
        "plan_review_status".into(),
        Value::String(plan_review_status.to_string()),
    );
    obj.insert(
        "exit_code".into(),
        match exit_code {
            Some(c) => Value::Number(c.into()),
            None => Value::Null,
        },
    );
    if let Some(d) = duration_ms_plan {
        obj.insert("duration_ms_plan".into(), Value::Number(d.into()));
    }
    if let Some(d) = duration_ms_execute {
        obj.insert("duration_ms_execute".into(), Value::Number(d.into()));
    }
    if let Some(d) = total_duration_ms {
        obj.insert("total_duration_ms".into(), Value::Number(d.into()));
    }
    if let Some(Value::Object(map)) = extra {
        for (k, v) in map {
            obj.insert(k, v);
        }
    }

    let path = run_dir.join("status.json");
    let body =
        serde_json::to_string_pretty(&Value::Object(obj)).unwrap_or_else(|_| "{}".to_string());
    if let Err(e) = crate::utils::write_owner_only_file_atomic(&path, body.as_bytes()) {
        eprintln!("[dispatch-v2] failed to write {}: {e}", path.display());
    }
}

/// Build the Stage-2 execution prompt — wraps the original task with the plan
/// body and the implement-plan skill instructions. The result is fed into the
/// same agent (claude / codex / custom) that V1 would have used.
pub(super) fn build_execute_prompt(
    plan_md: &str,
    original_task: &str,
    base_prompt: &str,
) -> String {
    let mut out = String::with_capacity(base_prompt.len() + plan_md.len() + 512);
    out.push_str("You are executing a pre-approved plan from Tachi Dispatch V2.\n\n");
    out.push_str(IMPLEMENT_PLAN_SKILL_FALLBACK);
    out.push_str("\n\n## Plan (from Stage 1)\n");
    out.push_str(plan_md.trim());
    out.push_str("\n\n## Original Task\n");
    out.push_str(original_task.trim());
    out.push_str("\n\n## Stage-1 Assembled Context\n");
    out.push_str(base_prompt.trim());
    out.push_str(
        "\n\nExecute the plan step by step. Do not redesign it. \
         When done, call `tachi_task(action=\"complete\")` with the dispatch_id.\n",
    );
    out
}

/// Best-effort parser for the four canonical sections of a plan. Used by
/// tests and by the kanban summary so we can surface plan health without
/// dumping the whole markdown blob.
pub(super) fn parse_plan_sections(plan_md: &str) -> PlanSections {
    let mut goal = String::new();
    let mut steps = String::new();
    let mut files = String::new();
    let mut validation = String::new();

    #[derive(Clone, Copy)]
    enum Section {
        Goal,
        Steps,
        Files,
        Validation,
    }

    let mut current: Option<Section> = None;
    for line in plan_md.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("## ") {
            let header = rest.trim().to_ascii_lowercase();
            current = match header.as_str() {
                "goal" => Some(Section::Goal),
                "steps" => Some(Section::Steps),
                "files" => Some(Section::Files),
                "validation" => Some(Section::Validation),
                _ => None,
            };
            continue;
        }
        match current {
            Some(Section::Goal) => {
                goal.push_str(line);
                goal.push('\n');
            }
            Some(Section::Steps) => {
                steps.push_str(line);
                steps.push('\n');
            }
            Some(Section::Files) => {
                files.push_str(line);
                files.push('\n');
            }
            Some(Section::Validation) => {
                validation.push_str(line);
                validation.push('\n');
            }
            None => {}
        }
    }

    PlanSections {
        goal: goal.trim().to_string(),
        steps: steps.trim().to_string(),
        files: files.trim().to_string(),
        validation: validation.trim().to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlanSections {
    pub goal: String,
    pub steps: String,
    pub files: String,
    pub validation: String,
}

impl PlanSections {
    pub fn is_complete(&self) -> bool {
        !self.goal.is_empty()
            && !self.steps.is_empty()
            && !self.files.is_empty()
            && !self.validation.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_enabled_default_off() {
        assert_eq!(v2_enabled(None, None), V2Decision::Disabled);
        assert_eq!(
            v2_enabled(Some("false"), Some("plan")),
            V2Decision::Disabled
        );
        assert_eq!(v2_enabled(Some(""), Some("")), V2Decision::Disabled);
    }

    #[test]
    fn v2_enabled_by_env() {
        assert_eq!(v2_enabled(Some("true"), None), V2Decision::Enabled);
        assert_eq!(v2_enabled(Some("1"), None), V2Decision::Enabled);
        assert_eq!(v2_enabled(Some("YES"), Some("plan")), V2Decision::Enabled);
        assert_eq!(v2_enabled(Some("on"), Some("execute")), V2Decision::Enabled);
    }

    #[test]
    fn v2_enabled_by_stage() {
        assert_eq!(v2_enabled(None, Some("auto")), V2Decision::Enabled);
        assert_eq!(v2_enabled(None, Some("AUTO")), V2Decision::Enabled);
        assert_eq!(v2_enabled(None, Some("plan_execute")), V2Decision::Enabled);
        // Legacy single-stage hints stay V1.
        assert_eq!(v2_enabled(None, Some("plan")), V2Decision::Disabled);
        assert_eq!(v2_enabled(None, Some("execute")), V2Decision::Disabled);
    }

    #[test]
    fn parse_plan_extracts_all_sections() {
        let md = r#"## Goal
Add a feature.

## Steps
1. Edit foo.rs
2. Run tests

## Files
- foo.rs
- bar.rs

## Validation
- cargo test
- cargo clippy
"#;
        let sections = parse_plan_sections(md);
        assert_eq!(sections.goal, "Add a feature.");
        assert!(sections.steps.contains("Edit foo.rs"));
        assert!(sections.files.contains("foo.rs"));
        assert!(sections.validation.contains("cargo test"));
        assert!(sections.is_complete());
    }

    #[test]
    fn parse_plan_marks_incomplete_when_section_missing() {
        let md = r#"## Goal
Only goal here.
"#;
        let sections = parse_plan_sections(md);
        assert!(!sections.is_complete());
        assert_eq!(sections.goal, "Only goal here.");
        assert!(sections.steps.is_empty());
    }

    #[test]
    fn build_execute_prompt_embeds_plan_and_task() {
        let prompt = build_execute_prompt("## Goal\nX", "do X", "ctx");
        assert!(prompt.contains("## Plan (from Stage 1)"));
        assert!(prompt.contains("## Goal\nX"));
        assert!(prompt.contains("do X"));
        assert!(prompt.contains("ctx"));
        assert!(prompt.contains("implement-plan"));
    }
}
