//! Shared dispatch trajectory and canonical status writers.
//!
//! New dispatches execute the host-supplied assignment without a server-side
//! planning call. Historical model-plan receipts and planner failures remain
//! readable and retain their original completion authority.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[cfg(all(test, windows))]
mod windows_status_writers {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    const DISPATCH_ID: &str = "20261030T000002Z-windows-status";

    fn ordinary_write(run: &Path, extra: Value) {
        write_status_json(
            run,
            DISPATCH_ID,
            true,
            None,
            None,
            "approved",
            None,
            None,
            None,
            None,
            Some(extra),
        );
    }

    #[test]
    fn generic_and_route_writers_share_the_windows_private_fence() {
        let temp = tempfile::tempdir().unwrap();
        let run = temp.path();
        ordinary_write(
            run,
            json!({"state":"TASK_STATE_WORKING", "project":"preserved"}),
        );
        stamp_route_decision_id(run, "route-first").unwrap();
        let alias = run.join("..").join(run.file_name().unwrap());
        assert!(Arc::ptr_eq(
            &status_json_lock_for(run),
            &status_json_lock_for(&alias)
        ));
        let before = std::fs::read(run.join("status.json")).unwrap();
        let status: Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(status["route_decision_id"], "route-first");
        assert_eq!(status["project"], "preserved");
        std::fs::hard_link(
            run.join(".status-mutation.lock"),
            run.join("invalid-fence-link"),
        )
        .unwrap();
        ordinary_write(&alias, json!({"state":"TASK_STATE_COMPLETED"}));
        assert!(stamp_route_decision_id(&alias, "route-refused").is_err());
        assert_eq!(std::fs::read(run.join("status.json")).unwrap(), before);
    }

    #[test]
    fn ordinary_windows_status_writer_refuses_a_failed_read_without_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("status.json");
        std::fs::write(&path, b"{malformed committed receipt").unwrap();
        ordinary_write(temp.path(), json!({"state":"TASK_STATE_COMPLETED"}));
        assert!(stamp_route_decision_id(temp.path(), "route-refused").is_err());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"{malformed committed receipt"
        );
    }

    #[test]
    fn windows_managed_terminal_finalization_stays_unavailable_without_writing() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("status.json");
        std::fs::write(&path, b"{\"state\":\"TASK_STATE_WORKING\"}").unwrap();
        let before = std::fs::read(&path).unwrap();
        let completion = write_status_json_for_terminal(
            temp.path(),
            DISPATCH_ID,
            true,
            None,
            None,
            "approved",
            None,
            None,
            None,
            None,
            Some(json!({"managed_cancellation_finalization": {}})),
            ManagedTerminalStatusAnchor::Missing,
        );
        assert!(matches!(
            completion,
            Some(crate::managed_run_control::CancelCompletion::Unavailable(
                "unsupported_platform"
            ))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!temp.path().join(".status-mutation.lock").exists());
    }
}

/// Recognize a historical planner failure without treating an ordinary
/// execution failure as a planner winner. Completion must preserve this fact.
pub(crate) fn planner_failure_winner(status: &serde_json::Map<String, Value>) -> bool {
    status
        .get("plan_failure")
        .and_then(Value::as_object)
        .and_then(|failure| failure.get("stage"))
        .and_then(Value::as_str)
        == Some("plan")
        && status.get("state").and_then(Value::as_str) == Some("TASK_STATE_FAILED")
        && !status.contains_key("model_plan")
}

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
    #[cfg(any(unix, windows))] fence: &crate::managed_run_control::StatusMutationFence,
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
        .write_atomic(
            &body,
            #[cfg(any(unix, windows))]
            fence,
        )
        .map_err(|error| format!("write {}: {error}", target.status_path().display()))
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
    #[cfg(any(unix, windows))]
    let anchor = crate::managed_run_control::AnchoredRunStatus::open_ordinary(run_dir)?;
    #[cfg(any(unix, windows))]
    let lock = anchor.lock();
    #[cfg(not(any(unix, windows)))]
    let lock = status_json_lock_for(run_dir);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    #[cfg(any(unix, windows))]
    let fence = anchor.acquire_fence()?;
    let path = run_dir.join("status.json");
    #[cfg(any(unix, windows))]
    let read = anchor.read_json();
    #[cfg(not(any(unix, windows)))]
    let read = crate::task_lifecycle::read_json_file(&path);
    let Some(Value::Object(mut status)) =
        read.map_err(|error| format!("read {}: {error}", path.display()))?
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
    #[cfg(any(unix, windows))]
    let written = anchor.write_atomic(&body, &fence);
    #[cfg(not(any(unix, windows)))]
    let written = crate::utils::write_owner_only_file_atomic(&path, &body);
    written.map_err(|error| format!("write {}: {error}", path.display()))
}

/// Return the shared, weakly retained mutex for one canonical run receipt.
/// Every `status.json` read-modify-write must take this lock, including ACP
/// identity acknowledgement, lifecycle terminalization, and route evidence.
/// Weak retention avoids keeping a lock entry for every historical run.
pub(crate) fn status_json_lock_for(run_dir: &std::path::Path) -> Arc<Mutex<()>> {
    #[cfg(windows)]
    if let Ok(anchor) = crate::managed_run_control::AnchoredRunStatus::open_ordinary(run_dir) {
        return anchor.lock();
    }
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
    #[cfg(not(any(unix, windows)))]
    Path(std::path::PathBuf),
    #[cfg(any(unix, windows))]
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
                ManagedTerminalStatusAnchor::Missing => {
                    crate::managed_run_control::AnchoredRunStatus::open_ordinary(run_dir)
                        .map(Self::Anchored)
                        .map_err(|error| {
                            tracing::warn!(%error, "status mutation fence unavailable");
                            "status_mutation_fence_unavailable"
                        })
                }
            }
        }
        #[cfg(windows)]
        {
            let _ = (dispatch_id, managed_anchor);
            if managed_finalization {
                return Err("unsupported_platform");
            }
            crate::managed_run_control::AnchoredRunStatus::open_ordinary(run_dir)
                .map(Self::Anchored)
                .map_err(|error| {
                    tracing::warn!(%error, "status mutation fence unavailable");
                    "status_mutation_fence_unavailable"
                })
        }
        #[cfg(not(any(unix, windows)))]
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

    fn status_path(&self) -> std::path::PathBuf {
        match self {
            #[cfg(not(any(unix, windows)))]
            Self::Path(path) => path.clone(),
            #[cfg(any(unix, windows))]
            Self::Anchored(status) => status.status_path(),
        }
    }

    fn read_json(&self) -> Result<Option<Value>, String> {
        match self {
            #[cfg(not(any(unix, windows)))]
            Self::Path(path) => crate::task_lifecycle::read_json_file(path),
            #[cfg(any(unix, windows))]
            Self::Anchored(status) => status.read_json(),
        }
    }

    fn write_atomic(
        &self,
        bytes: &[u8],
        #[cfg(any(unix, windows))] fence: &crate::managed_run_control::StatusMutationFence,
    ) -> Result<(), String> {
        match self {
            #[cfg(not(any(unix, windows)))]
            Self::Path(path) => crate::utils::write_owner_only_file_atomic(path, bytes),
            #[cfg(any(unix, windows))]
            Self::Anchored(status) => status.write_atomic(bytes, fence),
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
            return managed_finalization_requested.then_some(
                crate::managed_run_control::CancelCompletion::Unavailable(reason),
            );
        }
    };
    let lock = match &target {
        #[cfg(not(any(unix, windows)))]
        StatusJsonTarget::Path(_) => status_json_lock_for(run_dir),
        #[cfg(any(unix, windows))]
        StatusJsonTarget::Anchored(status) => status.lock(),
    };
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    #[cfg(any(unix, windows))]
    let fence = match &target {
        StatusJsonTarget::Anchored(anchor) => match anchor.acquire_fence() {
            Ok(fence) => fence,
            Err(error) => {
                tracing::warn!(%error, "status mutation fence unavailable");
                return managed_finalization_requested.then_some(
                    crate::managed_run_control::CancelCompletion::Unavailable("persist_failed"),
                );
            }
        },
    };
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
            // Historical model plans are immutable. No generic writer may
            // create, clear, or overwrite one through `extra`; the existing
            // receipt is carried forward below.
            if matches!(
                k.as_str(),
                "model_plan" | "acp_permission" | "acp_permission_history"
            ) {
                continue;
            }
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
    let mut terminal_cancellation_reconciled = false;

    let path = target.status_path();
    #[cfg(any(unix, windows))]
    let previous_status = match target.read_json() {
        Ok(status) => status,
        Err(error) => {
            tracing::warn!(%error, "status mutation skipped after failed locked read");
            return managed_finalization_requested.then_some(
                crate::managed_run_control::CancelCompletion::Unavailable(
                    "managed_terminal_status_unreadable",
                ),
            );
        }
    };
    #[cfg(not(any(unix, windows)))]
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
                terminal_cancellation_reconciled |=
                    crate::managed_run_control::reconcile_pending_cancellation_unavailable(
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
                // Preserve historical plan content and its original receipt;
                // a status revision must never rebind artifact_revision.
                "model_plan",
                "acp_permission",
                "acp_permission_history",
                "native_session",
                // Durable managed-run identity and the append-only
                // reconciliation observation are stamped once and carried
                // forward by every later canonical writer (completion,
                // watchdog, cancellation finalization). No writer may erase
                // or rewrite them; reconciliation only appends.
                "managed_run_identity",
                "managed_run_reconciliation",
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
                terminal_cancellation_reconciled |=
                    crate::managed_run_control::reconcile_pending_cancellation_unavailable(
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
            let native_retention_failed = obj.get("state").and_then(Value::as_str)
                == Some("TASK_STATE_FAILED")
                && obj.get("exec_env_postflight").is_some_and(|receipt| {
                    receipt.get("status").and_then(Value::as_str) == Some("delegated_to_harness")
                        && receipt.get("resource_fence").and_then(Value::as_str) == Some("failed")
                });
            if !native_retention_failed
                && previous
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
    if managed_terminal.is_some() || terminal_cancellation_reconciled {
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
            target.write_atomic(
                body.as_bytes(),
                #[cfg(any(unix, windows))]
                &fence,
            )
        };
    #[cfg(not(test))]
    let write_result = target.write_atomic(
        body.as_bytes(),
        #[cfg(any(unix, windows))]
        &fence,
    );
    if let Err(e) = write_result {
        if managed_finalization.is_some() {
            if let Err(fallback_error) = persist_managed_terminal_status_failure(
                &target,
                &mut obj,
                #[cfg(any(unix, windows))]
                &fence,
            ) {
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

    #[test]
    fn historical_model_plan_survives_generic_route_and_terminal_writers() {
        use serde_json::json;
        use tachi_llm::PersistedModelInvocationReceiptV1;

        let temp = tempfile::tempdir().expect("run directory");
        let content = "## Goal\ncanonical host-visible historical plan";
        let invocation = PersistedModelInvocationReceiptV1::test_fixture_reasoning(5)
            .bound_to_content(content, "dispatch:d1:stage:plan".to_string(), 4);
        let model_plan = json!({
            "dispatch_id": "d1",
            "stage": "plan",
            "content": content,
            "payload_digest": PersistedModelInvocationReceiptV1::content_hash_for(content),
            "artifact_revision": 4,
            "model_invocation": invocation,
        });
        std::fs::write(
            temp.path().join("status.json"),
            json!({
                "dispatch_id": "d1",
                "state": "TASK_STATE_WORKING",
                "status_revision": 4,
                "model_plan": model_plan,
            })
            .to_string(),
        )
        .expect("seed historical receipt");

        write_status_json(
            temp.path(),
            "d1",
            true,
            Some("fixed"),
            None,
            "approved",
            None,
            None,
            None,
            None,
            Some(json!({"state": "TASK_STATE_WORKING", "model_plan": {"forged": true}})),
        );
        stamp_route_decision_id(temp.path(), "route-historical").expect("stamp route");
        write_status_json(
            temp.path(),
            "d1",
            true,
            Some("fixed"),
            Some("done"),
            "approved",
            Some(0),
            None,
            None,
            None,
            Some(json!({"state": "TASK_STATE_COMPLETED"})),
        );

        let status: Value = serde_json::from_slice(
            &std::fs::read(temp.path().join("status.json")).expect("status bytes"),
        )
        .expect("status JSON");
        assert_eq!(status["model_plan"], model_plan);
        assert!(status["status_revision"].as_u64().unwrap() > 4);
        assert_eq!(status["route_decision_id"], "route-historical");
        assert_eq!(status["state"], "TASK_STATE_COMPLETED");
    }

    #[test]
    fn planner_failure_winner_requires_the_historical_producer_shape() {
        use serde_json::json;
        let winner = json!({
            "state": "TASK_STATE_FAILED",
            "plan_failure": {"stage": "plan", "error": "x"},
        });
        assert!(planner_failure_winner(winner.as_object().unwrap()));
        let bare = json!({"state": "TASK_STATE_FAILED"});
        assert!(!planner_failure_winner(bare.as_object().unwrap()));
        let with_plan = json!({
            "state": "TASK_STATE_FAILED",
            "plan_failure": {"stage": "plan"},
            "model_plan": {"content": "## Goal\nx"},
        });
        assert!(!planner_failure_winner(with_plan.as_object().unwrap()));
        let other_stage = json!({
            "state": "TASK_STATE_FAILED",
            "plan_failure": {"stage": "execute"},
        });
        assert!(!planner_failure_winner(other_stage.as_object().unwrap()));
    }
}
