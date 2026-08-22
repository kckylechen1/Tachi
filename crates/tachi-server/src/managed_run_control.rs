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
#[derive(Default)] pub(crate) struct ManagedRunControlRegistry { entries: Mutex<HashMap<String, Entry>>, next: Mutex<u64> }
struct Entry { generation: u64, sender: mpsc::Sender<ManagedCancelCommand> }
pub(crate) struct ManagedCancelCommand { pub(crate) expected_status_revision: u64, pub(crate) observed_status_revision: u64, pub(crate) response: oneshot::Sender<CancelCompletion> }
pub(crate) enum CancelCompletion { Confirmed, Unconfirmed }
pub(crate) struct ManagedRunGuard { registry: Arc<ManagedRunControlRegistry>, dispatch_id: String, generation: u64 }
impl Drop for ManagedRunGuard { fn drop(&mut self) { let mut entries = self.registry.entries.lock().unwrap_or_else(|p| p.into_inner()); if entries.get(&self.dispatch_id).is_some_and(|entry| entry.generation == self.generation) { entries.remove(&self.dispatch_id); } } }
impl ManagedRunControlRegistry {
    pub(crate) fn register(self: &Arc<Self>, dispatch_id: &str) -> Result<(mpsc::Receiver<ManagedCancelCommand>, ManagedRunGuard), String> {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        if entries.contains_key(dispatch_id) { return Err("duplicate managed custom dispatch generation".to_string()); }
        let mut next = self.next.lock().unwrap_or_else(|p| p.into_inner()); *next = next.checked_add(1).ok_or_else(|| "managed custom generation overflow".to_string())?;
        let (sender, receiver) = mpsc::channel(CONTROL_CHANNEL_CAPACITY);
        entries.insert(dispatch_id.to_string(), Entry { generation: *next, sender });
        Ok((receiver, ManagedRunGuard { registry: Arc::clone(self), dispatch_id: dispatch_id.to_string(), generation: *next }))
    }
}
pub(crate) async fn request_managed_custom_cancel(server: &MemoryServer, dispatch_id: &str, expected: u64) -> Result<String, String> {
    #[cfg(not(unix))] { let _ = server; return Ok(unavailable(dispatch_id, expected, None, "unsupported_platform")); }
    #[cfg(unix)] {
        if !crate::dispatch_ops::is_valid_dispatch_id(dispatch_id) { return Ok(unavailable(dispatch_id, expected, None, "invalid_dispatch_identity")); }
        let run_dir = crate::dispatch_ops::dispatch_runs_root().join(dispatch_id); let lock = crate::dispatch_ops::status_json_lock_for(&run_dir);
        let (sender, observed, revised) = { let _guard = lock.lock().unwrap_or_else(|p| p.into_inner()); let path = run_dir.join("status.json"); let Some(mut status) = crate::task_lifecycle::read_json_file(&path)? else { return Ok(unavailable(dispatch_id, expected, None, "absent_same_daemon_handle")); }; let Some(object) = status.as_object_mut() else { return Ok(unavailable(dispatch_id, expected, None, "malformed_canonical_receipt")); }; let Some(observed) = object.get("status_revision").and_then(Value::as_u64) else { return Ok(unavailable(dispatch_id, expected, None, "historical_receipt_without_status_revision")); }; if observed != expected { return Ok(unavailable(dispatch_id, expected, Some(observed), "stale_status_revision")); } if object.get("state").and_then(Value::as_str).is_some_and(|state| state != "TASK_STATE_WORKING") || object.contains_key("completion_recovery") || object.contains_key("resolved_completion") { return Ok(unavailable(dispatch_id, expected, Some(observed), "terminal_or_recovery_state")); } let entries = server.managed_run_controls.entries.lock().unwrap_or_else(|p| p.into_inner()); let Some(entry) = entries.get(dispatch_id) else { return Ok(unavailable(dispatch_id, expected, Some(observed), "absent_same_daemon_handle")); }; object.insert("cancellation".into(), json!({"receipt":"cancellation_requested","expected_status_revision":expected,"observed_status_revision":observed,"lifecycle_owner":"memory_server_managed_custom","backend":"custom","timestamp":Utc::now().to_rfc3339()})); let revised = advance_status_revision(object)?; let body = serde_json::to_vec_pretty(&status).map_err(|e| format!("serialize cancellation receipt: {e}"))?; crate::utils::write_owner_only_file_atomic(&path, &body).map_err(|e| format!("persist cancellation_requested: {e}"))?; (entry.sender.clone(), observed, revised) };
        let (response, receiver) = oneshot::channel(); if sender.try_send(ManagedCancelCommand { expected_status_revision: expected, observed_status_revision: observed, response }).is_err() { return Ok(unavailable(dispatch_id, expected, Some(revised), "duplicate_or_closed_cancellation")); }
        match receiver.await { Ok(CancelCompletion::Confirmed) => Ok(json!({"receipt":"cancellation_confirmed","dispatch_id":dispatch_id,"expected_status_revision":expected,"observed_status_revision":revised,"termination_proof":"unix_process_group_absent","lifecycle_owner":"memory_server_managed_custom","backend":"custom","timestamp":Utc::now().to_rfc3339()}).to_string()), Ok(CancelCompletion::Unconfirmed) => Ok(json!({"receipt":"termination_unconfirmed","dispatch_id":dispatch_id,"expected_status_revision":expected,"observed_status_revision":revised,"lifecycle_owner":"memory_server_managed_custom","backend":"custom","timestamp":Utc::now().to_rfc3339()}).to_string()), Err(_) => Ok(unavailable(dispatch_id, expected, Some(revised), "completion_or_timeout_winner")) }
    }
}
fn unavailable(dispatch_id: &str, expected: u64, observed: Option<u64>, reason: &str) -> String { json!({"receipt":"cancellation_unavailable","dispatch_id":dispatch_id,"expected_status_revision":expected,"observed_status_revision":observed,"reason":reason,"lifecycle_owner":"unknown_or_unavailable","backend":"unknown_or_unavailable","timestamp":Utc::now().to_rfc3339()}).to_string() }

fn advance_status_revision(status: &mut serde_json::Map<String, Value>) -> Result<u64, String> {
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
