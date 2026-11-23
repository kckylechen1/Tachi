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
            validate_idle(
                &status,
                server,
                dispatch_id,
                expected_revision,
                prompt.is_some(),
            )?;
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
    is_continue: bool,
) -> Result<(), String> {
    if status["dispatch_id"] != dispatch_id
        || status["managed_run_identity"]["controller_epoch_id"] != server.controller_epoch
        || status["execution_classification"] != "managed_native_acp"
    {
        return Err("native session is not controlled by this daemon; do not redispatch".into());
    }
    if status["status_revision"].as_u64() != Some(expected) {
        return Err(format!("stale_status_revision: observed_status_revision={}; read status before a new instruction", status["status_revision"]));
    }
    if status.get("cancellation").is_some()
        || status.get("completion_recovery").is_some()
        || status.get("resolved_completion").is_some()
        || !status["acp_permission"].is_null()
        || !status["closure_kind"].is_null()
    {
        return Err("native session is not at an admitted input boundary".into());
    }
    let supervised = status["state"] == "TASK_STATE_WORKING"
        && status["native_session"]["phase"] == "awaiting_instruction"
        && status["native_session"]["supervised"] == true;
    if supervised || (is_continue && is_clarification_pause(status)) {
        Ok(())
    } else {
        Err("native session is not at an admitted input boundary".into())
    }
}

pub(crate) fn is_clarification_pause(status: &Value) -> bool {
    status["state"] == "TASK_STATE_INPUT_REQUIRED"
        && status["execution_classification"] == "managed_native_acp"
        && status["closure_kind"].is_null()
        && status["acp_permission"].is_null()
        && status["native_session"]["phase"] == "awaiting_clarification"
        && !status["native_session"]["failure_class"]
            .as_str()
            .is_some_and(|s| s.to_ascii_lowercase().contains("auth"))
        && status["clarification_request"]["request_id"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 128)
        && status["clarification_request"]["question"]
            .as_str()
            .is_some_and(|s| safe_clarification_text(s).is_ok())
}

/// This input channel carries factual text only, never credentials or approvals.
pub(crate) fn safe_clarification_text(text: &str) -> Result<String, String> {
    if text.len() > 4096 {
        return Err("clarification text exceeds 4096 bytes".into());
    }
    let text: String = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    if text.is_empty()
        || lower.contains("auth_required")
        || [
            "enter your password",
            "provide your password",
            "provide credentials",
            "enter credentials",
            "sign in to",
            "log in to",
            "authentication required",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        || crate::memory_search_ops::scrub_secrets(text).1 != 0
    {
        return Err(
            "credential/authentication input is unavailable through clarification continue".into(),
        );
    }
    Ok(text.to_string())
}

pub(crate) fn pause_for_clarification(
    server: &MemoryServer,
    dispatch_id: &str,
    mut session: Value,
    question: &str,
) -> Result<Value, String> {
    let question = safe_clarification_text(question)?;
    session["phase"] = json!("awaiting_clarification");
    update_phase_inner(
        server,
        dispatch_id,
        None,
        session,
        Some(json!({
            "request_id": format!("clarification:{}", uuid::Uuid::new_v4().simple()),
            "question": question,
        })),
    )
}

/// Update the existing receipt; telemetry is only a projection of that revision.
pub(crate) fn update_phase(
    server: &MemoryServer,
    dispatch_id: &str,
    expected: Option<u64>,
    session: Value,
) -> Result<Value, String> {
    update_phase_inner(server, dispatch_id, expected, session, None)
}

fn update_phase_inner(
    server: &MemoryServer,
    dispatch_id: &str,
    expected: Option<u64>,
    session: Value,
    clarification: Option<Value>,
) -> Result<Value, String> {
    #[cfg(not(unix))]
    {
        let _ = (server, dispatch_id, expected, session, clarification);
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
            validate_idle(
                &status,
                server,
                dispatch_id,
                expected,
                session["phase"] == "instruction_accepted",
            )?;
        }
        let was_clarification = is_clarification_pause(&status);
        // Only the live driver may withdraw a pending ask on resolution,
        // timeout, cancellation or failure. A finish command cannot use this.
        if was_clarification
            && (expected.is_some()
                || matches!(
                    session["phase"].as_str(),
                    Some("running" | "failed" | "cancelled")
                ))
        {
            status["state"] = json!("TASK_STATE_WORKING");
            status
                .as_object_mut()
                .ok_or("invalid native session receipt")?
                .remove("clarification_request");
        }
        if status["state"] != "TASK_STATE_WORKING"
            || status.get("completion_recovery").is_some()
            || status.get("resolved_completion").is_some()
        {
            return Err("native session lost lifecycle ownership".into());
        }
        if let Some(clarification) = clarification {
            let owner = crate::claims_ops::current_run_owner(server)?;
            crate::mcp_tasks::require_run_owner(&anchor, &owner, dispatch_id)
                .map_err(|_| "native clarification owner unavailable")?;
            if status["dispatch_id"] != dispatch_id
                || status["managed_run_identity"]["controller_epoch_id"] != server.controller_epoch
                || status["execution_classification"] != "managed_native_acp"
                || status["native_session"]["phase"] != "running"
                || status.get("cancellation").is_some()
                || !status["acp_permission"].is_null()
                || !status["closure_kind"].is_null()
            {
                return Err("native clarification lost lifecycle ownership".into());
            }
            status["state"] = json!("TASK_STATE_INPUT_REQUIRED");
            status["clarification_request"] = clarification;
        }
        status["native_session"] = session;
        status["updated_at"] = json!(memcore::time_compat::rfc3339_compat(jiff::Timestamp::now()));
        advance_status_revision(
            status
                .as_object_mut()
                .ok_or("invalid native session receipt")?,
        )?;
        anchor.write_atomic(
            &serde_json::to_vec_pretty(&status).map_err(|e| e.to_string())?,
            &fence,
        )?;
        drop(fence);
        drop(_guard);
        server.task_notifications.notify(dispatch_id);
        let projection = json!({
            "state": status["state"],
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn clarification_continue_is_owned_revision_fenced_and_never_finish() {
        let _serial = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let (server, _) = crate::tests::make_server_with_project_fixture("clarification-project");
        let root = tempfile::tempdir().unwrap();
        let _env = crate::test_support::EnvRestore::set_path("TACHI_HOME", root.path());
        server.set_session_identity(
            Some("clarification-test".into()),
            Some("clarification-project".into()),
            Some(tachi_hub::ToolProfile::standard()),
        );
        crate::claims_ops::admit_agent_connection(
            &server,
            Some("agent.clarification".into()),
            true,
        )
        .unwrap();
        let dispatch = "native-clarification-cas";
        let run = root.path().join("runs").join(dispatch);
        std::fs::create_dir_all(&run).unwrap();
        crate::mcp_tasks::prepare_run_binding(
            &server,
            &run,
            dispatch,
            Some("clarification-project"),
        )
        .unwrap();
        let (_cancel, _guard) = server.managed_run_controls.register(dispatch).unwrap();
        let anchor = AnchoredRunStatus::open(&run).unwrap();
        server
            .managed_run_controls
            .retain_accepted_status_anchor(dispatch, anchor.clone())
            .unwrap();
        let initial = json!({"dispatch_id": dispatch, "state": "TASK_STATE_WORKING", "status_revision": 1,
            "execution_classification": "managed_native_acp", "managed_run_identity": {"controller_epoch_id": server.controller_epoch},
            "native_session": {"phase": "running", "supervised": true, "session_id": "same-session"}});
        let write = |value: &Value| {
            let lock = anchor.lock();
            let _guard = lock.lock().unwrap();
            let fence = anchor.acquire_fence().unwrap();
            anchor
                .write_atomic(&serde_json::to_vec(value).unwrap(), &fence)
                .unwrap();
        };
        write(&initial);
        let paused = pause_for_clarification(
            &server,
            dispatch,
            initial["native_session"].clone(),
            "Which branch?\u{0000}",
        )
        .unwrap();
        assert_eq!(paused["state"], "TASK_STATE_INPUT_REQUIRED");
        assert_eq!(paused["status_revision"], 2);
        let status = anchor.read_json().unwrap().unwrap();
        assert_eq!(status["clarification_request"]["question"], "Which branch?");
        let before = std::fs::read(run.join("status.json")).unwrap();
        let error = request_turn(&server, dispatch, 2, Some("main".into()))
            .await
            .unwrap_err();
        assert!(error.contains("control is unavailable"), "{error}");
        let mut commands = server
            .managed_run_controls
            .install_session_control(dispatch)
            .unwrap();
        assert!(request_turn(&server, dispatch, 2, None)
            .await
            .unwrap_err()
            .contains("admitted input boundary"));
        let stale = request_turn(&server, dispatch, 1, Some("main".into()))
            .await
            .unwrap_err();
        assert!(
            stale.contains("stale_status_revision: observed_status_revision=2"),
            "{stale}"
        );
        let foreign = server.clone_for_mcp_session();
        crate::claims_ops::admit_agent_connection(&foreign, Some("agent.foreign".into()), true)
            .unwrap();
        assert!(request_turn(&foreign, dispatch, 2, Some("main".into()))
            .await
            .is_err());
        assert_eq!(std::fs::read(run.join("status.json")).unwrap(), before);
        for (name, field, value) in [
            (
                "permission",
                "acp_permission",
                json!({"request_id":"approval"}),
            ),
            ("partial", "closure_kind", json!("partial")),
            ("authentication", "state", json!("TASK_STATE_AUTH_REQUIRED")),
            (
                "cancellation",
                "cancellation",
                json!({"state":"cancellation_requested"}),
            ),
            ("recovery", "completion_recovery", json!({})),
            (
                "orphan",
                "managed_run_identity",
                json!({"controller_epoch_id":"previous-daemon"}),
            ),
            ("unmanaged", "execution_classification", json!("external")),
            (
                "unsupervised boundary",
                "native_session",
                json!({"phase":"awaiting_instruction","supervised":false}),
            ),
        ] {
            let mut refused = status.clone();
            refused[field] = value;
            write(&refused);
            let bytes = std::fs::read(run.join("status.json")).unwrap();
            assert!(
                request_turn(&server, dispatch, 2, Some("main".into()))
                    .await
                    .is_err(),
                "{name}"
            );
            assert_eq!(
                std::fs::read(run.join("status.json")).unwrap(),
                bytes,
                "{name}"
            );
            assert!(
                commands.try_recv().is_err(),
                "{name} enqueued an instruction"
            );
        }
        write(&status);
        let (accepted, ()) = tokio::join!(
            request_turn(&server, dispatch, 2, Some("main".into())),
            async {
                let command = commands.recv().await.unwrap();
                assert_eq!(command.prompt.as_deref(), Some("main"));
                let mut session = status["native_session"].clone();
                session["phase"] = json!("instruction_accepted");
                let result =
                    update_phase(&server, dispatch, Some(command.expected_revision), session);
                command.response.send(result).unwrap();
            }
        );
        let receipt: Value = serde_json::from_str(&accepted.unwrap()).unwrap();
        assert_eq!(receipt["state"], "TASK_STATE_WORKING");
        assert_eq!(receipt["dispatch_id"], dispatch);
        assert_eq!(receipt["status_revision"], 3);
        assert!(anchor
            .read_json()
            .unwrap()
            .unwrap()
            .get("clarification_request")
            .is_none());
        assert!(request_turn(&server, dispatch, 2, Some("main".into()))
            .await
            .unwrap_err()
            .contains("observed_status_revision=3"));
        let mut ordinary = initial.clone();
        ordinary["native_session"]["phase"] = json!("awaiting_instruction");
        write(&ordinary);
        assert!(
            validate_idle(&ordinary, &server, dispatch, 1, false).is_ok(),
            "ordinary supervised finish stays admitted"
        );
    }
}
