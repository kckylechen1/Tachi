use super::*;

// ─── Workspace directory + MCP config preparation ────────────────────────────

pub(super) async fn prepare_workspace_and_mcp(
    server: &MemoryServer,
    workspace_dir: &Path,
    dispatch_id: &str,
    inject_tachi: bool,
    inject_hub: bool,
    tool_profile: Option<&str>,
    agent_seat: Option<&str>,
    allowed_mcp_servers: &[String],
    requested_project: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    if inject_tachi {
        if let Some(requested) = requested_project {
            let bound = server.session_project().ok_or_else(|| {
                "project-bound MCP injection requires the caller to bind its workspace/project first; a dispatch argument alone cannot select child authority".to_string()
            })?;
            let mut arguments = Some(serde_json::Map::from_iter([
                ("action".into(), json!("start")),
                ("project".into(), json!(requested)),
            ]));
            crate::session_identity::enforce_server_session_project(
                server,
                "tachi_staff",
                &mut arguments,
                &bound,
                "Staff MCP injection",
                crate::session_identity::EnforcementRole::Preflight,
            )
            .map_err(|error| error.to_string())?;
        }
    }
    // Create isolated workspace directory
    tokio::fs::create_dir_all(workspace_dir)
        .await
        .map_err(|e| format!("Failed to create workspace dir: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(workspace_dir, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|e| format!("Failed to set workspace dir permissions: {e}"))?;
    }

    // Generate MCP config if requested
    let mcp_config_path = if inject_tachi || inject_hub {
        generate_mcp_config(
            server,
            dispatch_id,
            inject_tachi,
            inject_hub,
            tool_profile,
            agent_seat,
            allowed_mcp_servers,
        )
        .await?
    } else {
        None
    };

    Ok(mcp_config_path)
}
