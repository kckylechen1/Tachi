use crate::server_state::MemoryServer;
use std::sync::{Arc, RwLock as StdRwLock};
use tachi_hub::ToolProfile;

impl MemoryServer {
    /// Scope host-provided workspace context to this call. Keep the admitted
    /// connection runtime shared so Presence updates survive subsequent calls.
    pub(crate) fn clone_for_workspace_call(
        &self,
        meta: &rmcp::model::RequestMetaObject,
    ) -> Result<Option<Self>, rmcp::ErrorData> {
        let invalid = |message| rmcp::ErrorData::invalid_params(message, None);
        use crate::session_identity::{aliased_meta_identity_string, zcode_workspace_path};
        let project =
            aliased_meta_identity_string(meta, "tachiProject", "tachi.project").map_err(invalid)?;
        let root = aliased_meta_identity_string(meta, "tachiWorkspaceRoot", "tachi.workspaceRoot")
            .map_err(invalid)?;
        let zcode = zcode_workspace_path(meta).map_err(invalid)?;
        if project.is_none() && root.is_none() && zcode.is_none() {
            return Ok(None);
        }
        // An explicit project name has the same precedence as at initialize.
        let roots: Vec<_> = root
            .filter(|_| project.is_none())
            .into_iter()
            .chain(zcode)
            .collect();
        let bound = self.session_project();
        let project = self
            .resolve_workspace_binding(bound.as_deref(), project.as_deref(), &roots)
            .map_err(invalid)?;
        let mut request_server = self.clone();
        request_server.request_project = project;
        Ok(Some(request_server))
    }

    /// A fixed project may be confirmed, never switched or used to create a
    /// conflicting library. Only an unbound, unique workspace can register.
    pub(crate) fn resolve_workspace_binding(
        &self,
        bound: Option<&str>,
        requested: Option<&str>,
        roots: &[String],
    ) -> Result<Option<String>, String> {
        let mut selected = bound.map(str::to_owned);
        if let Some(requested) = requested {
            let (project, _) = self.resolve_server_named_project_binding(requested)?;
            if let Some(bound) = selected.as_deref() {
                self.require_same_workspace_library(bound, &project)?;
            } else {
                selected = Some(project);
            }
        }
        if let Some(project) = selected.as_deref() {
            for root in roots {
                let candidate = self.resolve_existing_workspace_project(root)?;
                self.require_same_workspace_library(project, &candidate)?;
            }
            Ok(selected)
        } else {
            self.resolve_or_register_workspaces(roots)
        }
    }

    fn require_same_workspace_library(&self, bound: &str, candidate: &str) -> Result<(), String> {
        if self.resolve_server_named_project_db_identity(bound)?
            != self.resolve_server_named_project_db_identity(candidate)?
        {
            return Err("Workspace context conflicts with the connection's project binding; reconnect with the correct workspace".into());
        }
        Ok(())
    }

    /// Roots is an optional compatibility input. Fetch it for each unbound
    /// project call so workspace changes never mutate a shared connection.
    #[allow(deprecated)]
    pub(crate) async fn clone_for_roots_call(
        &self,
        request: &rmcp::model::CallToolRequestParams,
        context: &rmcp::service::RequestContext<rmcp::service::RoleServer>,
        mode: crate::mcp_peer::McpPeerMode,
    ) -> Result<
        std::ops::ControlFlow<rmcp::model::InputRequiredResult, Option<Self>>,
        rmcp::ErrorData,
    > {
        use rmcp::model::{InputRequest, InputRequiredResult, ListRootsResult};
        use std::ops::ControlFlow;
        const INPUT_ID: &str = "tachi-workspace-roots";
        let empty = serde_json::Map::new();
        let args = request.arguments.as_ref().unwrap_or(&empty);
        if self.session_project().is_some()
            || !(crate::session_identity::project_defaults_to_bound_project(&request.name, args)
                || request.name == "tachi_staff")
            || crate::session_identity::requested_global_scope(&request.name, args)?
            || (args
                .get("project")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|project| !project.trim().is_empty())
                && crate::session_identity::explicit_project_can_cross_binding(&request.name, args))
            || !context
                .client_capabilities()
                .is_some_and(|caps| caps.roots.is_some())
        {
            return Ok(ControlFlow::Continue(None));
        }
        let invalid = |message| rmcp::ErrorData::invalid_params(message, None);
        let roots = match mode {
            crate::mcp_peer::McpPeerMode::Modern20260728 => {
                match request
                    .input_responses
                    .as_ref()
                    .and_then(|inputs| inputs.get(INPUT_ID))
                {
                    Some(value) => serde_json::from_value::<ListRootsResult>(value.clone())
                        .map_err(|_| invalid("Invalid workspace roots response".into()))?,
                    None if request.input_responses.is_some() => {
                        return Err(invalid(
                            "Workspace roots response is missing; provide the current workspace"
                                .into(),
                        ));
                    }
                    None => {
                        return Ok(ControlFlow::Break(
                            InputRequiredResult::from_input_requests(
                                std::collections::BTreeMap::from([(
                                    INPUT_ID.to_string(),
                                    InputRequest::ListRoots(Default::default()),
                                )]),
                            ),
                        ))
                    }
                }
            }
            crate::mcp_peer::McpPeerMode::Legacy => tokio::select! {
                _ = context.ct.cancelled() => return Err(invalid("Workspace discovery was cancelled".into())),
                result = tokio::time::timeout(std::time::Duration::from_secs(2), context.peer.list_roots()) => {
                    result.map_err(|_| invalid("Workspace discovery timed out; configure X-Tachi-Workspace-Root or tachiWorkspaceRoot".into()))?
                        .map_err(|_| invalid("Client could not provide workspace roots; configure X-Tachi-Workspace-Root or tachiWorkspaceRoot".into()))?
                }
            },
        };
        if roots.roots.is_empty() || roots.roots.len() > 64 {
            return Err(invalid("Workspace discovery requires 1–64 roots; provide the current workspace, or explicitly select storage_scope=global".into()));
        }
        let paths = roots
            .roots
            .into_iter()
            .map(|root| {
                let uri = url::Url::parse(&root.uri).map_err(|_| {
                    invalid("Workspace roots must be local file:// directory URIs".into())
                })?;
                if uri.scheme() != "file"
                    || uri.host_str().is_some_and(|host| host != "localhost")
                    || uri.query().is_some()
                    || uri.fragment().is_some()
                    || !uri.username().is_empty()
                    || uri.password().is_some()
                {
                    return Err(invalid(
                        "Workspace roots must be local file:// directory URIs".into(),
                    ));
                }
                uri.to_file_path()
                    .ok()
                    .and_then(|path| path.to_str().map(str::to_owned))
                    .ok_or_else(|| {
                        invalid("Workspace root must be an absolute local directory".into())
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let project = self
            .resolve_or_register_workspaces(&paths)
            .map_err(invalid)?;
        let mut request_server = self.clone();
        request_server.request_project = project;
        Ok(ControlFlow::Continue(Some(request_server)))
    }

    pub(crate) fn clone_for_mcp_session(&self) -> Self {
        let mut clone = self.clone();
        clone.request_project = None;
        let mut runtime = self.agent_runtime_read().clone();
        // #1255: each MCP session gets its own opaque rate-limit identity so
        // burst/RPM windows keyed by session_id do not bleed across clones
        // that still share the process-global RateLimiter.
        runtime.rate_limit_session_id = uuid::Uuid::new_v4().to_string();
        runtime.presence_session_id = uuid::Uuid::new_v4().to_string();
        runtime.presence_client_label = None;
        runtime.presence_report = None;
        runtime.presence_report_error = None;
        clone.agent_runtime = Arc::new(StdRwLock::new(runtime));
        clone
    }

    /// #1255: opaque rate-limit session id stamped on this server/clone.
    pub(crate) fn rate_limit_session_id(&self) -> String {
        self.agent_runtime_read().rate_limit_session_id.clone()
    }

    /// Audit C1: re-key this clone's rate-limit bucket to a stable key (a
    /// validated `X-Tachi-Rate-Limit-Session` header or a modern request's
    /// resolved identity). Only changes which burst/RPM window is used; it
    /// carries no identity or authority.
    pub(crate) fn set_rate_limit_session_id(&self, session_id: String) {
        self.agent_runtime_write().rate_limit_session_id = session_id;
    }

    pub(crate) fn set_tool_profile(&self, profile: Option<ToolProfile>) {
        self.agent_runtime_write().tool_profile = profile;
    }

    pub(crate) fn active_tool_profile(&self) -> Option<ToolProfile> {
        self.agent_runtime_read().tool_profile
    }

    pub(crate) fn set_daemon_proxy_token(&self, token: String) {
        self.agent_runtime_write().daemon_proxy_token = Some(token);
    }

    pub(crate) fn set_published_daemon_mcp_url(&self, url: String) {
        self.agent_runtime_write().published_daemon_mcp_url = Some(url);
    }

    pub(crate) fn published_daemon_mcp_url(&self) -> Option<String> {
        self.agent_runtime_read().published_daemon_mcp_url.clone()
    }

    /// Project a public client descriptor from the caller's admitted session.
    /// Profile/identity headers are assertions, never privileged authorization.
    /// An unbound session stays unbound; daemon cwd is not caller context.
    pub(crate) fn http_mcp_entry(
        &self,
        profile: &str,
        client: &str,
        agent_identity: Option<&str>,
        dispatch_depth: u32,
    ) -> Result<serde_json::Value, String> {
        use crate::session_identity::*;
        crate::server_handler::parse_http_tool_profile(profile)
            .map_err(|error| format!("{error}; use supported owner CLI commands for management; HTTP client metadata cannot grant owner authority"))?;
        let url = self.published_daemon_mcp_url().ok_or_else(|| {
            "HTTP MCP injection requires a running Tachi daemon; start a supervised `tachi --daemon --port <fixed-port> serve` before dispatch/attachment".to_string()
        })?;
        let parsed = url::Url::parse(&url).map_err(|_| "invalid published daemon MCP URL")?;
        if parsed.scheme() != "http"
            || !matches!(parsed.host_str(), Some("127.0.0.1" | "localhost"))
            || parsed.port_or_known_default() == Some(0)
            || parsed.path() != "/mcp"
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err("published daemon MCP URL must be a loopback HTTP /mcp endpoint".into());
        }
        let mut headers = serde_json::Map::new();
        for (name, value) in [
            (HEADER_PROFILE, profile.to_string()),
            (HEADER_CLIENT, client.to_string()),
            (HEADER_DISPATCH_DEPTH, dispatch_depth.to_string()),
        ] {
            headers.insert(name.into(), value.into());
        }
        if let Some(project) = self.session_project() {
            headers.insert(HEADER_PROJECT.into(), project.into());
        }
        if let Some(identity) = agent_identity {
            let identity = agent_identity_from_env_value(Some(identity))
                .ok_or_else(|| "agent identity must not be empty".to_string())?;
            headers.insert(HEADER_AGENT_IDENTITY.into(), identity.into());
        }
        // A stable seat bucket also covers modern clients without a principal.
        // It carries no identity/authority and does not replace admission.
        use sha2::{Digest, Sha256};
        headers.insert(
            HEADER_RATE_LIMIT_SESSION.into(),
            format!("{:x}", Sha256::digest(client.as_bytes())).into(),
        );
        Ok(serde_json::json!({"type":"http", "url":url, "headers":headers}))
    }

    pub(crate) fn admits_internal_proxy_token(&self, claimed: Option<&str>) -> bool {
        let runtime = self.agent_runtime_read();
        let (Some(expected), Some(claimed)) = (runtime.daemon_proxy_token.as_deref(), claimed)
        else {
            return false;
        };
        if expected.len() != claimed.len() {
            return false;
        }
        expected
            .as_bytes()
            .iter()
            .zip(claimed.as_bytes())
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
    }

    pub(crate) fn set_session_identity(
        &self,
        client: Option<String>,
        project: Option<String>,
        profile: Option<ToolProfile>,
    ) {
        let mut runtime = self.agent_runtime_write();
        runtime.session_client = client;
        runtime.session_project = project;
        if let Some(profile) = profile {
            runtime.tool_profile = Some(profile);
        }
    }

    pub(crate) fn session_project(&self) -> Option<String> {
        self.request_project
            .clone()
            .or_else(|| self.agent_runtime_read().session_project.clone())
    }

    /// #1251: stamp this session's raw dispatch recursion-depth marker. Kept a
    /// separate setter from `set_session_identity` (rather than a 4th param)
    /// so the many existing `set_session_identity` call sites are untouched;
    /// only the two callers that actually know the depth — the daemon's
    /// `apply_http_session_identity` (from the wire header) and the CLI
    /// in-process server build (from the process's own env) — set it.
    pub(crate) fn set_session_dispatch_depth(&self, depth: Option<String>) {
        self.agent_runtime_write().session_dispatch_depth = depth;
    }

    /// #1251: the raw dispatch recursion-depth marker for this session, fed to
    /// `session_identity::resolve_dispatch_depth` at the gate and when stamping
    /// a child's depth in `dispatch_ops::mcp_config`.
    pub(crate) fn session_dispatch_depth(&self) -> Option<String> {
        self.agent_runtime_read().session_dispatch_depth.clone()
    }

    pub(crate) fn session_client(&self) -> Option<String> {
        self.agent_runtime_read().session_client.clone()
    }

    pub(crate) fn native_tool_visibility(&self) -> Vec<(String, String, bool)> {
        let env_patterns = crate::server_handler::current_exposed_tool_patterns();
        let profile = self.active_tool_profile();
        let mut tools = self
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| {
                let name = tool.name.as_ref().to_string();
                let description = tool
                    .description
                    .as_ref()
                    .map(|text| crate::utils::compact_text_line(text.as_ref(), 96))
                    .unwrap_or_default();
                let visible = tachi_hub::tool_visible(&name, profile, env_patterns.as_deref());
                (name, description, visible)
            })
            .collect::<Vec<_>>();
        tools.sort_by(|a, b| a.0.cmp(&b.0));
        tools
    }

    /// Observe the caller and stamp the last MCP activity; called centrally from
    /// `ServerHandler::call_tool` on every tool invocation (including calls a
    /// stdio child forwards to this daemon).
    pub(crate) fn touch_activity(&self) {
        crate::staffing_ops::presence::observe_client(self);
        self.last_activity_ms.store(
            chrono::Utc::now().timestamp_millis(),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// Shared handle to the last-activity clock, for the serve-loop idle reaper.
    pub(crate) fn activity_clock(&self) -> Arc<std::sync::atomic::AtomicI64> {
        Arc::clone(&self.last_activity_ms)
    }
}

#[cfg(test)]
mod http_projection_tests {
    use crate::session_identity::*;

    #[test]
    fn public_mcp_projection_keeps_scope_and_seat_without_private_authority() {
        let server = crate::tests::make_server();
        server.set_published_daemon_mcp_url("http://127.0.0.1:6919/mcp".into());
        server.set_daemon_proxy_token("private-proxy-capability".into());
        server.set_session_identity(
            Some("parent-seat".into()),
            Some("bound-project".into()),
            None,
        );
        server.set_work_claim_connection(
            Some("parent-principal".into()),
            "parent-connection".into(),
            "self_asserted".into(),
        );
        let entry = server
            .http_mcp_entry("delegate", "child-seat", None, 2)
            .unwrap();
        assert_eq!(entry["type"], "http");
        assert_eq!(entry["headers"][HEADER_PROJECT], "bound-project");
        assert_eq!(entry["headers"][HEADER_CLIENT], "child-seat");
        assert_eq!(entry["headers"][HEADER_PROFILE], "delegate");
        assert_eq!(entry["headers"][HEADER_DISPATCH_DEPTH], "2");
        assert!(entry["headers"].get(HEADER_AGENT_IDENTITY).is_none());
        assert!(entry["headers"].get(HEADER_INTERNAL_PROXY_TOKEN).is_none());
        assert!(entry["headers"].get(HEADER_OWNER_TOOL).is_none());
        assert!(!entry.to_string().contains("parent-principal"));
        assert!(!entry.to_string().contains("private-proxy-capability"));
        assert!(valid_rate_limit_session_key(
            entry["headers"][HEADER_RATE_LIMIT_SESSION]
                .as_str()
                .unwrap()
        ));
        assert_ne!(
            entry["headers"][HEADER_RATE_LIMIT_SESSION],
            server
                .http_mcp_entry("delegate", "other-child", None, 2)
                .unwrap()["headers"][HEADER_RATE_LIMIT_SESSION]
        );
        let clone = server.clone_for_mcp_session();
        assert_eq!(
            clone.published_daemon_mcp_url(),
            server.published_daemon_mcp_url()
        );
    }

    #[test]
    fn public_mcp_projection_refuses_missing_endpoint_and_privileged_profiles() {
        let server = crate::tests::make_server();
        assert!(server
            .http_mcp_entry("delegate", "child", None, 1)
            .unwrap_err()
            .contains("running Tachi daemon"));
        server.set_published_daemon_mcp_url("http://127.0.0.1:6919/mcp".into());
        for profile in ["ops", "admin", "observe+operate", "invented"] {
            assert!(
                server.http_mcp_entry(profile, "child", None, 1).is_err(),
                "{profile}"
            );
        }
        let entry = server
            .http_mcp_entry("delegate", "child", Some("admitted-child"), 1)
            .unwrap();
        assert_eq!(entry["headers"][HEADER_AGENT_IDENTITY], "admitted-child");
        assert!(
            entry["headers"].get(HEADER_PROJECT).is_none(),
            "unbound caller must remain unbound"
        );
        assert!(server
            .http_mcp_entry("delegate", "child", Some("bad\nidentity"), 1)
            .is_err());
    }
}
