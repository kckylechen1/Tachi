use super::*;

pub(crate) fn new_dispatch_id(now: chrono::DateTime<Utc>, agent: &str) -> String {
    let timestamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let sanitized = agent.replace(|c: char| !c.is_ascii_alphanumeric(), "-");
    let suffix = uuid::Uuid::new_v4().as_simple().to_string()[..8].to_string();
    format!("{}-{}-{}", timestamp, sanitized, suffix)
}

pub(super) fn dispatch_runs_root() -> PathBuf {
    crate::path_utils::tachi_home().join("runs")
}

pub(super) fn dispatch_status_is_terminal(dispatch_id: &str) -> bool {
    let status_path = dispatch_runs_root().join(dispatch_id).join("status.json");
    let Ok(Some(status)) = crate::task_lifecycle::read_json_file(&status_path) else {
        return false;
    };
    let state = status
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    matches!(
        state,
        "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED"
    ) || status.get("exit_code").is_some()
}

pub(super) fn dispatch_dedupe_lock_is_stale(
    existing: &serde_json::Value,
    dispatch_id: &str,
) -> bool {
    let status_path = dispatch_runs_root().join(dispatch_id).join("status.json");
    if status_path.exists() {
        return false;
    }
    let Some(created_at) = existing
        .get("created_at")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let Ok(created_at) = chrono::DateTime::parse_from_rfc3339(created_at) else {
        return false;
    };
    Utc::now().signed_duration_since(created_at.with_timezone(&Utc))
        > chrono::Duration::seconds(DISPATCH_DEDUPE_STALE_LOCK_SECS)
}

pub(super) fn dispatch_dedupe_lock_file_is_stale(lock_path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(lock_path) else {
        return true;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    let Ok(age) = std::time::SystemTime::now().duration_since(modified) else {
        return false;
    };
    age > std::time::Duration::from_secs(DISPATCH_DEDUPE_STALE_LOCK_SECS as u64)
}

pub(super) fn dispatch_dedupe_root() -> PathBuf {
    dispatch_runs_root().join(".dispatch-dedupe")
}

pub(super) fn reserve_dispatch_dedupe_lock(
    lock_dir: &Path,
    scope: &str,
    task: &str,
    dispatch_id: &str,
    flow_id: Option<&str>,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(lock_dir).map_err(|e| format!("create dispatch dedupe dir: {e}"))?;
    let task_hash = crate::utils::stable_hash(task);
    let lock_path = lock_dir.join(format!("{task_hash}.json"));
    let mut payload = json!({
        "scope": scope,
        "task_hash": task_hash,
        "dispatch_id": dispatch_id,
        "task": task,
        "created_at": Utc::now().to_rfc3339(),
    });
    if let Some(flow_id) = flow_id {
        payload["flow_id"] = json!(flow_id);
    }
    let payload =
        serde_json::to_vec_pretty(&payload).map_err(|e| format!("serialize dedupe lock: {e}"))?;

    let mut lock_options = std::fs::OpenOptions::new();
    lock_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        lock_options.mode(0o600);
    }

    match lock_options.open(&lock_path) {
        Ok(mut file) => {
            use std::io::Write;
            let write_result = (|| -> Result<(), String> {
                file.write_all(&payload)
                    .map_err(|e| format!("write dispatch dedupe lock: {e}"))?;
                file.sync_all()
                    .map_err(|e| format!("fsync dispatch dedupe lock: {e}"))?;
                crate::utils::sync_parent_dir(&lock_path)
            })();
            if let Err(err) = write_result {
                let _ = std::fs::remove_file(&lock_path);
                return Err(err);
            }
            Ok(lock_path)
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = match crate::task_lifecycle::read_json_file(&lock_path) {
                Ok(Some(existing)) => existing,
                Ok(None) => json!({}),
                Err(_) if dispatch_dedupe_lock_file_is_stale(&lock_path) => {
                    let _ = std::fs::remove_file(&lock_path);
                    return reserve_dispatch_dedupe_lock(
                        lock_dir,
                        scope,
                        task,
                        dispatch_id,
                        flow_id,
                    );
                }
                Err(err) => return Err(err),
            };
            let existing_dispatch_id = existing
                .get("dispatch_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("<unknown>");
            if dispatch_status_is_terminal(existing_dispatch_id) {
                let _ = std::fs::remove_file(&lock_path);
                return reserve_dispatch_dedupe_lock(lock_dir, scope, task, dispatch_id, flow_id);
            }
            if dispatch_dedupe_lock_is_stale(&existing, existing_dispatch_id) {
                let _ = std::fs::remove_file(&lock_path);
                return reserve_dispatch_dedupe_lock(lock_dir, scope, task, dispatch_id, flow_id);
            }
            let scope_label = flow_id.unwrap_or("global");
            Err(format!(
                "duplicate dispatch blocked for {scope_label} scope and same task; active dispatch_id: {existing_dispatch_id}"
            ))
        }
        Err(err) => Err(format!("create dispatch dedupe lock: {err}")),
    }
}

pub(super) fn reserve_flow_dispatch_slot(
    flow_id: Option<&str>,
    task: &str,
    dispatch_id: &str,
) -> Result<Option<PathBuf>, String> {
    let Some(flow_id) = flow_id.filter(|id| !id.trim().is_empty()) else {
        return Ok(None);
    };
    let Ok(run_dir) = crate::shell_ops::run_dir_for_flow_id(flow_id) else {
        return Ok(None);
    };
    let lock_dir = run_dir.join(".dispatch-dedupe");
    reserve_dispatch_dedupe_lock(&lock_dir, "flow", task, dispatch_id, Some(flow_id)).map(Some)
}

pub(super) fn reserve_global_dispatch_slot(
    task: &str,
    dispatch_id: &str,
) -> Result<PathBuf, String> {
    reserve_dispatch_dedupe_lock(&dispatch_dedupe_root(), "global", task, dispatch_id, None)
}

pub(super) fn reserve_dispatch_slot(
    flow_id: Option<&str>,
    task: &str,
    dispatch_id: &str,
) -> Result<Option<PathBuf>, String> {
    if flow_id.filter(|id| !id.trim().is_empty()).is_some() {
        reserve_flow_dispatch_slot(flow_id, task, dispatch_id)
    } else {
        reserve_global_dispatch_slot(task, dispatch_id).map(Some)
    }
}

pub(super) fn release_flow_dispatch_slot(lock_path: Option<PathBuf>) {
    if let Some(path) = lock_path {
        let _ = std::fs::remove_file(path);
    }
}
