use super::*;

pub(super) fn count_skills_in_source(
    source_dir: &Path,
    manifest: Option<&PackManifest>,
) -> Result<u32, String> {
    Ok(collect_skill_files_for_agent(source_dir, AgentKind::Generic, manifest)?.len() as u32)
}

pub(super) fn collect_skill_files_for_agent(
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

pub(super) fn select_skill_root(source_dir: &Path, agent: AgentKind) -> PathBuf {
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

pub(super) fn collect_skill_files_from_assets(
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

pub(super) fn collect_skill_files_from_root(root: &Path) -> Result<Vec<SkillFile>, String> {
    let mut files = Vec::new();
    collect_skill_files_recursive(root, root, Path::new(""), &mut files)?;
    Ok(files)
}

pub(super) fn collect_skill_files_recursive(
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

pub(super) fn dedupe_skill_files(files: Vec<SkillFile>) -> Vec<SkillFile> {
    let mut seen = BTreeMap::new();
    for file in files {
        seen.entry(normalize_rel_path(&file.relative_target))
            .or_insert(file);
    }
    seen.into_values().collect()
}

pub(super) fn copy_skill_files(files: &[SkillFile], target_dir: &Path) -> Result<u32, String> {
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
pub(super) fn project_skills_as_cursor_rules(
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

pub(super) fn skill_name_for_cursor(file: &SkillFile, pack_name: &str) -> String {
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
