use super::*;

// ─── Run-root resolution ─────────────────────────────────────────────────────

pub(super) fn cached_git_root() -> Option<&'static PathBuf> {
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

pub(super) fn slugify(s: &str) -> String {
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

pub(super) fn new_flow_id(title: Option<&str>, task: Option<&str>) -> String {
    let now = Utc::now();
    let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let basis = title
        .or(task)
        .map(|s| s.to_string())
        .unwrap_or_else(|| "flow".to_string());
    let suffix = uuid::Uuid::new_v4().as_simple().to_string()[..8].to_string();
    format!("flow_{}_{}_{}", stamp, slugify(&basis), suffix)
}

pub(crate) fn validate_flow_id(id: &str) -> Result<(), String> {
    if !id.starts_with("flow_")
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "Invalid flow_id: '{}'. Expected a safe id starting with 'flow_' and containing only ASCII letters, numbers, '_' or '-'. Example: flow_20260609T014037Z_tachi_dispatch_ux_smoke",
            id
        ));
    }
    Ok(())
}

pub(crate) fn run_dir_for_flow_id(flow_id: &str) -> Result<PathBuf, String> {
    validate_flow_id(flow_id)?;
    Ok(shell_runs_root().join(flow_id))
}

/// Cross-flow closure-debt scan. Walks every flow run dir and surfaces:
///   - `unclosed_loop`: work produced a `result.md` but close_loop never ran
///     (issue/PR not written back, lesson not sunk, spec drift not flagged), and
///   - `spec_drift`: a flow that closed with an unresolved spec advisory
///     (docs referenced but no spec recorded — the canonical spec may be stale).
///
/// This is the session-start safety net for an agent's cross-session
/// forgetfulness: a per-flow briefing only sees the flow in scope, which is
/// exactly when a reminder is NOT needed. Output is capped at `limit`; if more
/// debt exists, a final summary item reports the overflow (never a silent cap).
pub(crate) fn scan_open_loops(limit: usize) -> Vec<Value> {
    let runs_root = shell_runs_root();
    let Ok(entries) = std::fs::read_dir(&runs_root) else {
        return Vec::new();
    };
    let mut debts = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let flow_id = entry.file_name().to_string_lossy().to_string();
        let close_loop_path = dir.join("close_loop.json");
        let has_close_loop = close_loop_path.exists();
        if dir.join("result.md").exists() && !has_close_loop {
            debts.push(json!({
                "kind": "unclosed_loop",
                "flow_id": flow_id,
                "detail": "Flow produced a result but close_loop has not run: issue/PR not written back, lesson not sunk to wiki, spec drift not flagged.",
                "action": format!("tachi_task(action='close_loop', flow_id='{flow_id}')"),
                "authority": "closure_debt",
            }));
        } else if has_close_loop {
            let spec_unresolved = std::fs::read_to_string(&close_loop_path)
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|v| {
                    v.get("closure_actions")
                        .and_then(|c| c.get("spec_advisory"))
                        .and_then(|s| s.get("status"))
                        .and_then(Value::as_str)
                        .map(|status| status == "advisory")
                })
                .unwrap_or(false);
            if spec_unresolved {
                debts.push(json!({
                    "kind": "spec_drift",
                    "flow_id": flow_id,
                    "detail": "Loop closed with docs referenced but no spec recorded — the canonical spec may be stale.",
                    "action": "Update the canonical spec, then re-run close_loop with spec_paths once corrected.",
                    "authority": "closure_debt",
                }));
            }
        }
    }
    debts.sort_by(|a, b| {
        let a_id = a.get("flow_id").and_then(Value::as_str).unwrap_or("");
        let b_id = b.get("flow_id").and_then(Value::as_str).unwrap_or("");
        a_id.cmp(b_id)
    });
    if debts.len() > limit {
        let overflow = debts.len() - limit;
        debts.truncate(limit);
        debts.push(json!({
            "kind": "more",
            "detail": format!("{overflow} more closure-debt item(s) not shown (showing {limit})."),
        }));
    }
    debts
}

pub(super) fn validate_slice_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("Invalid convoy slice id: '{}'", id));
    }
    Ok(())
}

// ─── Status / events helpers ─────────────────────────────────────────────────

pub(super) fn read_status(run_dir: &Path) -> Value {
    let status_path = run_dir.join("status.json");
    match std::fs::read_to_string(&status_path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|err| {
            tracing::warn!(
                path = %status_path.display(),
                error = %err,
                "shell status JSON parse failed; continuing with empty status"
            );
            json!({})
        }),
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %status_path.display(),
                    error = %err,
                    "shell status read failed; continuing with empty status"
                );
            }
            json!({})
        }
    }
}

pub(super) async fn read_status_async(run_dir: &Path) -> Value {
    let status_path = run_dir.join("status.json");
    match tokio::fs::read_to_string(&status_path).await {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|err| {
            tracing::warn!(
                path = %status_path.display(),
                error = %err,
                "shell status JSON parse failed; continuing with empty status"
            );
            json!({})
        }),
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %status_path.display(),
                    error = %err,
                    "shell status read failed; continuing with empty status"
                );
            }
            json!({})
        }
    }
}

// ─── Helpers: flow lifecycle ─────────────────────────────────────────────────

pub(super) fn resolve_or_create_flow(
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
        std::fs::create_dir_all(run_dir.join("artifacts"))
            .map_err(|e| format!("create flow artifacts dir: {e}"))?;
        return Ok((fid, run_dir, created));
    }
    let fid = new_flow_id(params.title.as_deref(), Some(task));
    let run_dir = runs_root.join(&fid);
    std::fs::create_dir_all(&run_dir).map_err(|e| format!("create flow run dir: {e}"))?;
    std::fs::create_dir_all(run_dir.join("artifacts"))
        .map_err(|e| format!("create flow artifacts dir: {e}"))?;
    Ok((fid, run_dir, true))
}

pub(super) fn injection_to_json(inj: &InjectionResult) -> Value {
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

pub(super) fn advance_stage(
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
        new_status.insert("created_at".into(), json!(now));
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
    new_status.insert("updated_at".into(), json!(now));
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
    crate::utils::write_run_status_file(run_dir, &Value::Object(new_status))?;
    let event_kind = if created {
        "flow_created"
    } else {
        "stage_entered"
    };
    crate::utils::append_run_event(
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

/// Global test lock for `TACHI_RUN_ROOT` env var mutations.
/// All test modules that set this env var must acquire this lock to avoid races.
#[cfg(test)]
pub(crate) fn tachi_run_root_env_lock() -> &'static std::sync::Mutex<()> {
    crate::utils::global_test_lock()
}
