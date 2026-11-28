//! Authenticated inbound tasks, destination-local admission, and truthful
//! lifecycle receipts. Worker launch remains disabled in this slice (`RefusalOnly`
//! / `AdmitOnly`).

use super::admission::Connection;
use super::config::DestinationExecutionMode;
use super::profile::{self, JobKind, DELEGATION, PROVENANCE};
use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use memcore::db::a2a::{
    admit_remote_task, is_retained_task_quota_exceeded, list_remote_tasks, read_remote_task,
    record_remote_task_cancel_requested, NewRemoteTask, RemoteAdapterKind, RemoteAdmissionDecision,
    RemoteJobGrant, RemoteLocalAdmissionSpec, RemoteRejectionReason, RemoteTask, RemoteTaskAccess,
    RemoteTaskHeader, RemoteTaskInsertOutcome,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

const LOG_TARGET: &str = "tachi::remote_a2a";

#[derive(Clone, Copy)]
struct RpcError {
    code: i32,
    message: &'static str,
}

const INVALID: RpcError = RpcError {
    code: -32602,
    message: "Invalid parameters",
};
const INTERNAL: RpcError = RpcError {
    code: -32603,
    message: "Internal error",
};
const NOT_FOUND: RpcError = RpcError {
    code: -32001,
    message: "Task not found",
};
const QUOTA_EXCEEDED: RpcError = RpcError {
    code: -32006,
    message: "Peer task quota exceeded",
};

/// Runs certificate revalidation, SQLite and store-gate work on the blocking
/// pool. A Tokio worker is never held while the store may block.
pub(super) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, tokio::task::JoinError> {
    tokio::task::spawn_blocking(work).await
}

fn response(status: StatusCode, value: Value) -> Response {
    let mut response = (status, Json(value)).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/a2a+json"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert("a2a-version", HeaderValue::from_static("1.0"));
    headers.insert(
        "a2a-extensions",
        HeaderValue::from_static(
            "urn:tachi:a2a:ext:delegation:v1,urn:tachi:a2a:ext:provenance-receipt:v1",
        ),
    );
    response
}

fn failure(id: Value, error: RpcError) -> Response {
    // Only the static code and reason reach the log: no request bytes or ids.
    tracing::warn!(
        target: LOG_TARGET,
        code = error.code,
        reason = error.message,
        "remote gateway: RPC refused"
    );
    response(
        StatusCode::OK,
        json!({"jsonrpc":"2.0", "id":id,
        "error":{"code":error.code,"message":error.message}}),
    )
}

fn admission_unavailable() -> Response {
    tracing::warn!(
        target: LOG_TARGET,
        reason = "remote admission unavailable",
        "remote gateway: request refused"
    );
    response(
        StatusCode::FORBIDDEN,
        json!({"error":"remote admission unavailable"}),
    )
}

/// Revalidates the certificate and the durable admission of a connection. It
/// blocks on the store, so it runs on the blocking pool.
async fn recheck(
    connection: &Arc<Connection>,
) -> Result<Result<(), String>, tokio::task::JoinError> {
    let connection = connection.clone();
    blocking(move || connection.current()).await
}

pub(super) async fn handle(
    State(connection): State<Arc<Connection>>,
    request: Request,
) -> Response {
    // Admission is rechecked before any byte of the body is read.
    if !matches!(recheck(&connection).await, Ok(Ok(()))) {
        return admission_unavailable();
    }
    let headers = request.headers();
    if headers
        .get("a2a-version")
        .and_then(|value| value.to_str().ok())
        != Some("1.0")
    {
        return failure(
            Value::Null,
            RpcError {
                code: -32009,
                message: "Version not supported",
            },
        );
    }
    let mut extensions = std::collections::BTreeSet::new();
    for value in headers.get_all("a2a-extensions") {
        let Ok(value) = value.to_str() else {
            return failure(Value::Null, INVALID);
        };
        extensions.extend(value.split(',').map(str::trim));
    }
    if !extensions.contains(DELEGATION) || !extensions.contains(PROVENANCE) {
        return failure(
            Value::Null,
            RpcError {
                code: -32008,
                message: "Required extensions not negotiated",
            },
        );
    }
    if extensions
        .iter()
        .any(|extension| ![DELEGATION, PROVENANCE].contains(extension))
    {
        return failure(
            Value::Null,
            RpcError {
                code: -32004,
                message: "Unsupported extension",
            },
        );
    }
    let media_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if !matches!(
        media_type,
        Some("application/json" | "application/a2a+json")
    ) || headers.contains_key(header::CONTENT_ENCODING)
    {
        return failure(
            Value::Null,
            RpcError {
                code: -32005,
                message: "Content type not supported",
            },
        );
    }
    let bytes = match tokio::time::timeout(
        Duration::from_secs(10),
        to_bytes(request.into_body(), profile::MAX_REQUEST_BYTES),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        _ => return failure(Value::Null, INVALID),
    };
    let Ok(request) = profile::parse_request(&bytes) else {
        return failure(
            Value::Null,
            RpcError {
                code: -32600,
                message: "Invalid request",
            },
        );
    };
    let id = request.id;
    // No database work or business input reaches a managed worker. Every
    // operation below rechecks durable admission inside the existing storage
    // transaction; that SQLite work stays off the Tokio worker.
    let method = request.method;
    let params = request.params;
    let stored = {
        let connection = connection.clone();
        blocking(move || dispatch(&connection, &method, params)).await
    };
    match stored {
        Ok(Ok(result)) => response(
            StatusCode::OK,
            json!({"jsonrpc":"2.0","id":id,"result":result}),
        ),
        Ok(Err(error)) => failure(id, error),
        Err(_) => failure(id, INTERNAL),
    }
}

fn typed<T: DeserializeOwned>(value: Value) -> Result<T, RpcError> {
    serde_json::from_value(value).map_err(|_| INVALID)
}

fn storage<T>(result: Result<Result<T, memcore::MemoryError>, String>) -> Result<T, RpcError> {
    let error = match result {
        Ok(Ok(value)) => return Ok(value),
        Ok(Err(error)) => error,
        Err(_) => return Err(INTERNAL),
    };
    Err(match &error {
        memcore::MemoryError::Duplicate(_) => RpcError {
            code: -32602,
            message: "Conflicting remote submission",
        },
        error if is_retained_task_quota_exceeded(error) => QUOTA_EXCEEDED,
        memcore::MemoryError::InvalidArg(_) => INVALID,
        _ => INTERNAL,
    })
}

fn binding(connection: &Connection) -> Result<memcore::VerifiedAdmissionBinding, RpcError> {
    let context = connection
        .server
        .verified_work_claim_connection()
        .ok_or(INTERNAL)?;
    Ok(memcore::VerifiedAdmissionBinding {
        admission_id: context.admission_id,
        agent_identity_id: context.agent_identity_id,
        connection_id: context.connection_id,
        issuer_id: context.issuer_id,
        verification_method: context.verification_method,
        verification_version: context.verification_version,
        trust_domain: context.trust_domain,
        verification_scope: context.verification_scope,
    })
}

fn access(connection: &Connection) -> RemoteTaskAccess {
    RemoteTaskAccess {
        destination_agent_identity_id: connection.config.agent_identity_id.clone(),
        destination_host_id: connection.config.host_id.clone(),
        grants: connection
            .peer
            .repositories
            .iter()
            .flat_map(|grant| {
                grant.jobs.iter().map(|job| RemoteJobGrant {
                    repository: grant.repository.clone(),
                    job: job.as_str().to_string(),
                })
            })
            .collect(),
    }
}

fn pinned_git_command(dest: &super::config::DestinationPolicy) -> Option<std::process::Command> {
    let (canonical_root, canonical_git_dir) =
        super::config::resolve_pinned_git_dir(&dest.repo_root).ok()?;
    if canonical_root != dest.canonical_repo_root || canonical_git_dir != dest.canonical_git_dir {
        return None;
    }
    let mut cmd = std::process::Command::new("git");
    cmd.arg("--git-dir")
        .arg(&canonical_git_dir)
        .arg("--work-tree")
        .arg(&canonical_root);
    if let Some(parent) = canonical_root.parent() {
        cmd.env("GIT_CEILING_DIRECTORIES", parent);
    }
    cmd.stdin(Stdio::null());
    Some(cmd)
}

fn verify_exact_git_commit(dest: &super::config::DestinationPolicy, head_sha: &str) -> bool {
    let Some(mut cmd) = pinned_git_command(dest) else {
        return false;
    };
    let target = format!("{head_sha}^{{commit}}");
    let output = cmd
        .args(["rev-parse", "--verify", "--quiet", &target])
        .output();
    matches!(
        output,
        Ok(out) if out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == head_sha
    )
}

fn verify_trusted_pr_head(
    dest: &super::config::DestinationPolicy,
    pull_request: u32,
    head_sha: &str,
) -> bool {
    let local_ref = format!("refs/pull/{pull_request}/head");
    let remote_ref = format!("refs/remotes/origin/pull/{pull_request}/head");
    let Some(mut cmd) = pinned_git_command(dest) else {
        return false;
    };
    let Ok(out) = cmd
        .args([
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            &local_ref,
            &remote_ref,
        ])
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut found = false;
    for line in stdout.lines() {
        let Some((refname, oid)) = line.split_once(' ') else {
            return false;
        };
        if refname == local_ref || refname == remote_ref {
            if oid.trim() != head_sha {
                return false;
            }
            found = true;
        }
    }
    found
}

fn admission_decision(
    connection: &Connection,
    job: &profile::RemoteJob,
) -> RemoteAdmissionDecision {
    match connection.config.execution_mode {
        DestinationExecutionMode::RefusalOnly => {
            RemoteAdmissionDecision::Reject(RemoteRejectionReason::ExecutionUnavailable)
        }
        DestinationExecutionMode::AdmitOnly => {
            let Some(dest) = connection
                .config
                .destinations
                .iter()
                .find(|d| d.repository == job.repository && d.allowed_jobs.contains(&job.job))
            else {
                return RemoteAdmissionDecision::Reject(RemoteRejectionReason::PolicyDenied);
            };
            if !verify_exact_git_commit(dest, &job.head_sha) {
                return RemoteAdmissionDecision::Reject(RemoteRejectionReason::HeadNotFound);
            }
            if job.job == JobKind::VerifyPr {
                let Some(pr) = job.pull_request else {
                    return RemoteAdmissionDecision::Reject(RemoteRejectionReason::PolicyDenied);
                };
                if !verify_trusted_pr_head(dest, pr, &job.head_sha) {
                    return RemoteAdmissionDecision::Reject(RemoteRejectionReason::HeadNotFound);
                }
            }
            let (adapter_kind, policy_profile, issue_ref) = match job.job {
                JobKind::ReviewRevision => (
                    RemoteAdapterKind::NativeReview,
                    dest.review_profile.clone().unwrap_or_default(),
                    format!("remote:{}:review_revision", job.repository),
                ),
                JobKind::VerifyPr => (
                    RemoteAdapterKind::Dgx2VerifyPr,
                    dest.verify_policy.clone().unwrap_or_default(),
                    format!(
                        "remote:{}:pr:{}",
                        job.repository,
                        job.pull_request.unwrap_or(0)
                    ),
                ),
            };
            RemoteAdmissionDecision::Admit(RemoteLocalAdmissionSpec {
                canonical_project: dest.canonical_project.clone(),
                adapter_kind,
                policy_profile,
                resource_slot: dest.resource_slot.clone(),
                max_concurrent_runs: dest.max_concurrent_runs,
                lease_ttl_secs: dest.lease_ttl_secs,
                issue_ref,
                worktree_path: dest.repo_root.to_string_lossy().into_owned(),
                writable: false,
            })
        }
    }
}

fn dispatch(connection: &Connection, method: &str, params: Value) -> Result<Value, RpcError> {
    let binding = binding(connection)?;
    let access = access(connection);
    match method {
        "SendMessage" => {
            let message: profile::SendMessage = typed(params)?;
            if message.message.role != "ROLE_USER"
                || !profile::opaque_token(&message.message.message_id, 128)
                || message.message.parts.len() != 1
                || !message.configuration.return_immediately
                || message.configuration.history_length != 0
            {
                return Err(INVALID);
            }
            let job = &message.message.parts[0].data;
            job.validate().map_err(|_| INVALID)?;
            if job.sender != connection.peer.agent_identity_id
                || job.recipient != connection.config.agent_identity_id
                || job.trust_domain != connection.config.trust_domain
                || !access.grants.iter().any(|grant| {
                    grant.repository == job.repository && grant.job == job.job.as_str()
                })
            {
                return Err(INVALID);
            }
            let decision = admission_decision(connection, job);
            let request = NewRemoteTask {
                envelope_id: format!("remote-task-{}", uuid::Uuid::new_v4()),
                canonical_body: job.canonical_body().map_err(|_| INVALID)?,
                nonce: job.nonce.clone(),
                idempotency_key: job.idempotency_key.clone(),
                issued_at: job.issued_at.clone(),
                expires_at: job.expires_at.clone(),
                recipient_agent_identity_id: job.recipient.clone(),
                header: RemoteTaskHeader {
                    repository: job.repository.clone(),
                    job: job.job.as_str().to_string(),
                    head_sha: job.head_sha.clone(),
                    pull_request: job.pull_request.map(i64::from),
                    origin_claim_id: job.origin_claim.claim_id.clone(),
                    origin_claim_version: job.origin_claim.transition_version as i64,
                    destination_host_id: connection.config.host_id.clone(),
                },
            };
            let outcome = storage(connection.server.with_global_store(|store| {
                Ok(admit_remote_task(
                    store.connection(),
                    &binding,
                    &connection.gateway_actor,
                    &access,
                    &request,
                    &decision,
                ))
            }))?;
            let task = match outcome {
                RemoteTaskInsertOutcome::Created(task) | RemoteTaskInsertOutcome::Replay(task) => {
                    task
                }
            };
            Ok(json!({"task":task_value(&task)?}))
        }
        "GetTask" => {
            let request: profile::GetTask = typed(params)?;
            if request.history_length != 0 || !profile::opaque_token(&request.id, 128) {
                return Err(INVALID);
            }
            let task = storage(connection.server.with_global_store(|store| {
                let task =
                    match read_remote_task(store.connection(), &binding, &access, &request.id) {
                        Ok(Some(task)) => task,
                        other => return Ok(other),
                    };
                if matches!(
                    task.envelope.current_state.as_str(),
                    "remote_admitted" | "remote_started" | "remote_cancel_requested"
                ) {
                    match memcore::db::a2a::reconcile_remote_run_if_expired(
                        store.connection(),
                        &connection.gateway_actor,
                        &request.id,
                    ) {
                        Ok(true) => {
                            return Ok(read_remote_task(
                                store.connection(),
                                &binding,
                                &access,
                                &request.id,
                            ));
                        }
                        Ok(false) => {}
                        Err(err) => return Ok(Err(err)),
                    }
                }
                Ok(Ok(Some(task)))
            }))?
            .ok_or(NOT_FOUND)?;
            task_value(&task)
        }
        "ListTasks" => {
            let request: profile::ListTasks = typed(params)?;
            if !(1..=100).contains(&request.page_size)
                || request.history_length != 0
                || request.include_artifacts
                || request
                    .page_token
                    .as_ref()
                    .is_some_and(|token| !profile::opaque_token(token, 128))
            {
                return Err(INVALID);
            }
            let page = storage(connection.server.with_global_store(|store| {
                Ok(list_remote_tasks(
                    store.connection(),
                    &binding,
                    &access,
                    request.page_size,
                    request.page_token.as_deref(),
                ))
            }))?;
            let tasks = page
                .tasks
                .iter()
                .map(task_value)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(
                json!({"tasks":tasks,"totalSize":page.total_size,"pageSize":request.page_size,
                "nextPageToken":page.next_cursor.unwrap_or_default()}),
            )
        }
        "CancelTask" => {
            let request: profile::TaskId = typed(params)?;
            if !profile::opaque_token(&request.id, 128) {
                return Err(INVALID);
            }
            let result = connection
                .server
                .with_global_store(|store| {
                    Ok(record_remote_task_cancel_requested(
                        store.connection(),
                        &binding,
                        &connection.gateway_actor,
                        &access,
                        &request.id,
                    ))
                })
                .map_err(|_| INTERNAL)?;
            match result {
                Ok(Some(task)) => task_value(&task),
                Ok(None) => Err(NOT_FOUND),
                Err(memcore::MemoryError::InvalidArg(_)) => Err(RpcError {
                    code: -32002,
                    message: "Task is not cancelable",
                }),
                Err(_) => Err(INTERNAL),
            }
        }
        "SubscribeToTask" => {
            let request: profile::TaskId = typed(params)?;
            if !profile::opaque_token(&request.id, 128) {
                return Err(INVALID);
            }
            storage(connection.server.with_global_store(|store| {
                Ok(read_remote_task(
                    store.connection(),
                    &binding,
                    &access,
                    &request.id,
                ))
            }))?
            .ok_or(NOT_FOUND)?;
            Err(RpcError {
                code: -32004,
                message: "Streaming is unavailable for this task",
            })
        }
        _ => Err(RpcError {
            code: -32601,
            message: "Method not found",
        }),
    }
}

fn task_value(task: &RemoteTask) -> Result<Value, RpcError> {
    let envelope = &task.envelope;
    let status_state = match envelope.current_state.as_str() {
        "remote_rejected" => "TASK_STATE_REJECTED",
        "remote_admitted" => "TASK_STATE_SUBMITTED",
        "remote_started" => "TASK_STATE_WORKING",
        "remote_cancel_requested" => {
            if task
                .receipts
                .iter()
                .any(|entry| entry.receipt.state == "remote_started")
            {
                "TASK_STATE_WORKING"
            } else {
                "TASK_STATE_SUBMITTED"
            }
        }
        "remote_completed" => "TASK_STATE_COMPLETED",
        "remote_failed" => "TASK_STATE_FAILED",
        "remote_canceled" => "TASK_STATE_CANCELED",
        _ => return Err(INTERNAL),
    };
    let status_receipt = task
        .receipts
        .iter()
        .find(|entry| entry.receipt.state == envelope.current_state && entry.detail.is_some())
        .ok_or(INTERNAL)?;
    let (local_claim, local_run_id) = if envelope.current_state == "remote_rejected" {
        (Value::Null, Value::Null)
    } else {
        let Some(run) = task.run.as_ref() else {
            return Err(INTERNAL);
        };
        (
            json!({"claim_id":run.local_claim_id,"transition_version":run.local_claim_version}),
            Value::String(run.local_run_id.clone()),
        )
    };
    let receipts = task
        .receipts
        .iter()
        .map(|entry| {
            let receipt = &entry.receipt;
            let fact = match receipt.state.as_str() {
                "remote_received" => "received",
                "remote_rejected" => "authorization_refused",
                "remote_admitted" => "admitted",
                "remote_started" => "started",
                "remote_cancel_requested" => "cancel_requested",
                "remote_completed" => "completed",
                "remote_failed" => "failed",
                "remote_canceled" => "canceled",
                "remote_cleanup" => "cleanup",
                _ => return Err(INTERNAL),
            };
            Ok(json!({
                "receipt_id":receipt.receipt_id,
                "revision":receipt.envelope_version,
                "fact":fact,
                "occurred_at":receipt.occurred_at,
                "actor":receipt.actor_agent_identity_id,
                "admission_ref":receipt.actor_admission_id,
                "detail":entry.detail
            }))
        })
        .collect::<Result<Vec<_>, RpcError>>()?;
    Ok(json!({
        "id":envelope.envelope_id, "contextId":format!("context:{}",envelope.envelope_id),
        "status":{"state":status_state,"timestamp":status_receipt.receipt.occurred_at},
        "metadata":{(PROVENANCE):{
            "schema_version":"tachi.remote_receipt.v1", "task_id":envelope.envelope_id,
            "input_digest":envelope.body_digest,"origin":envelope.issuer_agent_identity_id,
            "origin_admission_ref":envelope.issuer_admission_id,
            "destination":envelope.recipient_agent_identity_id,"destination_host_id":task.header.destination_host_id,
            "trust_domain":envelope.issuer_trust_domain,"repository":task.header.repository,
            "head_sha":task.header.head_sha,"job":task.header.job,
            "origin_claim":{"claim_id":task.header.origin_claim_id,"transition_version":task.header.origin_claim_version},
            "local_claim":local_claim,"local_run_id":local_run_id,"revision":envelope.state_version,"receipts":receipts
        }}
    }))
}

pub(super) async fn agent_card(State(connection): State<Arc<Connection>>) -> Response {
    if !matches!(recheck(&connection).await, Ok(Ok(()))) {
        return admission_unavailable();
    }
    let Some(endpoint) = &connection.config.public_endpoint else {
        return response(
            StatusCode::NOT_FOUND,
            json!({"error":"remote profile is not configured"}),
        );
    };
    let Ok(url) = url::Url::parse(endpoint).and_then(|url| url.join("/rpc")) else {
        return response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error":"remote profile unavailable"}),
        );
    };
    let (card_desc, provenance_desc, skill_name, skill_desc) =
        match connection.config.execution_mode {
            DestinationExecutionMode::RefusalOnly => (
                "Authenticated task receipts; execution is unavailable.",
                "Authenticated host refusal receipts",
                "Record remote task refusal",
                "Validate a bounded review or verification request and retain its refusal for reconciliation. No worker is started.",
            ),
            DestinationExecutionMode::AdmitOnly => (
                "Authenticated task admission and run-correlation receipts; worker launch is disabled.",
                "Authenticated host admission and lifecycle receipts",
                "Admit bounded remote task",
                "Validate a bounded review or verification request, verify commit and local claim/slot admission, and retain correlation receipts without launching a worker.",
            ),
        };
    response(
        StatusCode::OK,
        json!({
            "name":"Tachi remote task admission", "description":card_desc,
            "version":"1.0", "supportedInterfaces":[{"url":url.as_str(),"protocolBinding":"JSONRPC","protocolVersion":"1.0"}],
            "capabilities":{"streaming":false,"pushNotifications":false,"extensions":[
                {"uri":DELEGATION,"required":true,"description":"Closed, repository-granted remote jobs"},
                {"uri":PROVENANCE,"required":true,"description":provenance_desc}
            ]},
            "defaultInputModes":["application/json"],"defaultOutputModes":["application/json"],
            "securitySchemes":{"owner_mtls":{"mtlsSecurityScheme":{}}},
            "securityRequirements":[{"schemes":{"owner_mtls":{"list":[]}}}],
            "skills":[{"id":"remote_job_admission","name":skill_name,
                "description":skill_desc,
                "tags":["review","verification"]}]
        }),
    )
}
