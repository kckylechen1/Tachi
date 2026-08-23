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

#[cfg(test)]
mod cancellation_receipt_regression_tests {
    use super::*;
    use serde_json::json;

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn closed_control_channel_returns_the_committed_unavailable_receipt_without_relocking() {
        let _serial = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home");
        let runs = tempfile::tempdir().expect("runs");
        let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
        let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
        let dispatch_id = "20260823T182501Z-closed-channel";
        let run_dir = crate::dispatch_ops::dispatch_runs_root().join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("run directory");
        std::fs::write(
            run_dir.join("status.json"),
            json!({
                "dispatch_id": dispatch_id,
                "state": "TASK_STATE_WORKING",
                "status_revision": 1,
                "execution_classification": "managed_custom",
            })
            .to_string(),
        )
        .expect("status");
        let server = MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
        let (receiver, _guard) = server
            .managed_run_controls
            .register(dispatch_id)
            .expect("register control");
        drop(receiver);

        let response = request_managed_custom_cancel(&server, dispatch_id, 1)
            .await
            .expect("closed channel returns an unavailable receipt");
        let status: Value = serde_json::from_slice(
            &std::fs::read(run_dir.join("status.json")).expect("canonical status"),
        )
        .expect("canonical JSON");
        assert_eq!(
            serde_json::from_str::<Value>(&response).expect("response JSON"),
            status["cancellation"],
            "response must be the committed canonical cancellation receipt"
        );
    }
}

struct Entry {
    generation: u64,
    sender: mpsc::Sender<ManagedCancelCommand>,
}

pub(crate) struct ManagedCancelCommand {
    pub(crate) expected_status_revision: u64,
    pub(crate) response: oneshot::Sender<CancelCompletion>,
    #[cfg(test)]
    pub(crate) test_observation: Option<crate::dispatch_ops::ManagedCancelTryWaitObservation>,
}

pub(crate) enum CancelCompletion {
    Confirmed {
        termination_proof: &'static str,
        status_revision: u64,
    },
    Unconfirmed,
    Unavailable(&'static str),
}

pub(crate) fn cancellation_blocks_terminal_writer(object: &serde_json::Map<String, Value>) -> bool {
    matches!(
        object
            .get("cancellation")
            .and_then(Value::as_object)
            .and_then(|receipt| receipt.get("receipt"))
            .and_then(Value::as_str),
        Some("cancellation_requested" | "cancellation_confirmed" | "termination_unconfirmed")
    )
}

pub(crate) fn reconcile_pending_cancellation_unavailable(
    object: &mut serde_json::Map<String, Value>,
    reason: &str,
) -> bool {
    let Some(receipt) = object.get("cancellation").and_then(Value::as_object) else {
        return false;
    };
    if receipt.get("receipt").and_then(Value::as_str) != Some("cancellation_requested") {
        return false;
    }
    let dispatch_id = object
        .get("dispatch_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let expected = receipt
        .get("expected_status_revision")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let observed = object
        .get("status_revision")
        .and_then(Value::as_u64)
        .unwrap_or(expected);
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
    true
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

    #[cfg(test)]
    pub(crate) fn contains(&self, dispatch_id: &str) -> bool {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(dispatch_id)
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
                #[cfg(test)]
                test_observation: None,
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
            Ok(CancelCompletion::Confirmed {
                termination_proof,
                status_revision,
            }) => {
                let _ = termination_proof;
                let canonical = canonical_cancellation_receipt(&run_dir).ok_or_else(|| {
                    "managed cancellation committed without a canonical receipt".to_string()
                })?;
                let mut response: Value = serde_json::from_str(&canonical).map_err(|error| {
                    format!("parse committed managed cancellation receipt: {error}")
                })?;
                response["observed_status_revision"] = Value::from(status_revision);
                serde_json::to_string(&response).map_err(|error| {
                    format!("serialize committed managed cancellation response: {error}")
                })
            }
            Ok(CancelCompletion::Unconfirmed) => canonical_cancellation_receipt(&run_dir)
                .ok_or_else(|| {
                    "managed cancellation committed without a canonical receipt".to_string()
                }),
            Ok(CancelCompletion::Unavailable(reason)) => {
                record_unavailable_if_pending(&run_dir, dispatch_id, expected, observed, reason)
            }
            Err(_) => record_unavailable_if_pending(
                &run_dir,
                dispatch_id,
                expected,
                observed,
                "completion_or_timeout_winner",
            ),
        }
    }
}

#[cfg(test)]
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
        || object.contains_key("resolved_completion")
        || object.contains_key("completion_recovery")
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

#[cfg(test)]
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
    if object.get("state").and_then(Value::as_str) != Some("TASK_STATE_WORKING")
        || object.contains_key("resolved_completion")
        || object.contains_key("completion_recovery")
        || !matches!(
            object
                .get("cancellation")
                .and_then(Value::as_object)
                .and_then(|receipt| receipt.get("receipt"))
                .and_then(Value::as_str),
            Some("cancellation_requested")
        )
    {
        return Err("managed cancellation lost lifecycle ownership".to_string());
    }
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
    object.insert(
        "state".to_string(),
        Value::String("TASK_STATE_FAILED".to_string()),
    );
    crate::managed_run_control::advance_status_revision(object)?;
    let body = serde_json::to_vec_pretty(&status)
        .map_err(|e| format!("serialize termination_unconfirmed: {e}"))?;
    crate::utils::write_owner_only_file_atomic(&path, &body)
        .map_err(|e| format!("persist termination_unconfirmed: {e}"))
}

/// Finalize a command that the managed subprocess dequeued. This is called by
/// the sole background terminal writer after result persistence, never by the
/// runner, so the waiter can observe only a canonical receipt.
#[cfg(test)]
pub(crate) fn finalize_dequeued_managed_cancellation(
    run_dir: &std::path::Path,
    expected: u64,
    runner_error: Option<&str>,
    termination_proof: Option<&'static str>,
) -> CancelCompletion {
    match runner_error {
        Some("managed_cancelled") => match confirm_managed_custom_cancellation(
            run_dir,
            expected,
            termination_proof.unwrap_or("unix_process_group_absent"),
        ) {
            Ok(status_revision) => CancelCompletion::Confirmed {
                termination_proof: termination_proof.unwrap_or("unix_process_group_absent"),
                status_revision,
            },
            Err(_) => match record_termination_unconfirmed(run_dir, expected) {
                Ok(()) => CancelCompletion::Unconfirmed,
                Err(_) => CancelCompletion::Unavailable("completion_or_timeout_winner"),
            },
        },
        Some(error)
            if error == "termination_unconfirmed"
                || error.starts_with("managed cancellation child probe failed") =>
        {
            match record_termination_unconfirmed(run_dir, expected) {
                Ok(()) => CancelCompletion::Unconfirmed,
                Err(_) => CancelCompletion::Unavailable("completion_or_timeout_winner"),
            }
        }
        _ => CancelCompletion::Unavailable("completion_or_timeout_winner"),
    }
}

pub(crate) enum ManagedTerminalCancellation {
    Confirmed { termination_proof: &'static str },
    Unconfirmed,
    Unavailable,
}

/// Apply a dequeued cancellation to the status object that the terminal writer
/// already owns. The caller advances the revision and persists exactly once.
pub(crate) fn apply_dequeued_cancellation_to_terminal_status(
    object: &mut serde_json::Map<String, Value>,
    expected: u64,
    runner_error: Option<&str>,
    termination_proof: Option<&'static str>,
) -> ManagedTerminalCancellation {
    let dispatch_id = object
        .get("dispatch_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let observed = object
        .get("status_revision")
        .and_then(Value::as_u64)
        .unwrap_or(expected);
    let requested = object
        .get("cancellation")
        .and_then(Value::as_object)
        .and_then(|receipt| receipt.get("receipt"))
        .and_then(Value::as_str)
        == Some("cancellation_requested");
    if !requested
        || !matches!(
            object.get("state").and_then(Value::as_str),
            Some(
                "TASK_STATE_WORKING"
                    | "TASK_STATE_COMPLETED"
                    | "TASK_STATE_FAILED"
                    | "TASK_STATE_CANCELED"
            )
        )
        || object.contains_key("resolved_completion")
        || object.contains_key("completion_recovery")
    {
        return ManagedTerminalCancellation::Unavailable;
    }
    match runner_error {
        Some("managed_cancelled") => {
            let proof = termination_proof.unwrap_or("unix_process_group_absent");
            object.insert(
                "cancellation".to_string(),
                cancellation_receipt(
                    "cancellation_confirmed",
                    &dispatch_id,
                    expected,
                    observed,
                    None,
                    Some(proof),
                ),
            );
            object.insert(
                "state".to_string(),
                Value::String("TASK_STATE_CANCELED".to_string()),
            );
            ManagedTerminalCancellation::Confirmed {
                termination_proof: proof,
            }
        }
        Some(error)
            if error == "termination_unconfirmed"
                || error.starts_with("managed cancellation child probe failed") =>
        {
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
            object.insert(
                "state".to_string(),
                Value::String("TASK_STATE_FAILED".to_string()),
            );
            ManagedTerminalCancellation::Unconfirmed
        }
        _ => {
            let _ =
                reconcile_pending_cancellation_unavailable(object, "completion_or_timeout_winner");
            ManagedTerminalCancellation::Unavailable
        }
    }
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
        let committed_receipt = object
            .get("cancellation")
            .cloned()
            .expect("cancellation receipt was inserted before persistence");
        let body = serde_json::to_vec_pretty(&status)
            .map_err(|e| format!("serialize cancellation_unavailable: {e}"))?;
        crate::utils::write_owner_only_file_atomic(&path, &body)
            .map_err(|e| format!("persist cancellation_unavailable: {e}"))?;
        return serde_json::to_string(&committed_receipt)
            .map_err(|error| format!("serialize committed cancellation receipt: {error}"));
    }
    if matches!(
        object
            .get("cancellation")
            .and_then(Value::as_object)
            .and_then(|receipt| receipt.get("receipt"))
            .and_then(Value::as_str),
        Some("cancellation_unavailable" | "cancellation_confirmed" | "termination_unconfirmed")
    ) {
        return serde_json::to_string(
            object
                .get("cancellation")
                .expect("canonical cancellation receipt was observed"),
        )
        .map_err(|error| format!("serialize canonical cancellation receipt: {error}"));
    }
    Ok(unavailable(dispatch_id, expected, Some(observed), reason))
}

fn canonical_cancellation_receipt(run_dir: &std::path::Path) -> Option<String> {
    let lock = crate::dispatch_ops::status_json_lock_for(run_dir);
    let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
    let status = crate::task_lifecycle::read_json_file(&run_dir.join("status.json")).ok()??;
    serde_json::to_string(status.get("cancellation")?).ok()
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
        "state": match receipt {
            "cancellation_requested" => Value::String("TASK_STATE_WORKING".to_string()),
            "cancellation_confirmed" => Value::String("TASK_STATE_CANCELED".to_string()),
            "termination_unconfirmed" => Value::String("TASK_STATE_FAILED".to_string()),
            _ => Value::Null,
        },
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
        "backend": "unknown_or_unavailable", "state": Value::Null,
        "timestamp": Utc::now().to_rfc3339(),
    })
    .to_string()
}

pub(crate) fn advance_status_revision(
    status: &mut serde_json::Map<String, Value>,
) -> Result<u64, String> {
    let canonical_state = status.get("state").cloned().unwrap_or(Value::Null);
    if let Some(receipt) = status
        .get_mut("cancellation")
        .and_then(Value::as_object_mut)
    {
        receipt.insert("state".to_string(), canonical_state);
    }
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
}

#[cfg(test)]
mod status_revision_writer_regression_tests {
    fn body_after<'a>(source: &'a str, marker: &str) -> &'a str {
        &source[source.find(marker).expect("canonical writer marker")..]
    }

    #[test]
    fn status_revision_advances_across_every_canonical_writer() {
        let control = include_str!("managed_run_control.rs");
        let status_writer = include_str!("dispatch_ops/dispatch_v2.rs");
        for (source, writer) in [
            (control, "pub(crate) fn mark_managed_custom_start"),
            (control, "pub(crate) async fn request_managed_custom_cancel"),
            (control, "pub(crate) fn confirm_managed_custom_cancellation"),
            (control, "pub(crate) fn record_termination_unconfirmed"),
            (control, "fn record_unavailable_if_pending"),
            (status_writer, "pub(crate) fn write_status_json"),
        ] {
            assert!(
                body_after(source, writer).contains("advance_status_revision"),
                "{writer} must advance the canonical status revision"
            );
        }
    }
}
