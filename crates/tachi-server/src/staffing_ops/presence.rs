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
    project: String,
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
    let Some(project) = runtime.session_project.clone() else {
        return;
    };
    let Ok((project, _)) = server.resolve_server_named_project_binding(&project) else {
        return;
    };
    let owner = runtime.rate_limit_session_id.clone();
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
            project,
            client: runtime.session_client.clone(),
            identity,
            owner,
            harness: None,
            dispatch_id: None,
            state: "recently_seen",
            basis: "mcp_request",
            observed_at: Utc::now(),
            ready_at: None,
            assignment: None,
        },
    );
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
    let runtime = server.agent_runtime_read();
    let key = format!("worker:{dispatch_id}");
    server.staff_presence.insert(
        key.clone(),
        Observation {
            id: key.clone(),
            kind: "worker",
            project,
            client: runtime.session_client.clone(),
            identity: runtime
                .work_claim_connection
                .as_ref()
                .and_then(|c| c.agent_identity_id.clone()),
            owner: runtime.rate_limit_session_id.clone(),
            harness: Some(harness.to_owned()),
            dispatch_id: Some(dispatch_id.to_owned()),
            state: "starting",
            basis: "dispatch_accepted",
            observed_at: Utc::now(),
            ready_at: None,
            assignment: Some(assignment),
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
    let Some(requested) = requested.or(bound.as_deref()) else {
        return Ok(None);
    };
    let (project, _) = server.resolve_server_named_project_binding(requested)?;
    if let Some(bound) = bound {
        let (bound, _) = server.resolve_server_named_project_binding(&bound)?;
        if bound != project {
            return Err("presence project must match the bound session".into());
        }
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
    let Some(project) = resolve_project(server, project)? else {
        return Ok(
            json!({"available": false, "reason": "project_binding_required", "items": [], "count": 0}),
        );
    };
    let limit = limit.min(DISPLAY_CAP);
    if limit == 0 {
        return Ok(
            json!({"available": true, "project": project, "items": [], "count": 0, "overflow": 0}),
        );
    }
    let now = Utc::now();
    let claims = server.with_global_store_read(|store| {
        memcore::db::session_claims::list_project_work_claims_bounded(
            store.connection(),
            &project,
            CAPACITY + 1,
        )
        .map_err(|error| error.to_string())
    });
    let (claims, claim_error) = match claims {
        Ok(rows) => (rows, None),
        Err(_) => (Vec::new(), Some("project_claims_unavailable")),
    };
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
        .filter(|row| row.project == project)
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
        let assignment_overflow = assignments.len().saturating_sub(ASSIGNMENT_CAP);
        assignments.truncate(ASSIGNMENT_CAP);
        let dispatch_overflow = active_dispatches.len().saturating_sub(ASSIGNMENT_CAP);
        active_dispatches.truncate(ASSIGNMENT_CAP);
        items.push(json!({"presence_id": row.id, "kind": row.kind, "client": row.client.as_deref().map(safe),
            "agent_identity_id": row.identity.as_deref().map(safe), "identity_trust": "asserted_label",
            "harness": row.harness.as_deref().map(safe), "dispatch_id": row.dispatch_id.as_deref().map(safe),
            "state": state, "basis": row.basis, "observed_at": row.observed_at,
            "ready_observed_at": row.ready_at, "accepts_new_work": Value::Null,
            "active_dispatch_ids": active_dispatches, "active_dispatch_overflow": dispatch_overflow,
            "assignments": assignments, "assignment_overflow": assignment_overflow}));
    }
    items.sort_by(|a, b| b["observed_at"].as_str().cmp(&a["observed_at"].as_str()));
    let total = items.len();
    items.truncate(limit);
    let omitted = server
        .staff_presence
        .omitted
        .load(std::sync::atomic::Ordering::Relaxed);
    Ok(
        json!({"available": true, "project": project, "controller_epoch": server.controller_epoch,
        "source": "daemon_observations", "advisory": true, "client_ttl_seconds": CLIENT_TTL_SECONDS,
        "count": items.len(), "overflow": total.saturating_sub(items.len()), "items": items,
        "incomplete": omitted > 0 || claim_error.is_some() || claims_truncated,
        "registry_omitted": omitted, "claims_truncated": claims_truncated, "claim_error": claim_error}),
    )
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
