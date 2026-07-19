use super::paths::runs_dir_for_server;
use super::status::{
    is_abandoned_working_run, parse_status_updated_at, stale_after_secs,
    state_matches_filter_with_closure_kind, status_state,
};
use crate::dispatch_ops::probe_harness_server_status;
use crate::MemoryServer;
use chrono::Utc;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub(super) fn dispatch_timestamp_key(name: &std::ffi::OsStr) -> Option<String> {
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

pub(super) fn collect_run_tasks_from_dir(
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
        let closure_kind = status.get("closure_kind").and_then(Value::as_str);
        if !state_matches_filter_with_closure_kind(state_filter, state, closure_kind) {
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
                "run ledger stayed WORKING for more than {}s without terminal status or exit_code",
                stale_after_secs(&status)
            ))
        } else {
            None
        };
        runs.push(json!({
            "dispatch_id": dispatch_id,
            "agent": status.get("agent").cloned().unwrap_or(serde_json::Value::Null),
            "state": state,
            "closure_kind": status.get("closure_kind").cloned().unwrap_or(serde_json::Value::Null),
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
            "harness_server_status": probe_harness_server_status(status.get("harness_server_url").and_then(Value::as_str)),
            "execution_backend": status.get("execution_backend").cloned().unwrap_or(serde_json::Value::Null),
            "identity_receipt": status.get("identity_receipt").cloned().unwrap_or(serde_json::Value::Null),
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
    server: &MemoryServer,
    dispatch_id: &str,
) -> Option<serde_json::Value> {
    collect_run_task_by_id(&runs_dir_for_server(server), dispatch_id)
}

/// tachi#1173 board autopsy review: `dispatch_id` here is caller-supplied
/// (via `tachi_task(action='wait'|'status'|'cancel')` or `tachi_board`'s
/// `flow_id` expansion) and gets joined directly onto `runs_dir` below. A
/// value containing a path separator or a `..` component would otherwise let
/// a caller read (or, worse, have `read_failure_tail` read) an arbitrary file
/// outside `~/.tachi/runs` -- e.g. `dispatch_id = "../../../../etc/passwd"`.
/// tachi#1173 k2 fix: the character allowlist and the canonicalize-and-confine
/// defense-in-depth layer are now the shared `dispatch_ops::path_gate` gate
/// (three more caller-supplied-dispatch_id call sites needed the identical
/// check) rather than a copy local to this module.
pub(super) fn collect_run_task_by_id(
    runs_dir: &Path,
    dispatch_id: &str,
) -> Option<serde_json::Value> {
    if !crate::dispatch_ops::is_valid_dispatch_id(dispatch_id) {
        return None;
    }
    let run_dir = runs_dir.join(dispatch_id);
    if !run_dir.is_dir() {
        return None;
    }
    if !crate::dispatch_ops::canonical_dir_is_within(&run_dir, runs_dir) {
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
            "run ledger stayed WORKING for more than {}s without terminal status or exit_code",
            stale_after_secs(&status)
        ))
    } else {
        None
    };
    Some(json!({
        "dispatch_id": dispatch_id,
        "agent": status.get("agent").cloned().unwrap_or(serde_json::Value::Null),
        "state": state,
        "closure_kind": status.get("closure_kind").cloned().unwrap_or(serde_json::Value::Null),
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
        "harness_server_status": probe_harness_server_status(status.get("harness_server_url").and_then(Value::as_str)),
        "execution_backend": status.get("execution_backend").cloned().unwrap_or(serde_json::Value::Null),
        "identity_receipt": status.get("identity_receipt").cloned().unwrap_or(serde_json::Value::Null),
        "acpx": status.get("acpx").cloned().unwrap_or(serde_json::Value::Null),
        "acpx_events": status.get("acpx_events").cloned().unwrap_or(serde_json::Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tachi#1173 board autopsy review discriminator: a `dispatch_id`
    /// containing a path-traversal or absolute-path payload must be rejected
    /// fail-closed -- and, critically, must never surface content from a
    /// decoy file planted outside `runs_dir` that a successful escape would
    /// have read.
    #[test]
    fn collect_run_task_by_id_rejects_path_traversal_dispatch_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runs_dir = tmp.path().join("runs");
        std::fs::create_dir_all(&runs_dir).expect("create runs_dir");

        // Decoy directory *outside* runs_dir. A successful traversal escape
        // would resolve into here and read this status.json.
        let decoy_dir = tmp.path().join("decoy");
        std::fs::create_dir_all(&decoy_dir).expect("create decoy dir");
        std::fs::write(
            decoy_dir.join("status.json"),
            serde_json::json!({
                "dispatch_id": "decoy-should-never-be-read",
                "state": "TASK_STATE_COMPLETED",
            })
            .to_string(),
        )
        .expect("write decoy status.json");

        for malicious in [
            "../decoy",
            "../../decoy",
            "..",
            "",
            "/etc/passwd",
            "a/../../decoy",
        ] {
            let result = collect_run_task_by_id(&runs_dir, malicious);
            assert!(
                result.is_none(),
                "dispatch_id {malicious:?} must be rejected fail-closed (treated as \
                 not-found), not resolved outside runs_dir; got: {result:?}"
            );
        }
    }

    /// Defense-in-depth layer 2: even a dispatch_id that passes the character
    /// allowlist (looks like a normal single path component) must not be
    /// able to escape runs_dir via a symlinked run directory.
    #[cfg(unix)]
    #[test]
    fn collect_run_task_by_id_rejects_symlinked_escape() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runs_dir = tmp.path().join("runs");
        std::fs::create_dir_all(&runs_dir).expect("create runs_dir");

        let decoy_target = tmp.path().join("decoy-target");
        std::fs::create_dir_all(&decoy_target).expect("create decoy target dir");
        std::fs::write(
            decoy_target.join("status.json"),
            serde_json::json!({
                "dispatch_id": "decoy-should-never-be-read",
                "state": "TASK_STATE_COMPLETED",
            })
            .to_string(),
        )
        .expect("write decoy status.json");

        let link_name = "valid-looking-id-2454fead";
        std::os::unix::fs::symlink(&decoy_target, runs_dir.join(link_name))
            .expect("create symlink");

        let result = collect_run_task_by_id(&runs_dir, link_name);
        assert!(
            result.is_none(),
            "a symlinked run_dir resolving outside runs_dir must be rejected even though \
             its name alone passes the character allowlist; got: {result:?}"
        );
    }

    /// Regression guard: a legitimate, real-shaped dispatch id (matching
    /// `dispatch::dedupe::new_dispatch_id`'s timestamp-agent-suffix format)
    /// must still resolve normally after the validation gate above.
    #[test]
    fn collect_run_task_by_id_still_resolves_legit_dispatch_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runs_dir = tmp.path().join("runs");
        let dispatch_id = "20260715T104905Z-claude-2454fead";
        let run_dir = runs_dir.join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("create run dir");
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::json!({
                "dispatch_id": dispatch_id,
                "state": "TASK_STATE_COMPLETED",
            })
            .to_string(),
        )
        .expect("write status.json");

        let task = collect_run_task_by_id(&runs_dir, dispatch_id);
        assert!(
            task.is_some(),
            "a legitimate, valid-charset dispatch id must still resolve: {task:?}"
        );
        assert_eq!(
            task.unwrap().get("dispatch_id").and_then(|v| v.as_str()),
            Some(dispatch_id)
        );
    }
}
