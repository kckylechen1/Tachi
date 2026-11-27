//! Authenticated inbound tasks and truthful refusal receipts. Execution is
//! unavailable until a destination claim/resource/lifecycle adapter is admitted.

use super::admission::Connection;
use super::profile::{self, DELEGATION, PROVENANCE};
use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use memcore::db::a2a::{
    insert_remote_rejection, is_retained_task_quota_exceeded, list_remote_tasks, read_remote_task,
    NewRemoteTask, RemoteJobGrant, RemoteTask, RemoteTaskAccess, RemoteTaskHeader,
    RemoteTaskInsertOutcome,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
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
                    origin_claim_id: job.origin_claim.claim_id.clone(),
                    origin_claim_version: job.origin_claim.transition_version as i64,
                    destination_host_id: connection.config.host_id.clone(),
                },
            };
            let outcome = storage(connection.server.with_global_store(|store| {
                Ok(insert_remote_rejection(
                    store.connection(),
                    &binding,
                    &connection.gateway_actor,
                    &access,
                    &request,
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
                Ok(read_remote_task(
                    store.connection(),
                    &binding,
                    &access,
                    &request.id,
                ))
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
        "CancelTask" | "SubscribeToTask" => {
            let request: profile::TaskId = typed(params)?;
            if !profile::opaque_token(&request.id, 128) {
                return Err(INVALID);
            }
            // Resolve authorization first, even when this terminal-only slice
            // cannot cancel or stream. Knowing another peer's ID reveals nothing.
            storage(connection.server.with_global_store(|store| {
                Ok(read_remote_task(
                    store.connection(),
                    &binding,
                    &access,
                    &request.id,
                ))
            }))?
            .ok_or(NOT_FOUND)?;
            Err(if method == "CancelTask" {
                RpcError {
                    code: -32002,
                    message: "Task is not cancelable",
                }
            } else {
                RpcError {
                    code: -32004,
                    message: "Streaming is unavailable for this terminal task",
                }
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
    let terminal = task
        .receipts
        .iter()
        .find(|entry| {
            entry.receipt.state == "remote_rejected"
                && entry.receipt.envelope_version == envelope.state_version
                && entry.detail.is_some()
        })
        .ok_or(INTERNAL)?;
    if envelope.current_state != "remote_rejected" {
        return Err(INTERNAL);
    }
    let receipts: Vec<_> = task.receipts.iter().map(|entry| {
        let receipt = &entry.receipt;
        json!({"receipt_id":receipt.receipt_id,"revision":receipt.envelope_version,
            "fact":if receipt.state == "remote_received" {"received"} else {"authorization_refused"},
            "occurred_at":receipt.occurred_at,"actor":receipt.actor_agent_identity_id,
            "admission_ref":receipt.actor_admission_id,"detail":entry.detail})
    }).collect();
    Ok(json!({
        "id":envelope.envelope_id, "contextId":format!("context:{}",envelope.envelope_id),
        "status":{"state":"TASK_STATE_REJECTED","timestamp":terminal.receipt.occurred_at},
        "metadata":{(PROVENANCE):{
            "schema_version":"tachi.remote_receipt.v1", "task_id":envelope.envelope_id,
            "input_digest":envelope.body_digest,"origin":envelope.issuer_agent_identity_id,
            "origin_admission_ref":envelope.issuer_admission_id,
            "destination":envelope.recipient_agent_identity_id,"destination_host_id":task.header.destination_host_id,
            "trust_domain":envelope.issuer_trust_domain,"repository":task.header.repository,
            "head_sha":task.header.head_sha,"job":task.header.job,
            "origin_claim":{"claim_id":task.header.origin_claim_id,"transition_version":task.header.origin_claim_version},
            "local_claim":null,"local_run_id":null,"revision":envelope.state_version,"receipts":receipts
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
    response(
        StatusCode::OK,
        json!({
            "name":"Tachi remote task admission", "description":"Authenticated task receipts; execution is unavailable.",
            "version":"1.0", "supportedInterfaces":[{"url":url.as_str(),"protocolBinding":"JSONRPC","protocolVersion":"1.0"}],
            "capabilities":{"streaming":false,"pushNotifications":false,"extensions":[
                {"uri":DELEGATION,"required":true,"description":"Closed, repository-granted remote jobs"},
                {"uri":PROVENANCE,"required":true,"description":"Authenticated host refusal receipts"}
            ]},
            "defaultInputModes":["application/json"],"defaultOutputModes":["application/json"],
            "securitySchemes":{"owner_mtls":{"mtlsSecurityScheme":{}}},
            "securityRequirements":[{"schemes":{"owner_mtls":{"list":[]}}}],
            "skills":[{"id":"remote_job_admission","name":"Record remote task refusal",
                "description":"Validate a bounded review or verification request and retain its refusal for reconciliation. No worker is started.",
                "tags":["review","verification"]}]
        }),
    )
}
