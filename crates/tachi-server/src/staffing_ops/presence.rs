//! Daemon-local observations, never launch authority or a second task ledger.
use crate::MemoryServer;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

const CAPACITY: usize = 512;
const DISPLAY_CAP: usize = 100;
const ASSIGNMENT_CAP: usize = 8;
const CLIENT_TTL_SECONDS: i64 = 300;

#[derive(Clone, Debug)]
struct Observation {
    id: String,
    kind: &'static str,
    project_db: std::path::PathBuf,
    client: Option<String>,
    identity: Option<String>,
    owner: String,
    harness: Option<String>,
    dispatch_id: Option<String>,
    state: &'static str,
    basis: &'static str,
    observed_at: DateTime<Utc>,
    ready_at: Option<DateTime<Utc>>,
    assignment: Option<Assignment>,
    reported_work: Option<ReportedWork>,
}

#[derive(Default, Debug)]
pub(crate) struct Registry {
    rows: Mutex<BTreeMap<String, Observation>>,
    omitted: std::sync::atomic::AtomicU64,
}

impl Registry {
    fn insert(&self, key: String, mut row: Observation) {
        let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        if !rows.contains_key(&key) && rows.len() >= CAPACITY {
            let removable = rows
                .iter()
                .filter(|(_, row)| row.kind == "client" || row.state == "disconnected")
                .min_by_key(|(_, row)| row.observed_at)
                .map(|(key, _)| key.clone());
            self.omitted
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if let Some(key) = removable {
                rows.remove(&key);
            } else {
                return;
            }
        }
        if let Some(previous) = rows.get(&key) {
            row.id = previous.id.clone();
        }
        rows.insert(key, row);
    }

    fn transition(&self, key: &str, state: &'static str, basis: &'static str) {
        let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(row) = rows.get_mut(key) {
            if row.state == "disconnected" && state != "disconnected" {
                return;
            }
            row.state = state;
            row.basis = basis;
            row.observed_at = Utc::now();
            if state == "ready" {
                row.ready_at = Some(row.observed_at);
            }
        }
    }
}

fn safe(value: &str) -> String {
    crate::claims_ops::sanitize_presence_field(value, 160)
}

/// A request is a sighting, not proof of a persistent connection or readiness.
pub(crate) fn observe_client(server: &MemoryServer) {
    let runtime = server.agent_runtime_read().clone();
    let Some(project) = runtime
        .presence_report
        .as_ref()
        .map(|r| r.project.clone())
        .or_else(|| runtime.session_project.clone())
    else {
        return;
    };
    let Ok((project, _)) = server.resolve_server_named_project_binding(&project) else {
        return;
    };
    let Ok(project_db) = server.resolve_server_named_project_db_identity(&project) else {
        return;
    };
    let owner = runtime.presence_session_id.clone();
    let key = format!("client:{owner}");
    let identity = runtime
        .work_claim_connection
        .as_ref()
        .and_then(|c| c.agent_identity_id.clone());
    server.staff_presence.insert(
        key,
        Observation {
            id: format!("client-{}", uuid::Uuid::new_v4()),
            kind: "client",
            project_db,
            client: runtime.presence_client().map(str::to_owned),
            identity,
            owner,
            harness: None,
            dispatch_id: None,
            state: "recently_seen",
            basis: "mcp_request",
            observed_at: Utc::now(),
            ready_at: None,
            assignment: None,
            reported_work: runtime.presence_report.clone(),
        },
    );
}

/// Public, ephemeral work intent. Never used for identity admission or claims.
#[derive(Clone, Debug)]
pub(crate) struct ReportedWork {
    session_id: Option<String>,
    project: String,
    agent: Option<String>,
    cwd: Option<String>,
    summary: Option<String>,
    issue_ref: Option<String>,
    flow_id: Option<String>,
}

impl ReportedWork {
    fn public_view(&self) -> Value {
        json!({"project": self.project, "agent": self.agent, "cwd": self.cwd,
            "summary": self.summary, "issue_ref": self.issue_ref,
            "flow_id": self.flow_id.as_deref().map(safe)})
    }
}

/// Brief/board already carry the agent's working context: publish it without
/// an extra registration call, durable claim, or transport-project mutation.
pub(crate) fn report_task_context(
    server: &MemoryServer,
    params: &tachi_params::TachiTaskParams,
) -> Result<(), String> {
    let result = try_report_task_context(server, params);
    server.agent_runtime_write().presence_report_error = result.as_ref().err().map(|e| safe(e));
    // Older briefing callers used cwd for document resolution (including relative
    // paths). Keep their briefing usable and make missing presence explicit.
    if params.session_id.is_none() {
        Ok(())
    } else {
        result
    }
}

fn try_report_task_context(
    server: &MemoryServer,
    params: &tachi_params::TachiTaskParams,
) -> Result<(), String> {
    if params.session_id.is_none()
        && params.agent_id.is_none()
        && params.cwd.is_none()
        && params.task.is_none()
    {
        return Ok(());
    }
    let session = params
        .session_id
        .as_deref()
        .map(|value| {
            let value = value.trim();
            if value.is_empty()
                || value.len() > 128
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
            {
                return Err(
                    "session_id must contain 1-128 ASCII letters, digits, '.', '_', ':' or '-'"
                        .to_string(),
                );
            }
            Ok(value)
        })
        .transpose()?;
    let cwd = params
        .cwd
        .as_deref()
        .map(|cwd| {
            if cwd.trim().is_empty() || cwd.len() > 2048 || cwd.chars().any(char::is_control) {
                return Err(
                    "presence cwd must be a nonempty absolute directory (at most 2048 bytes)"
                        .to_string(),
                );
            }
            server
                .resolve_existing_workspace_project(cwd)
                .map(|project| (cwd.to_string(), project))
        })
        .transpose()?;
    let runtime = server.agent_runtime_read().clone();
    let project = if let Some((_, project)) = &cwd {
        resolve_project(server, Some(project))?
    } else {
        resolve_project(server, params.project.as_deref())?
    };
    let Some(project) = project else {
        return Ok(());
    };
    if let Some(requested) = params.project.as_deref() {
        let requested = resolve_project(server, Some(requested))?
            .ok_or_else(|| "presence project is unavailable".to_string())?;
        if server.resolve_server_named_project_db_identity(&requested)?
            != server.resolve_server_named_project_db_identity(&project)?
        {
            return Err("presence cwd and project must refer to the same project".into());
        }
    }
    if params.flow_id.as_ref().is_some_and(|flow| flow.len() > 160) {
        return Err("presence flow_id must be at most 160 bytes".into());
    }
    let report = ReportedWork {
        session_id: session.map(str::to_owned),
        project,
        agent: params
            .agent_id
            .as_deref()
            .or(runtime.presence_client())
            .map(safe),
        cwd: cwd.map(|(cwd, _)| crate::claims_ops::sanitize_presence_field(&cwd, 2048)),
        summary: params.task.as_deref().map(safe),
        issue_ref: params.issue_ref.as_deref().map(safe),
        flow_id: params.flow_id.clone(),
    };
    {
        let mut state = server.agent_runtime_write();
        if let Some(session) = session {
            use sha2::{Digest, Sha256};
            let identity = state
                .work_claim_connection
                .as_ref()
                .and_then(|c| c.agent_identity_id.as_deref());
            let key = json!([identity, state.presence_client(), session]).to_string();
            state.presence_session_id = format!("reported-{:x}", Sha256::digest(key.as_bytes()));
        }
        state.presence_report = Some(report);
    }
    // Remove only this connection's superseded anonymous sighting.
    let current = server.agent_runtime_read().presence_session_id.clone();
    if current != runtime.presence_session_id && runtime.presence_report.is_none() {
        server
            .staff_presence
            .rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&format!("client:{}", runtime.presence_session_id));
    }
    observe_client(server);
    Ok(())
}

#[derive(Clone, Debug)]
pub(crate) struct Assignment {
    pub summary: String,
    pub issue_ref: Option<String>,
    pub flow_id: Option<String>,
}

#[derive(Clone)]
pub(crate) struct WorkerObservation {
    registry: Arc<Registry>,
    key: String,
}

pub(crate) struct WorkerGuard(WorkerObservation);
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.0
            .registry
            .transition(&self.0.key, "disconnected", "runner_owner_released");
    }
}

tokio::task_local! { static WORKER: Option<WorkerObservation>; }

pub(crate) async fn observe_runner<F: Future>(
    worker: Option<WorkerObservation>,
    future: F,
) -> F::Output {
    WORKER.scope(worker, future).await
}

pub(crate) fn current_worker() -> Option<WorkerObservation> {
    WORKER.try_with(Clone::clone).ok().flatten()
}

pub(crate) fn worker_event(state: &'static str, basis: &'static str) {
    if let Some(worker) = current_worker() {
        worker.registry.transition(&worker.key, state, basis);
    }
}

pub(crate) fn register_worker(
    server: &MemoryServer,
    dispatch_id: &str,
    harness: &str,
    project: Option<&str>,
    mut assignment: Assignment,
) -> Option<(WorkerObservation, WorkerGuard)> {
    let project = project
        .map(str::to_owned)
        .or_else(|| server.session_project())?;
    let (project, _) = server.resolve_server_named_project_binding(&project).ok()?;
    assignment.summary = assignment.summary.chars().take(200).collect();
    let project_db = server
        .resolve_server_named_project_db_identity(&project)
        .ok()?;
    let runtime = server.agent_runtime_read();
    let key = format!("worker:{dispatch_id}");
    server.staff_presence.insert(
        key.clone(),
        Observation {
            id: key.clone(),
            kind: "worker",
            project_db,
            client: runtime.presence_client().map(str::to_owned),
            identity: runtime
                .work_claim_connection
                .as_ref()
                .and_then(|c| c.agent_identity_id.clone()),
            owner: runtime.presence_session_id.clone(),
            harness: Some(harness.to_owned()),
            dispatch_id: Some(dispatch_id.to_owned()),
            state: "starting",
            basis: "dispatch_accepted",
            observed_at: Utc::now(),
            ready_at: None,
            assignment: Some(assignment),
            reported_work: None,
        },
    );
    let observation = WorkerObservation {
        registry: server.staff_presence.clone(),
        key,
    };
    Some((observation.clone(), WorkerGuard(observation)))
}

fn resolve_project(
    server: &MemoryServer,
    requested: Option<&str>,
) -> Result<Option<String>, String> {
    let bound = server.session_project();
    let reported = server
        .agent_runtime_read()
        .presence_report
        .as_ref()
        .map(|r| r.project.clone());
    let Some(requested) = requested.or(bound.as_deref()).or(reported.as_deref()) else {
        return Ok(None);
    };
    let (project, _) = server.resolve_server_named_project_binding(requested)?;
    if let Some(bound) = bound {
        let (bound, _) = server.resolve_server_named_project_binding(&bound)?;
        if server.resolve_server_named_project_db_identity(&bound)?
            != server.resolve_server_named_project_db_identity(&project)?
        {
            return Err("presence project must match the bound session".into());
        }
        return Ok(Some(bound));
    }
    Ok(Some(project))
}

/// Shared by Staffing and the task board. No status files are rewritten.
pub(crate) fn roster(
    server: &MemoryServer,
    project: Option<&str>,
    flow: Option<&str>,
    limit: usize,
) -> Result<Value, String> {
    let runtime = server.agent_runtime_read().clone();
    let identity = runtime
        .work_claim_connection
        .as_ref()
        .and_then(|connection| connection.agent_identity_id.as_deref());
    let self_view = json!({
        "client": runtime.presence_client().map(safe),
        "session_id": runtime.presence_report.as_ref().and_then(|r| r.session_id.as_deref()),
        "presence_session_key": runtime.presence_session_id,
        "reported_work": runtime.presence_report.as_ref().map(ReportedWork::public_view),
        "reported": runtime.presence_report.is_some(),
        "report_error": runtime.presence_report_error,
        "agent_identity_id": identity.map(safe),
        "project_bound": runtime.session_project.is_some(),
        "identity_configured": identity.is_some(),
        "identity_header": "x-tachi-agent-identity",
        "workspace_header": "x-tachi-workspace-root",
    });
    let Some(project) = resolve_project(server, project)? else {
        return Ok(
            json!({"available": false, "reason": "project_binding_required", "self": self_view, "items": [], "count": 0}),
        );
    };
    let limit = limit.min(DISPLAY_CAP);
    if limit == 0 {
        return Ok(
            json!({"available": true, "project": project, "self": self_view, "items": [], "count": 0, "overflow": 0}),
        );
    }
    let now = Utc::now();
    // Published work intent is public to same-project observers. Canonical
    // WorkClaims retain the existing admission + active-lease read boundary.
    let binding = crate::claims_ops::project_ownership::LocalProject::capture(server);
    let claims = binding
        .filter(|binding| binding.project == project)
        .map(|binding| {
            binding.with_current(server, |conn| {
                if !memcore::db::session_claims::has_current_project_claim(
                    conn,
                    &binding.project,
                    &binding.identity,
                    &binding.connection_id,
                    &now.to_rfc3339(),
                )? {
                    return Ok(None);
                }
                memcore::db::session_claims::list_current_project_work_claims_bounded(
                    conn,
                    &binding.project,
                    &now.to_rfc3339(),
                    flow,
                    CAPACITY + 1,
                )
                .map(Some)
            })
        });
    let (claims, claims_available, claim_error) = match claims {
        Some(Ok(Some(rows))) => (rows, true, None),
        Some(Err(_)) => (Vec::new(), false, Some("project_claims_unavailable")),
        _ => (Vec::new(), false, None),
    };
    let project_db = server.resolve_server_named_project_db_identity(&project)?;
    let claims_truncated = claims.len() > CAPACITY;
    let claims: Vec<_> = claims
        .iter()
        .take(CAPACITY)
        .filter(|c| {
            c.state == memcore::ClaimState::Active
                && c.lease_expires_at
                    .as_deref()
                    .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                    .is_some_and(|t| t > now)
        })
        .collect();
    let rows: Vec<_> = server
        .staff_presence
        .rows
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .filter(|row| row.project_db == project_db)
        .cloned()
        .collect();
    let mut items = Vec::new();
    for row in &rows {
        let mut assignments: Vec<_> = claims.iter().filter(|c| {
            if let Some(dispatch) = row.dispatch_id.as_deref() { c.dispatch_id.as_deref() == Some(dispatch) }
            else { row.identity.is_some() && c.agent_identity_id == row.identity }
        }).filter(|c| flow.is_none_or(|f| c.flow_id.as_deref() == Some(f))).map(|c| json!({
            "claim_id": safe(&c.claim_id), "dispatch_id": c.dispatch_id.as_deref().map(safe),
            "issue_ref": c.issue_ref.as_deref().map(safe), "flow_id": c.flow_id.as_deref().map(safe),
            "role": c.role.as_deref().map(safe), "source": "work_claim", "online_evidence": false,
            "worktree_path": c.worktree_path.as_deref().map(safe),
            "heartbeat_at": c.heartbeat_at, "lease_expires_at": c.lease_expires_at,
            "transition_version": c.transition_version,
        })).collect();
        for worker in rows.iter().filter(|w| {
            w.kind == "worker" && (w.id == row.id || row.kind == "client" && w.owner == row.owner)
        }) {
            if let Some(a) = worker
                .assignment
                .as_ref()
                .filter(|a| flow.is_none_or(|f| a.flow_id.as_deref() == Some(f)))
            {
                assignments.push(
                    json!({"dispatch_id": worker.dispatch_id.as_deref().map(safe),
                    "summary": safe(&a.summary), "issue_ref": a.issue_ref.as_deref().map(safe),
                    "flow_id": a.flow_id.as_deref().map(safe), "source": "dispatch_assignment",
                    "worker_state": worker.state}),
                );
            }
        }
        if let Some(report) = row
            .reported_work
            .as_ref()
            .filter(|r| flow.is_none_or(|f| r.flow_id.as_deref() == Some(f)))
        {
            assignments.push(
                json!({"summary": report.summary, "issue_ref": report.issue_ref,
                "flow_id": report.flow_id.as_deref().map(safe), "source": "session_report", "authority": false}),
            );
        }
        // A flow-filtered view must not spill other project work into the reply.
        if flow.is_some() && assignments.is_empty() {
            continue;
        }
        let state = if row.kind == "client"
            && (now - row.observed_at).num_seconds() >= CLIENT_TTL_SECONDS
        {
            "stale"
        } else {
            row.state
        };
        let mut active_dispatches: Vec<_> = rows
            .iter()
            .filter(|worker| {
                worker.kind == "worker"
                    && (worker.id == row.id || row.kind == "client" && worker.owner == row.owner)
                    && worker.state != "disconnected"
                    && flow.is_none_or(|f| {
                        worker
                            .assignment
                            .as_ref()
                            .is_some_and(|a| a.flow_id.as_deref() == Some(f))
                    })
            })
            .filter_map(|worker| worker.dispatch_id.as_deref().map(safe))
            .collect();
        assignments
            .sort_by_key(|assignment| assignment["worker_state"].as_str() == Some("disconnected"));
        let assignment_overflow = assignments.len().saturating_sub(ASSIGNMENT_CAP);
        assignments.truncate(ASSIGNMENT_CAP);
        let dispatch_overflow = active_dispatches.len().saturating_sub(ASSIGNMENT_CAP);
        active_dispatches.truncate(ASSIGNMENT_CAP);
        items.push(json!({"presence_id": row.id, "kind": row.kind, "client": row.client.as_deref().map(safe),
            "is_self": row.kind == "client" && row.owner == runtime.presence_session_id,
            "reported_work": row.reported_work.as_ref().filter(|r| flow.is_none_or(|f| r.flow_id.as_deref() == Some(f))).map(ReportedWork::public_view),
            "agent_identity_id": row.identity.as_deref().map(safe), "identity_trust": "asserted_label",
            "harness": row.harness.as_deref().map(safe), "dispatch_id": row.dispatch_id.as_deref().map(safe),
            "state": state, "basis": row.basis, "observed_at": row.observed_at,
            "ready_observed_at": row.ready_at, "accepts_new_work": Value::Null,
            "active_dispatch_ids": active_dispatches, "active_dispatch_overflow": dispatch_overflow,
            "assignments": assignments, "assignment_overflow": assignment_overflow}));
    }
    items.sort_by(|a, b| {
        let priority = |row: &Value| match row["state"].as_str() {
            Some("starting" | "ready" | "busy") => 0,
            Some("recently_seen") => 1,
            _ => 2,
        };
        priority(a)
            .cmp(&priority(b))
            .then_with(|| b["observed_at"].as_str().cmp(&a["observed_at"].as_str()))
    });
    let total = items.len();
    items.truncate(limit);
    let omitted = server
        .staff_presence
        .omitted
        .load(std::sync::atomic::Ordering::Relaxed);
    Ok(
        json!({"available": true, "project": project, "controller_epoch": server.controller_epoch,
        "self": self_view,
        "source": "daemon_observations", "advisory": true, "client_ttl_seconds": CLIENT_TTL_SECONDS,
        "count": items.len(), "overflow": total.saturating_sub(items.len()), "items": items,
        "incomplete": omitted > 0 || claim_error.is_some() || claims_truncated,
        "registry_omitted": omitted, "claims_truncated": claims_truncated, "claim_error": claim_error,
        "claims_available": claims_available,
        "claims_unavailable_reason": if claims_available { None } else { Some("project_read_eligibility_required_or_unavailable") }}),
    )
}

/// Optional peer context must not turn a usable task result into a failure.
pub(crate) fn task_roster(
    server: &MemoryServer,
    project: Option<&str>,
    flow: Option<&str>,
    limit: usize,
) -> Value {
    roster(server, project, flow, limit).unwrap_or_else(|_| {
        json!({
            "available": false, "reason": "presence_project_unavailable", "items": [], "count": 0,
        })
    })
}

pub(crate) fn staff_roster(
    server: &MemoryServer,
    params: &tachi_params::TachiStaffParams,
) -> Result<String, String> {
    for (name, supplied) in [
        ("task", params.task.is_some()),
        ("dispatch_id", params.dispatch_id.is_some()),
        ("staffing_reason", params.staffing_reason.is_some()),
        ("profile", params.profile.is_some()),
        ("worker", params.worker.is_some()),
        ("stage", params.stage.is_some()),
        ("issue_ref", params.issue_ref.is_some()),
        ("pr_ref", params.pr_ref.is_some()),
        ("recommendation_ref", params.recommendation_ref.is_some()),
        ("declared_file_scope", params.declared_file_scope.is_some()),
        (
            "expected_status_revision",
            params.expected_status_revision.is_some(),
        ),
    ] {
        if supplied {
            return Err(format!("tachi_staff: roster rejects `{name}`"));
        }
    }
    serde_json::to_string(&roster(
        server,
        params.project.as_deref(),
        params.flow_id.as_deref(),
        DISPLAY_CAP,
    )?)
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
