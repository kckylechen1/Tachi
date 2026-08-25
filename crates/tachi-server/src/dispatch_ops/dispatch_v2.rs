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

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum StatusJsonLockKey {
    #[cfg(unix)]
    UnixDirectory {
        device: u64,
        inode: u64,
    },
    Path(std::path::PathBuf),
}

#[cfg(test)]
fn managed_terminal_status_write_failures(
) -> &'static Mutex<std::collections::HashSet<std::path::PathBuf>> {
    static FAILURES: OnceLock<Mutex<std::collections::HashSet<std::path::PathBuf>>> =
        OnceLock::new();
    FAILURES.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

#[cfg(test)]
pub(crate) struct ManagedTerminalStatusWriteFailureGuard {
    status_path: std::path::PathBuf,
}

#[cfg(test)]
impl Drop for ManagedTerminalStatusWriteFailureGuard {
    fn drop(&mut self) {
        managed_terminal_status_write_failures()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.status_path);
    }
}

#[cfg(test)]
pub(crate) fn fail_next_managed_terminal_status_write(
    run_dir: &std::path::Path,
) -> ManagedTerminalStatusWriteFailureGuard {
    let status_path = run_dir.join("status.json");
    let inserted = managed_terminal_status_write_failures()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(status_path.clone());
    assert!(inserted, "one managed terminal write fault per status path");
    ManagedTerminalStatusWriteFailureGuard { status_path }
}

#[cfg(test)]
fn take_managed_terminal_status_write_failure(status_path: &std::path::Path) -> bool {
    managed_terminal_status_write_failures()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(status_path)
}

/// The managed terminal writer is the sole durable owner of cancellation
/// classification. If its first atomic replacement fails, preserve that
/// ownership under the same receipt mutex with an explicitly unavailable
/// FAILED receipt before volatile dispatch ownership is released.
fn persist_managed_terminal_status_failure(
    target: &StatusJsonTarget,
    status: &mut serde_json::Map<String, Value>,
) -> Result<(), String> {
    status.insert(
        "state".to_string(),
        Value::String("TASK_STATE_FAILED".to_string()),
    );
    {
        let cancellation = status
            .entry("cancellation".to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        let cancellation = cancellation
            .as_object_mut()
            .ok_or_else(|| "managed cancellation receipt is not an object".to_string())?;
        cancellation.insert(
            "receipt".to_string(),
            Value::String("cancellation_unavailable".to_string()),
        );
        cancellation.insert(
            "reason".to_string(),
            Value::String("persist_failed".to_string()),
        );
        cancellation.insert(
            "state".to_string(),
            Value::String("TASK_STATE_FAILED".to_string()),
        );
        cancellation.insert("termination_proof".to_string(), Value::Null);
    }
    crate::managed_run_control::advance_status_revision(status)?;
    let revision = status
        .get("status_revision")
        .cloned()
        .unwrap_or(Value::Null);
    status
        .get_mut("cancellation")
        .and_then(Value::as_object_mut)
        .expect("managed cancellation receipt was just initialized")
        .insert("observed_status_revision".to_string(), revision);
    let body = serde_json::to_vec_pretty(&Value::Object(status.clone()))
        .map_err(|error| format!("serialize {}: {error}", target.status_path().display()))?;
    target
        .write_atomic(&body)
        .map_err(|error| format!("write {}: {error}", target.status_path().display()))
}

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
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC);
        if let Ok(directory) = options.open(run_dir) {
            if let Ok(metadata) = directory.metadata() {
                return status_json_lock_for_identity(metadata.dev(), metadata.ino());
            }
        }
    }
    // Keep a non-panicking absolute fallback for defensive callers that are
    // still assembling a new run directory or whose directory disappeared.
    let path = run_dir.canonicalize().unwrap_or_else(|_| {
        if run_dir.is_absolute() {
            run_dir.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|current_dir| current_dir.join(run_dir))
                .unwrap_or_else(|_| run_dir.to_path_buf())
        }
    });
    status_json_lock_for_key(StatusJsonLockKey::Path(path))
}

#[cfg(unix)]
pub(crate) fn status_json_lock_for_identity(device: u64, inode: u64) -> Arc<Mutex<()>> {
    status_json_lock_for_key(StatusJsonLockKey::UnixDirectory { device, inode })
}

fn status_json_lock_for_key(lock_key: StatusJsonLockKey) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<StatusJsonLockKey, Weak<Mutex<()>>>>> = OnceLock::new();
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

enum StatusJsonTarget {
    Path(std::path::PathBuf),
    #[cfg(unix)]
    Anchored(crate::managed_run_control::AnchoredRunStatus),
}

pub(crate) enum ManagedTerminalStatusAnchor {
    Missing,
    #[cfg(unix)]
    Anchored(crate::managed_run_control::AnchoredRunStatus),
}

impl StatusJsonTarget {
    fn for_run(
        run_dir: &std::path::Path,
        dispatch_id: &str,
        managed_finalization: bool,
        managed_anchor: ManagedTerminalStatusAnchor,
    ) -> Result<Self, &'static str> {
        #[cfg(unix)]
        {
            match managed_anchor {
                ManagedTerminalStatusAnchor::Anchored(anchor) => Ok(Self::Anchored(anchor)),
                ManagedTerminalStatusAnchor::Missing if managed_finalization => {
                    tracing::warn!(
                        dispatch_id,
                        run_dir = %run_dir.display(),
                        "managed terminal writer refused finalization without the accepted run anchor"
                    );
                    Err("managed_terminal_status_anchor_missing")
                }
                ManagedTerminalStatusAnchor::Missing => Ok(Self::Path(run_dir.join("status.json"))),
            }
        }
        #[cfg(not(unix))]
        {
            if managed_finalization {
                let _ = dispatch_id;
                let _ = managed_anchor;
                Err("unsupported_platform")
            } else {
                Ok(Self::Path(run_dir.join("status.json")))
            }
        }
    }

    fn lock(&self) -> Arc<Mutex<()>> {
        match self {
            Self::Path(path) => status_json_lock_for(
                path.parent()
                    .expect("status.json path is always constructed beneath a run directory"),
            ),
            #[cfg(unix)]
            Self::Anchored(status) => status.lock(),
        }
    }

    fn status_path(&self) -> std::path::PathBuf {
        match self {
            Self::Path(path) => path.clone(),
            #[cfg(unix)]
            Self::Anchored(status) => status.status_path(),
        }
    }

    fn read_json(&self) -> Result<Option<Value>, String> {
        match self {
            Self::Path(path) => crate::task_lifecycle::read_json_file(path),
            #[cfg(unix)]
            Self::Anchored(status) => status.read_json(),
        }
    }

    fn write_atomic(&self, bytes: &[u8]) -> Result<(), String> {
        match self {
            Self::Path(path) => crate::utils::write_owner_only_file_atomic(path, bytes),
            #[cfg(unix)]
            Self::Anchored(status) => status.write_atomic(bytes),
        }
    }
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
) -> Option<crate::managed_run_control::CancelCompletion> {
    write_status_json_inner(
        run_dir,
        dispatch_id,
        v2,
        plan_generated_at,
        executed_at,
        plan_review_status,
        exit_code,
        duration_ms_plan,
        duration_ms_execute,
        total_duration_ms,
        extra,
        ManagedTerminalStatusAnchor::Missing,
        |object| crate::managed_run_control::advance_status_revision(object),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn write_status_json_for_terminal(
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
    managed_anchor: ManagedTerminalStatusAnchor,
) -> Option<crate::managed_run_control::CancelCompletion> {
    write_status_json_inner(
        run_dir,
        dispatch_id,
        v2,
        plan_generated_at,
        executed_at,
        plan_review_status,
        exit_code,
        duration_ms_plan,
        duration_ms_execute,
        total_duration_ms,
        extra,
        managed_anchor,
        |object| crate::managed_run_control::advance_status_revision(object),
    )
}

#[cfg(all(test, unix))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_status_json_with_managed_anchor(
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
    managed_anchor: &crate::managed_run_control::AnchoredRunStatus,
) -> Option<crate::managed_run_control::CancelCompletion> {
    write_status_json_inner(
        run_dir,
        dispatch_id,
        v2,
        plan_generated_at,
        executed_at,
        plan_review_status,
        exit_code,
        duration_ms_plan,
        duration_ms_execute,
        total_duration_ms,
        extra,
        ManagedTerminalStatusAnchor::Anchored(managed_anchor.clone()),
        |object| crate::managed_run_control::advance_status_revision(object),
    )
}

#[allow(clippy::collapsible_match, clippy::too_many_arguments)]
fn write_status_json_inner(
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
    managed_anchor: ManagedTerminalStatusAnchor,
    advance_revision: fn(&mut serde_json::Map<String, Value>) -> Result<u64, String>,
) -> Option<crate::managed_run_control::CancelCompletion> {
    let managed_finalization_requested = extra
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|extra| extra.get("managed_cancellation_finalization"))
        .is_some_and(|finalization| !finalization.is_null());
    let target = match StatusJsonTarget::for_run(
        run_dir,
        dispatch_id,
        managed_finalization_requested,
        managed_anchor,
    ) {
        Ok(target) => target,
        Err(reason) => {
            return Some(crate::managed_run_control::CancelCompletion::Unavailable(
                reason,
            ));
        }
    };
    let lock = target.lock();
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
    let managed_finalization = obj
        .remove("managed_cancellation_finalization")
        .filter(|finalization| !finalization.is_null());
    debug_assert_eq!(
        managed_finalization.is_some(),
        managed_finalization_requested
    );
    let mut managed_terminal = None;

    let path = target.status_path();
    let previous_status = target.read_json().ok().flatten();
    // Managed cancellation may acknowledge only a committed canonical
    // receipt.  Ordinary terminal writes retain their historical best-effort
    // compatibility, but this path must refuse a missing/corrupt prior
    // receipt rather than manufacturing CANCELED in a fresh object.
    if managed_finalization.is_some() && !matches!(&previous_status, Some(Value::Object(_))) {
        return Some(crate::managed_run_control::CancelCompletion::Unavailable(
            "managed_terminal_status_unreadable",
        ));
    }
    // Host-profile routing is fixed at dispatch acceptance. Later lifecycle
    // writers (preflight failure, watchdog, completion) describe a changing
    // state but must not erase that admission decision from the receipt.
    if let Some(previous_status) = previous_status {
        if let Value::Object(mut previous) = previous_status {
            if managed_finalization.is_some()
                && previous.get("dispatch_id").and_then(Value::as_str) != Some(dispatch_id)
            {
                return Some(crate::managed_run_control::CancelCompletion::Unavailable(
                    "managed_terminal_dispatch_identity_mismatch",
                ));
            }
            let proposed_terminal = obj
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(|state| {
                    matches!(
                        state,
                        "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED"
                    )
                });
            let completion_admitted = previous
                .get("completion_recovery")
                .and_then(Value::as_object)
                .and_then(|recovery| recovery.get("status"))
                .and_then(Value::as_str)
                == Some("completion_admitted");
            if proposed_terminal && managed_finalization.is_none() {
                let _ = crate::managed_run_control::reconcile_pending_cancellation_unavailable(
                    &mut previous,
                    "completion_or_timeout_winner",
                );
            }
            if managed_finalization.is_none()
                && crate::managed_run_control::cancellation_blocks_terminal_writer(&previous)
                && proposed_terminal
            {
                return None;
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
                if key == "completion_recovery" && proposed_terminal && completion_admitted {
                    // A terminal background owner supersedes a short-lived
                    // handler admission fence. Keeping it would make the
                    // later handler rollback erase the only durable state
                    // after this writer releases registry/slot ownership.
                    continue;
                }
                if !obj.contains_key(key) {
                    if let Some(value) = previous.get(key) {
                        obj.insert(key.to_string(), value.clone());
                    }
                }
            }
            if proposed_terminal && managed_finalization.is_none() {
                let _ = crate::managed_run_control::reconcile_pending_cancellation_unavailable(
                    &mut obj,
                    "completion_or_timeout_winner",
                );
            }
            if let Some(finalization) = managed_finalization.as_ref() {
                let expected = finalization
                    .get("expected_status_revision")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let runner_error = finalization.get("runner_error").and_then(Value::as_str);
                let proof = finalization
                    .get("termination_proof")
                    .and_then(Value::as_str);
                let proof = match proof {
                    Some("spawn_suppressed") => Some("spawn_suppressed"),
                    Some("unix_process_group_absent") => Some("unix_process_group_absent"),
                    _ => None,
                };
                let credential_cleanup_failed = finalization
                    .get("credential_cleanup_failed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let result_persist_failed = finalization
                    .get("result_persist_failed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                managed_terminal = Some(
                    crate::managed_run_control::apply_dequeued_cancellation_to_terminal_status(
                        &mut obj,
                        expected,
                        runner_error,
                        proof,
                        credential_cleanup_failed,
                        result_persist_failed,
                    ),
                );
            }
            // A bounded local lock exhaustion has durable eval evidence but
            // no canonical dispatch outcome yet. Keep the run observably
            // non-terminal until a later completion call reconciles that
            // outcome and clears this marker itself.
            if previous
                .get("completion_recovery")
                .and_then(Value::as_object)
                .and_then(|recovery| recovery.get("status"))
                .and_then(Value::as_str)
                .is_some_and(|status| status != "completion_admitted")
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
    if let Err(error) = advance_revision(&mut obj) {
        eprintln!("[dispatch-v2] refusing status write: {error}");
        return None;
    }
    if managed_terminal.is_some() {
        let committed_revision = obj.get("status_revision").cloned().unwrap_or(Value::Null);
        if let Some(cancellation) = obj.get_mut("cancellation").and_then(Value::as_object_mut) {
            cancellation.insert("observed_status_revision".to_string(), committed_revision);
        }
    }
    let completion = managed_terminal.map(|terminal| match terminal {
        crate::managed_run_control::ManagedTerminalCancellation::Confirmed {
            termination_proof,
        } => crate::managed_run_control::CancelCompletion::Confirmed {
            termination_proof,
            status_revision: obj
                .get("status_revision")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        },
        crate::managed_run_control::ManagedTerminalCancellation::Unconfirmed => {
            crate::managed_run_control::CancelCompletion::Unconfirmed
        }
        crate::managed_run_control::ManagedTerminalCancellation::Unavailable(reason) => {
            crate::managed_run_control::CancelCompletion::Unavailable(reason)
        }
    });
    let body = serde_json::to_string_pretty(&Value::Object(obj.clone()))
        .unwrap_or_else(|_| "{}".to_string());
    #[cfg(test)]
    let write_result =
        if managed_finalization.is_some() && take_managed_terminal_status_write_failure(&path) {
            Err("injected managed terminal status write failure".to_string())
        } else {
            target.write_atomic(body.as_bytes())
        };
    #[cfg(not(test))]
    let write_result = target.write_atomic(body.as_bytes());
    if let Err(e) = write_result {
        if managed_finalization.is_some() {
            if let Err(fallback_error) = persist_managed_terminal_status_failure(&target, &mut obj)
            {
                eprintln!(
                    "[dispatch-v2] managed terminal write failed ({e}); fallback also failed: {fallback_error}"
                );
            }
            return Some(crate::managed_run_control::CancelCompletion::Unavailable(
                "persist_failed",
            ));
        }
        eprintln!("[dispatch-v2] failed to write {}: {e}", path.display());
    }
    completion
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

    #[cfg(unix)]
    #[test]
    fn managed_finalization_refuses_a_corrupt_prior_status_without_writing_canceled() {
        let temp = tempfile::tempdir().expect("temporary run directory");
        let path = temp.path().join("status.json");
        std::fs::write(&path, b"{not json").expect("write corrupt status");
        let status_anchor = crate::managed_run_control::AnchoredRunStatus::open(temp.path())
            .expect("anchor managed run");

        let completion = write_status_json_with_managed_anchor(
            temp.path(),
            "20260823T182599Z-corrupt-managed-terminal",
            false,
            None,
            None,
            "n/a",
            None,
            None,
            None,
            None,
            Some(serde_json::json!({
                "state": "TASK_STATE_CANCELED",
                "managed_cancellation_finalization": {
                    "expected_status_revision": 1,
                    "runner_error": "managed_cancelled",
                    "termination_proof": "unix_process_group_absent",
                }
            })),
            &status_anchor,
        );

        assert!(matches!(
            completion,
            Some(crate::managed_run_control::CancelCompletion::Unavailable(
                "managed_terminal_status_unreadable"
            ))
        ));
        assert_eq!(
            std::fs::read(&path).expect("read unchanged status"),
            b"{not json"
        );
    }

    #[cfg(unix)]
    #[test]
    fn managed_finalization_refuses_an_atomic_status_write_failure_without_confirmation() {
        let temp = tempfile::tempdir().expect("temporary run directory");
        let status_path = temp.path().join("status.json");
        let initial = serde_json::json!({
            "dispatch_id": "20260823T182520Z-managed-write-failure",
            "state": "TASK_STATE_WORKING",
            "status_revision": 7,
            "execution_classification": "managed_custom",
        });
        std::fs::write(&status_path, initial.to_string()).expect("seed managed status");
        let _failure = fail_next_managed_terminal_status_write(temp.path());
        let status_anchor = crate::managed_run_control::AnchoredRunStatus::open(temp.path())
            .expect("anchor managed run");

        let completion = write_status_json_with_managed_anchor(
            temp.path(),
            "20260823T182520Z-managed-write-failure",
            false,
            None,
            None,
            "n/a",
            Some(143),
            None,
            None,
            None,
            Some(serde_json::json!({
                "state": "TASK_STATE_CANCELED",
                "managed_cancellation_finalization": {
                    "expected_status_revision": 7,
                    "runner_error": "managed_cancelled",
                    "termination_proof": "unix_process_group_absent",
                }
            })),
            &status_anchor,
        );

        match completion {
            Some(crate::managed_run_control::CancelCompletion::Unavailable(reason)) => {
                assert_eq!(reason, "persist_failed");
            }
            _ => panic!("atomic status failure must not confirm managed cancellation"),
        }
        let after: Value = serde_json::from_slice(
            &std::fs::read(&status_path).expect("read fallback managed status"),
        )
        .expect("parse untouched managed status");
        assert_eq!(after["state"], "TASK_STATE_FAILED");
        assert_eq!(after["cancellation"]["receipt"], "cancellation_unavailable");
        assert_eq!(after["cancellation"]["reason"], "persist_failed");
        assert_eq!(
            after["cancellation"]["observed_status_revision"], after["status_revision"],
            "fallback receipt must bind its committed root revision"
        );
    }

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
