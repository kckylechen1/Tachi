//! Native turns use the existing daemon control entry and canonical run receipt.
use super::*;

pub(crate) struct SessionCommand {
    pub expected_revision: u64,
    pub prompt: Option<String>,
    pub response: oneshot::Sender<Result<Value, String>>,
}

pub(crate) struct NativeSessionControl {
    pub server: MemoryServer,
    pub cancellation: Option<mpsc::Receiver<ManagedCancelCommand>>,
    pub turns: mpsc::Receiver<SessionCommand>,
    pub mailbox_actor: memcore::A2aTransitionActor,
    pub mailbox_project: String,
}

impl ManagedRunControlRegistry {
    pub(crate) fn install_session_control(
        &self,
        dispatch_id: &str,
    ) -> Result<mpsc::Receiver<SessionCommand>, String> {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let entry = entries
            .get_mut(dispatch_id)
            .ok_or("native session lost its control entry")?;
        if entry.session_sender.is_some() {
            return Err("native session control already installed".into());
        }
        let (sender, receiver) = mpsc::channel(1);
        entry.session_sender = Some(sender);
        Ok(receiver)
    }
}

/// No durable process locator grants control. A restart can only observe the run.
pub(crate) async fn request_turn(
    server: &MemoryServer,
    dispatch_id: &str,
    expected_revision: u64,
    prompt: Option<String>,
) -> Result<String, String> {
    #[cfg(not(unix))]
    {
        let _ = (server, dispatch_id, expected_revision, prompt);
        return Err("native session control is unavailable on this platform".into());
    }
    #[cfg(unix)]
    {
        let owner = crate::claims_ops::current_run_owner(server)?;
        let anchor = server
            .managed_run_controls
            .accepted_status_anchor(dispatch_id)
            .ok_or("native session is not controlled by this daemon; do not redispatch")?;
        crate::mcp_tasks::require_run_owner(&anchor, &owner, dispatch_id)
            .map_err(|_| "native session control is unavailable")?;
        let lock = anchor.lock();
        {
            let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
            let status = anchor
                .read_json()?
                .ok_or("native session receipt unavailable")?;
            validate_idle(&status, server, dispatch_id, expected_revision)?;
        }
        let sender = server
            .managed_run_controls
            .entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(dispatch_id)
            .and_then(|entry| entry.session_sender.clone())
            .ok_or("native session control is unavailable; do not redispatch")?;
        let (response, receiver) = oneshot::channel();
        sender
            .try_send(SessionCommand {
                expected_revision,
                prompt,
                response,
            })
            .map_err(|_| "native session has another pending instruction or lost its controller")?;
        let result = receiver.await.map_err(|_| {
            "native session instruction outcome is unknown; read status before any retry"
        })??;
        serde_json::to_string(&result).map_err(|e| e.to_string())
    }
}

pub(crate) fn validate_idle(
    status: &Value,
    server: &MemoryServer,
    dispatch_id: &str,
    expected: u64,
) -> Result<(), String> {
    if status["dispatch_id"] != dispatch_id
        || status["managed_run_identity"]["controller_epoch_id"] != server.controller_epoch
        || status["execution_classification"] != "managed_native_acp"
        || status["state"] != "TASK_STATE_WORKING"
    {
        return Err("native session is not controlled by this daemon; do not redispatch".into());
    }
    if status["status_revision"].as_u64() != Some(expected) {
        return Err("stale_status_revision: read status before a new instruction".into());
    }
    if status.get("cancellation").is_some()
        || status.get("completion_recovery").is_some()
        || status.get("resolved_completion").is_some()
        || status["native_session"]["phase"] != "awaiting_instruction"
        || status["native_session"]["supervised"] != true
    {
        return Err("native session is not at a supervised turn boundary".into());
    }
    Ok(())
}

/// Update the existing receipt; telemetry is only a projection of that revision.
pub(crate) fn update_phase(
    server: &MemoryServer,
    dispatch_id: &str,
    expected: Option<u64>,
    session: Value,
) -> Result<Value, String> {
    #[cfg(not(unix))]
    {
        let _ = (server, dispatch_id, expected, session);
        Err("native session control requires an anchored host".into())
    }
    #[cfg(unix)]
    {
        let anchor = server
            .managed_run_controls
            .accepted_status_anchor(dispatch_id)
            .ok_or("native session lost its accepted status anchor")?;
        let lock = anchor.lock();
        let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
        let fence = anchor.acquire_fence()?;
        let mut status = anchor
            .read_json()?
            .ok_or("native session receipt unavailable")?;
        if let Some(expected) = expected {
            validate_idle(&status, server, dispatch_id, expected)?;
        }
        if status["state"] != "TASK_STATE_WORKING"
            || status.get("completion_recovery").is_some()
            || status.get("resolved_completion").is_some()
        {
            return Err("native session lost lifecycle ownership".into());
        }
        status["native_session"] = session;
        status["updated_at"] = json!(Utc::now().to_rfc3339());
        advance_status_revision(
            status
                .as_object_mut()
                .ok_or("invalid native session receipt")?,
        )?;
        anchor.write_atomic(
            &serde_json::to_vec_pretty(&status).map_err(|e| e.to_string())?,
            &fence,
        )?;
        let projection = json!({
            "dispatch_id": dispatch_id, "status_revision": status["status_revision"],
            "controller_epoch_id": server.controller_epoch, "session": status["native_session"],
        });
        let path = crate::dispatch_ops::dispatch_runs_root()
            .join(dispatch_id)
            .join("telemetry.json");
        crate::utils::write_owner_only_file_atomic(
            &path,
            &serde_json::to_vec_pretty(&projection).map_err(|e| e.to_string())?,
        )?;
        Ok(projection)
    }
}

pub(crate) fn admit_worker(
    server: &MemoryServer,
    dispatch_id: &str,
) -> Result<(memcore::A2aTransitionActor, String), String> {
    let project =
        crate::a2a_ops::project_key(server)?.ok_or("managed native worker needs a project")?;
    let identity = format!("staff:{dispatch_id}");
    let connection = format!("managed:{}:{dispatch_id}", server.controller_epoch);
    server.with_global_store(|store| {
        memcore::insert_agent_identity(
            store.connection(),
            &memcore::AgentIdentity {
                agent_identity_id: identity.clone(),
                display_name: None,
                seat: None,
                capability_json: None,
                created_at: String::new(),
            },
        )
        .map_err(|e| e.to_string())?;
        memcore::db::a2a::record_a2a_project_admission(
            store.connection(),
            &format!("admission-{}", uuid::Uuid::new_v4()),
            &identity,
            &connection,
            &project,
        )
        .map_err(|e| e.to_string())
    })?;
    Ok((
        memcore::A2aTransitionActor {
            agent_identity_id: identity,
            connection_id: connection,
        },
        project,
    ))
}
