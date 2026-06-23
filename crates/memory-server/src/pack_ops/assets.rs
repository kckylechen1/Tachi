use super::*;

pub(super) fn copy_asset_refs(
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

pub(super) fn copy_path_recursive(source: &Path, destination: &Path) -> Result<(), String> {
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
pub(super) fn asset_ref(path: &str, target: Option<&str>, kind: Option<&str>) -> PackAssetRef {
    PackAssetRef {
        path: path.to_string(),
        target: target.map(|v| v.to_string()),
        kind: kind.map(|v| v.to_string()),
        description: None,
        metadata: Value::Null,
    }
}

pub(super) fn dedupe_assets(assets: Vec<PackAssetRef>) -> Vec<PackAssetRef> {
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

pub(super) fn safe_join(base: &Path, rel: &Path) -> PathBuf {
    let mut joined = base.to_path_buf();
    for component in rel.components() {
        if let Component::Normal(value) = component {
            let segment = sanitize_safe_path_name(&value.to_string_lossy());
            joined.push(segment);
        }
    }
    joined
}

pub(super) fn normalize_rel_path(path: &Path) -> String {
    let mut parts = Vec::new();
    for component in path.components() {
        if let Component::Normal(value) = component {
            parts.push(value.to_string_lossy().to_string());
        }
    }
    parts.join("/")
}

/// Format a SKILL.md content as a Cursor .mdc rule.
pub(super) fn format_as_mdc(pack_name: &str, skill_name: &str, content: &str) -> String {
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
