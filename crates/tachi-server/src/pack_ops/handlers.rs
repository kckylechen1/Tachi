use super::*;

pub(crate) async fn handle_pack_list(
    server: &MemoryServer,
    params: PackListParams,
) -> Result<String, String> {
    let enabled_only = params.enabled_only.unwrap_or(false);
    let packs = server.with_global_store_read(|store| {
        store
            .pack_list(enabled_only)
            .map_err(|e| format!("pack_list: {e}"))
    })?;

    if packs.is_empty() {
        return Ok(r#"{"packs":[],"count":0}"#.to_string());
    }

    serde_json::to_string(&serde_json::json!({
        "packs": packs,
        "count": packs.len(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}

/// Get details of a single pack.
pub(crate) async fn handle_pack_get(
    server: &MemoryServer,
    params: PackGetParams,
) -> Result<String, String> {
    let pack = server.with_global_store_read(|store| {
        store
            .pack_get(&params.id)
            .map_err(|e| format!("pack_get: {e}"))
    })?;

    match pack {
        Some(p) => serde_json::to_string(&p).map_err(|e| format!("serialize: {e}")),
        None => Err(format!("Pack '{}' not found", params.id)),
    }
}

/// Register a pack (used after git clone / download).
pub(crate) async fn handle_pack_register(
    server: &MemoryServer,
    params: PackRegisterParams,
) -> Result<String, String> {
    let local_path = params.local_path.clone().unwrap_or_default();
    let descriptor = if local_path.is_empty() {
        None
    } else {
        Some(inspect_pack_source(Path::new(&local_path))?)
    };

    let manifest_pack = descriptor
        .as_ref()
        .and_then(|d| d.manifest.as_ref())
        .map(|m| &m.pack);
    let skill_count = descriptor.as_ref().map(|d| d.skill_count).unwrap_or(0);
    let metadata = merge_pack_metadata(params.metadata.clone(), descriptor.as_ref());

    let pack = Pack {
        id: params.id.clone(),
        name: params
            .name
            .or_else(|| manifest_pack.and_then(|m| m.name.clone()))
            .unwrap_or_else(|| params.id.clone()),
        source: params
            .source
            .or_else(|| manifest_pack.and_then(|m| m.source.clone()))
            .unwrap_or_default(),
        version: params
            .version
            .or_else(|| manifest_pack.and_then(|m| m.version.clone()))
            .unwrap_or_else(|| "latest".to_string()),
        description: params
            .description
            .or_else(|| manifest_pack.and_then(|m| m.description.clone()))
            .unwrap_or_default(),
        skill_count,
        enabled: true,
        local_path,
        metadata: serde_json::to_string(&metadata)
            .map_err(|e| format!("metadata serialize: {e}"))?,
        installed_at: String::new(),
        updated_at: String::new(),
    };

    server.with_global_store(|store| {
        store
            .pack_register(&pack)
            .map_err(|e| format!("pack_register: {e}"))
    })?;

    Ok(serde_json::json!({
        "status": "registered",
        "pack_id": params.id,
        "skill_count": skill_count,
        "manifest_path": descriptor
            .as_ref()
            .and_then(|d| d.manifest_path.as_ref())
            .map(|p| p.display().to_string()),
    })
    .to_string())
}

/// Remove a pack and its projections.
pub(crate) async fn handle_pack_remove(
    server: &MemoryServer,
    params: PackRemoveParams,
) -> Result<String, String> {
    let pack = server.with_global_store_read(|store| {
        store
            .pack_get(&params.id)
            .map_err(|e| format!("pack_get: {e}"))
    })?;

    if pack.is_none() {
        return Err(format!("Pack '{}' not found", params.id));
    }

    let projections = server.with_global_store_read(|store| {
        store
            .projection_list(None, Some(&params.id))
            .map_err(|e| format!("projection_list: {e}"))
    })?;

    let mut cleaned_agents = Vec::new();
    if params.clean_files.unwrap_or(true) {
        for proj in &projections {
            if !proj.projected_path.is_empty() {
                let path = PathBuf::from(&proj.projected_path);
                if path.exists() {
                    if let Err(e) = std::fs::remove_dir_all(&path) {
                        tracing::warn!(
                            "Failed to remove projected files at {}: {e}",
                            proj.projected_path
                        );
                    } else {
                        cleaned_agents.push(proj.agent.clone());
                    }
                }
            }
        }
    }

    let deleted = server.with_global_store(|store| {
        store
            .pack_delete(&params.id)
            .map_err(|e| format!("pack_delete: {e}"))
    })?;

    Ok(serde_json::json!({
        "status": if deleted { "removed" } else { "not_found" },
        "pack_id": params.id,
        "cleaned_agents": cleaned_agents,
    })
    .to_string())
}

/// Project a pack's assets to one or more agents.
pub(crate) async fn handle_pack_project(
    server: &MemoryServer,
    params: PackProjectParams,
) -> Result<String, String> {
    let pack = server
        .with_global_store_read(|store| {
            store
                .pack_get(&params.pack_id)
                .map_err(|e| format!("pack_get: {e}"))
        })?
        .ok_or_else(|| format!("Pack '{}' not found", params.pack_id))?;

    let agents: Vec<AgentKind> = params
        .agents
        .iter()
        .filter_map(|s| AgentKind::from_str(s))
        .collect();

    if agents.is_empty() {
        return Err("No valid agent kinds provided. Use: claude, codex, cursor, gemini, openclaw, antigravity, trae, kiro, generic".to_string());
    }

    let mut results = Vec::new();

    for agent in &agents {
        match project_pack_to_agent(&pack, *agent) {
            Ok(summary) => {
                let proj = AgentProjection {
                    agent: agent.as_str().to_string(),
                    pack_id: pack.id.clone(),
                    enabled: true,
                    projected_path: summary.path.clone(),
                    skill_count: summary.skill_count,
                    synced_at: String::new(),
                };

                if let Err(e) = server.with_global_store(|store| {
                    store
                        .projection_upsert(&proj)
                        .map_err(|e| format!("projection_upsert: {e}"))
                }) {
                    tracing::warn!(
                        "Failed to save projection record for {}: {e}",
                        agent.as_str()
                    );
                }

                results.push(serde_json::json!({
                    "agent": agent.as_str(),
                    "status": "projected",
                    "path": summary.path,
                    "skill_count": summary.skill_count,
                    "workflow_count": summary.workflow_count,
                    "overlay_count": summary.overlay_count,
                    "runtime_count": summary.runtime_count,
                    "projection_manifest": summary.projection_manifest,
                }));
            }
            Err(e) => {
                results.push(serde_json::json!({
                    "agent": agent.as_str(),
                    "status": "failed",
                    "error": e,
                }));
            }
        }
    }

    Ok(serde_json::json!({
        "pack_id": pack.id,
        "projections": results,
    })
    .to_string())
}

/// List agent projections.
pub(crate) async fn handle_projection_list(
    server: &MemoryServer,
    params: ProjectionListParams,
) -> Result<String, String> {
    let projections = server.with_global_store_read(|store| {
        store
            .projection_list(params.agent.as_deref(), params.pack_id.as_deref())
            .map_err(|e| format!("projection_list: {e}"))
    })?;

    serde_json::to_string(&serde_json::json!({
        "projections": projections,
        "count": projections.len(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}

// ─── Projection Logic ────────────────────────────────────────────────────────
