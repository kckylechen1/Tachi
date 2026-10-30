//! Deferred `tachi_staff(result, format=json)` reads of an existing run.
//!
//! The sidecar stores only result-access ownership and the protocol request's
//! handle/result. Execution, cancellation and reconciliation remain owned by
//! the canonical run receipt and its existing controller.

use crate::managed_run_control::AnchoredRunStatus;
use crate::MemoryServer;
use chrono::{DateTime, Utc};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, CreateTaskResult,
    DetailedTask, GetTaskResult, Task, TaskPayload, TaskStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

const TTL_MS: u64 = 24 * 60 * 60 * 1000;
const MAX_STATUS_BYTES: usize = 1024 * 1024;
const TASK_PREFIX: &str = "tachi-result:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultOwner {
    agent_identity_id: String,
    canonical_project: String,
    source_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultBinding {
    task_id: String,
    created_at: String,
    observed_revision: u64,
    cancel_revision: Option<u64>,
    /// An immutable final response to this result request, not a work verdict.
    terminal: Option<DetailedTask>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunBinding {
    schema: u32,
    dispatch_id: String,
    owner: ResultOwner,
    result: Option<ResultBinding>,
}

fn unavailable() -> rmcp::ErrorData {
    rmcp::ErrorData::invalid_params("task unavailable", None)
}

fn storage_error(error: String) -> rmcp::ErrorData {
    tracing::warn!(%error, "MCP result binding storage unavailable");
    rmcp::ErrorData::internal_error("task storage unavailable", None)
}

fn current_owner(server: &MemoryServer) -> Result<ResultOwner, rmcp::ErrorData> {
    let Some((Some(agent_identity_id), _, _)) = server.work_claim_connection() else {
        return Err(unavailable());
    };
    if crate::claims_ops::current_admission_receipt_ref(server)
        .map_err(|_| unavailable())?
        .is_none()
        || !crate::memory_resources::sandbox_policy_clear(server).map_err(|_| unavailable())?
    {
        return Err(unavailable());
    }
    let project = server.session_project().ok_or_else(unavailable)?;
    let (canonical_project, _) = server
        .resolve_server_named_project_binding(&project)
        .map_err(|_| unavailable())?;
    let source_fingerprint = server
        .with_named_project_store_read_identity_checked(&canonical_project, |store| {
            crate::memory_resources::verified_project_source(server, &canonical_project, store)
                .ok_or_else(|| "project source unavailable".to_string())
        })
        .map_err(|_| unavailable())?;
    Ok(ResultOwner {
        agent_identity_id,
        canonical_project,
        source_fingerprint,
    })
}

fn admit(server: &MemoryServer, action: &str) -> Result<ResultOwner, rmcp::ErrorData> {
    let profile = server.active_tool_profile();
    if !tachi_hub::tool_visible(
        "tachi_staff",
        profile,
        crate::server_handler::current_exposed_tool_patterns().as_deref(),
    ) || !tachi_hub::facade_action_allowed("tachi_staff", Some(action), profile)
    {
        return Err(unavailable());
    }
    current_owner(server)
}

/// Called once before the launcher can execute a worker. Historical/unbound
/// runs remain usable through their existing facade; they gain no Tasks grant.
pub(crate) fn prepare_run_binding(
    server: &MemoryServer,
    run_dir: &Path,
    dispatch_id: &str,
    project: Option<&str>,
) -> Result<(), String> {
    let Ok(owner) = current_owner(server) else {
        return Ok(());
    };
    let Some(project) = project else {
        return Ok(());
    };
    let (canonical_project, _) = server.resolve_server_named_project_binding(project)?;
    if canonical_project != owner.canonical_project {
        return Ok(());
    }
    let anchor =
        AnchoredRunStatus::open(run_dir).map_err(|e| format!("anchor result binding: {e:?}"))?;
    let lock = anchor.lock();
    let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
    let fence = anchor.acquire_fence()?;
    if anchor.read_mcp_binding()?.is_some() {
        return Err("result binding already exists; refusing to transfer run ownership".into());
    }
    let binding = RunBinding {
        schema: 1,
        dispatch_id: dispatch_id.into(),
        owner,
        result: None,
    };
    anchor.write_mcp_binding(
        &serde_json::to_vec(&binding).map_err(|e| e.to_string())?,
        &fence,
    )
}

fn anchor_for(
    server: &MemoryServer,
    dispatch_id: &str,
) -> Result<AnchoredRunStatus, rmcp::ErrorData> {
    if dispatch_id.len() > 256 || !crate::dispatch_ops::is_valid_dispatch_id(dispatch_id) {
        return Err(unavailable());
    }
    server
        .managed_run_controls
        .accepted_status_anchor(dispatch_id)
        .map(Ok)
        .unwrap_or_else(|| {
            AnchoredRunStatus::open(&crate::dispatch_ops::dispatch_runs_root().join(dispatch_id))
                .map_err(|_| unavailable())
        })
}

fn read_binding(
    anchor: &AnchoredRunStatus,
    owner: &ResultOwner,
    dispatch_id: &str,
) -> Result<RunBinding, rmcp::ErrorData> {
    let raw = anchor
        .read_mcp_binding()
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    let binding: RunBinding = serde_json::from_str(&raw).map_err(|_| unavailable())?;
    if binding.schema != 1 || binding.dispatch_id != dispatch_id || &binding.owner != owner {
        return Err(unavailable());
    }
    Ok(binding)
}

fn read_status(anchor: &AnchoredRunStatus, dispatch_id: &str) -> Result<Value, rmcp::ErrorData> {
    let status = anchor
        .read_json_bounded(MAX_STATUS_BYTES)
        .map_err(storage_error)?
        .ok_or_else(unavailable)?;
    if status.get("dispatch_id").and_then(Value::as_str) != Some(dispatch_id)
        || status
            .get("status_revision")
            .and_then(Value::as_u64)
            .is_none()
    {
        return Err(unavailable());
    }
    Ok(status)
}

fn terminal_receipt(status: &Value) -> bool {
    matches!(
        status.get("state").and_then(Value::as_str),
        Some("TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED")
    ) || (status["state"] == "TASK_STATE_INPUT_REQUIRED" && status["closure_kind"] == "partial")
}

fn result_response(
    server: &MemoryServer,
    dispatch_id: &str,
    mut status: Value,
    anchor: &AnchoredRunStatus,
) -> CallToolResult {
    let result = (|| {
        let report = crate::tools::dispatch_result_report(anchor.read_mcp_result_report()?, None);
        crate::tools::decorate_staff_receipt(server, dispatch_id, &mut status, Some(report))?;
        crate::tools::formatting::format_facade_response(
            "Tachi staff result",
            "result",
            &serde_json::to_string(&status).map_err(|e| e.to_string())?,
            Some("json"),
            false,
        )
    })();
    match result {
        Ok(body) => CallToolResult::success(vec![ContentBlock::text(body)]),
        Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]),
    }
}

fn expired(binding: &ResultBinding) -> bool {
    DateTime::parse_from_rfc3339(&binding.created_at)
        .map(|created| {
            Utc::now().signed_duration_since(created).num_milliseconds() >= TTL_MS as i64
        })
        .unwrap_or(true)
}

fn task_metadata(binding: &ResultBinding, status: &Value) -> Task {
    // A result request can be created long after its worker started. The run's
    // earlier timestamp cannot precede this protocol task's own creation.
    let updated = status
        .get("updated_at")
        .and_then(Value::as_str)
        .filter(|updated| {
            DateTime::parse_from_rfc3339(updated)
                .ok()
                .zip(DateTime::parse_from_rfc3339(&binding.created_at).ok())
                .is_some_and(|(updated, created)| updated >= created)
        });
    Task::new(
        &binding.task_id,
        TaskStatus::Working,
        &binding.created_at,
        updated.unwrap_or(&binding.created_at),
    )
    .with_ttl_ms(TTL_MS)
    .with_poll_interval_ms(1000)
}

fn project_task(
    server: &MemoryServer,
    dispatch_id: &str,
    binding: &ResultBinding,
    status: &Value,
    anchor: &AnchoredRunStatus,
) -> Result<DetailedTask, rmcp::ErrorData> {
    if let Some(terminal) = &binding.terminal {
        return Ok(terminal.clone());
    }
    let mut task = task_metadata(binding, status);
    if terminal_receipt(status) {
        let cancelled = binding.cancel_revision.is_some_and(|revision| {
            status["cancellation"]["expected_status_revision"].as_u64() == Some(revision)
                && status["cancellation"]["receipt"] == "cancellation_confirmed"
                && status["state"] == "TASK_STATE_CANCELED"
        });
        if cancelled {
            return Ok(DetailedTask::new(task, TaskPayload::Cancelled));
        }
        let result =
            serde_json::to_value(result_response(server, dispatch_id, status.clone(), anchor))
                .map_err(|e| storage_error(e.to_string()))?;
        return Ok(DetailedTask::new(
            task,
            TaskPayload::Completed {
                result: result.as_object().cloned().ok_or_else(unavailable)?,
            },
        ));
    }
    let projection =
        crate::staffing_ops::staff_status_projection_from_receipt(server, dispatch_id, status);
    task.status_message = Some(match projection.as_ref().and_then(|p| p["execution_state"].as_str()) {
        Some("orphaned" | "unknown") => "Run outcome is unknown; the current daemon has not recovered execution control.".to_string(),
        _ if status["state"] == "TASK_STATE_INPUT_REQUIRED" => "The run needs input through its lifecycle owner; MCP input delivery is unavailable for this operation.".to_string(),
        _ => "Waiting for the existing run's final result; execution and acceptance are separate facts.".to_string(),
    });
    Ok(DetailedTask::new(task, TaskPayload::Working))
}

pub(crate) fn begin_result(
    server: &MemoryServer,
    params: &CallToolRequestParams,
) -> Result<CallToolResponse, rmcp::ErrorData> {
    let owner = admit(server, "result")?;
    let args: tachi_params::TachiStaffParams =
        serde_json::from_value(Value::Object(params.arguments.clone().unwrap_or_default()))
            .map_err(|e| rmcp::ErrorData::invalid_params(e.to_string(), None))?;
    let dispatch_id = args.dispatch_id.ok_or_else(unavailable)?;
    let anchor = anchor_for(server, &dispatch_id)?;
    let lock = anchor.lock();
    let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
    // Authorization precedes opening the canonical receipt or result body.
    read_binding(&anchor, &owner, &dispatch_id)?;
    let fence = anchor.acquire_fence().map_err(storage_error)?;
    let mut binding = read_binding(&anchor, &owner, &dispatch_id)?;
    let status = read_status(&anchor, &dispatch_id)?;
    if terminal_receipt(&status) {
        return Ok(result_response(server, &dispatch_id, status, &anchor).into());
    }
    if binding.result.as_ref().is_none_or(expired) {
        binding.result = Some(ResultBinding {
            task_id: format!(
                "{TASK_PREFIX}{dispatch_id}:{}",
                uuid::Uuid::new_v4().simple()
            ),
            created_at: Utc::now().to_rfc3339(),
            observed_revision: status["status_revision"].as_u64().ok_or_else(unavailable)?,
            cancel_revision: None,
            terminal: None,
        });
        anchor
            .write_mcp_binding(
                &serde_json::to_vec(&binding).map_err(|e| storage_error(e.to_string()))?,
                &fence,
            )
            .map_err(storage_error)?;
    }
    let result = binding.result.as_ref().ok_or_else(unavailable)?;
    let task = project_task(server, &dispatch_id, result, &status, &anchor)?;
    Ok(CallToolResponse::Task(CreateTaskResult::new(task.task)))
}

pub(crate) fn dispatch_from_task(task_id: &str) -> Result<&str, rmcp::ErrorData> {
    let (dispatch, suffix) = task_id
        .strip_prefix(TASK_PREFIX)
        .and_then(|s| s.split_once(':'))
        .ok_or_else(unavailable)?;
    if dispatch.len() > 256
        || !crate::dispatch_ops::is_valid_dispatch_id(dispatch)
        || suffix.len() != 32
        || !suffix
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(unavailable());
    }
    Ok(dispatch)
}

fn checked_result<'a>(
    binding: &'a RunBinding,
    task_id: &str,
) -> Result<&'a ResultBinding, rmcp::ErrorData> {
    binding
        .result
        .as_ref()
        .filter(|b| b.task_id == task_id && !expired(b))
        .ok_or_else(unavailable)
}

pub(crate) fn get(server: &MemoryServer, task_id: &str) -> Result<GetTaskResult, rmcp::ErrorData> {
    let owner = admit(server, "result")?;
    let dispatch_id = dispatch_from_task(task_id)?;
    let anchor = anchor_for(server, dispatch_id)?;
    let lock = anchor.lock();
    let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
    read_binding(&anchor, &owner, dispatch_id)?;
    let fence = anchor.acquire_fence().map_err(storage_error)?;
    let mut binding = read_binding(&anchor, &owner, dispatch_id)?;
    let request = checked_result(&binding, task_id)?;
    if let Some(terminal) = &request.terminal {
        return Ok(GetTaskResult::new(terminal.clone()));
    }
    let status = read_status(&anchor, dispatch_id)?;
    let revision = status["status_revision"].as_u64().ok_or_else(unavailable)?;
    if revision < request.observed_revision {
        return Err(unavailable());
    }
    let task = project_task(server, dispatch_id, request, &status, &anchor)?;
    let request = binding.result.as_mut().ok_or_else(unavailable)?;
    let changed = request.observed_revision != revision || task.status().is_terminal();
    request.observed_revision = revision;
    if task.status().is_terminal() {
        request.terminal = Some(task.clone());
    }
    if changed {
        anchor
            .write_mcp_binding(
                &serde_json::to_vec(&binding).map_err(|e| storage_error(e.to_string()))?,
                &fence,
            )
            .map_err(storage_error)?;
    }
    Ok(GetTaskResult::new(task))
}

/// This result-wait operation has no outstanding MCP input requests. Unknown,
/// duplicate and superseded response keys are acknowledged without writes.
pub(crate) fn update(server: &MemoryServer, task_id: &str) -> Result<(), rmcp::ErrorData> {
    let owner = admit(server, "result")?;
    let dispatch_id = dispatch_from_task(task_id)?;
    let anchor = anchor_for(server, dispatch_id)?;
    let binding = read_binding(&anchor, &owner, dispatch_id)?;
    checked_result(&binding, task_id)?;
    Ok(())
}

pub(crate) async fn cancel(server: &MemoryServer, task_id: &str) -> Result<(), rmcp::ErrorData> {
    let owner = admit(server, "cancel")?;
    let dispatch_id = dispatch_from_task(task_id)?;
    let anchor = anchor_for(server, dispatch_id)?;
    let expected = {
        let lock = anchor.lock();
        let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
        read_binding(&anchor, &owner, dispatch_id)?;
        let fence = anchor.acquire_fence().map_err(storage_error)?;
        let mut binding = read_binding(&anchor, &owner, dispatch_id)?;
        if checked_result(&binding, task_id)?.terminal.is_some() {
            return Ok(());
        }
        let status = read_status(&anchor, dispatch_id)?;
        if terminal_receipt(&status) {
            return Ok(());
        }
        let accepted = server
            .managed_run_controls
            .accepted_status_anchor(dispatch_id)
            .ok_or_else(|| {
                rmcp::ErrorData::invalid_params(
                    "cancellation unavailable: no current controller",
                    None,
                )
            })?;
        if !accepted.same_opened_directory(&anchor) {
            return Err(unavailable());
        }
        let request = binding.result.as_mut().ok_or_else(unavailable)?;
        if request.cancel_revision.is_some_and(|revision| {
            status["cancellation"]["expected_status_revision"].as_u64() == Some(revision)
                && status["cancellation"]["receipt"] == "cancellation_requested"
        }) {
            return Ok(());
        }
        let revision = status["status_revision"].as_u64().ok_or_else(unavailable)?;
        request.cancel_revision = Some(revision);
        anchor
            .write_mcp_binding(
                &serde_json::to_vec(&binding).map_err(|e| storage_error(e.to_string()))?,
                &fence,
            )
            .map_err(storage_error)?;
        revision
    };
    let raw =
        crate::managed_run_control::request_managed_custom_cancel(server, dispatch_id, expected)
            .await
            .map_err(storage_error)?;
    let receipt: Value = serde_json::from_str(&raw).map_err(|e| storage_error(e.to_string()))?;
    match receipt["receipt"].as_str() {
        Some("cancellation_requested" | "cancellation_confirmed") => Ok(()),
        _ => Err(rmcp::ErrorData::invalid_params(
            "cancellation unavailable",
            Some(json!({"reason":receipt["reason"]})),
        )),
    }
}

#[cfg(test)]
mod tests;
