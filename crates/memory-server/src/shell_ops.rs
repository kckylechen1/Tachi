//! `tachi_shell` — skill-gated flow orchestration facade.
//!
//! Implements the MVP described in `wiki/agent/tachi/Tachi-Shell-重构计划-2026-05-04.md`.
//!
//! Each `tachi_shell(action=...)` call is a thin coordinator that:
//!   1. Resolves / creates a `flow_id` and its run directory.
//!   2. Injects the meta skill SOP file required for the stage.
//!   3. Writes / updates `instruction.md`, `status.json`, `events.jsonl`.
//!   4. For `kanban` / `status` it delegates to existing read-only handlers.
//!   5. For `dispatch` it can optionally hand off to the existing
//!      `dispatch_ops::handle_tachi_dispatch` (Phase 4 hook).
//!
//! This module deliberately does **not** re-implement clanker dispatch,
//! kanban, or skill discovery — it composes existing infra.

use crate::{MemoryServer, TachiBoardParams, TachiDispatchParams, TachiShellParams};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// ─── Stage / meta-skill mapping ──────────────────────────────────────────────

/// MVP hardcoded stage → meta skill SOP file path (relative to repo root).
fn meta_skill_for_stage(stage: &str) -> Option<&'static str> {
    match stage {
        "brainstorm" => Some("skill/superpowers/skills/brainstorming/SKILL.md"),
        "plan" => Some("skill/superpowers/skills/writing-plans/SKILL.md"),
        "dispatch" => Some("skill/superpowers/skills/executing-plans/SKILL.md"),
        "review" => Some("skill/superpowers/skills/requesting-code-review/SKILL.md"),
        "ship" => Some("skill/superpowers/skills/finishing-a-development-branch/SKILL.md"),
        _ => None,
    }
}

/// Stages that bear a skill gate and create/advance a flow run.
const STAGE_ACTIONS: &[&str] = &["brainstorm", "plan", "dispatch", "review", "ship"];

// ─── Run-root resolution ─────────────────────────────────────────────────────

fn cached_git_root() -> Option<&'static PathBuf> {
    static GIT_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    GIT_ROOT
        .get_or_init(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .output()
                .ok()
                .filter(|out| out.status.success())
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        })
        .as_ref()
}

/// Resolve the runs root directory.
///
/// Order:
/// 1. `$TACHI_RUN_ROOT`
/// 2. `<repo_root>/.tachi/runs/` if a git repo is detected via `git rev-parse --show-toplevel`
/// 3. `$TACHI_HOME/runs/`
/// 4. `$HOME/.tachi/runs/`
/// 5. `<temp>/tachi/runs/`
pub(crate) fn shell_runs_root() -> PathBuf {
    if let Ok(p) = std::env::var("TACHI_RUN_ROOT") {
        return PathBuf::from(p);
    }
    if let Some(root) = cached_git_root() {
        return root.join(".tachi").join("runs");
    }
    if let Ok(home) = std::env::var("TACHI_HOME") {
        return PathBuf::from(home).join("runs");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".tachi").join("runs");
    }
    std::env::temp_dir().join("tachi").join("runs")
}

fn slugify(s: &str) -> String {
    let s = s.trim().to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut last_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed: String = out.trim_matches('-').chars().take(40).collect();
    if trimmed.is_empty() {
        "flow".to_string()
    } else {
        trimmed
    }
}

fn new_flow_id(title: Option<&str>, task: Option<&str>) -> String {
    let now = Utc::now();
    let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let basis = title
        .or(task)
        .map(|s| s.to_string())
        .unwrap_or_else(|| "flow".to_string());
    format!("flow_{}_{}", stamp, slugify(&basis))
}

fn validate_flow_id(id: &str) -> Result<(), String> {
    if !id.starts_with("flow_")
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("Invalid flow_id: '{}'", id));
    }
    Ok(())
}

// ─── Status / events helpers ─────────────────────────────────────────────────

fn read_status(run_dir: &Path) -> Value {
    let status_path = run_dir.join("status.json");
    match std::fs::read_to_string(&status_path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|_| json!({})),
        Err(_) => json!({}),
    }
}

fn write_status(run_dir: &Path, status: &Value) -> Result<(), String> {
    let status_path = run_dir.join("status.json");
    let serialized =
        serde_json::to_string_pretty(status).map_err(|e| format!("serialize status.json: {e}"))?;
    std::fs::write(&status_path, serialized).map_err(|e| format!("write status.json: {e}"))?;
    Ok(())
}

fn append_event(run_dir: &Path, event: Value) -> Result<(), String> {
    use std::io::Write;
    let path = run_dir.join("events.jsonl");
    let line = serde_json::to_string(&event).map_err(|e| format!("serialize event: {e}"))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open events.jsonl: {e}"))?;
    writeln!(f, "{}", line).map_err(|e| format!("write events.jsonl: {e}"))?;
    Ok(())
}

// ─── Meta skill injection ────────────────────────────────────────────────────

/// Resolve the meta skill SOP file, falling back through several roots.
fn resolve_meta_skill(rel_path: &str) -> Option<PathBuf> {
    // 1. repo root (git toplevel)
    if let Some(root) = cached_git_root() {
        let p = root.join(rel_path);
        if p.exists() {
            return Some(p);
        }
    }
    // 2. cwd
    let p = PathBuf::from(rel_path);
    if p.exists() {
        return Some(p);
    }
    // 3. cargo manifest dir (for tests)
    if let Ok(d) = std::env::var("CARGO_MANIFEST_DIR") {
        let p = PathBuf::from(d).join("..").join("..").join(rel_path);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

#[derive(Debug)]
struct InjectionResult {
    required: bool,
    rel_path: Option<String>,
    source_path: Option<String>,
    injected_path: Option<String>,
    content_hash: Option<String>,
    loaded: bool,
    warning: Option<String>,
}

fn inject_meta_skill(stage: &str, run_dir: &Path) -> InjectionResult {
    let rel = match meta_skill_for_stage(stage) {
        Some(r) => r,
        None => {
            return InjectionResult {
                required: false,
                rel_path: None,
                source_path: None,
                injected_path: None,
                content_hash: None,
                loaded: false,
                warning: None,
            };
        }
    };
    let injected_dir = run_dir.join("injected");
    if let Err(e) = std::fs::create_dir_all(&injected_dir) {
        return InjectionResult {
            required: true,
            rel_path: Some(rel.to_string()),
            source_path: None,
            injected_path: None,
            content_hash: None,
            loaded: false,
            warning: Some(format!("create injected dir failed: {e}")),
        };
    }
    let resolved = match resolve_meta_skill(rel) {
        Some(p) => p,
        None => {
            return InjectionResult {
                required: true,
                rel_path: Some(rel.to_string()),
                source_path: None,
                injected_path: None,
                content_hash: None,
                loaded: false,
                warning: Some(format!(
                    "meta skill file '{}' not found in any known root",
                    rel
                )),
            };
        }
    };
    let bytes = match std::fs::read(&resolved) {
        Ok(b) => b,
        Err(e) => {
            return InjectionResult {
                required: true,
                rel_path: Some(rel.to_string()),
                source_path: Some(resolved.to_string_lossy().to_string()),
                injected_path: None,
                content_hash: None,
                loaded: false,
                warning: Some(format!("read meta skill failed: {e}")),
            };
        }
    };
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    let digest = format!("{:016x}", hasher.finish());
    // Flatten path: `superpowers-<stage>.md`
    let basename = format!("superpowers-{}.md", stage);
    let target = injected_dir.join(&basename);
    if let Err(e) = std::fs::write(&target, &bytes) {
        return InjectionResult {
            required: true,
            rel_path: Some(rel.to_string()),
            source_path: Some(resolved.to_string_lossy().to_string()),
            injected_path: None,
            content_hash: Some(digest),
            loaded: false,
            warning: Some(format!("write injected meta skill failed: {e}")),
        };
    }
    InjectionResult {
        required: true,
        rel_path: Some(rel.to_string()),
        source_path: Some(resolved.to_string_lossy().to_string()),
        injected_path: Some(target.to_string_lossy().to_string()),
        content_hash: Some(digest),
        loaded: true,
        warning: None,
    }
}

// (removed hex_encode helper — DefaultHasher used inline above for content fingerprinting.)

// ─── instruction.md generation ───────────────────────────────────────────────

fn build_instruction_md(
    flow_id: &str,
    stage: &str,
    task: &str,
    injection: &InjectionResult,
    notes: Option<&str>,
    validation: &[String],
    allowed_scope: &[String],
) -> String {
    let mut s = String::new();
    s.push_str(&format!("# Tachi Flow Instruction — {}\n\n", flow_id));
    s.push_str(&format!("Stage: **{}**\n\n", stage));
    s.push_str("## Task\n\n");
    s.push_str(task.trim());
    s.push_str("\n\n");
    s.push_str("## Required Reading (Injected SOP)\n\n");
    if let Some(p) = injection.injected_path.as_deref() {
        s.push_str(&format!("- `{}`", p));
        if let Some(rel) = injection.rel_path.as_deref() {
            s.push_str(&format!(" (source: `{}`)", rel));
        }
        if let Some(hash) = injection.content_hash.as_deref() {
            s.push_str(&format!(" fingerprint={}", &hash[..16]));
        }
        s.push('\n');
    } else if injection.required {
        s.push_str(&format!(
            "- WARNING: required meta skill `{}` could not be loaded: {}\n",
            injection.rel_path.as_deref().unwrap_or("?"),
            injection.warning.as_deref().unwrap_or("unknown error")
        ));
    } else {
        s.push_str("- (no meta skill required for this stage)\n");
    }
    s.push('\n');

    s.push_str("## Allowed Scope\n\n");
    if allowed_scope.is_empty() {
        s.push_str("- (unspecified — keep changes minimal and reversible)\n");
    } else {
        for a in allowed_scope {
            s.push_str(&format!("- {}\n", a));
        }
    }
    s.push('\n');

    s.push_str("## Validation Commands\n\n");
    if validation.is_empty() {
        match stage {
            "ship" | "dispatch" | "review" => {
                s.push_str("- `cargo test -p memory-server`\n");
            }
            _ => {
                s.push_str("- (none specified)\n");
            }
        }
    } else {
        for v in validation {
            s.push_str(&format!("- `{}`\n", v));
        }
    }
    s.push('\n');

    s.push_str("## Expected Outputs\n\n");
    s.push_str("Write all artifacts under the run directory:\n\n");
    s.push_str("- `result.md` — what was done, decisions taken, blockers\n");
    s.push_str("- `validation.md` — verification evidence (logs, test summaries)\n");
    s.push_str("- `status.json` — updated status (handled by tachi_shell when stage advances)\n");
    s.push_str("- `events.jsonl` — append-only event log\n");
    s.push_str("- `artifacts/` — anything else (diffs, gitleaks output, PR body, etc.)\n\n");

    if let Some(n) = notes {
        s.push_str("## Notes\n\n");
        s.push_str(n.trim());
        s.push_str("\n\n");
    }

    s
}

// ─── Action handlers ─────────────────────────────────────────────────────────

/// Top-level dispatcher used by `MemoryServer::tachi_shell`.
pub(crate) async fn handle_tachi_shell(
    server: &MemoryServer,
    params: TachiShellParams,
) -> Result<String, String> {
    let action = params.action.to_ascii_lowercase();
    match action.as_str() {
        "brainstorm" | "plan" | "review" | "ship" => {
            handle_stage_action(server, &action, params).await
        }
        "dispatch" => handle_dispatch_action(server, params).await,
        "kanban" => handle_kanban_action(server, params).await,
        "status" => handle_status_action(params).await,
        _ => Err(format!(
            "Invalid action '{}'. Use 'brainstorm', 'plan', 'dispatch', 'kanban', 'status', 'review', or 'ship'.",
            params.action
        )),
    }
}

async fn handle_stage_action(
    _server: &MemoryServer,
    stage: &str,
    params: TachiShellParams,
) -> Result<String, String> {
    let task = params
        .task
        .clone()
        .ok_or_else(|| format!("'task' is required for action='{}'", stage))?;
    let (flow_id, run_dir, created) = resolve_or_create_flow(&params, &task)?;
    let injection = inject_meta_skill(stage, &run_dir);

    let instruction = build_instruction_md(
        &flow_id,
        stage,
        &task,
        &injection,
        params.notes.as_deref(),
        &params.validation,
        &params.allowed_scope,
    );
    let instr_path = run_dir.join("instruction.md");
    std::fs::write(&instr_path, instruction).map_err(|e| format!("write instruction.md: {e}"))?;

    advance_stage(&run_dir, &flow_id, stage, &task, &injection, created)?;

    let resp = json!({
        "flow_id": flow_id,
        "stage": stage,
        "run_dir": run_dir.to_string_lossy(),
        "instruction_path": instr_path.to_string_lossy(),
        "injected_skill": injection_to_json(&injection),
        "async": false,
        "created": created,
    });
    serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))
}

async fn handle_dispatch_action(
    server: &MemoryServer,
    params: TachiShellParams,
) -> Result<String, String> {
    let task = params
        .task
        .clone()
        .ok_or_else(|| "'task' is required for action='dispatch'".to_string())?;
    let (flow_id, run_dir, created) = resolve_or_create_flow(&params, &task)?;
    let injection = inject_meta_skill("dispatch", &run_dir);

    let instruction = build_instruction_md(
        &flow_id,
        "dispatch",
        &task,
        &injection,
        params.notes.as_deref(),
        &params.validation,
        &params.allowed_scope,
    );
    let instr_path = run_dir.join("instruction.md");
    std::fs::write(&instr_path, &instruction).map_err(|e| format!("write instruction.md: {e}"))?;

    advance_stage(&run_dir, &flow_id, "dispatch", &task, &injection, created)?;

    // Phase 4 hook: optionally invoke the existing async dispatcher.
    let mut dispatch_id: Option<String> = None;
    let mut dispatch_error: Option<String> = None;
    let mut async_fired = false;
    if params.async_dispatch {
        let agent = params.agent.clone().unwrap_or_else(|| "claude".to_string());
        // Prefix the subagent prompt with a pointer to the instruction packet
        // so the clanker reads from disk rather than chat context.
        let prompt = format!(
            "You are executing Tachi flow `{flow_id}`, stage `dispatch`.\n\n\
             Read and follow these injected SOP files before changing code:\n\
             - {injected}\n\n\
             Then read the full instruction packet:\n\
             - {instr}\n\n\
             Original task:\n\n{task}\n",
            flow_id = flow_id,
            injected = injection.injected_path.as_deref().unwrap_or("(none)"),
            instr = instr_path.to_string_lossy(),
            task = task,
        );
        let dp = TachiDispatchParams {
            agent,
            task: prompt,
            cwd: params.cwd.clone(),
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
            project: params.project.clone(),
            stage: Some("execute".to_string()),
        };
        match crate::dispatch_ops::handle_tachi_dispatch(server, dp).await {
            Ok(s) => {
                async_fired = true;
                if let Ok(v) = serde_json::from_str::<Value>(&s) {
                    if let Some(d) = v.get("dispatch_id").and_then(|d| d.as_str()) {
                        dispatch_id = Some(d.to_string());
                    }
                }
                // Append dispatch_id to flow status
                if let Some(d) = dispatch_id.as_deref() {
                    let mut status = read_status(&run_dir);
                    let arr = status
                        .as_object_mut()
                        .map(|o| o.entry("dispatch_ids").or_insert_with(|| json!([])));
                    if let Some(v) = arr {
                        if let Some(a) = v.as_array_mut() {
                            a.push(json!(d));
                        }
                    }
                    let _ = write_status(&run_dir, &status);
                    let _ = append_event(
                        &run_dir,
                        json!({
                            "event": "dispatch_spawned",
                            "flow_id": flow_id,
                            "dispatch_id": d,
                            "timestamp": Utc::now().to_rfc3339(),
                        }),
                    );
                }
            }
            Err(e) => {
                dispatch_error = Some(e);
            }
        }
    }

    let resp = json!({
        "flow_id": flow_id,
        "stage": "dispatch",
        "run_dir": run_dir.to_string_lossy(),
        "instruction_path": instr_path.to_string_lossy(),
        "injected_skill": injection_to_json(&injection),
        "async": async_fired,
        "dispatch_id": dispatch_id,
        "dispatch_error": dispatch_error,
        "created": created,
    });
    serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))
}

async fn handle_kanban_action(
    server: &MemoryServer,
    params: TachiShellParams,
) -> Result<String, String> {
    let bp = TachiBoardParams {
        state_filter: params.state_filter.clone(),
        limit: params.limit,
        project: params.project.clone(),
    };
    crate::dispatch_ops::handle_tachi_board(server, bp).await
}

async fn handle_status_action(params: TachiShellParams) -> Result<String, String> {
    let runs_root = shell_runs_root();
    if let Some(flow_id) = params.flow_id.as_deref() {
        validate_flow_id(flow_id)?;
        let run_dir = runs_root.join(flow_id);
        if !run_dir.exists() {
            return serde_json::to_string(&json!({
                "flow_id": flow_id,
                "found": false,
                "message": "no run directory exists for this flow_id",
            }))
            .map_err(|e| format!("serialize: {e}"));
        }
        let status = read_status(&run_dir);
        return serde_json::to_string(&json!({
            "flow_id": flow_id,
            "found": true,
            "run_dir": run_dir.to_string_lossy(),
            "status": status,
        }))
        .map_err(|e| format!("serialize: {e}"));
    }
    // List recent flows
    let limit = params.limit.unwrap_or(20);
    let mut flows = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(&runs_root) {
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("flow_") {
                continue;
            }
            let status = read_status(&entry.path());
            flows.push(json!({
                "flow_id": name,
                "stage": status.get("stage"),
                "state": status.get("state"),
                "updated_at": status.get("updated_at"),
            }));
        }
    }
    flows.sort_by(|a, b| {
        let ta = a.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        let tb = b.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        tb.cmp(ta)
    });
    flows.truncate(limit);
    serde_json::to_string(&json!({
        "flows": flows,
        "count": flows.len(),
        "runs_root": runs_root.to_string_lossy(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}

// ─── Helpers: flow lifecycle ─────────────────────────────────────────────────

fn resolve_or_create_flow(
    params: &TachiShellParams,
    task: &str,
) -> Result<(String, PathBuf, bool), String> {
    let runs_root = shell_runs_root();
    std::fs::create_dir_all(&runs_root).map_err(|e| format!("create runs root: {e}"))?;
    if let Some(fid) = params.flow_id.clone() {
        validate_flow_id(&fid)?;
        let run_dir = runs_root.join(&fid);
        let created = !run_dir.exists();
        std::fs::create_dir_all(&run_dir).map_err(|e| format!("create flow run dir: {e}"))?;
        std::fs::create_dir_all(run_dir.join("artifacts")).ok();
        return Ok((fid, run_dir, created));
    }
    let fid = new_flow_id(params.title.as_deref(), Some(task));
    let run_dir = runs_root.join(&fid);
    std::fs::create_dir_all(&run_dir).map_err(|e| format!("create flow run dir: {e}"))?;
    std::fs::create_dir_all(run_dir.join("artifacts")).ok();
    Ok((fid, run_dir, true))
}

fn injection_to_json(inj: &InjectionResult) -> Value {
    json!({
        "required": inj.required,
        "rel_path": inj.rel_path,
        "source_path": inj.source_path,
        "injected_path": inj.injected_path,
        "content_hash": inj.content_hash,
        "loaded": inj.loaded,
        "warning": inj.warning,
    })
}

fn advance_stage(
    run_dir: &Path,
    flow_id: &str,
    stage: &str,
    task: &str,
    injection: &InjectionResult,
    created: bool,
) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();
    let mut status = read_status(run_dir);
    let obj = status.as_object_mut();
    let mut new_status: serde_json::Map<String, Value> = match obj {
        Some(o) => o.clone(),
        None => serde_json::Map::new(),
    };
    if created || !new_status.contains_key("flow_id") {
        new_status.insert("flow_id".into(), json!(flow_id));
        new_status.insert("created_at".into(), json!(now.clone()));
        new_status.insert("task".into(), json!(task));
        new_status.insert("dispatch_ids".into(), json!([]));
        new_status.insert("history".into(), json!([]));
    }
    let prev_stage = new_status
        .get("stage")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    new_status.insert("stage".into(), json!(stage));
    new_status.insert("state".into(), json!(stage_state_for(stage)));
    new_status.insert("updated_at".into(), json!(now.clone()));
    new_status.insert(
        "injected".into(),
        json!({
            "stage": stage,
            "rel_path": injection.rel_path,
            "injected_path": injection.injected_path,
            "content_hash": injection.content_hash,
            "loaded": injection.loaded,
            "warning": injection.warning,
        }),
    );
    if let Some(arr) = new_status.get_mut("history").and_then(|v| v.as_array_mut()) {
        arr.push(json!({
            "stage": stage,
            "from": prev_stage,
            "at": now,
        }));
    }
    write_status(run_dir, &Value::Object(new_status))?;
    let event_kind = if created {
        "flow_created"
    } else {
        "stage_entered"
    };
    append_event(
        run_dir,
        json!({
            "event": event_kind,
            "flow_id": flow_id,
            "stage": stage,
            "from_stage": prev_stage,
            "timestamp": now,
            "injected": injection.injected_path,
        }),
    )?;
    let _ = STAGE_ACTIONS; // touch to silence dead-code if list trims later
    Ok(())
}

fn stage_state_for(stage: &str) -> &'static str {
    match stage {
        "brainstorm" | "plan" | "review" | "ship" => "instruction_ready",
        "dispatch" => "dispatch_ready",
        _ => "unknown",
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_runs_root() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "tachi-shell-test-{}",
            Utc::now().format("%Y%m%dT%H%M%S%fZ")
        ));
        std::fs::create_dir_all(&d).unwrap();
        // SAFETY: tests in this module are serialized by setting/unsetting
        // env vars sequentially within a single test. CI runs cargo test
        // single-threaded by default for env-coupled tests is not guaranteed,
        // so we keep each test self-contained and use unique dirs.
        unsafe {
            std::env::set_var("TACHI_RUN_ROOT", &d);
        }
        d
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("   "), "flow");
        assert_eq!(slugify("已经-OK_test"), "ok-test");
    }

    #[test]
    fn meta_skill_mapping_is_complete() {
        for stage in &["brainstorm", "plan", "dispatch", "review", "ship"] {
            assert!(meta_skill_for_stage(stage).is_some(), "stage {stage}");
        }
        assert!(meta_skill_for_stage("kanban").is_none());
        assert!(meta_skill_for_stage("status").is_none());
    }

    #[test]
    fn build_instruction_includes_required_sections() {
        let inj = InjectionResult {
            required: true,
            rel_path: Some("skill/x/SKILL.md".into()),
            source_path: Some("/abs/skill/x/SKILL.md".into()),
            injected_path: Some(".tachi/runs/flow_x/injected/superpowers-plan.md".into()),
            content_hash: Some("a".repeat(16)),
            loaded: true,
            warning: None,
        };
        let s = build_instruction_md(
            "flow_x",
            "plan",
            "do the thing",
            &inj,
            Some("be careful"),
            &["cargo test".to_string()],
            &["crates/memory-server/**".to_string()],
        );
        assert!(s.contains("flow_x"));
        assert!(s.contains("Stage: **plan**"));
        assert!(s.contains("do the thing"));
        assert!(s.contains("superpowers-plan.md"));
        assert!(s.contains("cargo test"));
        assert!(s.contains("crates/memory-server/**"));
        assert!(s.contains("be careful"));
    }

    #[test]
    fn flow_id_is_well_formed() {
        let id = new_flow_id(Some("Refactor Tachi Shell"), None);
        assert!(id.starts_with("flow_"));
        assert!(id.contains("refactor-tachi-shell"));
    }

    #[test]
    fn resolve_or_create_flow_creates_dir() {
        let _root = temp_runs_root();
        let p = TachiShellParams {
            action: "plan".into(),
            flow_id: None,
            task: Some("hello".into()),
            title: Some("hello".into()),
            agent: None,
            cwd: None,
            async_dispatch: false,
            project: None,
            state_filter: None,
            limit: None,
            notes: None,
            validation: vec![],
            allowed_scope: vec![],
        };
        let (fid, dir, created) = resolve_or_create_flow(&p, "hello").unwrap();
        assert!(created);
        assert!(dir.exists());
        assert!(fid.starts_with("flow_"));
    }

    #[test]
    fn advance_stage_writes_status_and_events() {
        let _root = temp_runs_root();
        let p = TachiShellParams {
            action: "plan".into(),
            flow_id: None,
            task: Some("t".into()),
            title: Some("t".into()),
            agent: None,
            cwd: None,
            async_dispatch: false,
            project: None,
            state_filter: None,
            limit: None,
            notes: None,
            validation: vec![],
            allowed_scope: vec![],
        };
        let (fid, dir, _created) = resolve_or_create_flow(&p, "t").unwrap();
        let inj = InjectionResult {
            required: true,
            rel_path: Some("skill/x".into()),
            source_path: None,
            injected_path: None,
            content_hash: None,
            loaded: false,
            warning: Some("missing".into()),
        };
        advance_stage(&dir, &fid, "plan", "t", &inj, true).unwrap();
        let status = read_status(&dir);
        assert_eq!(status.get("stage").and_then(|v| v.as_str()), Some("plan"));
        assert_eq!(
            status.get("state").and_then(|v| v.as_str()),
            Some("instruction_ready")
        );
        let events = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
        assert!(events.contains("flow_created"));
    }

    #[test]
    fn flow_id_rejects_path_traversal() {
        for invalid in [
            "../../etc",
            "flow_../../etc",
            "/tmp/evil",
            "flow_/tmp/evil",
            "flow_..",
            "flow_bad/name",
            "flow_bad\\name",
            "notflow_20260505",
        ] {
            assert!(
                validate_flow_id(invalid).is_err(),
                "expected invalid flow_id to be rejected: {invalid}"
            );
        }
        assert!(validate_flow_id("flow_20260505T000000Z_demo-1").is_ok());
    }

    #[tokio::test]
    async fn status_action_returns_not_found_for_missing_flow() {
        let _root = temp_runs_root();
        let p = TachiShellParams {
            action: "status".into(),
            flow_id: Some("flow_does_not_exist".into()),
            task: None,
            title: None,
            agent: None,
            cwd: None,
            async_dispatch: false,
            project: None,
            state_filter: None,
            limit: None,
            notes: None,
            validation: vec![],
            allowed_scope: vec![],
        };
        let out = handle_status_action(p).await.unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v.get("found").and_then(|x| x.as_bool()), Some(false));
    }

    #[tokio::test]
    async fn status_action_lists_known_flows() {
        let root = temp_runs_root();
        // Pre-create a flow dir + status.json
        let fdir = root.join("flow_20260505T000000Z_demo");
        std::fs::create_dir_all(&fdir).unwrap();
        std::fs::write(
            fdir.join("status.json"),
            r#"{"flow_id":"flow_20260505T000000Z_demo","stage":"plan","state":"instruction_ready","updated_at":"2026-05-05T00:00:00Z"}"#,
        )
        .unwrap();
        let p = TachiShellParams {
            action: "status".into(),
            flow_id: None,
            task: None,
            title: None,
            agent: None,
            cwd: None,
            async_dispatch: false,
            project: None,
            state_filter: None,
            limit: Some(10),
            notes: None,
            validation: vec![],
            allowed_scope: vec![],
        };
        let out = handle_status_action(p).await.unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let flows = v
            .get("flows")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            flows
                .iter()
                .any(|f| f.get("flow_id").and_then(|x| x.as_str())
                    == Some("flow_20260505T000000Z_demo")),
            "expected demo flow in listing, got {v}"
        );
    }
}
