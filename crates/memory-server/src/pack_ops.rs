// pack_ops.rs — Pack system server operations
//
// Handles pack registration, listing, removal, and agent projection.

use crate::server_state::MemoryServer;
use crate::tool_params::{
    PackGetParams, PackListParams, PackProjectParams, PackRegisterParams, PackRemoveParams,
    ProjectionListParams,
};
use crate::utils::sanitize_safe_path_name;
use chrono::Utc;
use memory_core::{AgentKind, AgentProjection, Pack, PackAssetRef, PackManifest, PackOverlay};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

const SKIPPED_DIRS: &[&str] = &[
    "node_modules",
    "browse",
    "scripts",
    "test",
    "benchmark",
    "docs",
    "lib",
    "bin",
];
const COMMON_OVERLAY_DIRS: &[&str] = &["commands", "hooks", "agents"];
const MANIFEST_FILE_NAMES: &[&str] = &["tachi-pack.json"];

#[derive(Debug, Clone)]
struct PackDescriptor {
    manifest_path: Option<PathBuf>,
    manifest: Option<PackManifest>,
    services: Vec<String>,
    workflow_assets: Vec<PackAssetRef>,
    runtime_assets: Vec<PackAssetRef>,
    common_overlay_assets: Vec<PackAssetRef>,
    agent_overlay_assets: BTreeMap<String, Vec<PackAssetRef>>,
    skill_count: u32,
    metadata: Value,
}

#[derive(Debug, Clone)]
struct SkillFile {
    source: PathBuf,
    relative_target: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
struct ProjectionSummary {
    path: String,
    skill_count: u32,
    workflow_count: u32,
    overlay_count: u32,
    runtime_count: u32,
    projection_manifest: String,
}

/// List installed packs.
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

fn project_pack_to_agent(pack: &Pack, agent: AgentKind) -> Result<ProjectionSummary, String> {
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

fn inspect_pack_source(source_dir: &Path) -> Result<PackDescriptor, String> {
    let (manifest_path, manifest) = load_pack_manifest(source_dir)?;

    let workflow_assets = if let Some(manifest) = manifest.as_ref() {
        if !manifest.workflows.is_empty() {
            manifest.workflows.clone()
        } else if source_dir.join("workflows").exists() {
            vec![asset_ref(
                "workflows",
                Some("workflows"),
                Some("workflow-tree"),
            )]
        } else {
            Vec::new()
        }
    } else if source_dir.join("workflows").exists() {
        vec![asset_ref(
            "workflows",
            Some("workflows"),
            Some("workflow-tree"),
        )]
    } else {
        Vec::new()
    };

    let runtime_assets = manifest
        .as_ref()
        .map(|m| m.runtime.clone())
        .unwrap_or_default();

    let services = manifest
        .as_ref()
        .map(|m| m.services.clone())
        .unwrap_or_default();

    let common_overlay_assets = discover_common_overlays(source_dir, manifest.as_ref());
    let agent_overlay_assets = discover_agent_overlays(source_dir, manifest.as_ref());
    let skill_count = count_skills_in_source(source_dir, manifest.as_ref())?;

    let metadata = json!({
        "manifest_path": manifest_path.as_ref().map(|p| p.display().to_string()),
        "discovered": {
            "skill_count": skill_count,
            "services": services,
            "workflows": workflow_assets.iter().map(|a| a.path.clone()).collect::<Vec<_>>(),
            "runtime": runtime_assets.iter().map(|a| a.path.clone()).collect::<Vec<_>>(),
            "common_overlays": common_overlay_assets.iter().map(|a| a.path.clone()).collect::<Vec<_>>(),
            "agent_overlays": agent_overlay_assets.iter().map(|(agent, items)| {
                json!({
                    "agent": agent,
                    "paths": items.iter().map(|a| a.path.clone()).collect::<Vec<_>>(),
                })
            }).collect::<Vec<_>>(),
        },
    });

    Ok(PackDescriptor {
        manifest_path,
        manifest,
        services,
        workflow_assets,
        runtime_assets,
        common_overlay_assets,
        agent_overlay_assets,
        skill_count,
        metadata,
    })
}

fn merge_pack_metadata(user_metadata: Option<Value>, descriptor: Option<&PackDescriptor>) -> Value {
    let mut metadata = match user_metadata {
        Some(Value::Object(map)) => Value::Object(map),
        Some(other) => json!({ "user_metadata": other }),
        None => json!({}),
    };

    if let Some(object) = metadata.as_object_mut() {
        if let Some(descriptor) = descriptor {
            object.insert("projection".into(), descriptor.metadata.clone());
            if let Some(manifest) = descriptor.manifest.as_ref() {
                object.insert(
                    "pack_manifest".into(),
                    serde_json::to_value(manifest).unwrap_or(Value::Null),
                );
            }
        }
    }

    metadata
}

fn load_pack_manifest(
    source_dir: &Path,
) -> Result<(Option<PathBuf>, Option<PackManifest>), String> {
    for name in MANIFEST_FILE_NAMES {
        let path = source_dir.join(name);
        if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            let manifest = serde_json::from_str::<PackManifest>(&raw)
                .map_err(|e| format!("parse {}: {e}", path.display()))?;
            return Ok((Some(path), Some(manifest)));
        }
    }

    Ok((None, None))
}

fn discover_common_overlays(
    source_dir: &Path,
    manifest: Option<&PackManifest>,
) -> Vec<PackAssetRef> {
    let mut assets = manifest
        .and_then(|m| m.overlays.get("common"))
        .map(flatten_overlay_assets)
        .unwrap_or_default();

    for dir in COMMON_OVERLAY_DIRS {
        let path = source_dir.join(dir);
        if path.exists() {
            assets.push(asset_ref(dir, Some(dir), Some("overlay")));
        }
    }

    dedupe_assets(assets)
}

fn discover_agent_overlays(
    source_dir: &Path,
    manifest: Option<&PackManifest>,
) -> BTreeMap<String, Vec<PackAssetRef>> {
    let mut overlays: BTreeMap<String, Vec<PackAssetRef>> = manifest
        .map(|m| {
            m.overlays
                .iter()
                .filter(|(agent, _)| agent.as_str() != "common")
                .map(|(agent, overlay)| (agent.clone(), flatten_overlay_assets(overlay)))
                .collect()
        })
        .unwrap_or_default();

    for (agent, paths) in [
        ("claude", vec![".claude", ".claude-plugin"]),
        ("codex", vec![".codex", ".agents"]),
        ("cursor", vec![".cursor", ".cursor-plugin"]),
        (
            "openclaw",
            vec![".openclaw", "openclaw", "integrations/openclaw"],
        ),
    ] {
        let slot = overlays.entry(agent.to_string()).or_default();
        for rel in paths {
            let path = source_dir.join(rel);
            if path.exists() {
                slot.push(asset_ref(rel, Some(rel), Some("overlay")));
            }
        }
    }

    overlays
        .into_iter()
        .map(|(agent, assets)| (agent, dedupe_assets(assets)))
        .collect()
}

fn flatten_overlay_assets(overlay: &PackOverlay) -> Vec<PackAssetRef> {
    let mut assets = Vec::new();
    assets.extend(overlay.files.clone());
    assets.extend(overlay.commands.clone());
    assets.extend(overlay.hooks.clone());
    assets.extend(overlay.agents.clone());
    assets
}

fn overlay_lookup_keys(agent: AgentKind) -> &'static [&'static str] {
    match agent {
        AgentKind::Claude => &["claude"],
        AgentKind::Codex => &["codex"],
        AgentKind::Cursor => &["cursor"],
        AgentKind::Gemini => &["gemini"],
        AgentKind::OpenClaw => &["openclaw"],
        AgentKind::Antigravity | AgentKind::Kiro => &["claude"],
        AgentKind::Trae => &["trae"],
        AgentKind::Generic => &["generic"],
    }
}

fn merge_overlay_manifest(agent: AgentKind, manifest: Option<&PackManifest>) -> Option<Value> {
    let manifest = manifest?;
    let mut merged = serde_json::Map::new();

    for key in overlay_lookup_keys(agent) {
        if let Some(overlay) = manifest.overlays.get(*key) {
            if let Some(Value::Object(object)) = overlay.manifest.clone() {
                for (k, v) in object {
                    merged.insert(k, v);
                }
            } else if let Some(value) = overlay.manifest.clone() {
                merged.insert("value".into(), value);
            }
        }
    }

    if merged.is_empty() {
        None
    } else {
        Some(Value::Object(merged))
    }
}

fn count_skills_in_source(
    source_dir: &Path,
    manifest: Option<&PackManifest>,
) -> Result<u32, String> {
    Ok(collect_skill_files_for_agent(source_dir, AgentKind::Generic, manifest)?.len() as u32)
}

fn collect_skill_files_for_agent(
    source_dir: &Path,
    agent: AgentKind,
    manifest: Option<&PackManifest>,
) -> Result<Vec<SkillFile>, String> {
    if let Some(manifest) = manifest {
        if !manifest.skills.is_empty() {
            return collect_skill_files_from_assets(source_dir, &manifest.skills);
        }
    }

    let skill_root = select_skill_root(source_dir, agent);
    collect_skill_files_from_root(&skill_root)
}

fn select_skill_root(source_dir: &Path, agent: AgentKind) -> PathBuf {
    let codex_skills = source_dir.join(".agents").join("skills");
    let generic_skills = source_dir.join("skills");

    if matches!(agent, AgentKind::Codex) && codex_skills.exists() {
        codex_skills
    } else if generic_skills.exists() {
        generic_skills
    } else {
        source_dir.to_path_buf()
    }
}

fn collect_skill_files_from_assets(
    source_dir: &Path,
    assets: &[PackAssetRef],
) -> Result<Vec<SkillFile>, String> {
    let mut files = Vec::new();
    for asset in assets {
        let source = source_dir.join(&asset.path);
        if !source.exists() {
            tracing::warn!("Skipping missing skill asset {}", source.display());
            continue;
        }

        if source.is_dir() {
            let root = source.clone();
            let target_root = asset
                .target
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(&asset.path));
            collect_skill_files_recursive(&root, &root, &target_root, &mut files)?;
        } else {
            let target = asset
                .target
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(&asset.path));
            files.push(SkillFile {
                source,
                relative_target: target,
            });
        }
    }

    Ok(dedupe_skill_files(files))
}

fn collect_skill_files_from_root(root: &Path) -> Result<Vec<SkillFile>, String> {
    let mut files = Vec::new();
    collect_skill_files_recursive(root, root, Path::new(""), &mut files)?;
    Ok(files)
}

fn collect_skill_files_recursive(
    current: &Path,
    root: &Path,
    target_prefix: &Path,
    out: &mut Vec<SkillFile>,
) -> Result<(), String> {
    let root_skill = current.join("SKILL.md");
    if root_skill.exists() {
        let relative = root_skill
            .strip_prefix(root)
            .map_err(|e| format!("strip_prefix {}: {e}", root_skill.display()))?
            .to_path_buf();
        out.push(SkillFile {
            source: root_skill,
            relative_target: target_prefix.join(relative),
        });
    }

    let entries =
        std::fs::read_dir(current).map_err(|e| format!("read_dir {}: {e}", current.display()))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        if dir_name.starts_with('.') || SKIPPED_DIRS.contains(&dir_name.as_str()) {
            continue;
        }
        collect_skill_files_recursive(&path, root, target_prefix, out)?;
    }

    Ok(())
}

fn dedupe_skill_files(files: Vec<SkillFile>) -> Vec<SkillFile> {
    let mut seen = BTreeMap::new();
    for file in files {
        seen.entry(normalize_rel_path(&file.relative_target))
            .or_insert(file);
    }
    seen.into_values().collect()
}

fn copy_skill_files(files: &[SkillFile], target_dir: &Path) -> Result<u32, String> {
    for file in files {
        let dest = safe_join(target_dir, &file.relative_target);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::copy(&file.source, &dest)
            .map_err(|e| format!("copy {} -> {}: {e}", file.source.display(), dest.display()))?;
    }
    Ok(files.len() as u32)
}

fn copy_asset_refs(
    source_dir: &Path,
    assets: &[PackAssetRef],
    target_dir: &Path,
) -> Result<(u32, Vec<String>), String> {
    let mut count = 0u32;
    let mut projected = Vec::new();

    for asset in assets {
        let source = source_dir.join(&asset.path);
        if !source.exists() {
            tracing::warn!("Skipping missing asset {}", source.display());
            continue;
        }

        let relative_target = asset
            .target
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&asset.path));
        let destination = safe_join(target_dir, &relative_target);
        copy_path_recursive(&source, &destination)?;
        projected.push(normalize_rel_path(
            destination
                .strip_prefix(target_dir)
                .unwrap_or(destination.as_path()),
        ));
        count += 1;
    }

    Ok((count, projected))
}

fn copy_path_recursive(source: &Path, destination: &Path) -> Result<(), String> {
    if source.is_dir() {
        std::fs::create_dir_all(destination)
            .map_err(|e| format!("mkdir {}: {e}", destination.display()))?;
        for entry in std::fs::read_dir(source)
            .map_err(|e| format!("read_dir {}: {e}", source.display()))?
            .flatten()
        {
            let child_source = entry.path();
            let child_destination = destination.join(entry.file_name());
            copy_path_recursive(&child_source, &child_destination)?;
        }
        Ok(())
    } else {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::copy(source, destination).map_err(|e| {
            format!(
                "copy {} -> {}: {e}",
                source.display(),
                destination.display()
            )
        })?;
        Ok(())
    }
}

fn project_skills_as_cursor_rules(
    files: &[SkillFile],
    target: &Path,
    pack_name: &str,
) -> Result<u32, String> {
    let mut count = 0u32;

    for file in files {
        let content = std::fs::read_to_string(&file.source)
            .map_err(|e| format!("read {}: {e}", file.source.display()))?;
        let skill_name = skill_name_for_cursor(file, pack_name);
        let mdc = format_as_mdc(pack_name, &skill_name, &content);
        let dest = target.join(format!("{pack_name}-{skill_name}.mdc"));
        std::fs::write(&dest, mdc).map_err(|e| format!("write {}: {e}", dest.display()))?;
        count += 1;
    }

    Ok(count)
}

fn skill_name_for_cursor(file: &SkillFile, pack_name: &str) -> String {
    let rel = normalize_rel_path(&file.relative_target);
    let name = rel
        .trim_end_matches("/SKILL.md")
        .trim_end_matches(".md")
        .trim_start_matches("./");
    if name.is_empty() || name == "SKILL" || name == "SKILL.md" {
        "main".to_string()
    } else {
        let sanitized = sanitize_safe_path_name(&name.replace('/', "-"));
        let trimmed = sanitized
            .strip_prefix(pack_name)
            .unwrap_or(&sanitized)
            .trim_matches('-')
            .to_string();
        trimmed.if_empty_then("main")
    }
}

trait IfEmptyThen {
    fn if_empty_then(self, fallback: &str) -> String;
}

impl IfEmptyThen for String {
    fn if_empty_then(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_string()
        } else {
            self
        }
    }
}

fn asset_ref(path: &str, target: Option<&str>, kind: Option<&str>) -> PackAssetRef {
    PackAssetRef {
        path: path.to_string(),
        target: target.map(|v| v.to_string()),
        kind: kind.map(|v| v.to_string()),
        description: None,
        metadata: Value::Null,
    }
}

fn dedupe_assets(assets: Vec<PackAssetRef>) -> Vec<PackAssetRef> {
    let mut seen = BTreeMap::new();
    for asset in assets {
        let key = format!(
            "{}::{}",
            asset.path,
            asset.target.clone().unwrap_or_default()
        );
        seen.entry(key).or_insert(asset);
    }
    seen.into_values().collect()
}

fn safe_join(base: &Path, rel: &Path) -> PathBuf {
    let mut joined = base.to_path_buf();
    for component in rel.components() {
        if let Component::Normal(value) = component {
            let segment = sanitize_safe_path_name(&value.to_string_lossy());
            joined.push(segment);
        }
    }
    joined
}

fn normalize_rel_path(path: &Path) -> String {
    let mut parts = Vec::new();
    for component in path.components() {
        if let Component::Normal(value) = component {
            parts.push(value.to_string_lossy().to_string());
        }
    }
    parts.join("/")
}

/// Format a SKILL.md content as a Cursor .mdc rule.
fn format_as_mdc(pack_name: &str, skill_name: &str, content: &str) -> String {
    format!(
        r#"---
description: {pack_name}/{skill_name} skill
globs:
alwaysApply: false
---

{content}
"#
    )
}
