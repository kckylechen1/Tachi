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
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
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
- shell commands that prove the change works (e.g. `cargo test -p tachi-server`).

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

/// Generous ceiling for the Stage-1 plan-stage provider call (#1214 BUG#3) —
/// a markdown plan (Goal/Steps/Files/Validation) can run longer than the
/// terse JSON verdicts the other provider-rollout consumers produce.
const PLAN_PROVIDER_MAX_TOKENS: u32 = 2000;

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

#[cfg(test)]
#[derive(Clone)]
pub(super) enum PlanStageTestOverride {
    Success {
        plan_md: String,
        duration_ms: u64,
    },
    SuccessWithAssertion {
        plan_md: String,
        duration_ms: u64,
        assert_before_return: PlanStageTestAssertion,
    },
    Failure(String),
    Pending,
}

#[cfg(test)]
pub(super) type PlanStageTestAssertion =
    for<'a> fn(
        &'a crate::MemoryServer,
        &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>;

#[cfg(test)]
fn plan_stage_test_override() -> &'static std::sync::Mutex<Option<PlanStageTestOverride>> {
    static OVERRIDE: std::sync::OnceLock<std::sync::Mutex<Option<PlanStageTestOverride>>> =
        std::sync::OnceLock::new();
    OVERRIDE.get_or_init(|| std::sync::Mutex::new(None))
}

/// One-shot planner outcome injection for the dispatch lifecycle discriminator.
/// Compiled out of production: provider execution remains the only runtime path.
#[cfg(test)]
pub(super) fn set_plan_stage_test_override(value: Option<PlanStageTestOverride>) {
    *plan_stage_test_override()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = value;
}

/// Run Stage 1. Returns the plan body and elapsed time, or a descriptive
/// error suitable for surfacing to the caller AND for writing to status.json.
pub(super) async fn run_plan_stage(
    server: &crate::MemoryServer,
    task: &str,
    label: &str,
) -> Result<PlanOutcome, String> {
    #[cfg(test)]
    let override_result = {
        plan_stage_test_override()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    };
    #[cfg(test)]
    if let Some(override_result) = override_result {
        return match override_result {
            PlanStageTestOverride::Success {
                plan_md,
                duration_ms,
            } => Ok(PlanOutcome {
                plan_md,
                duration_ms,
            }),
            PlanStageTestOverride::SuccessWithAssertion {
                plan_md,
                duration_ms,
                assert_before_return,
            } => {
                assert_before_return(server, label).await;
                Ok(PlanOutcome {
                    plan_md,
                    duration_ms,
                })
            }
            PlanStageTestOverride::Failure(error) => Err(error),
            PlanStageTestOverride::Pending => std::future::pending().await,
        };
    }
    if task.trim().is_empty() {
        return Err("dispatch v2: task is empty; cannot plan".to_string());
    }

    let composed = format!("{}\n\n# Task\n{}", PLAN_SYSTEM_PROMPT, task.trim());

    let started = Instant::now();
    let outcome = call_plan_llm(server, label, &composed, task.trim())
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

/// Run the Stage-1 plan-stage LLM call, either via the CLI pool (pre-#1087
/// default) or — when `TACHI_CLAUDE_POOL_PROVIDER_FIRST` is set — via the
/// provider executor first, with the CLI pool as a fallback for the rollout
/// cycle. Either way the run-directory artifact contract
/// (`prompt.md`/`result.md`/`status.json`) is preserved, since the path
/// goes through `LlmCallRecorder::record_call` (#1214 BUG#3 lineage: this
/// call site previously had no flag gate at all — the fifth live pool
/// consumer the flag-coverage audit missed). #1261 step 2/3 removed the
/// CLI fallback branch; step 3/3 renamed the recorder (formerly
/// `ClaudePool::call_via_provider`) to its executor-agnostic name.
async fn call_plan_llm(
    server: &crate::MemoryServer,
    label: &str,
    composed_prompt: &str,
    task: &str,
) -> Result<tachi_llm::llm_recorder::RecordedCallOutcome, String> {
    let llm = server.llm.clone();
    let task_owned = task.to_string();
    server
        .llm_recorder
        .record_call(label, composed_prompt, move || async move {
            llm.call_reasoning_llm_provider_only(
                PLAN_SYSTEM_PROMPT,
                &task_owned,
                None,
                0.2,
                PLAN_PROVIDER_MAX_TOKENS,
            )
            .await
        })
        .await
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
pub(crate) fn stamp_route_decision_id(
    run_dir: &std::path::Path,
    route_decision_id: &str,
) -> Result<(), String> {
    let lock = status_json_lock_for(run_dir);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = run_dir.join("status.json");
    let Some(Value::Object(mut status)) = crate::task_lifecycle::read_json_file(&path)
        .map_err(|error| format!("read {}: {error}", path.display()))?
    else {
        return Err(format!("missing or malformed {}", path.display()));
    };
    status.insert(
        "route_decision_id".to_string(),
        Value::String(route_decision_id.to_string()),
    );
    crate::managed_run_control::advance_status_revision(&mut status)?;
    let body = serde_json::to_vec_pretty(&Value::Object(status))
        .map_err(|error| format!("serialize {}: {error}", path.display()))?;
    crate::utils::write_owner_only_file_atomic(&path, &body)
        .map_err(|error| format!("write {}: {error}", path.display()))
}

/// Return the shared, weakly retained mutex for one canonical run receipt.
/// Every `status.json` read-modify-write must take this lock, including ACP
/// identity acknowledgement, lifecycle terminalization, and route evidence.
/// Weak retention avoids keeping a lock entry for every historical run.
pub(crate) fn status_json_lock_for(run_dir: &std::path::Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<std::path::PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    // Run directories exist before any status writer can legitimately update
    // them, so their canonical path gives every relative, `..`, or symlink
    // spelling the same receipt mutex. Keep a non-panicking absolute fallback
    // for defensive callers that are still assembling a new run directory.
    let lock_key = run_dir.canonicalize().unwrap_or_else(|_| {
        if run_dir.is_absolute() {
            run_dir.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|current_dir| current_dir.join(run_dir))
                .unwrap_or_else(|_| run_dir.to_path_buf())
        }
    });
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(&lock_key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(lock_key, Arc::downgrade(&lock));
    lock
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn write_status_json(
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
    let lock = status_json_lock_for(run_dir);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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
    // Host-profile routing is fixed at dispatch acceptance. Later lifecycle
    // writers (preflight failure, watchdog, completion) describe a changing
    // state but must not erase that admission decision from the receipt.
    if let Ok(previous) = std::fs::read_to_string(&path) {
        if let Ok(Value::Object(previous)) = serde_json::from_str::<Value>(&previous) {
            let proposed_terminal = obj
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(|state| {
                    matches!(
                        state,
                        "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED"
                    )
                });
            if crate::managed_run_control::cancellation_blocks_terminal_writer(&previous)
                && proposed_terminal
            {
                // `cancellation_requested` is the linearization point. A
                // late watchdog/completion/timeout write must reconcile via
                // the cancellation owner, not overwrite its pending receipt.
                return;
            }
            for key in [
                "host_profile",
                "execution_level",
                "identity_receipt",
                "resolved_completion",
                "completion_recovery",
                "project",
                // tachi#1675 PR1 Seam B: `route_decision_id` is stamped ONCE,
                // as a best-effort convenience copy of the `route_decisions`
                // row's id, by `staffing_ops::staff_start` shortly after
                // acceptance — never by this function itself. This
                // preserve-if-absent list is what carries it forward through
                // every later status.json rewrite (preflight failure,
                // watchdog, completion). NO writer downstream of acceptance
                // may ever explicitly `extra`-emit this key (including as
                // `Value::Null`): this list only fills a key that is ABSENT
                // from the new `obj`, so an explicit `null` would still
                // overwrite (erase) the preserved value instead of being
                // skipped — design D2 / codex finding 2.
                "route_decision_id",
                "status_revision",
                "execution_classification",
                "lifecycle_owner",
                "cancellation",
            ] {
                if !obj.contains_key(key) {
                    if let Some(value) = previous.get(key) {
                        obj.insert(key.to_string(), value.clone());
                    }
                }
            }
            // A bounded local lock exhaustion has durable eval evidence but
            // no canonical dispatch outcome yet. Keep the run observably
            // non-terminal until a later completion call reconciles that
            // outcome and clears this marker itself.
            if previous.get("completion_recovery").is_some()
                && !obj.contains_key("resolved_completion")
            {
                let non_terminal_state = previous
                    .get("state")
                    .and_then(Value::as_str)
                    .filter(|state| {
                        matches!(
                            *state,
                            "TASK_STATE_PENDING"
                                | "TASK_STATE_WORKING"
                                | "TASK_STATE_RUNNING"
                                | "TASK_STATE_INPUT_REQUIRED"
                        )
                    })
                    .unwrap_or("TASK_STATE_WORKING");
                obj.insert(
                    "state".to_string(),
                    Value::String(non_terminal_state.to_string()),
                );
                obj.insert("closure_kind".to_string(), Value::Null);
            }
        }
    }
    if let Err(error) = crate::managed_run_control::advance_status_revision(&mut obj) {
        eprintln!("[dispatch-v2] refusing status write: {error}");
        return;
    }
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
    fn terminal_status_rewrite_preserves_resolved_completion_receipt() {
        let temp = tempfile::tempdir().expect("temporary run directory");
        std::fs::write(
            temp.path().join("status.json"),
            serde_json::json!({
                "resolved_completion": {
                    "state": "TASK_STATE_INPUT_REQUIRED",
                    "closure_kind": "partial",
                }
            })
            .to_string(),
        )
        .expect("write completion receipt");

        write_status_json(
            temp.path(),
            "20260719T000002Z-preserve-completion-receipt",
            false,
            None,
            None,
            "n/a",
            Some(0),
            None,
            None,
            None,
            Some(serde_json::json!({ "state": "TASK_STATE_INPUT_REQUIRED" })),
        );

        let status: Value = serde_json::from_slice(
            &std::fs::read(temp.path().join("status.json")).expect("read rewritten status"),
        )
        .expect("rewritten status JSON");
        assert_eq!(
            status["resolved_completion"]["closure_kind"],
            serde_json::json!("partial"),
            "terminal rewrite must not erase the handler's authoritative receipt"
        );
    }

    #[test]
    fn base_and_route_status_writers_advance_the_shared_revision() {
        let temp = tempfile::tempdir().expect("temporary run directory");
        std::fs::write(
            temp.path().join("status.json"),
            serde_json::json!({ "status_revision": 7 }).to_string(),
        )
        .expect("seed status revision");

        write_status_json(
            temp.path(),
            "20260823T010105Z-revision-writers",
            false,
            None,
            None,
            "n/a",
            Some(0),
            None,
            None,
            None,
            Some(serde_json::json!({ "state": "TASK_STATE_WORKING" })),
        );
        let after_base: Value = serde_json::from_slice(
            &std::fs::read(temp.path().join("status.json")).expect("read base status"),
        )
        .expect("parse base status");
        assert_eq!(after_base["status_revision"], 8);

        stamp_route_decision_id(temp.path(), "route-revision-writers")
            .expect("stamp route decision");
        let after_route: Value = serde_json::from_slice(
            &std::fs::read(temp.path().join("status.json")).expect("read route status"),
        )
        .expect("parse route status");
        assert_eq!(after_route["status_revision"], 9);
    }

    /// A completion whose canonical outcome write exhausted its bounded local
    /// lock retry is still pending recovery. An exit-zero finalizer must not
    /// replace that barrier with a terminal success or erase the receipt the
    /// next `tachi_complete` needs to reconcile safely.
    #[test]
    fn terminal_status_rewrite_preserves_pending_completion_recovery_barrier() {
        let temp = tempfile::tempdir().expect("temporary run directory");
        let recovery = serde_json::json!({
            "status": "pending_canonical_outcome",
            "state": "TASK_STATE_COMPLETED",
            "eval_ledger_id": "eval-pending-recovery",
            "reviewed": true,
            "dispatch_outcome": {"recorded": false, "error": "database is locked"},
        });
        std::fs::write(
            temp.path().join("status.json"),
            serde_json::json!({
                "state": "TASK_STATE_WORKING",
                "completion_recovery": recovery,
            })
            .to_string(),
        )
        .expect("write pending completion recovery receipt");

        write_status_json(
            temp.path(),
            "20260726T000001Z-pending-completion-recovery",
            false,
            None,
            None,
            "n/a",
            Some(0),
            None,
            None,
            None,
            Some(serde_json::json!({ "state": "TASK_STATE_COMPLETED" })),
        );

        let status: Value = serde_json::from_slice(
            &std::fs::read(temp.path().join("status.json")).expect("read rewritten status"),
        )
        .expect("rewritten status JSON");
        assert_eq!(
            status["completion_recovery"], recovery,
            "a final status rewrite must retain the pending canonical-outcome barrier"
        );
        assert_eq!(
            status["state"],
            serde_json::json!("TASK_STATE_WORKING"),
            "exit zero must not terminalize a run whose canonical outcome is still pending"
        );
    }

    // ── #1261 step 2/3: CLI fallback removed from call_plan_llm ──────────
    //
    // Before #1261, this test proved the #1214 BUG#3 flag gate existed by
    // asserting the flag-off path hit the CLI binary resolver and surfaced
    // its "existing executable" error. With the CLI fallback removed, the
    // invariant flips: `call_plan_llm` must NEVER touch the CLI binary
    // resolver, regardless of CLAUDE_BIN / TACHI_CLAUDE_POOL_PROVIDER_FIRST.
    // Pointing CLAUDE_BIN at a nonexistent path is now a no-op for this
    // code path — the call goes straight to the provider executor (which
    // fails because no real provider is configured in the test harness,
    // but crucially NOT with the CLI-binary-resolution error the old test
    // required). This is the discriminating guard against a CLI-fallback
    // regression: if someone re-adds the `claude_pool.call()` branch, the
    // nonexistent CLAUDE_BIN would surface "existing executable" again
    // and this test would fail.
    #[test]
    fn call_plan_llm_never_reaches_cli_binary_resolver() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let prev_rollout = std::env::var("TACHI_CLAUDE_POOL_PROVIDER_FIRST").ok();
        // Explicitly set the legacy opt-OUT value: if any CLI path still
        // existed, this is the flag state that would have routed to it.
        std::env::set_var("TACHI_CLAUDE_POOL_PROVIDER_FIRST", "0");
        let prev_bin = std::env::var("CLAUDE_BIN").ok();
        std::env::set_var(
            "CLAUDE_BIN",
            "/nonexistent/__tachi_test_planstage_no_cli__/claude",
        );

        let temp = tempfile::tempdir().expect("temp dispatch v2 cli-removal db");
        let server = crate::MemoryServer::new(
            temp.path().join("global.db"),
            Some(temp.path().join("project.db")),
        )
        .expect("server");

        let result = tokio::runtime::Runtime::new()
            .expect("tokio runtime")
            .block_on(call_plan_llm(&server, "plan", "composed prompt", "task"));

        // The call no longer reaches the CLI binary resolver: regardless
        // of whether the provider path succeeds or fails in the test
        // harness, it must NOT surface the CLI-binary-resolution error
        // text the old behavior produced. That error string ("existing
        // executable") is unique to `resolve_claude_binary` — its absence
        // proves the CLI path is unreachable from this call site.
        let cli_resolver_unreachable = match &result {
            Err(err) => !err.contains("existing executable"),
            Ok(_) => true,
        };
        assert!(
            cli_resolver_unreachable,
            "call_plan_llm must never reach the CLI binary resolver after #1261 step 2; \
             got: {result:?}"
        );

        match prev_rollout {
            Some(v) => std::env::set_var("TACHI_CLAUDE_POOL_PROVIDER_FIRST", v),
            None => std::env::remove_var("TACHI_CLAUDE_POOL_PROVIDER_FIRST"),
        }
        match prev_bin {
            Some(v) => std::env::set_var("CLAUDE_BIN", v),
            None => std::env::remove_var("CLAUDE_BIN"),
        }
    }

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
