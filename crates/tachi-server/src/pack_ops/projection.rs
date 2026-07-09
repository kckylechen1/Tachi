use super::*;

pub(super) fn project_pack_to_agent(
    pack: &Pack,
    agent: AgentKind,
) -> Result<ProjectionSummary, String> {
    let source_dir = PathBuf::from(&pack.local_path);
    if !source_dir.exists() {
        return Err(format!(
            "Pack source directory not found: {}",
            pack.local_path
        ));
    }

    let descriptor = inspect_pack_source(&source_dir)?;
    let (base_dir_template, _) = agent.skill_target();
    let home = dirs::home_dir().ok_or_else(|| "Cannot determine home directory".to_string())?;
    let base_dir = base_dir_template.replace("~", &home.to_string_lossy());
    let pack_short_name =
        sanitize_safe_path_name(pack.id.split('/').next_back().unwrap_or(&pack.id));
    let target_dir = PathBuf::from(&base_dir).join(&pack_short_name);
    std::fs::create_dir_all(&target_dir)
        .map_err(|e| format!("Failed to create {}: {e}", target_dir.display()))?;

    let skill_files =
        collect_skill_files_for_agent(&source_dir, agent, descriptor.manifest.as_ref())?;
    let skill_count = match agent {
        AgentKind::Cursor => {
            project_skills_as_cursor_rules(&skill_files, &target_dir, &pack_short_name)?
        }
        _ => copy_skill_files(&skill_files, &target_dir)?,
    };

    let (workflow_count, workflow_paths) = copy_asset_refs(
        &source_dir,
        &descriptor.workflow_assets,
        &target_dir.join("_workflows"),
    )?;
    let (runtime_count, runtime_paths) = copy_asset_refs(
        &source_dir,
        &descriptor.runtime_assets,
        &target_dir.join("_runtime"),
    )?;

    let mut overlay_assets = descriptor.common_overlay_assets.clone();
    for key in overlay_lookup_keys(agent) {
        if let Some(entries) = descriptor.agent_overlay_assets.get(*key) {
            overlay_assets.extend(entries.clone());
        }
    }

    let (mut overlay_count, overlay_paths) = copy_asset_refs(
        &source_dir,
        &overlay_assets,
        &target_dir.join("_overlay").join(agent.as_str()),
    )?;

    let overlay_manifest = merge_overlay_manifest(agent, descriptor.manifest.as_ref());
    if let Some(ref overlay_json) = overlay_manifest {
        let overlay_manifest_path = target_dir
            .join("_overlay")
            .join(agent.as_str())
            .join("overlay-manifest.json");
        crate::utils::write_json_file_owner_only(&overlay_manifest_path, overlay_json)?;
        overlay_count += 1;
    }

    let projection_manifest_path = target_dir.join("tachi-projection.json");
    let projection_manifest = json!({
        "schema_version": "tachi.pack.projection.v1",
        "pack_id": pack.id.clone(),
        "agent": agent.as_str(),
        "generated_at": Utc::now().to_rfc3339(),
        "source_path": pack.local_path.clone(),
        "manifest_path": descriptor
            .manifest_path
            .as_ref()
            .map(|p| p.display().to_string()),
        "services": descriptor.services,
        "counts": {
            "skills": skill_count,
            "workflows": workflow_count,
            "overlays": overlay_count,
            "runtime": runtime_count,
        },
        "paths": {
            "skills": skill_files
                .iter()
                .map(|f| normalize_rel_path(&f.relative_target))
                .collect::<Vec<_>>(),
            "workflows": workflow_paths,
            "overlays": overlay_paths,
            "runtime": runtime_paths,
        },
        "overlay_manifest": overlay_manifest,
    });
    crate::utils::write_json_file_owner_only(&projection_manifest_path, &projection_manifest)?;

    Ok(ProjectionSummary {
        path: target_dir.display().to_string(),
        skill_count,
        workflow_count,
        overlay_count,
        runtime_count,
        projection_manifest: projection_manifest_path.display().to_string(),
    })
}
