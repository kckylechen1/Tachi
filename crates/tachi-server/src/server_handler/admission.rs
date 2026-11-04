use super::HttpSessionIdentity;
use crate::MemoryServer;
use std::sync::{Arc, RwLock};
use tachi_hub::{default_tool_profile, ToolProfile};

pub(crate) fn parse_http_tool_profile(raw: &str) -> Result<ToolProfile, rmcp::ErrorData> {
    tachi_hub::parse_tool_profile(raw).ok_or_else(|| {
        rmcp::ErrorData::invalid_params(
            format!(
                "unsupported Tachi profile '{raw}'; ordinary agents use the default entry. Ops/Admin selectors are retired; use supported owner CLI commands for management. Legacy standard, observe, remember, coordinate and delegate restrictions remain supported"
            ),
            None,
        )
    })
}

impl MemoryServer {
    pub(super) fn admit_owner_tool(
        &self,
        tool: Option<&str>,
        token: Option<&str>,
        profile: Option<ToolProfile>,
    ) -> Result<Option<String>, rmcp::ErrorData> {
        let Some(tool) = tool else {
            return Ok(None);
        };
        if !self.admits_internal_proxy_token(token) {
            return Err(rmcp::ErrorData::invalid_params(
                "owner tool operation requires the daemon owner's private capability",
                None,
            ));
        }
        if profile.unwrap_or_else(default_tool_profile) != default_tool_profile() {
            return Err(rmcp::ErrorData::invalid_params(
                "owner tool operation cannot override a restricted worker or legacy profile",
                None,
            ));
        }
        if tool.is_empty()
            || tool.len() > 128
            || !tool
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        {
            return Err(rmcp::ErrorData::invalid_params(
                "owner tool operation requires one exact tool name",
                None,
            ));
        }
        Ok(Some(tool.to_owned()))
    }

    /// Apply admitted owner authority only to this exact call. Keep the
    /// original session's profile, project and rate-limit identity intact.
    pub(super) fn owner_tool_server(
        &self,
        tool: &str,
        identity: HttpSessionIdentity,
    ) -> Result<Option<Self>, rmcp::ErrorData> {
        if let Some(error) = identity.owner_tool_error {
            return Err(rmcp::ErrorData::invalid_params(error, None));
        }
        let profile = self.active_tool_profile();
        let Some(expected) = self.admit_owner_tool(
            identity.owner_tool.as_deref(),
            identity.internal_proxy_token.as_deref(),
            profile,
        )?
        else {
            return Ok(None);
        };
        // Legacy session identity is immutable. Current headers may narrow
        // admission but must never override the bound session's restrictions.
        if let Some(asserted) = identity.profile.as_deref() {
            if parse_http_tool_profile(asserted)? != profile.unwrap_or_else(default_tool_profile) {
                return Err(rmcp::ErrorData::invalid_params(
                    "owner tool operation cannot change the bound session profile",
                    None,
                ));
            }
        }
        if expected != tool {
            return Err(rmcp::ErrorData::invalid_params(
                "owner tool operation does not match the requested tool",
                None,
            ));
        }
        let mut runtime = self.agent_runtime_read().clone();
        // This internal policy is never selected by public metadata or used
        // for discovery. Handler-specific checks and retired-route guards run.
        runtime.tool_profile = Some(ToolProfile::admin());
        let mut server = self.clone();
        server.agent_runtime = Arc::new(RwLock::new(runtime));
        Ok(Some(server))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn owner_call_keeps_session_restrictions_and_rate_limit_identity() {
        let (mut server, _home) = crate::tests::make_server_with_temp_home();
        server.set_daemon_proxy_token("owner-test-capability".into());
        server.set_session_identity(Some("owner-cli".into()), Some("fixture".into()), None);
        server.request_project = Some("workspace-fixture".into());
        for token in [None, Some("wrong-capability")] {
            assert!(server
                .admit_owner_tool(Some("runtime_info"), token, None)
                .is_err());
        }
        for profile in [
            ToolProfile::delegate(),
            ToolProfile::observe(),
            ToolProfile::remember(),
            ToolProfile::coordinate(),
        ] {
            assert!(server
                .admit_owner_tool(
                    Some("runtime_info"),
                    Some("owner-test-capability"),
                    Some(profile)
                )
                .is_err());
        }
        let identity = HttpSessionIdentity {
            owner_tool: Some("runtime_info".into()),
            internal_proxy_token: Some("owner-test-capability".into()),
            ..Default::default()
        };
        assert!(server
            .owner_tool_server("archive_memory", identity.clone())
            .is_err());
        let owner = server
            .owner_tool_server("runtime_info", identity.clone())
            .unwrap()
            .unwrap();
        assert_eq!(owner.active_tool_profile(), Some(ToolProfile::admin()));
        assert_eq!(server.active_tool_profile(), Some(default_tool_profile()));
        assert_eq!(
            owner.rate_limit_session_id(),
            server.rate_limit_session_id()
        );
        assert!(Arc::ptr_eq(&owner.rate_limiter, &server.rate_limiter));
        assert_eq!(owner.session_project(), server.session_project());
        assert_eq!(
            owner.session_project().as_deref(),
            Some("workspace-fixture")
        );
        assert_eq!(owner.session_client(), server.session_client());
        assert!(server
            .owner_tool_server("runtime_info", HttpSessionIdentity::default())
            .unwrap()
            .is_none());
        assert_eq!(
            server.clone_for_mcp_session().active_tool_profile(),
            Some(default_tool_profile())
        );
        for asserted in ["delegate", "standard+delegate", "delegate+standard"] {
            let mut restricted = identity.clone();
            restricted.profile = Some(asserted.into());
            assert!(server
                .owner_tool_server("runtime_info", restricted)
                .is_err());
        }
        server.set_tool_profile(Some(ToolProfile::delegate()));
        let mut override_attempt = identity;
        override_attempt.profile = Some("standard".into());
        assert!(server
            .owner_tool_server("runtime_info", override_attempt)
            .is_err());
    }
}
