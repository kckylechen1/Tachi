//! Progress tracking and filesystem utilities for `tachi_memory(action="progress")`.

use super::evidence_format::{format_agent_status, json_string, wants_json};
use crate::tool_params::*;
use crate::MemoryServer;
use chrono::Utc;
use serde_json::json;
use std::path::{Path, PathBuf};

pub(crate) async fn handle_memory_progress(
    _server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let flow_id = params
        .flow_id
        .clone()
        .unwrap_or_else(|| format!("memory_{}", Utc::now().format("%Y%m%d")));
    let event = params
        .event
        .clone()
        .unwrap_or_else(|| "progress".to_string());
    let raw_text = params
        .text
        .clone()
        .or_else(|| params.summary.clone())
        .ok_or_else(|| "text or summary is required when action='progress'".to_string())?;
    let (text, redactions) = crate::memory_search_ops::scrub_secrets(&raw_text);
    let run_dir = progress_run_dir(&flow_id)?;
    let now = Utc::now().to_rfc3339();
    let line = json!({
        "timestamp": now,
        "flow_id": flow_id,
        "event": event,
        "state": params.state,
        "title": params.title,
        "summary": params.summary,
        "text": text,
        "project": params.project,
        "domain": params.domain,
        "secret_redactions": redactions,
    });
    append_jsonl(&run_dir.join("progress.jsonl"), &line)?;
    update_progress_status(&run_dir, &line)?;
    if wants_json(params.format.as_deref()) {
        return json_string(&json!({
            "status": "recorded",
            "flow_id": flow_id,
            "log": run_dir.join("progress.jsonl").display().to_string(),
            "secret_redactions": redactions,
            "event": line,
        }));
    }
    Ok(format_agent_status(
        "Tachi progress",
        &[
            ("status", "recorded".to_string()),
            ("flow", flow_id),
            ("log", run_dir.join("progress.jsonl").display().to_string()),
            ("secret_redactions", redactions.to_string()),
        ],
        None,
        None,
    ))
}

fn validate_progress_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("Invalid flow_id: '{id}'"));
    }
    Ok(())
}

fn progress_run_dir(flow_id: &str) -> Result<PathBuf, String> {
    validate_progress_id(flow_id)?;
    let root = crate::shell_ops::shell_runs_root();
    let run_dir = root.join(flow_id);
    std::fs::create_dir_all(&run_dir).map_err(|e| format!("create progress run dir: {e}"))?;
    Ok(run_dir)
}

fn append_jsonl(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let line =
        serde_json::to_string(value).map_err(|e| format!("serialize progress event: {e}"))?;
    crate::utils::append_owner_only_jsonl_line(path, &line)
}

fn update_progress_status(run_dir: &Path, line: &serde_json::Value) -> Result<(), String> {
    let status_path = run_dir.join("status.json");
    with_progress_status_lock(&status_path, || {
        let mut status = std::fs::read_to_string(&status_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .unwrap_or_else(|| json!({}));
        if !status.is_object() {
            status = json!({});
        }
        let obj = status
            .as_object_mut()
            .ok_or_else(|| "progress status was not a JSON object".to_string())?;
        obj.insert("flow_id".to_string(), line["flow_id"].clone());
        obj.insert("updated_at".to_string(), line["timestamp"].clone());
        obj.insert("last_event".to_string(), line["event"].clone());
        if !line["state"].is_null() {
            obj.insert("state".to_string(), line["state"].clone());
        }
        if !line["title"].is_null() {
            obj.insert("title".to_string(), line["title"].clone());
        }
        let body =
            serde_json::to_string_pretty(&status).map_err(|e| format!("serialize status: {e}"))?;
        crate::utils::write_owner_only_file_atomic(&status_path, body.as_bytes())
    })
}

#[cfg(unix)]
fn with_progress_status_lock<T>(
    status_path: &Path,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    use std::fs::OpenOptions;
    use std::os::unix::io::AsRawFd;

    let lock_path = status_path.with_extension("json.lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("open lock {}: {e}", lock_path.display()))?;
    let fd = lock_file.as_raw_fd();
    // SAFETY: `fd` is borrowed from a valid open `File`. `flock(LOCK_EX)` only
    // passes integer flags to the OS and does not dereference Rust pointers.
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
    if rc != 0 {
        return Err(format!(
            "flock {}: {}",
            lock_path.display(),
            std::io::Error::last_os_error()
        ));
    }
    let result = f();
    // SAFETY: `fd` is still valid — the lock file remains in scope. `flock(LOCK_UN)`
    // is a pure kernel operation; failure is non-fatal because the descriptor close
    // will release the lock anyway.
    unsafe {
        libc::flock(fd, libc::LOCK_UN);
    }
    result
}

#[cfg(not(unix))]
fn with_progress_status_lock<T>(
    _status_path: &Path,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    f()
}
