use super::*;

use super::kanban_helpers::{
    get_kanban_state, init_kanban_task, should_cleanup_run, update_kanban_state,
};
use super::mcp_config::generate_mcp_config;
use super::prompt::{assemble_prompt, resolve_effective_skills};
use super::subprocess::{
    build_claude_command, build_codex_command, build_custom_command, run_agent_subprocess,
    tail_chars,
};
use super::v2::{
    build_execute_prompt, parse_plan, plan_review_enabled, plan_timeout_secs, v2_enabled,
    IMPLEMENT_PLAN_FALLBACK_SKILL, IMPLEMENT_PLAN_SKILL, PLAN_SYSTEM_PROMPT,
};

use std::collections::HashMap;
use std::path::Path;

// ─── Dispatch result ─────────────────────────────────────────────────────────

#[allow(dead_code)]
pub(crate) struct DispatchResult {
    pub output: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}

pub(crate) fn apply_unlocked_vault_env(cmd: &mut Command, server: &MemoryServer) -> usize {
    let Ok(secrets) = server.unlocked_env_secrets_for_child_env() else {
        return 0;
    };

    let mut injected = 0usize;
    for (name, value) in secrets {
        if std::env::var_os(&name).is_some() {
            continue;
        }
        cmd.env(name, value);
        injected += 1;
    }
    injected
}

pub(crate) fn new_dispatch_id(now: chrono::DateTime<Utc>, agent: &str) -> String {
    let timestamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let sanitized = agent.replace(|c: char| !c.is_ascii_alphanumeric(), "-");
    let suffix = uuid::Uuid::new_v4().as_simple().to_string()[..8].to_string();
    format!("{}-{}-{}", timestamp, sanitized, suffix)
}

/// Snapshot of the relevant `DISPATCH_V2_*` env vars at dispatch time.
/// Captured once at entry so tests can inject overrides and so the V1
/// vs V2 routing decision is consistent for the lifetime of the call.
fn current_dispatch_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    for key in [
        "DISPATCH_V2_ENABLED",
        "DISPATCH_V2_PLAN_REVIEW",
        "DISPATCH_V2_PLAN_TIMEOUT_SECS",
    ] {
        if let Ok(v) = std::env::var(key) {
            env.insert(key.to_string(), v);
        }
    }
    env
}

/// Append a JSON event line to a trajectory.jsonl file. Best-effort —
/// failures are silently swallowed so trajectory I/O can never crash a
/// dispatch.
fn append_trajectory(trajectory_path: &Path, event: &serde_json::Value) {
    use std::io::Write;
    let Ok(line) = serde_json::to_string(event) else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .open(trajectory_path)
    {
        let _ = writeln!(f, "{}", line);
    }
}

/// Build a workspace directory under `$TACHI_HOME/runs/<id>` (or
/// `$HOME/.tachi/runs/<id>`, or `$TMPDIR/tachi/runs/<id>`).
fn workspace_dir_for(dispatch_id: &str) -> PathBuf {
    let base = if let Ok(home) = std::env::var("TACHI_HOME") {
        PathBuf::from(home)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".tachi")
    } else {
        std::env::temp_dir().join("tachi")
    };
    base.join("runs").join(dispatch_id)
}

// ─── Main dispatch handler ───────────────────────────────────────────────────

pub(crate) async fn handle_tachi_dispatch(
    server: &MemoryServer,
    params: TachiDispatchParams,
) -> Result<String, String> {
    let now = Utc::now();
    let dispatch_id = new_dispatch_id(now, &params.agent);

    let agent_norm = params.agent.to_ascii_lowercase();
    let timeout = Duration::from_secs(params.timeout_secs);

    // 1. Create isolated workspace directory
    let workspace_dir = workspace_dir_for(&dispatch_id);
    std::fs::create_dir_all(&workspace_dir)
        .map_err(|e| format!("Failed to create workspace dir: {e}"))?;

    // 2. Generate MCP config if requested
    let inject_tachi = params.inject_tachi_mcp.unwrap_or(false);
    let inject_hub = params.inject_hub_mcps.unwrap_or(false);
    // Codex's `codex exec` CLI does not consume an external mcp-config file
    // the way Claude Code's `--mcp-config` does (see build_codex_command,
    // where the generated config path is deliberately ignored). Failing
    // loudly here is clearer than silently producing a config file the
    // subprocess will never read.
    if matches!(agent_norm.as_str(), "codex" | "codex-cli" | "openai")
        && (inject_tachi || inject_hub)
    {
        return Err(
            "inject_tachi_mcp / inject_hub_mcps are not supported for the codex backend. \
             Configure MCP servers in ~/.codex/config.toml instead, or dispatch with agent='claude'."
                .to_string(),
        );
    }
    let mcp_config_path = if inject_tachi || inject_hub {
        generate_mcp_config(server, &dispatch_id, inject_tachi, inject_hub).await?
    } else {
        None
    };

    // 3. Route V1 vs V2.
    let env_snapshot = current_dispatch_env();
    if v2_enabled(&params, &env_snapshot) {
        run_v2_two_stage(
            server,
            params,
            dispatch_id,
            agent_norm,
            workspace_dir,
            mcp_config_path,
            timeout,
            env_snapshot,
        )
        .await
    } else {
        run_v1_single_stage(
            server,
            params,
            dispatch_id,
            agent_norm,
            workspace_dir,
            mcp_config_path,
            timeout,
        )
        .await
    }
}

// ─── V1 single-stage (legacy) ────────────────────────────────────────────────

async fn run_v1_single_stage(
    server: &MemoryServer,
    params: TachiDispatchParams,
    dispatch_id: String,
    agent_norm: String,
    workspace_dir: PathBuf,
    mcp_config_path: Option<PathBuf>,
    timeout: Duration,
) -> Result<String, String> {
    // Assemble prompt & write audit files to workspace
    let prompt = assemble_prompt(server, &params).await;
    let (effective_skills_for_files, _) = resolve_effective_skills(&params);
    let plan_path = workspace_dir.join("plan.md");
    std::fs::write(&plan_path, &prompt).map_err(|e| format!("Failed to write plan file: {e}"))?;

    // Write prompt.md (full assembled prompt for tracked run)
    let prompt_md_path = workspace_dir.join("prompt.md");
    std::fs::write(&prompt_md_path, &prompt)
        .map_err(|e| format!("Failed to write prompt.md: {e}"))?;

    // Write context.md (summary of injected context/skills — for MVP, same as prompt)
    let context_md_path = workspace_dir.join("context.md");
    let context_summary = {
        let mut sections = Vec::new();
        sections.push(format!("# Dispatch Context: {}", dispatch_id));
        sections.push(format!("Agent: {}", params.agent));
        sections.push(format!(
            "Stage: {}",
            params.stage.as_deref().unwrap_or("none")
        ));
        sections.push(format!("Skills: {:?}", effective_skills_for_files));
        sections.push(String::new());
        sections.push(prompt.clone());
        sections.join("\n\n")
    };
    std::fs::write(&context_md_path, &context_summary)
        .map_err(|e| format!("Failed to write context.md: {e}"))?;

    // Write trajectory.jsonl — initial dispatch_started event
    let trajectory_path = workspace_dir.join("trajectory.jsonl");
    {
        let started_event = json!({
            "event": "dispatch_started",
            "dispatch_id": dispatch_id,
            "agent": params.agent,
            "stage": params.stage,
            "timestamp": Utc::now().to_rfc3339(),
            "v2": false,
        });
        let line = serde_json::to_string(&started_event)
            .map_err(|e| format!("Failed to serialize started event: {e}"))?;
        std::fs::write(&trajectory_path, format!("{}\n", line))
            .map_err(|e| format!("Failed to write trajectory.jsonl: {e}"))?;
    }

    // Initialize kanban task
    init_kanban_task(
        server,
        &dispatch_id,
        &params,
        Some(&plan_path.to_string_lossy()),
    )
    .await?;

    // Build command
    let cmd = build_agent_command(&agent_norm, &params, &prompt, mcp_config_path.as_ref())?;

    // Spawn watchdog-managed subprocess
    spawn_execute_with_watchdog(
        server.clone(),
        cmd,
        timeout,
        mcp_config_path.clone(),
        workspace_dir.clone(),
        trajectory_path.clone(),
        dispatch_id.clone(),
        agent_norm.clone(),
        params.stage.clone(),
        None,
        None,
    );

    // Immediately return — main agent is unblocked!
    let response = json!({
        "dispatch_id": dispatch_id,
        "task": {
            "id": dispatch_id,
            "status": { "state": "TASK_STATE_WORKING" },
        },
        "agent": agent_norm,
        "message": "Task dispatched to background. You are unblocked. Use tachi_board to check status.",
        "plan_file": plan_path.to_string_lossy(),
        "prompt_file": prompt_md_path.to_string_lossy(),
        "context_file": context_md_path.to_string_lossy(),
        "trajectory_file": trajectory_path.to_string_lossy(),
        "run_dir": workspace_dir.to_string_lossy(),
        "v2": false,
    });

    serde_json::to_string(&response).map_err(|e| format!("serialize: {e}"))
}

// ─── V2 two-stage Plan → Execute ─────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn run_v2_two_stage(
    server: &MemoryServer,
    mut params: TachiDispatchParams,
    dispatch_id: String,
    agent_norm: String,
    workspace_dir: PathBuf,
    mcp_config_path: Option<PathBuf>,
    timeout: Duration,
    env: HashMap<String, String>,
) -> Result<String, String> {
    let trajectory_path = workspace_dir.join("trajectory.jsonl");

    // Trajectory: dispatch_started
    let started_event = json!({
        "event": "dispatch_started",
        "dispatch_id": dispatch_id,
        "agent": params.agent,
        "stage": params.stage,
        "timestamp": Utc::now().to_rfc3339(),
        "v2": true,
    });
    let started_line = serde_json::to_string(&started_event)
        .map_err(|e| format!("Failed to serialize started event: {e}"))?;
    std::fs::write(&trajectory_path, format!("{}\n", started_line))
        .map_err(|e| format!("Failed to write trajectory.jsonl: {e}"))?;

    // Kanban initial row.
    let plan_path = workspace_dir.join("plan.md");
    init_kanban_task(
        server,
        &dispatch_id,
        &params,
        Some(&plan_path.to_string_lossy()),
    )
    .await?;

    // ─── Stage 1: Plan ───────────────────────────────────────────────────
    let plan_started = std::time::Instant::now();
    let plan_started_at = Utc::now();

    // Build the planning prompt. We pass the original task plus any
    // explicit context_query the caller supplied. The system prompt is
    // prepended directly into the call payload (the pool's `call` API
    // accepts a single prompt; we concatenate system + user).
    let planning_payload = format!(
        "{}\n\n---\nTask:\n{}\n",
        PLAN_SYSTEM_PROMPT,
        params.task.trim()
    );

    // Honor the V2 plan timeout by routing through CLAUDE_POOL_TIMEOUT_SECS
    // is NOT possible here — that env is read only at pool construction.
    // Instead we wrap the call in a tokio timeout so V2 callers get a
    // predictable upper bound even when the pool's own timeout is larger.
    let plan_timeout = Duration::from_secs(plan_timeout_secs(&env));
    let plan_label = format!("dispatch-plan-{}", &dispatch_id);
    let plan_call = server.claude_pool.call(&plan_label, &planning_payload);
    let plan_outcome = match tokio::time::timeout(plan_timeout, plan_call).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(e)) => {
            let err = format!("Stage 1 (plan) LLM call failed: {e}");
            mark_v2_failed(server, &dispatch_id, &trajectory_path, &err).await;
            return Err(err);
        }
        Err(_) => {
            let err = format!(
                "Stage 1 (plan) LLM call timed out after {}s",
                plan_timeout.as_secs()
            );
            mark_v2_failed(server, &dispatch_id, &trajectory_path, &err).await;
            return Err(err);
        }
    };

    let plan_text = plan_outcome.text.trim().to_string();
    if plan_text.is_empty() {
        let err = "Stage 1 (plan) produced an empty plan.md".to_string();
        mark_v2_failed(server, &dispatch_id, &trajectory_path, &err).await;
        return Err(err);
    }

    std::fs::write(&plan_path, &plan_text).map_err(|e| format!("Failed to write plan.md: {e}"))?;

    // Parse + validate plan structure. We don't hard-fail when sections
    // are missing — Stage 2 can still cope with a degraded plan — but
    // we surface the gap in trajectory.jsonl so reviewers can spot it.
    let parsed = parse_plan(&plan_text);
    let plan_complete = parsed.is_complete();
    let missing_sections: Vec<&str> = [
        ("goal", parsed.goal.is_empty()),
        ("steps", parsed.steps.is_empty()),
        ("files", parsed.files.is_empty()),
        ("validation", parsed.validation.is_empty()),
    ]
    .into_iter()
    .filter_map(|(name, empty)| if empty { Some(name) } else { None })
    .collect();
    if !plan_complete {
        append_trajectory(
            &trajectory_path,
            &json!({
                "event": "plan_incomplete",
                "dispatch_id": dispatch_id,
                "timestamp": Utc::now().to_rfc3339(),
                "missing_sections": missing_sections,
            }),
        );
    }

    let plan_duration_ms = plan_started.elapsed().as_millis() as u64;
    append_trajectory(
        &trajectory_path,
        &json!({
            "event": "plan_generated",
            "dispatch_id": dispatch_id,
            "timestamp": Utc::now().to_rfc3339(),
            "duration_ms": plan_duration_ms,
            "plan_bytes": plan_text.len(),
            "plan_complete": plan_complete,
        }),
    );

    // ─── Stage 1.5: optional review gate ─────────────────────────────────
    let review_enabled = plan_review_enabled(&env);
    let plan_review_status = if review_enabled {
        let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
        if is_tty {
            // Interactive operator approval.
            let approved = dialoguer::Confirm::new()
                .with_prompt(format!(
                    "Tachi dispatch {dispatch_id}: approve generated plan and proceed to execute?"
                ))
                .default(false)
                .interact()
                .unwrap_or(false);
            if !approved {
                let err = "Plan rejected by operator at review gate".to_string();
                append_trajectory(
                    &trajectory_path,
                    &json!({
                        "event": "plan_rejected",
                        "dispatch_id": dispatch_id,
                        "timestamp": Utc::now().to_rfc3339(),
                    }),
                );
                let _ = update_kanban_state(
                    server,
                    &dispatch_id,
                    "TASK_STATE_CANCELED",
                    None,
                    Some(true),
                )
                .await;
                write_v2_status(
                    &workspace_dir,
                    &dispatch_id,
                    plan_started_at,
                    plan_duration_ms,
                    None,
                    "rejected",
                    None,
                );
                return Err(err);
            }
            append_trajectory(
                &trajectory_path,
                &json!({
                    "event": "plan_approved",
                    "dispatch_id": dispatch_id,
                    "timestamp": Utc::now().to_rfc3339(),
                    "via": "tty",
                }),
            );
            "approved_tty"
        } else {
            // Non-TTY: park in pending_review and return early. An
            // operator must explicitly resume the dispatch via a future
            // tool (not in scope here) or re-dispatch.
            let _ = update_kanban_state(
                server,
                &dispatch_id,
                "TASK_STATE_PENDING_REVIEW",
                None,
                Some(false),
            )
            .await;
            append_trajectory(
                &trajectory_path,
                &json!({
                    "event": "plan_awaiting_review",
                    "dispatch_id": dispatch_id,
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
            write_v2_status(
                &workspace_dir,
                &dispatch_id,
                plan_started_at,
                plan_duration_ms,
                None,
                "pending_review",
                None,
            );
            let response = json!({
                "dispatch_id": dispatch_id,
                "task": {
                    "id": dispatch_id,
                    "status": { "state": "TASK_STATE_PENDING_REVIEW" },
                },
                "agent": agent_norm,
                "message": "V2 dispatch paused at plan_review gate (non-TTY). Inspect plan.md and resume manually.",
                "plan_file": plan_path.to_string_lossy(),
                "trajectory_file": trajectory_path.to_string_lossy(),
                "run_dir": workspace_dir.to_string_lossy(),
                "v2": true,
                "plan_review_status": "pending_review",
            });
            return serde_json::to_string(&response)
                .map_err(|e| format!("serialize: {e}"))?
                .pipe(Ok);
        }
    } else {
        "auto_approved"
    };

    // ─── Stage 2: Execute ────────────────────────────────────────────────
    // Inject implement-plan skill into the params for prompt assembly.
    let implement_skill = if server.get_capability(IMPLEMENT_PLAN_SKILL).is_ok() {
        IMPLEMENT_PLAN_SKILL
    } else {
        IMPLEMENT_PLAN_FALLBACK_SKILL
    };
    if !params
        .skills
        .iter()
        .any(|s| s == implement_skill || s == IMPLEMENT_PLAN_SKILL)
    {
        params.skills.push(implement_skill.to_string());
    }

    // Build the execute prompt by stitching plan.md + original task,
    // then wrap it through the standard prompt assembly so memory/wiki
    // context and skill prompts still get injected.
    let stitched = build_execute_prompt(&plan_text, &params.task);
    let mut exec_params = params.clone();
    exec_params.task = stitched;
    let exec_prompt = assemble_prompt(server, &exec_params).await;
    let (effective_skills, _) = resolve_effective_skills(&exec_params);

    // Persist audit files (prompt.md / context.md).
    let prompt_md_path = workspace_dir.join("prompt.md");
    std::fs::write(&prompt_md_path, &exec_prompt)
        .map_err(|e| format!("Failed to write prompt.md: {e}"))?;
    let context_md_path = workspace_dir.join("context.md");
    let context_summary = format!(
        "# Dispatch Context: {dispatch_id}\nAgent: {agent}\nStage: plan_execute (v2)\nPlanReview: {review}\nSkills: {skills:?}\n\n{exec_prompt}",
        agent = params.agent,
        review = plan_review_status,
        skills = effective_skills,
    );
    std::fs::write(&context_md_path, &context_summary)
        .map_err(|e| format!("Failed to write context.md: {e}"))?;

    append_trajectory(
        &trajectory_path,
        &json!({
            "event": "execute_started",
            "dispatch_id": dispatch_id,
            "timestamp": Utc::now().to_rfc3339(),
            "agent": agent_norm,
        }),
    );

    let cmd = build_agent_command(
        &agent_norm,
        &exec_params,
        &exec_prompt,
        mcp_config_path.as_ref(),
    )?;

    // Spawn execution with watchdog; v2 metadata threaded through so the
    // background task can update status.json with execute timings.
    spawn_execute_with_watchdog(
        server.clone(),
        cmd,
        timeout,
        mcp_config_path.clone(),
        workspace_dir.clone(),
        trajectory_path.clone(),
        dispatch_id.clone(),
        agent_norm.clone(),
        params.stage.clone(),
        Some(V2Meta {
            plan_started_at,
            plan_duration_ms,
            plan_review_status: plan_review_status.to_string(),
        }),
        None,
    );

    let response = json!({
        "dispatch_id": dispatch_id,
        "task": {
            "id": dispatch_id,
            "status": { "state": "TASK_STATE_WORKING" },
        },
        "agent": agent_norm,
        "message": "V2 two-stage dispatch: plan generated, execute spawned. Use tachi_board to check status.",
        "plan_file": plan_path.to_string_lossy(),
        "prompt_file": prompt_md_path.to_string_lossy(),
        "context_file": context_md_path.to_string_lossy(),
        "trajectory_file": trajectory_path.to_string_lossy(),
        "run_dir": workspace_dir.to_string_lossy(),
        "v2": true,
        "plan_review_status": plan_review_status,
        "duration_ms_plan": plan_duration_ms,
    });

    serde_json::to_string(&response).map_err(|e| format!("serialize: {e}"))
}

// Small extension trait so we can `.pipe(Ok)` for an early-return
// readability win without pulling in `tap`.
trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}
impl<T> Pipe for T {}

#[derive(Clone)]
struct V2Meta {
    plan_started_at: chrono::DateTime<Utc>,
    plan_duration_ms: u64,
    plan_review_status: String,
}

/// Best-effort: mark a V2 dispatch as FAILED in kanban and append a
/// trajectory event when Stage 1 fails. Status.json is written so the
/// audit trail still shows a Stage 1 failure even when Stage 2 never
/// ran.
async fn mark_v2_failed(
    server: &MemoryServer,
    dispatch_id: &str,
    trajectory_path: &Path,
    err: &str,
) {
    append_trajectory(
        trajectory_path,
        &json!({
            "event": "plan_failed",
            "dispatch_id": dispatch_id,
            "timestamp": Utc::now().to_rfc3339(),
            "error": err,
        }),
    );
    let _ = update_kanban_state(server, dispatch_id, "TASK_STATE_FAILED", None, Some(false)).await;
}

/// Write the V2 `status.json` audit file. `executed_at` / `execute_ms`
/// are None for pre-execute statuses (pending_review / rejected /
/// stage-1-failed).
fn write_v2_status(
    workspace_dir: &Path,
    dispatch_id: &str,
    plan_started_at: chrono::DateTime<Utc>,
    plan_duration_ms: u64,
    execute_meta: Option<(chrono::DateTime<Utc>, u64, Option<i32>)>,
    plan_review_status: &str,
    error: Option<&str>,
) {
    let (executed_at, execute_ms, exit_code) = match execute_meta {
        Some((ts, ms, code)) => (Some(ts.to_rfc3339()), Some(ms), code),
        None => (None, None, None),
    };
    let total_ms = plan_duration_ms + execute_ms.unwrap_or(0);
    let body = json!({
        "dispatch_id": dispatch_id,
        "v2": true,
        "plan_generated_at": plan_started_at.to_rfc3339(),
        "duration_ms_plan": plan_duration_ms,
        "executed_at": executed_at,
        "duration_ms_execute": execute_ms,
        "total_duration_ms": total_ms,
        "exit_code": exit_code,
        "plan_review_status": plan_review_status,
        "error": error,
    });
    let path = workspace_dir.join("status.json");
    let _ = std::fs::write(
        path,
        serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()),
    );
}

// ─── Shared: command construction + watchdog spawn ──────────────────────────

fn build_agent_command(
    agent_norm: &str,
    params: &TachiDispatchParams,
    prompt: &str,
    mcp_config_path: Option<&PathBuf>,
) -> Result<Command, String> {
    match agent_norm {
        "claude" | "claude-code" | "claude-cli" => {
            Ok(build_claude_command(params, prompt, mcp_config_path))
        }
        "codex" | "codex-cli" | "openai" => {
            Ok(build_codex_command(params, prompt, mcp_config_path))
        }
        "custom" => build_custom_command(params, prompt),
        other => Err(format!(
            "Unknown agent '{}'. Use 'claude', 'codex', or 'custom'.",
            other
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_execute_with_watchdog(
    server: MemoryServer,
    mut cmd: Command,
    timeout: Duration,
    mcp_config_path: Option<PathBuf>,
    workspace_dir: PathBuf,
    trajectory_path: PathBuf,
    dispatch_id: String,
    agent_norm: String,
    stage_for_traj: Option<String>,
    v2_meta: Option<V2Meta>,
    _unused: Option<()>,
) {
    let _ = apply_unlocked_vault_env(&mut cmd, &server);

    // Scope guard for MCP config cleanup (moved into spawned task).
    struct McpCleanup(Option<PathBuf>);
    impl Drop for McpCleanup {
        fn drop(&mut self) {
            if let Some(ref path) = self.0 {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    tokio::task::spawn(async move {
        let _mcp_cleanup = McpCleanup(mcp_config_path);
        let execute_started_at = Utc::now();
        let execute_started = std::time::Instant::now();

        let result = run_agent_subprocess(cmd, timeout).await;
        let execute_ms = execute_started.elapsed().as_millis() as u64;

        // Append subprocess_finished event to trajectory.jsonl
        let full_output = match &result {
            Ok(r) => r.output.clone(),
            Err(e) => e.clone(),
        };
        {
            let (exit_code, output_tail) = match &result {
                Err(e) => (None, e.chars().take(200).collect::<String>()),
                Ok(r) => (r.exit_code, tail_chars(&r.output, 500)),
            };
            append_trajectory(
                &trajectory_path,
                &json!({
                    "event": "subprocess_finished",
                    "dispatch_id": dispatch_id,
                    "agent": agent_norm,
                    "stage": stage_for_traj,
                    "exit_code": exit_code,
                    "timestamp": Utc::now().to_rfc3339(),
                    "output_tail": output_tail,
                }),
            );
        }

        // Save full output to result.md for orchestrator eval
        {
            let result_path = workspace_dir.join("result.md");
            let _ = std::fs::write(&result_path, &full_output);
        }

        // V2 audit: write final status.json + dispatch_finished event.
        if let Some(meta) = &v2_meta {
            let exit_code = match &result {
                Ok(r) => r.exit_code,
                Err(_) => None,
            };
            write_v2_status(
                &workspace_dir,
                &dispatch_id,
                meta.plan_started_at,
                meta.plan_duration_ms,
                Some((execute_started_at, execute_ms, exit_code)),
                &meta.plan_review_status,
                match &result {
                    Ok(_) => None,
                    Err(e) => Some(e.as_str()),
                },
            );
            append_trajectory(
                &trajectory_path,
                &json!({
                    "event": "dispatch_finished",
                    "dispatch_id": dispatch_id,
                    "timestamp": Utc::now().to_rfc3339(),
                    "exit_code": exit_code,
                    "duration_ms_execute": execute_ms,
                    "total_duration_ms": meta.plan_duration_ms + execute_ms,
                }),
            );
        }

        // --- WATCHDOG: check if sub-agent properly closed the loop ---
        tokio::time::sleep(Duration::from_secs(2)).await; // grace period for tachi_complete to propagate

        let kanban_state = get_kanban_state(&server, &dispatch_id).await;
        let is_closed = matches!(
            kanban_state.as_deref(),
            Some("TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED")
        );
        if !is_closed {
            let exited_ok = matches!(&result, Ok(r) if r.exit_code == Some(0));

            if exited_ok {
                let tail = tail_chars(&full_output, 500);
                eprintln!(
                    "[watchdog] dispatch {} exited 0 without tachi_complete; marking kanban COMPLETED as unreviewed. tail={}",
                    dispatch_id, tail
                );
                let _ = update_kanban_state(
                    &server,
                    &dispatch_id,
                    "TASK_STATE_COMPLETED",
                    None,
                    Some(false),
                )
                .await;
            } else {
                let note = match &result {
                    Err(e) => format!("Watchdog: {}", e),
                    Ok(r) => {
                        let tail = tail_chars(&r.output, 500);
                        format!(
                            "Watchdog: Agent crashed (exit {:?}). Stderr tail: {}",
                            r.exit_code, tail
                        )
                    }
                };

                let ts = Utc::now();
                let eval_id = format!(
                    "eval_ws_{}_{}",
                    ts.format("%Y%m%dT%H%M%SZ"),
                    dispatch_id.chars().take(16).collect::<String>()
                );
                let metadata = json!({
                    "task_id": eval_id,
                    "agent": format!("watchdog/{}", agent_norm),
                    "outcome": "failure",
                    "dispatch_id": dispatch_id,
                    "auto_synthesized": true,
                });
                let save_params = SaveMemoryParams {
                    text: note.clone(),
                    summary: format!("Watchdog auto-close FAILURE: {}", dispatch_id),
                    path: format!("/eval/{}/{}", ts.format("%Y%m%d"), eval_id),
                    importance: 0.4,
                    category: "experience".to_string(),
                    topic: "eval".to_string(),
                    keywords: vec![
                        "eval".to_string(),
                        "watchdog".to_string(),
                        "failure".to_string(),
                        "auto_synthesized".to_string(),
                    ],
                    persons: Vec::new(),
                    entities: Vec::new(),
                    location: String::new(),
                    scope: "project".to_string(),
                    vector: None,
                    id: Some(eval_id.clone()),
                    force: true,
                    auto_link: false,
                    project: None,
                    retention_policy: Some("durable".to_string()),
                    domain: Some("system".to_string()),
                    timestamp: None,
                    metadata: Some(metadata),
                };
                let _ = crate::memory_search_ops::handle_save_memory(&server, save_params).await;

                let _ = update_kanban_state(
                    &server,
                    &dispatch_id,
                    "TASK_STATE_FAILED",
                    Some(&eval_id),
                    Some(false),
                )
                .await;
            }
        }

        let should_cleanup = match &result {
            Ok(r) => should_cleanup_run(r.exit_code, kanban_state.as_deref()),
            Err(_) => false,
        };
        if should_cleanup {
            let _ = std::fs::remove_dir_all(workspace_dir);
        }
    });
}
