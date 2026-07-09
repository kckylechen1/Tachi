use super::*;

pub(super) fn inspect_pack_source(source_dir: &Path) -> Result<PackDescriptor, String> {
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

pub(super) fn merge_pack_metadata(
    user_metadata: Option<Value>,
    descriptor: Option<&PackDescriptor>,
) -> Value {
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

pub(super) fn load_pack_manifest(
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

pub(super) fn discover_common_overlays(
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

pub(super) fn discover_agent_overlays(
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

pub(super) fn flatten_overlay_assets(overlay: &PackOverlay) -> Vec<PackAssetRef> {
    let mut assets = Vec::new();
    assets.extend(overlay.files.clone());
    assets.extend(overlay.commands.clone());
    assets.extend(overlay.hooks.clone());
    assets.extend(overlay.agents.clone());
    assets
}

pub(super) fn overlay_lookup_keys(agent: AgentKind) -> &'static [&'static str] {
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

pub(super) fn merge_overlay_manifest(
    agent: AgentKind,
    manifest: Option<&PackManifest>,
) -> Option<Value> {
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
