use super::*;

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
        "failure_class": inj.failure_class,
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
    Ok(())
}

fn stage_state_for(stage: &str) -> &'static str {
    match stage {
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
