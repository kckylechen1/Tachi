//! Volatile same-daemon control for managed custom runs. Registry entries carry
//! only a bounded channel and generation; Child/PID/PGID/command/env remain in
//! the background execution task.
use crate::MemoryServer;
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

const CONTROL_CHANNEL_CAPACITY: usize = 1;

#[derive(Default)]
pub(crate) struct ManagedRunControlRegistry {
    entries: Mutex<HashMap<String, Entry>>,
    next: Mutex<u64>,
}

struct Entry {
    generation: u64,
    sender: mpsc::Sender<ManagedCancelCommand>,
}

pub(crate) struct ManagedCancelCommand {
    pub(crate) expected_status_revision: u64,
    pub(crate) response: oneshot::Sender<CancelCompletion>,
}

pub(crate) enum CancelCompletion {
    Confirmed {
        termination_proof: &'static str,
        status_revision: u64,
    },
    Unconfirmed,
    Unavailable(&'static str),
}

pub(crate) struct ManagedRunGuard {
    registry: Arc<ManagedRunControlRegistry>,
    dispatch_id: String,
    generation: u64,
}

impl Drop for ManagedRunGuard {
    fn drop(&mut self) {
        let mut entries = self
            .registry
            .entries
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if entries
            .get(&self.dispatch_id)
            .is_some_and(|entry| entry.generation == self.generation)
        {
            entries.remove(&self.dispatch_id);
        }
    }
}

impl ManagedRunControlRegistry {
    pub(crate) fn register(
        self: &Arc<Self>,
        dispatch_id: &str,
    ) -> Result<(mpsc::Receiver<ManagedCancelCommand>, ManagedRunGuard), String> {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        if entries.contains_key(dispatch_id) {
            return Err("duplicate managed custom dispatch generation".to_string());
        }
        let mut next = self.next.lock().unwrap_or_else(|p| p.into_inner());
        *next = next
            .checked_add(1)
            .ok_or_else(|| "managed custom generation overflow".to_string())?;
        let (sender, receiver) = mpsc::channel(CONTROL_CHANNEL_CAPACITY);
        entries.insert(
            dispatch_id.to_string(),
            Entry {
                generation: *next,
                sender,
            },
        );
        Ok((
            receiver,
            ManagedRunGuard {
                registry: Arc::clone(self),
                dispatch_id: dispatch_id.to_string(),
                generation: *next,
            },
        ))
    }
}

pub(crate) fn mark_managed_custom_start(
    run_dir: &std::path::Path,
    dispatch_id: &str,
) -> Result<(), String> {
    let lock = crate::dispatch_ops::status_json_lock_for(run_dir);
    let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
    let path = run_dir.join("status.json");
    let Some(mut status) = crate::task_lifecycle::read_json_file(&path)? else {
        return Err(format!("managed custom run is missing {}", path.display()));
    };
    let object = status
        .as_object_mut()
        .ok_or_else(|| format!("managed custom status is not an object: {}", path.display()))?;
    if object.get("dispatch_id").and_then(Value::as_str) != Some(dispatch_id) {
        return Err("managed custom status dispatch identity mismatch".to_string());
    }
    object.insert(
        "execution_classification".to_string(),
        Value::String("managed_custom".to_string()),
    );
    object.insert(
        "lifecycle_owner".to_string(),
        Value::String("memory_server_managed_custom".to_string()),
    );
    crate::managed_run_control::advance_status_revision(object)?;
    let body = serde_json::to_vec_pretty(&status)
        .map_err(|e| format!("serialize managed custom status: {e}"))?;
    crate::utils::write_owner_only_file_atomic(&path, &body)
        .map_err(|e| format!("persist managed custom classification: {e}"))
}

pub(crate) async fn request_managed_custom_cancel(
    server: &MemoryServer,
    dispatch_id: &str,
    expected: u64,
) -> Result<String, String> {
    #[cfg(not(unix))]
    {
        let _ = server;
        return Ok(unavailable(
            dispatch_id,
            expected,
            None,
            "unsupported_platform",
        ));
    }
    #[cfg(unix)]
    {
        if !crate::dispatch_ops::is_valid_dispatch_id(dispatch_id) {
            return Ok(unavailable(
                dispatch_id,
                expected,
                None,
                "invalid_dispatch_identity",
            ));
        }
        let run_dir = crate::dispatch_ops::dispatch_runs_root().join(dispatch_id);
        let lock = crate::dispatch_ops::status_json_lock_for(&run_dir);
        let (sender, observed) = {
            let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
            let path = run_dir.join("status.json");
            let Some(mut status) = crate::task_lifecycle::read_json_file(&path)? else {
                return Ok(unavailable(
                    dispatch_id,
                    expected,
                    None,
                    "absent_same_daemon_handle",
                ));
            };
            let Some(object) = status.as_object_mut() else {
                return Ok(unavailable(
                    dispatch_id,
                    expected,
                    None,
                    "malformed_canonical_receipt",
                ));
            };
            let Some(observed) = object.get("status_revision").and_then(Value::as_u64) else {
                return Ok(unavailable(
                    dispatch_id,
                    expected,
                    None,
                    "historical_receipt_without_status_revision",
                ));
            };
            if observed != expected {
                return Ok(unavailable(
                    dispatch_id,
                    expected,
                    Some(observed),
                    "stale_status_revision",
                ));
            }
            if object.get("state").and_then(Value::as_str) != Some("TASK_STATE_WORKING")
                || object.contains_key("completion_recovery")
                || object.contains_key("resolved_completion")
            {
                return Ok(unavailable(
                    dispatch_id,
                    expected,
                    Some(observed),
                    "terminal_or_recovery_state",
                ));
            }
            if object
                .get("execution_classification")
                .and_then(Value::as_str)
                != Some("managed_custom")
            {
                return Ok(unavailable(
                    dispatch_id,
                    expected,
                    Some(observed),
                    "non_managed_custom_execution",
                ));
            }
            if object.get("cancellation").is_some() {
                return Ok(unavailable(
                    dispatch_id,
                    expected,
                    Some(observed),
                    "duplicate_cancellation",
                ));
            }
            let sender = {
                let entries = server
                    .managed_run_controls
                    .entries
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let Some(entry) = entries.get(dispatch_id) else {
                    return Ok(unavailable(
                        dispatch_id,
                        expected,
                        Some(observed),
                        "absent_same_daemon_handle",
                    ));
                };
                entry.sender.clone()
            };
            object.insert(
                "cancellation".to_string(),
                cancellation_receipt(
                    "cancellation_requested",
                    dispatch_id,
                    expected,
                    observed,
                    None,
                    None,
                ),
            );
            crate::managed_run_control::advance_status_revision(object)?;
            let body = serde_json::to_vec_pretty(&status)
                .map_err(|e| format!("serialize cancellation_requested: {e}"))?;
            crate::utils::write_owner_only_file_atomic(&path, &body)
                .map_err(|e| format!("persist cancellation_requested: {e}"))?;
            (sender, observed)
        };
        let (response, receiver) = oneshot::channel();
        if sender
            .try_send(ManagedCancelCommand {
                expected_status_revision: expected,
                response,
            })
            .is_err()
        {
            return record_unavailable_if_pending(
                &run_dir,
                dispatch_id,
                expected,
                observed,
                "duplicate_or_closed_cancellation",
            );
        }
        match receiver.await {
            Ok(CancelCompletion::Confirmed { termination_proof, status_revision }) => Ok(json!({
                "receipt": "cancellation_confirmed", "dispatch_id": dispatch_id,
                "expected_status_revision": expected, "observed_status_revision": status_revision,
                "termination_proof": termination_proof, "lifecycle_owner": "memory_server_managed_custom",
                "backend": "custom", "timestamp": Utc::now().to_rfc3339(),
            }).to_string()),
            Ok(CancelCompletion::Unconfirmed) => Ok(json!({
                "receipt": "termination_unconfirmed", "dispatch_id": dispatch_id,
                "expected_status_revision": expected, "observed_status_revision": observed,
                "lifecycle_owner": "memory_server_managed_custom", "backend": "custom",
                "timestamp": Utc::now().to_rfc3339(),
            }).to_string()),
            Ok(CancelCompletion::Unavailable(reason)) => record_unavailable_if_pending(&run_dir, dispatch_id, expected, observed, reason),
            Err(_) => record_unavailable_if_pending(&run_dir, dispatch_id, expected, observed, "completion_or_timeout_winner"),
        }
    }
}

pub(crate) fn confirm_managed_custom_cancellation(
    run_dir: &std::path::Path,
    expected: u64,
    termination_proof: &'static str,
) -> Result<u64, String> {
    let lock = crate::dispatch_ops::status_json_lock_for(run_dir);
    let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
    let path = run_dir.join("status.json");
    let Some(mut status) = crate::task_lifecycle::read_json_file(&path)? else {
        return Err("managed cancellation status is absent".to_string());
    };
    let object = status
        .as_object_mut()
        .ok_or_else(|| "managed cancellation status is malformed".to_string())?;
    let dispatch_id = object
        .get("dispatch_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "managed cancellation dispatch identity is absent".to_string())?
        .to_string();
    if object.get("state").and_then(Value::as_str) != Some("TASK_STATE_WORKING")
        || object
            .get("execution_classification")
            .and_then(Value::as_str)
            != Some("managed_custom")
    {
        return Err("managed cancellation lost lifecycle ownership".to_string());
    }
    let observed = object
        .get("status_revision")
        .and_then(Value::as_u64)
        .ok_or_else(|| "managed cancellation status_revision is absent".to_string())?;
    if object
        .get("cancellation")
        .and_then(Value::as_object)
        .and_then(|receipt| receipt.get("receipt"))
        .and_then(Value::as_str)
        != Some("cancellation_requested")
    {
        return Err("managed cancellation request receipt is absent".to_string());
    }
    object.insert(
        "cancellation".to_string(),
        cancellation_receipt(
            "cancellation_confirmed",
            &dispatch_id,
            expected,
            observed,
            None,
            Some(termination_proof),
        ),
    );
    object.insert(
        "state".to_string(),
        Value::String("TASK_STATE_CANCELED".to_string()),
    );
    let revision = crate::managed_run_control::advance_status_revision(object)?;
    let body = serde_json::to_vec_pretty(&status)
        .map_err(|e| format!("serialize cancellation_confirmed: {e}"))?;
    crate::utils::write_owner_only_file_atomic(&path, &body)
        .map_err(|e| format!("persist cancellation_confirmed: {e}"))?;
    Ok(revision)
}

pub(crate) fn record_termination_unconfirmed(
    run_dir: &std::path::Path,
    expected: u64,
) -> Result<(), String> {
    let lock = crate::dispatch_ops::status_json_lock_for(run_dir);
    let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
    let path = run_dir.join("status.json");
    let Some(mut status) = crate::task_lifecycle::read_json_file(&path)? else {
        return Err("managed cancellation status is absent".to_string());
    };
    let object = status
        .as_object_mut()
        .ok_or_else(|| "managed cancellation status is malformed".to_string())?;
    let dispatch_id = object
        .get("dispatch_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "managed cancellation dispatch identity is absent".to_string())?
        .to_string();
    let observed = object
        .get("status_revision")
        .and_then(Value::as_u64)
        .ok_or_else(|| "managed cancellation status_revision is absent".to_string())?;
    object.insert(
        "cancellation".to_string(),
        cancellation_receipt(
            "termination_unconfirmed",
            &dispatch_id,
            expected,
            observed,
            Some("termination_unconfirmed"),
            None,
        ),
    );
    crate::managed_run_control::advance_status_revision(object)?;
    let body = serde_json::to_vec_pretty(&status)
        .map_err(|e| format!("serialize termination_unconfirmed: {e}"))?;
    crate::utils::write_owner_only_file_atomic(&path, &body)
        .map_err(|e| format!("persist termination_unconfirmed: {e}"))
}

fn record_unavailable_if_pending(
    run_dir: &std::path::Path,
    dispatch_id: &str,
    expected: u64,
    fallback_observed: u64,
    reason: &str,
) -> Result<String, String> {
    let lock = crate::dispatch_ops::status_json_lock_for(run_dir);
    let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
    let path = run_dir.join("status.json");
    let Some(mut status) = crate::task_lifecycle::read_json_file(&path)? else {
        return Ok(unavailable(
            dispatch_id,
            expected,
            Some(fallback_observed),
            reason,
        ));
    };
    let Some(object) = status.as_object_mut() else {
        return Ok(unavailable(
            dispatch_id,
            expected,
            Some(fallback_observed),
            reason,
        ));
    };
    let observed = object
        .get("status_revision")
        .and_then(Value::as_u64)
        .unwrap_or(fallback_observed);
    if object.get("state").and_then(Value::as_str) == Some("TASK_STATE_WORKING")
        && object
            .get("cancellation")
            .and_then(Value::as_object)
            .and_then(|receipt| receipt.get("receipt"))
            .and_then(Value::as_str)
            == Some("cancellation_requested")
    {
        object.insert(
            "cancellation".to_string(),
            cancellation_receipt(
                "cancellation_unavailable",
                dispatch_id,
                expected,
                observed,
                Some(reason),
                None,
            ),
        );
        crate::managed_run_control::advance_status_revision(object)?;
        let body = serde_json::to_vec_pretty(&status)
            .map_err(|e| format!("serialize cancellation_unavailable: {e}"))?;
        crate::utils::write_owner_only_file_atomic(&path, &body)
            .map_err(|e| format!("persist cancellation_unavailable: {e}"))?;
    }
    Ok(unavailable(dispatch_id, expected, Some(observed), reason))
}

fn cancellation_receipt(
    receipt: &str,
    dispatch_id: &str,
    expected: u64,
    observed: u64,
    reason: Option<&str>,
    termination_proof: Option<&str>,
) -> Value {
    json!({
        "receipt": receipt, "dispatch_id": dispatch_id,
        "expected_status_revision": expected, "observed_status_revision": observed,
        "lifecycle_owner": "memory_server_managed_custom", "backend": "custom",
        "reason": reason, "termination_proof": termination_proof,
        "timestamp": Utc::now().to_rfc3339(),
    })
}

fn unavailable(dispatch_id: &str, expected: u64, observed: Option<u64>, reason: &str) -> String {
    json!({
        "receipt": "cancellation_unavailable", "dispatch_id": dispatch_id,
        "expected_status_revision": expected, "observed_status_revision": observed,
        "reason": reason, "lifecycle_owner": "unknown_or_unavailable",
        "backend": "unknown_or_unavailable", "timestamp": Utc::now().to_rfc3339(),
    })
    .to_string()
}

pub(crate) fn advance_status_revision(
    status: &mut serde_json::Map<String, Value>,
) -> Result<u64, String> {
    let next = match status.get("status_revision") {
        Some(Value::Number(value)) => value
            .as_u64()
            .ok_or_else(|| "status_revision is not a u64".to_string())?
            .checked_add(1)
            .ok_or_else(|| "status_revision overflow".to_string())?,
        Some(_) => return Err("status_revision is not a u64".to_string()),
        None => 1,
    };
    status.insert("status_revision".to_string(), Value::Number(next.into()));
    Ok(next)
}

#[cfg(test)]
mod issue_1825_tests {
    use super::*;

    fn status(dispatch_id: &str, revision: u64) -> Value {
        json!({
            "dispatch_id": dispatch_id,
            "state": "TASK_STATE_WORKING",
            "status_revision": revision,
            "execution_classification": "managed_custom",
        })
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn managed_custom_cancel_rejects_stale_revision_and_foreign_server() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home");
        let runs = tempfile::tempdir().expect("runs");
        let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
        let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
        let dispatch_id = "20260823T010101Z-custom-deadbeef";
        let run_dir = crate::dispatch_ops::dispatch_runs_root().join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("run dir");
        std::fs::write(
            run_dir.join("status.json"),
            status(dispatch_id, 3).to_string(),
        )
        .expect("status");
        let first =
            MemoryServer::new(home.path().join("first.sqlite"), None).expect("first server");
        let second =
            MemoryServer::new(home.path().join("second.sqlite"), None).expect("second server");
        let (mut receiver, _run_guard) = first
            .managed_run_controls
            .register(dispatch_id)
            .expect("first registry");

        let stale = request_managed_custom_cancel(&first, dispatch_id, 2)
            .await
            .expect("stale response");
        assert_eq!(
            serde_json::from_str::<Value>(&stale).unwrap()["reason"],
            "stale_status_revision"
        );
        assert!(
            receiver.try_recv().is_err(),
            "stale revision must not signal the child"
        );

        let foreign = request_managed_custom_cancel(&second, dispatch_id, 3)
            .await
            .expect("foreign response");
        assert_eq!(
            serde_json::from_str::<Value>(&foreign).unwrap()["reason"],
            "absent_same_daemon_handle"
        );
        assert!(
            receiver.try_recv().is_err(),
            "foreign server must not signal the child"
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn managed_custom_cancel_race_matrix() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home");
        let runs = tempfile::tempdir().expect("runs");
        let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
        let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
        let dispatch_id = "20260823T010102Z-custom-deadbeef";
        let run_dir = crate::dispatch_ops::dispatch_runs_root().join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("run dir");
        std::fs::write(
            run_dir.join("status.json"),
            status(dispatch_id, 1).to_string(),
        )
        .expect("status");
        let server = MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
        let (mut receiver, _run_guard) = server
            .managed_run_controls
            .register(dispatch_id)
            .expect("registry");
        let first_server = server.clone();
        let first_id = dispatch_id.to_string();
        let first = tokio::spawn(async move {
            request_managed_custom_cancel(&first_server, &first_id, 1)
                .await
                .expect("first response")
        });
        let command = receiver
            .recv()
            .await
            .expect("first request owns the bounded control channel");
        let duplicate = request_managed_custom_cancel(&server, dispatch_id, 2)
            .await
            .expect("duplicate response");
        assert_eq!(
            serde_json::from_str::<Value>(&duplicate).unwrap()["reason"],
            "duplicate_cancellation"
        );
        drop(command);
        let first = first.await.expect("first task");
        assert_eq!(
            serde_json::from_str::<Value>(&first).unwrap()["receipt"],
            "cancellation_unavailable"
        );
        let canonical: Value = serde_json::from_str(
            &std::fs::read_to_string(run_dir.join("status.json")).expect("canonical status"),
        )
        .expect("json");
        assert_eq!(
            canonical["cancellation"]["receipt"],
            "cancellation_unavailable"
        );
        assert_eq!(canonical["state"], "TASK_STATE_WORKING");

        for (winner, terminal_state, completion, reason) in [
            (
                "completion",
                "TASK_STATE_COMPLETED",
                Some(CancelCompletion::Unavailable("completion_winner")),
                "completion_winner",
            ),
            (
                "timeout",
                "TASK_STATE_FAILED",
                None,
                "completion_or_timeout_winner",
            ),
        ] {
            let suffix = if winner == "completion" { 3 } else { 4 };
            let dispatch_id = format!("20260823T01010{suffix}Z-custom-deadbeef");
            let run_dir = crate::dispatch_ops::dispatch_runs_root().join(&dispatch_id);
            std::fs::create_dir_all(&run_dir).expect("winner run dir");
            std::fs::write(
                run_dir.join("status.json"),
                status(&dispatch_id, 1).to_string(),
            )
            .expect("winner status");
            let server = MemoryServer::new(home.path().join(format!("{winner}.sqlite")), None)
                .expect("winner server");
            let (mut receiver, _run_guard) = server
                .managed_run_controls
                .register(&dispatch_id)
                .expect("winner registry");
            let request_server = server.clone();
            let request_id = dispatch_id.clone();
            let request = tokio::spawn(async move {
                request_managed_custom_cancel(&request_server, &request_id, 1)
                    .await
                    .expect("winner cancellation response")
            });
            let command = receiver.recv().await.expect("winner control command");

            crate::dispatch_ops::write_status_json(
                &run_dir,
                &dispatch_id,
                false,
                None,
                None,
                "n/a",
                Some(1),
                None,
                None,
                None,
                Some(json!({
                    "state": terminal_state,
                    "result_written": true,
                    "result": format!("{winner} won the lifecycle race"),
                })),
            );
            let terminal_before_reply: Value = serde_json::from_slice(
                &std::fs::read(run_dir.join("status.json")).expect("winner terminal status"),
            )
            .expect("winner terminal JSON");
            if let Some(completion) = completion {
                assert!(
                    command.response.send(completion).is_ok(),
                    "completion winner reply"
                );
            } else {
                drop(command);
            }
            let response: Value =
                serde_json::from_str(&request.await.expect("winner request task"))
                    .expect("winner response JSON");
            assert_eq!(
                response["receipt"], "cancellation_unavailable",
                "{winner} response"
            );
            assert_eq!(response["reason"], reason, "{winner} reason");
            let terminal_after_reply: Value = serde_json::from_slice(
                &std::fs::read(run_dir.join("status.json")).expect("winner final status"),
            )
            .expect("winner final JSON");
            assert_eq!(
                terminal_after_reply, terminal_before_reply,
                "{winner} winner must not regress or rewrite terminal status"
            );
            assert!(
                receiver.try_recv().is_err(),
                "{winner} winner must not request a second child cleanup"
            );
        }
    }
}
