use super::*;

// ─── Workspace directory + MCP config preparation ────────────────────────────

pub(super) async fn prepare_workspace_and_mcp(
    server: &MemoryServer,
    workspace_dir: &Path,
    dispatch_id: &str,
    inject_tachi: bool,
    inject_hub: bool,
    tool_profile: Option<&str>,
    allowed_mcp_servers: &[String],
) -> Result<Option<PathBuf>, String> {
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
            allowed_mcp_servers,
        )
        .await?
    } else {
        None
    };

    Ok(mcp_config_path)
}
