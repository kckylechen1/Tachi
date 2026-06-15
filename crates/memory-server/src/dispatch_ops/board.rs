use super::*;
use chrono::{DateTime, Duration as ChronoDuration};
use serde_json::Value;
use std::path::Path;

// ─── Task Board (Kanban) handler ──────────────────────────────────────────────

const RUN_STALE_FALLBACK_SECS: i64 = 30 * 60;
const RUN_STALE_GRACE_SECS: i64 = 60;
const RUN_STALE_MAX_TIMEOUT_SECS: i64 = 30 * 24 * 60 * 60;

fn tachi_home() -> PathBuf {
    if let Ok(home) = std::env::var("TACHI_HOME") {
        PathBuf::from(home)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".tachi")
    } else {
        std::env::temp_dir().join("tachi")
    }
}

fn runs_dir_for_server(server: &crate::MemoryServer) -> PathBuf {
    let global_db = server.global_db_path_buf();
    if global_db.file_name().and_then(|name| name.to_str()) == Some("memory.db") {
        if let Some(global_dir) = global_db.parent() {
            if global_dir.file_name().and_then(|name| name.to_str()) == Some("global") {
                if let Some(app_home) = global_dir.parent() {
                    return app_home.join("runs");
                }
            }
        }
    }
    tachi_home().join("runs")
}

fn map_filter_state(state_filter: &str) -> &str {
    match state_filter {
        "working" => "TASK_STATE_WORKING",
        "completed" => "TASK_STATE_COMPLETED",
        "failed" => "TASK_STATE_FAILED",
        "pending" => "TASK_STATE_PENDING",
        "input_required" => "TASK_STATE_INPUT_REQUIRED",
        "canceled" => "TASK_STATE_CANCELED",
        other => other,
    }
}

fn status_state(status: &serde_json::Value, result_written: bool) -> &'static str {
    if let Some(state) = status.get("state").and_then(|s| s.as_str()) {
        return match state {
            "TASK_STATE_COMPLETED" => "TASK_STATE_COMPLETED",
            "TASK_STATE_FAILED" => "TASK_STATE_FAILED",
            "TASK_STATE_PENDING" => "TASK_STATE_PENDING",
            "TASK_STATE_INPUT_REQUIRED" => "TASK_STATE_INPUT_REQUIRED",
            "TASK_STATE_CANCELED" => "TASK_STATE_CANCELED",
            _ => "TASK_STATE_WORKING",
        };
    }
    if status.get("plan_review_status").and_then(|s| s.as_str()) == Some("pending_review") {
        return "TASK_STATE_INPUT_REQUIRED";
    }
    match status.get("exit_code") {
        Some(serde_json::Value::Number(n)) if n.as_i64() == Some(0) => "TASK_STATE_COMPLETED",
        Some(serde_json::Value::Number(_)) => "TASK_STATE_FAILED",
        _ if result_written => "TASK_STATE_COMPLETED",
        _ => "TASK_STATE_WORKING",
    }
}

fn status_timeout_secs(status: &Value) -> Option<i64> {
    status
        .get("timeout_secs")
        .and_then(Value::as_i64)
        .filter(|secs| *secs > 0)
        .map(|secs| secs.min(RUN_STALE_MAX_TIMEOUT_SECS))
}

fn stale_after_secs(status: &Value) -> i64 {
    status_timeout_secs(status)
        .map(|secs| secs.saturating_add(RUN_STALE_GRACE_SECS))
        .unwrap_or(RUN_STALE_FALLBACK_SECS)
}

fn parse_status_updated_at(status: &Value, status_path: &Path) -> Option<DateTime<Utc>> {
    status
        .get("updated_at")
        .and_then(Value::as_str)
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|| {
            std::fs::metadata(status_path)
                .ok()
                .and_then(|m| m.modified().ok())
                .map(DateTime::<Utc>::from)
        })
}

fn is_unresolved_exit(status: &Value) -> bool {
    match status.get("exit_code") {
        Some(Value::Number(_)) => false,
        Some(Value::Null) | None => true,
        _ => true,
    }
}

fn is_abandoned_working_run(
    status: &Value,
    result_written: bool,
    updated_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    if status_state(status, result_written) != "TASK_STATE_WORKING" {
        return false;
    }
    if result_written || !is_unresolved_exit(status) {
        return false;
    }
    let Some(updated_at) = updated_at else {
        return false;
    };
    now.signed_duration_since(updated_at) > ChronoDuration::seconds(stale_after_secs(status))
}

fn dispatch_timestamp_key(name: &std::ffi::OsStr) -> Option<String> {
    let name = name.to_str()?;
    let bytes = name.as_bytes();
    if bytes.len() < 16 {
        return None;
    }
    for idx in 0..=bytes.len().saturating_sub(16) {
        let candidate = &bytes[idx..idx + 16];
        let valid = candidate[0..8].iter().all(u8::is_ascii_digit)
            && candidate[8] == b'T'
            && candidate[9..15].iter().all(u8::is_ascii_digit)
            && candidate[15] == b'Z';
        if valid {
            return Some(name[idx..idx + 16].to_string());
        }
    }
    None
}

fn collect_run_tasks_from_dir(
    runs_dir: PathBuf,
    state_filter: &str,
    limit: usize,
) -> Vec<serde_json::Value> {
    let Ok(read_dir) = std::fs::read_dir(&runs_dir) else {
        return Vec::new();
    };

    let mut entries = read_dir.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by(|a, b| {
        dispatch_timestamp_key(&b.file_name())
            .cmp(&dispatch_timestamp_key(&a.file_name()))
            .then_with(|| b.file_name().cmp(&a.file_name()))
    });

    let target_state = map_filter_state(state_filter);
    let mut runs = Vec::new();
    let now = Utc::now();

    for entry in entries {
        if runs.len() >= limit {
            break;
        }
        let run_dir = entry.path();
        if !run_dir.is_dir() {
            continue;
        }
        let status_path = run_dir.join("status.json");
        let Ok(status_raw) = std::fs::read_to_string(&status_path) else {
            continue;
        };
        let Ok(status) = serde_json::from_str::<serde_json::Value>(&status_raw) else {
            continue;
        };
        let Some(dispatch_id) = status
            .get("dispatch_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                run_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
            })
        else {
            continue;
        };
        let result_written = run_dir.join("result.md").exists();
        let updated_at_dt = parse_status_updated_at(&status, &status_path);
        let abandoned = is_abandoned_working_run(&status, result_written, updated_at_dt, now);
        let state = if abandoned {
            "TASK_STATE_FAILED"
        } else {
            status_state(&status, result_written)
        };
        if state_filter != "all" && state != target_state {
            continue;
        }
        let updated_at = status
            .get("updated_at")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                std::fs::metadata(&status_path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(chrono::DateTime::<Utc>::from)
                    .map(|dt| dt.to_rfc3339())
            });
        let stale_reason = if abandoned {
            Some(format!(
                "run ledger stayed WORKING for more than {}s without result.md or exit_code",
                stale_after_secs(&status)
            ))
        } else {
            None
        };
        runs.push(json!({
            "dispatch_id": dispatch_id,
            "agent": status.get("agent").cloned().unwrap_or(serde_json::Value::Null),
            "state": state,
            "exit_code": status.get("exit_code").cloned().unwrap_or(serde_json::Value::Null),
            "summary": status.get("task").cloned().unwrap_or(serde_json::Value::Null),
            "updated_at": updated_at,
            "run_dir": run_dir.to_string_lossy(),
            "result_written": result_written,
            "source": "run",
            "stale": abandoned,
            "stale_reason": stale_reason,
            "state_source": if abandoned { "run_stale_timeout" } else { "run" },
            "harness_transport": status.get("harness_transport").cloned().unwrap_or(serde_json::Value::Null),
            "harness_server_url": status.get("harness_server_url").cloned().unwrap_or(serde_json::Value::Null),
            "harness_server_status": super::probe_harness_server_status(status.get("harness_server_url").and_then(Value::as_str)),
            "execution_backend": status.get("execution_backend").cloned().unwrap_or(serde_json::Value::Null),
            "acpx": status.get("acpx").cloned().unwrap_or(serde_json::Value::Null),
            "acpx_events": status.get("acpx_events").cloned().unwrap_or(serde_json::Value::Null),
        }));
    }

    runs.sort_by(|a, b| {
        b.get("updated_at")
            .and_then(|v| v.as_str())
            .cmp(&a.get("updated_at").and_then(|v| v.as_str()))
    });
    runs
}

pub(crate) fn collect_run_task_for_server(
    server: &crate::MemoryServer,
    dispatch_id: &str,
) -> Option<serde_json::Value> {
    collect_run_task_by_id(&runs_dir_for_server(server), dispatch_id)
}

fn collect_run_task_by_id(runs_dir: &Path, dispatch_id: &str) -> Option<serde_json::Value> {
    let run_dir = runs_dir.join(dispatch_id);
    if !run_dir.is_dir() {
        return None;
    }
    collect_run_task_from_dir(&run_dir)
}

fn collect_run_task_from_dir(run_dir: &Path) -> Option<serde_json::Value> {
    let status_path = run_dir.join("status.json");
    let status_raw = std::fs::read_to_string(&status_path).ok()?;
    let status = serde_json::from_str::<serde_json::Value>(&status_raw).ok()?;
    let dispatch_id = status
        .get("dispatch_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            run_dir
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })?;
    let result_written = run_dir.join("result.md").exists();
    let updated_at_dt = parse_status_updated_at(&status, &status_path);
    let now = Utc::now();
    let abandoned = is_abandoned_working_run(&status, result_written, updated_at_dt, now);
    let state = if abandoned {
        "TASK_STATE_FAILED"
    } else {
        status_state(&status, result_written)
    };
    let updated_at = status
        .get("updated_at")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            std::fs::metadata(&status_path)
                .ok()
                .and_then(|m| m.modified().ok())
                .map(chrono::DateTime::<Utc>::from)
                .map(|dt| dt.to_rfc3339())
        });
    let stale_reason = if abandoned {
        Some(format!(
            "run ledger stayed WORKING for more than {}s without result.md or exit_code",
            stale_after_secs(&status)
        ))
    } else {
        None
    };
    Some(json!({
        "dispatch_id": dispatch_id,
        "agent": status.get("agent").cloned().unwrap_or(serde_json::Value::Null),
        "state": state,
        "exit_code": status.get("exit_code").cloned().unwrap_or(serde_json::Value::Null),
        "summary": status.get("task").cloned().unwrap_or(serde_json::Value::Null),
        "updated_at": updated_at,
        "run_dir": run_dir.to_string_lossy(),
        "result_written": result_written,
        "source": "run",
        "stale": abandoned,
        "stale_reason": stale_reason,
        "state_source": if abandoned { "run_stale_timeout" } else { "run" },
        "harness_transport": status.get("harness_transport").cloned().unwrap_or(serde_json::Value::Null),
        "harness_server_url": status.get("harness_server_url").cloned().unwrap_or(serde_json::Value::Null),
        "harness_server_status": super::probe_harness_server_status(status.get("harness_server_url").and_then(Value::as_str)),
        "execution_backend": status.get("execution_backend").cloned().unwrap_or(serde_json::Value::Null),
        "acpx": status.get("acpx").cloned().unwrap_or(serde_json::Value::Null),
        "acpx_events": status.get("acpx_events").cloned().unwrap_or(serde_json::Value::Null),
    }))
}

fn flow_dispatch_ids(flow_id: &str) -> Result<Vec<String>, String> {
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id)?;
    let status_path = run_dir.join("status.json");
    let Some(status) = crate::task_lifecycle::read_json_file(&status_path)? else {
        return Ok(Vec::new());
    };
    Ok(status
        .get("dispatch_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect())
}

fn merge_run_task(
    existing: &mut serde_json::Value,
    run_task: &serde_json::Value,
    authoritative_state: bool,
) {
    if let Some(obj) = existing.as_object_mut() {
        for key in [
            "run_dir",
            "result_written",
            "exit_code",
            "stale",
            "stale_reason",
            "state_source",
            "harness_transport",
            "harness_server_url",
            "harness_server_status",
            "execution_backend",
            "acpx",
            "acpx_events",
        ] {
            if obj.get(key).is_none() {
                obj.insert(
                    key.to_string(),
                    run_task
                        .get(key)
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                );
            }
        }
        if authoritative_state || run_task.get("stale").and_then(|v| v.as_bool()) == Some(true) {
            obj.insert(
                "state".to_string(),
                run_task
                    .get("state")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            );
            obj.insert(
                "updated_at".to_string(),
                run_task
                    .get("updated_at")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            );
        }
        obj.insert("source".to_string(), json!("kanban+run"));
    }
}

pub(crate) async fn handle_tachi_board(
    server: &MemoryServer,
    params: TachiBoardParams,
) -> Result<String, String> {
    let limit = params.limit.unwrap_or(20);
    let flow_filter = params
        .flow_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);

    let rows = crate::memory_search_ops::search_memory_rows(
        server,
        SearchMemoryParams {
            query: "kanban dispatch task".to_string(),
            query_vec: None,
            top_k: limit,
            path_prefix: Some("/kanban/tasks/".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: Some(0.7),
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            agent_role: None,
            project: params.project.clone(),
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: true,
        },
        false,
    )
    .await?;

    let state_filter = params.state_filter.as_deref().unwrap_or("all");

    // Build compact board view
    let mut tasks: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let meta = row.get("metadata").cloned().unwrap_or(json!({}));
            json!({
                "dispatch_id": meta.get("dispatch_id"),
                "agent": meta.get("agent"),
                "state": meta.get("a2a_state"),
                "eval_id": meta.get("eval_ledger_id"),
                "summary": row.get("summary"),
                "updated_at": meta.get("updated_at"),
                "source": "kanban",
            })
        })
        .collect();
    let kanban_count = tasks.len();
    let mut seen = std::collections::HashSet::new();
    for task in &tasks {
        if let Some(id) = task.get("dispatch_id").and_then(|v| v.as_str()) {
            seen.insert(id.to_string());
        }
    }
    let runs_dir = runs_dir_for_server(server);
    let mut flow_run_count = 0usize;
    if let Some(flow_id) = flow_filter.as_deref() {
        let flow_ids = flow_dispatch_ids(flow_id)?;
        let flow_id_set: std::collections::HashSet<String> = flow_ids.iter().cloned().collect();
        tasks.retain(|task| {
            task.get("dispatch_id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| flow_id_set.contains(id))
        });
        for dispatch_id in &flow_ids {
            if seen.contains(dispatch_id) {
                if let Some(run_task) = collect_run_task_by_id(&runs_dir, dispatch_id) {
                    flow_run_count += 1;
                    if let Some(existing) = tasks.iter_mut().find(|candidate| {
                        candidate.get("dispatch_id").and_then(|v| v.as_str())
                            == Some(dispatch_id.as_str())
                    }) {
                        merge_run_task(existing, &run_task, true);
                    }
                }
                continue;
            }
            if let Some(task) = collect_run_task_by_id(&runs_dir, dispatch_id) {
                flow_run_count += 1;
                seen.insert(dispatch_id.clone());
                tasks.push(task);
            }
        }
    }
    let run_scan_limit = limit.saturating_mul(5).max(50);
    let run_tasks = if flow_filter.is_some() {
        Vec::new()
    } else {
        tokio::task::spawn_blocking(move || {
            collect_run_tasks_from_dir(runs_dir, "all", run_scan_limit)
        })
        .await
        .unwrap_or_default()
    };
    let run_count = if flow_filter.is_some() {
        flow_run_count
    } else {
        run_tasks.len()
    };
    for task in run_tasks {
        let dispatch_id = task
            .get("dispatch_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if let Some(id) = dispatch_id.as_deref() {
            if seen.contains(id) {
                if let Some(existing) = tasks.iter_mut().find(|candidate| {
                    candidate.get("dispatch_id").and_then(|v| v.as_str()) == Some(id)
                }) {
                    merge_run_task(existing, &task, false);
                }
                continue;
            }
            seen.insert(id.to_string());
        }
        tasks.push(task);
    }
    if state_filter != "all" {
        let target_state = map_filter_state(state_filter);
        tasks.retain(|task| task.get("state").and_then(|v| v.as_str()) == Some(target_state));
    }
    tasks.sort_by(|a, b| {
        b.get("updated_at")
            .and_then(|v| v.as_str())
            .cmp(&a.get("updated_at").and_then(|v| v.as_str()))
    });
    tasks.truncate(limit);

    serde_json::to_string(&json!({
        "board": "kanban",
        "flow_id": flow_filter,
        "filter": state_filter,
        "count": tasks.len(),
        "kanban_count": kanban_count,
        "run_count": run_count,
        "tasks": tasks,
    }))
    .map_err(|e| format!("serialize board: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch_ops::probe_harness_server_status;
    use std::ffi::OsStr;
    use std::io::{Read, Write};

    const OPENCODE_DOC_FIXTURE: &str = r#"{
        "openapi":"3.1.0",
        "info":{"title":"opencode","version":"1.0.0"},
        "paths":{
            "/api/session":{"post":{}},
            "/api/session/{sessionID}/prompt":{"post":{}},
            "/api/session/{sessionID}/wait":{"post":{}},
            "/api/model":{"get":{}},
            "/api/provider":{"get":{}}
        }
    }"#;

    struct EnvRestore {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvRestore {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, old }
        }

        fn remove(key: &'static str) -> Self {
            let old = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, old }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            if let Some(old) = &self.old {
                std::env::set_var(self.key, old);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn spawn_opencode_doc_probe_server() -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe server");
        let port = listener.local_addr().expect("local addr").port();
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept probe");
                let mut buf = [0_u8; 1024];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let body = if request.starts_with("GET /doc ") {
                    OPENCODE_DOC_FIXTURE
                } else {
                    "<title>OpenCode</title>"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
        });
        (format!("http://127.0.0.1:{port}"), handle)
    }

    #[test]
    fn dispatch_timestamp_key_extracts_embedded_timestamp() {
        assert_eq!(
            dispatch_timestamp_key(OsStr::new("flow_20260606T151945Z_tachi")),
            Some("20260606T151945Z".to_string())
        );
        assert_eq!(
            dispatch_timestamp_key(OsStr::new("20260607T045032Z-codex-09802a7f")),
            Some("20260607T045032Z".to_string())
        );
        assert_eq!(dispatch_timestamp_key(OsStr::new("mcp-smoke-test")), None);
    }

    #[test]
    fn harness_probe_rejects_non_local_urls() {
        let status = probe_harness_server_status(Some("https://example.com:4321"));
        assert_eq!(status["reachable"], serde_json::Value::Null);
        assert_eq!(status["evidence_strength"], json!("none"));
    }

    #[test]
    fn harness_probe_labels_tcp_only_as_weak_evidence() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind local listener");
        let port = listener.local_addr().expect("local addr").port();

        let status = probe_harness_server_status(Some(&format!("http://127.0.0.1:{port}")));

        assert_eq!(status["reachable"], json!(true));
        assert_eq!(status["probe"], json!("tcp"));
        assert_eq!(status["evidence_strength"], json!("weak"));
        assert_eq!(status["readiness"], json!("tcp_only"));
        assert!(
            status["warning"]
                .as_str()
                .is_some_and(|warning| warning.contains("OpenCode API version")),
            "TCP-only probe must explain what it did not verify: {status:#}"
        );
    }

    #[test]
    fn harness_probe_reports_http_responsive_readiness() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind local listener");
        let port = listener.local_addr().expect("local addr").port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept probe");
            let mut buf = [0_u8; 1024];
            let _ = stream.read(&mut buf);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\n\r\nopencode service")
                .expect("write response");
        });

        let status = probe_harness_server_status(Some(&format!("http://127.0.0.1:{port}")));
        server.join().expect("probe server thread");

        assert_eq!(status["reachable"], json!(true));
        assert_eq!(status["probe"], json!("http"));
        assert_eq!(status["evidence_strength"], json!("medium"));
        assert_eq!(status["readiness"], json!("http_responsive"));
        assert_eq!(status["layers"]["tcp_reachable"], json!("passed"));
        assert_eq!(status["layers"]["http_health"], json!("passed"));
        assert_eq!(status["opencode_hint"], json!(true));
    }

    #[test]
    fn harness_probe_requires_password_for_opencode_api_attach_ready() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _password = EnvRestore::remove("OPENCODE_SERVER_PASSWORD");
        let (server_url, server) = spawn_opencode_doc_probe_server();

        let status = probe_harness_server_status(Some(&server_url));
        server.join().expect("probe server thread");

        assert_eq!(status["reachable"], json!(true));
        assert_eq!(status["attach_ready"], json!(false));
        assert_eq!(status["readiness"], json!("server_auth_required"));
        assert_eq!(status["layers"]["opencode_api_version"], json!("passed"));
        assert_eq!(
            status["layers"]["session_create_smoke"],
            json!("route_available")
        );
        assert_eq!(
            status["layers"]["credential_ready"],
            json!("unsafe_to_probe")
        );
        assert!(status["sensitive_endpoints_skipped"]
            .as_array()
            .is_some_and(|items| items.contains(&json!("/api/model"))));
    }

    #[test]
    fn harness_probe_reports_attach_ready_for_authenticated_opencode_schema() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _password = EnvRestore::set("OPENCODE_SERVER_PASSWORD", "test-password");
        let (server_url, server) = spawn_opencode_doc_probe_server();

        let status = probe_harness_server_status(Some(&server_url));
        server.join().expect("probe server thread");

        assert_eq!(status["reachable"], json!(true));
        assert_eq!(status["attach_ready"], json!(true));
        assert_eq!(status["readiness"], json!("attach_ready"));
        assert_eq!(status["evidence_strength"], json!("strong"));
        assert_eq!(
            status["api_capabilities"]["routes"]["session_create"],
            json!(true)
        );
        assert_eq!(
            status["layers"]["model_available"],
            json!("unsafe_to_probe")
        );
    }
}
