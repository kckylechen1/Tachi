//! A live Native ACP permission waiter. Durable state remains in status.json;
//! only this runner's one-shot response channel can resume the child process.
use crate::claims_ops::{current_run_owner, RunOwner};
use crate::managed_run_control::{AnchoredRunStatus, StatusMutationFence};
use crate::MemoryServer;
use chrono::Utc;
use rmcp::model::{InputRequest, InputRequests, InputResponses};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

const MAX_BYTES: usize = 1024 * 1024;
const WAIT_LIMIT: Duration = Duration::from_secs(300);
const FIELD: &str = "acp_permission";
const HISTORY: &str = "acp_permission_history";
const MAX_DECISIONS: usize = 64;

#[cfg(test)]
pub(crate) mod tests;

#[derive(Clone)]
pub(crate) struct NativeAcpApprovalContext {
    server: MemoryServer,
    owner: RunOwner,
    admission: Admission,
    grant_digest: String,
}

impl std::fmt::Debug for NativeAcpApprovalContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeAcpApprovalContext")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
struct Admission {
    agent: String,
    connection: String,
    verified: Option<crate::server_state::VerifiedAdmissionContext>,
}

impl Admission {
    fn capture(server: &MemoryServer) -> Result<Self, String> {
        let (Some(agent), connection, state) = server
            .work_claim_connection()
            .ok_or("permission admission unavailable")?
        else {
            return Err("permission principal unavailable".into());
        };
        let verified = server.verified_work_claim_connection();
        if state != "self_asserted" && !(state == "verified" && verified.is_some()) {
            return Err("permission admission unavailable".into());
        }
        Ok(Self {
            agent,
            connection,
            verified,
        })
    }

    fn current(&self, conn: &rusqlite::Connection) -> Result<bool, memcore::MemoryError> {
        if let Some(v) = &self.verified {
            return memcore::has_current_verified_admission(
                conn,
                &v.admission_id,
                &v.agent_identity_id,
                &v.connection_id,
                &v.issuer_id,
                &v.verification_method,
                &v.verification_version,
                &v.trust_domain,
                &v.verification_scope,
            );
        }
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM identity_admissions WHERE agent_identity_id=?1 AND connection_id=?2 AND state='self_asserted')",
            rusqlite::params![self.agent, self.connection], |row| row.get(0),
        )?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingPermission {
    schema: u32,
    request_id: String,
    dispatch_id: String,
    controller_epoch: String,
    owner: RunOwner,
    grant_digest: String,
    request_digest: String,
    status_revision: u64,
    expires_at: String,
    input: InputRequest,
    decision: Option<PermissionDecision>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionDecision {
    event: PermissionEvent,
    request_id: String,
    controller_epoch: String,
    owner: RunOwner,
    request_digest: String,
    grant_digest: String,
    status_revision: u64,
    approved: bool,
    decided_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum PermissionEvent {
    #[serde(rename = "acp_permission_decided")]
    Decided,
    #[serde(rename = "acp_permission_withdrawn")]
    Withdrawn,
}

struct Waiter {
    anchor: AnchoredRunStatus,
    pending: PendingPermission,
    admission: Admission,
    expires: Instant,
    active: Arc<AtomicBool>,
    sender: Option<oneshot::Sender<bool>>,
}

impl Waiter {
    fn live(&self) -> bool {
        self.active.load(Ordering::Acquire)
            && self.expires > Instant::now()
            && self
                .sender
                .as_ref()
                .is_some_and(|sender| !sender.is_closed())
    }

    fn matches(&self, anchor: &AnchoredRunStatus, status: &Value, owner: &RunOwner) -> bool {
        self.live()
            && self.anchor.same_opened_directory(anchor)
            && &self.pending.owner == owner
            && status["dispatch_id"] == self.pending.dispatch_id
            && status["state"] == "TASK_STATE_INPUT_REQUIRED"
            && status["status_revision"].as_u64() == Some(self.pending.status_revision)
            && serde_json::to_value(&self.pending).ok().as_ref() == Some(&status[FIELD])
    }
}

#[derive(Default)]
pub(crate) struct NativeAcpApprovals {
    pending: Mutex<HashMap<String, Waiter>>,
}

impl NativeAcpApprovalContext {
    pub(crate) fn from_environment(
        server: &MemoryServer,
        grant: &tachi_params::ExecutionGrant,
    ) -> Result<Option<Arc<Self>>, String> {
        match std::env::var("TACHI_ACP_APPROVAL_MODE").as_deref() {
            Err(std::env::VarError::NotPresent) | Ok("automatic") => Ok(None),
            Ok("interactive") => Ok(Some(Arc::new(Self {
                server: server.clone(),
                owner: current_run_owner(server)?,
                admission: Admission::capture(server)?,
                grant_digest: digest(&serde_json::to_vec(grant).map_err(|e| e.to_string())?),
            }))),
            _ => Err("TACHI_ACP_APPROVAL_MODE must be automatic or interactive".into()),
        }
    }

    pub(super) async fn decide(
        &self,
        run_dir: &Path,
        dispatch: &str,
        params: &Value,
    ) -> Result<bool, String> {
        if current_run_owner(&self.server)? != self.owner {
            return Err("permission owner changed".into());
        }
        let bytes = serde_json::to_vec(params).map_err(|e| e.to_string())?;
        if bytes.len() > 64 * 1024 {
            return Err("permission request exceeds 64 KiB".into());
        }
        let anchor = AnchoredRunStatus::open(run_dir)
            .map_err(|e| format!("permission anchor unavailable: {e:?}"))?;
        let (mut receiver, mut guard) = {
            let lock = anchor.lock();
            let _lock = lock.lock().unwrap_or_else(|p| p.into_inner());
            crate::mcp_tasks::require_run_owner(&anchor, &self.owner, dispatch)
                .map_err(|_| "permission run binding unavailable")?;
            let fence = anchor.acquire_fence()?;
            let mut status = read_status(&anchor)?;
            if decision_history(&status)?.len() >= MAX_DECISIONS {
                return Err("run reached its 64 interactive permission decisions limit".into());
            }
            if status["dispatch_id"] != dispatch || status["state"] != "TASK_STATE_WORKING" {
                return Err("permission run is not working".into());
            }
            let mut pending = self
                .server
                .native_acp_approvals
                .pending
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if pending.contains_key(dispatch) {
                return Err("run already has a permission waiter".into());
            }
            let revision = advance(&mut status)?;
            let request_id = format!("acp-permission:{}", uuid::Uuid::new_v4().simple());
            let title: String = params["toolCall"]["title"]
                .as_str()
                .unwrap_or("read/search operation")
                .chars()
                .filter(|c| !c.is_control())
                .take(256)
                .collect();
            let input = serde_json::from_value(json!({
                "method":"elicitation/create", "params":{
                    "mode":"form", "message":format!("Allow this single read/search operation within the existing execution grant? Agent-supplied title: {title}"),
                    "requestedSchema":{"type":"object", "properties":{"approve":{"type":"boolean", "title":"Allow once"}}, "required":["approve"]}
                }
            })).map_err(|e| format!("permission input schema: {e}"))?;
            let expires = Instant::now() + WAIT_LIMIT;
            let active = Arc::new(AtomicBool::new(true));
            let record = PendingPermission {
                schema: 1,
                request_id: request_id.clone(),
                dispatch_id: dispatch.into(),
                controller_epoch: self.server.controller_epoch.clone(),
                owner: self.owner.clone(),
                grant_digest: self.grant_digest.clone(),
                request_digest: digest(&bytes),
                status_revision: revision,
                expires_at: (Utc::now() + WAIT_LIMIT).to_rfc3339(),
                input,
                decision: None,
            };
            status["state"] = json!("TASK_STATE_INPUT_REQUIRED");
            status[FIELD] = serde_json::to_value(&record).map_err(|e| e.to_string())?;
            write_status(&anchor, &status, &fence)?;
            let (sender, receiver) = oneshot::channel();
            pending.insert(
                dispatch.into(),
                Waiter {
                    anchor: anchor.clone(),
                    pending: record,
                    admission: self.admission.clone(),
                    expires,
                    active: active.clone(),
                    sender: Some(sender),
                },
            );
            (
                receiver,
                WaitGuard {
                    registry: self.server.native_acp_approvals.clone(),
                    anchor: anchor.clone(),
                    dispatch: dispatch.into(),
                    request_id,
                    expires,
                    active,
                },
            )
        };
        let deadline = tokio::time::Instant::from_std(guard.expires);
        let mut revisions = tokio::time::interval(Duration::from_secs(1));
        let decision = loop {
            tokio::select! {
                biased;
                decision = &mut receiver => break decision.unwrap_or(false),
                _ = tokio::time::sleep_until(deadline) => break false,
                _ = revisions.tick() => {
                    if !guard.refresh_revision(&self.server, &self.owner)? { break false; }
                }
            }
        };
        drop(guard);
        Ok(decision)
    }
}

impl NativeAcpApprovals {
    pub(crate) fn accepted_anchor(&self, dispatch: &str) -> Option<AnchoredRunStatus> {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(dispatch)
            .filter(|entry| entry.live())
            .map(|entry| entry.anchor.clone())
    }

    /// Caller holds the canonical status lock. Reopening a durable record never
    /// recreates a waiter or confers process control in a later daemon epoch.
    pub(crate) fn input_requests(
        &self,
        anchor: &AnchoredRunStatus,
        status: &Value,
        owner: &RunOwner,
    ) -> Option<InputRequests> {
        let pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        let entry = pending.get(status["dispatch_id"].as_str()?)?;
        entry.matches(anchor, status, owner).then(|| {
            InputRequests::from([(
                entry.pending.request_id.clone(),
                entry.pending.input.clone(),
            )])
        })
    }

    /// Commit one decision under the status fence and the admission transaction,
    /// then deliver it to this runner's waiting channel, at most once.
    pub(crate) fn update(
        &self,
        server: &MemoryServer,
        owner: &RunOwner,
        anchor: &AnchoredRunStatus,
        mut status: Value,
        responses: &InputResponses,
        fence: &StatusMutationFence,
    ) -> Result<(), rmcp::ErrorData> {
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        let Some(entry) = status["dispatch_id"]
            .as_str()
            .and_then(|id| pending.get_mut(id))
        else {
            return Ok(());
        };
        if !entry.matches(anchor, &status, owner) {
            return Ok(());
        }
        let Some(response) = responses.get(&entry.pending.request_id) else {
            return Ok(());
        };
        if !tachi_hub::facade_action_allowed(
            "tachi_staff",
            Some("start"),
            server.active_tool_profile(),
        ) {
            return Err(input_unavailable());
        }
        let approved = parse_decision(response)?;
        let reply_admission = Admission::capture(server).map_err(|_| input_unavailable())?;
        status["state"] = json!("TASK_STATE_WORKING");
        append_decision(
            &mut status,
            &entry.pending,
            approved,
            PermissionEvent::Decided,
        )
        .map_err(|_| input_unavailable())?;
        let mutation = |conn: &rusqlite::Connection| -> Result<(), memcore::MemoryError> {
            let policy: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sandbox_rules LIMIT 1)",
                [],
                |row| row.get(0),
            )?;
            if policy
                || !entry.live()
                || !entry.admission.current(conn)?
                || !reply_admission.current(conn)?
            {
                return Err(memcore::MemoryError::InvalidArg(
                    "permission admission is no longer current".into(),
                ));
            }
            write_status(anchor, &status, fence).map_err(memcore::MemoryError::Internal)
        };
        crate::verified_admission::with_current_admission_write(server, |conn| {
            if conn.is_autocommit() {
                let tx = rusqlite::Transaction::new_unchecked(
                    conn,
                    rusqlite::TransactionBehavior::Immediate,
                )?;
                mutation(&tx)?;
                tx.commit()?;
                Ok(())
            } else {
                mutation(conn)
            }
        })
        .map_err(|_| input_unavailable())?;
        // A commit failure leaves a decided record but never releases execution.
        // A subsequent caller cannot replay it, and the runner times out closed.
        if let Some(sender) = entry.sender.take() {
            let _ = sender.send(approved);
        }
        Ok(())
    }
}

fn parse_decision(response: &Value) -> Result<bool, rmcp::ErrorData> {
    let invalid = || rmcp::ErrorData::invalid_params("invalid permission input response", None);
    let object = response.as_object().ok_or_else(invalid)?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "resultType" | "action" | "content" | "_meta"))
        || object.get("resultType").is_some_and(|v| v != "complete")
        || object.get("_meta").is_some_and(|v| !v.is_object())
        || response.to_string().len() > 4096
    {
        return Err(invalid());
    }
    match response["action"].as_str() {
        Some("accept") => {
            let content = response["content"].as_object().ok_or_else(invalid)?;
            if content.len() != 1 {
                return Err(invalid());
            }
            content
                .get("approve")
                .and_then(Value::as_bool)
                .ok_or_else(invalid)
        }
        Some("decline" | "cancel") if !object.contains_key("content") => Ok(false),
        _ => Err(invalid()),
    }
}

fn input_unavailable() -> rmcp::ErrorData {
    rmcp::ErrorData::invalid_params("permission input unavailable", None)
}

fn read_status(anchor: &AnchoredRunStatus) -> Result<Value, String> {
    anchor
        .read_json_bounded(MAX_BYTES)?
        .filter(Value::is_object)
        .ok_or_else(|| "permission status unavailable".into())
}

fn advance(status: &mut Value) -> Result<u64, String> {
    let object = status
        .as_object_mut()
        .ok_or("permission status is not an object")?;
    crate::managed_run_control::advance_status_revision(object)?;
    object.insert("updated_at".into(), json!(Utc::now().to_rfc3339()));
    object["status_revision"]
        .as_u64()
        .ok_or_else(|| "permission revision unavailable".into())
}

fn write_status(
    anchor: &AnchoredRunStatus,
    status: &Value,
    fence: &StatusMutationFence,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(status).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("permission status exceeds 1 MiB".into());
    }
    anchor.write_atomic(&bytes, fence)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn decision_history(status: &Value) -> Result<Vec<PermissionDecision>, String> {
    match status.get(HISTORY) {
        Some(value) => serde_json::from_value(value.clone()).map_err(|e| e.to_string()),
        None => Ok(Vec::new()),
    }
}

fn append_decision(
    status: &mut Value,
    pending: &PendingPermission,
    approved: bool,
    event: PermissionEvent,
) -> Result<(), String> {
    let mut history = decision_history(status)?;
    if history.len() >= MAX_DECISIONS || history.iter().any(|d| d.request_id == pending.request_id)
    {
        return Err("permission decision history cannot accept another event".into());
    }
    let revision = advance(status)?;
    let decision = PermissionDecision {
        event,
        request_id: pending.request_id.clone(),
        controller_epoch: pending.controller_epoch.clone(),
        owner: pending.owner.clone(),
        request_digest: pending.request_digest.clone(),
        grant_digest: pending.grant_digest.clone(),
        status_revision: revision,
        approved,
        decided_at: Utc::now().to_rfc3339(),
    };
    status[FIELD]["decision"] = serde_json::to_value(&decision).map_err(|e| e.to_string())?;
    history.push(decision);
    status[HISTORY] = serde_json::to_value(history).map_err(|e| e.to_string())?;
    Ok(())
}

struct WaitGuard {
    registry: Arc<NativeAcpApprovals>,
    anchor: AnchoredRunStatus,
    dispatch: String,
    request_id: String,
    expires: Instant,
    active: Arc<AtomicBool>,
}

impl WaitGuard {
    /// Route/identity metadata can advance status while ACP is waiting. Only
    /// the live runner may reissue the unchanged request at that new revision;
    /// old input keys remain superseded and can never resume this waiter.
    fn refresh_revision(
        &mut self,
        server: &MemoryServer,
        owner: &RunOwner,
    ) -> Result<bool, String> {
        let lock = self.anchor.lock();
        let _lock = lock.lock().unwrap_or_else(|p| p.into_inner());
        let fence = self.anchor.acquire_fence()?;
        let mut status = read_status(&self.anchor)?;
        let mut registry = self
            .registry
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(entry) = registry.get_mut(&self.dispatch) else {
            return Ok(false);
        };
        if entry.pending.request_id != self.request_id
            || !entry.anchor.same_opened_directory(&self.anchor)
        {
            return Ok(false);
        }
        if entry.sender.is_none() {
            return Ok(true);
        }
        if !entry.live()
            || status["state"] != "TASK_STATE_INPUT_REQUIRED"
            || status["dispatch_id"] != self.dispatch
            || serde_json::to_value(&entry.pending).ok().as_ref() != Some(&status[FIELD])
        {
            return Ok(false);
        }
        let revision = status["status_revision"]
            .as_u64()
            .ok_or("permission revision unavailable")?;
        if revision < entry.pending.status_revision {
            return Ok(false);
        }
        if revision == entry.pending.status_revision {
            return Ok(true);
        }
        if &current_run_owner(server)? != owner {
            return Err("permission owner changed".into());
        }
        let mut renewed = entry.pending.clone();
        renewed.status_revision = advance(&mut status)?;
        renewed.request_id = format!("acp-permission:{}", uuid::Uuid::new_v4().simple());
        status[FIELD] = serde_json::to_value(&renewed).map_err(|e| e.to_string())?;
        write_status(&self.anchor, &status, &fence)?;
        self.request_id = renewed.request_id.clone();
        entry.pending = renewed;
        Ok(true)
    }
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        // Withdraw live delivery authority before waiting on any file/DB lock.
        self.active.store(false, Ordering::Release);
        let lock = self.anchor.lock();
        let _lock = lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut pending = self
            .registry
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if pending
            .get(&self.dispatch)
            .is_none_or(|p| p.pending.request_id != self.request_id)
        {
            return;
        }
        let Some(entry) = pending.remove(&self.dispatch) else {
            return;
        };
        let cleanup = || -> Result<(), String> {
            let fence = self.anchor.acquire_fence()?;
            let mut status = read_status(&self.anchor)?;
            if serde_json::to_value(&entry.pending).ok().as_ref() == Some(&status[FIELD])
                && status["status_revision"].as_u64() == Some(entry.pending.status_revision)
                && status["state"] == "TASK_STATE_INPUT_REQUIRED"
            {
                status["state"] = json!("TASK_STATE_WORKING");
                append_decision(
                    &mut status,
                    &entry.pending,
                    false,
                    PermissionEvent::Withdrawn,
                )?;
                write_status(&self.anchor, &status, &fence)?;
            }
            Ok(())
        };
        if let Err(error) = cleanup() {
            tracing::warn!(%error, "Native ACP permission withdrawal unavailable");
        }
    }
}
