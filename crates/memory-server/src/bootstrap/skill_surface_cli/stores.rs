use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::*;

pub(super) fn skill_store_specs(home: &Path, hosts: &[&str]) -> Vec<SkillStoreSpec> {
    let mut specs = vec![
        SkillStoreSpec {
            id: "cc-switch",
            role: "source",
            path: home.join(".cc-switch").join("skills"),
            format: SkillStoreFormat::SkillMd,
        },
        SkillStoreSpec {
            id: "tachi",
            role: "source",
            path: home.join(".tachi").join("skills"),
            format: SkillStoreFormat::SkillMd,
        },
        SkillStoreSpec {
            id: "agents",
            role: "source",
            path: home.join(".agents").join("skills"),
            format: SkillStoreFormat::SkillMd,
        },
    ];

    for host in hosts {
        match *host {
            "claude" => specs.push(SkillStoreSpec {
                id: "claude",
                role: "host",
                path: home.join(".claude").join("skills"),
                format: SkillStoreFormat::SkillMd,
            }),
            "codex" => specs.push(SkillStoreSpec {
                id: "codex",
                role: "host",
                path: home.join(".codex").join("skills"),
                format: SkillStoreFormat::SkillMd,
            }),
            "gemini" => specs.push(SkillStoreSpec {
                id: "gemini",
                role: "host",
                path: home.join(".gemini").join("skills"),
                format: SkillStoreFormat::SkillMd,
            }),
            "cursor" => specs.push(SkillStoreSpec {
                id: "cursor",
                role: "host",
                path: home.join(".cursor").join("rules"),
                format: SkillStoreFormat::CursorMdc,
            }),
            "antigravity" => specs.push(SkillStoreSpec {
                id: "antigravity",
                role: "host",
                path: home.join(".gemini").join("antigravity").join("skills"),
                format: SkillStoreFormat::None,
            }),
            _ => {}
        }
    }

    specs
}

pub(super) fn scan_skill_store(
    spec: &SkillStoreSpec,
) -> (SkillStoreSummary, Vec<SkillEntryStatus>) {
    let mut summary = SkillStoreSummary {
        id: spec.id.to_string(),
        role: spec.role.to_string(),
        path: spec.path.display().to_string(),
        format: format_name(spec.format).to_string(),
        exists: spec.path.exists(),
        entries: 0,
        skills_with_content: 0,
        symlinks: 0,
        broken_symlinks: 0,
        missing_skill_files: 0,
    };
    let mut entries = Vec::new();

    if !spec.path.exists() || matches!(spec.format, SkillStoreFormat::None) {
        return (summary, entries);
    }

    match spec.format {
        SkillStoreFormat::SkillMd => scan_skill_md_store(spec, &mut summary, &mut entries),
        SkillStoreFormat::CursorMdc => scan_cursor_store(spec, &mut summary, &mut entries),
        SkillStoreFormat::None => {}
    }

    (summary, entries)
}

fn scan_skill_md_store(
    spec: &SkillStoreSpec,
    summary: &mut SkillStoreSummary,
    entries: &mut Vec<SkillEntryStatus>,
) {
    let Ok(read_dir) = std::fs::read_dir(&spec.path) else {
        return;
    };

    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }

        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !(meta.is_dir() || meta.file_type().is_symlink()) {
            continue;
        }

        summary.entries += 1;
        let is_symlink = meta.file_type().is_symlink();
        if is_symlink {
            summary.symlinks += 1;
        }

        let symlink_target = if is_symlink {
            std::fs::read_link(&path)
                .ok()
                .map(|target| resolve_link_target(&path, &target).display().to_string())
        } else {
            None
        };
        let target_exists = symlink_target
            .as_ref()
            .map(|target| Path::new(target).exists());
        if target_exists == Some(false) {
            summary.broken_symlinks += 1;
        }

        let skill_file = path.join("SKILL.md");
        let content = std::fs::read_to_string(&skill_file).ok();
        let mut issues = Vec::new();
        if target_exists == Some(false) {
            issues.push("broken_symlink".to_string());
        }
        if content.is_none() {
            summary.missing_skill_files += 1;
            issues.push("missing_SKILL.md".to_string());
        } else {
            summary.skills_with_content += 1;
        }

        entries.push(SkillEntryStatus {
            store: spec.id.to_string(),
            role: spec.role.to_string(),
            name,
            path: path.display().to_string(),
            is_symlink,
            symlink_target,
            target_exists,
            hash: content.map(|content| crate::utils::stable_hash(&content)),
            issues,
        });
    }
}

fn scan_cursor_store(
    spec: &SkillStoreSpec,
    summary: &mut SkillStoreSummary,
    entries: &mut Vec<SkillEntryStatus>,
) {
    let Ok(read_dir) = std::fs::read_dir(&spec.path) else {
        return;
    };

    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("mdc") {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().to_string();
        let name = file_name
            .strip_prefix("tachi-")
            .unwrap_or(&file_name)
            .trim_end_matches(".mdc")
            .to_string();
        let content = std::fs::read_to_string(&path).ok();

        summary.entries += 1;
        if content.is_some() {
            summary.skills_with_content += 1;
        } else {
            summary.missing_skill_files += 1;
        }

        entries.push(SkillEntryStatus {
            store: spec.id.to_string(),
            role: spec.role.to_string(),
            name,
            path: path.display().to_string(),
            is_symlink: false,
            symlink_target: None,
            target_exists: None,
            hash: content.map(|content| crate::utils::stable_hash(&content)),
            issues: Vec::new(),
        });
    }
}

fn resolve_link_target(link_path: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        link_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(target)
    }
}

fn format_name(format: SkillStoreFormat) -> &'static str {
    match format {
        SkillStoreFormat::SkillMd => "skill_md",
        SkillStoreFormat::CursorMdc => "cursor_mdc",
        SkillStoreFormat::None => "none",
    }
}

pub(super) fn build_drift_groups(entries: &[SkillEntryStatus]) -> Vec<SkillDriftGroup> {
    let mut by_name: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
    for entry in entries {
        let Some(hash) = entry.hash.as_deref() else {
            continue;
        };
        by_name
            .entry(entry.name.clone())
            .or_default()
            .entry(hash.to_string())
            .or_default()
            .insert(entry.store.clone());
    }

    by_name
        .into_iter()
        .filter_map(|(name, hashes)| {
            if hashes.len() <= 1 {
                return None;
            }
            Some(SkillDriftGroup {
                name,
                hashes: hashes
                    .into_iter()
                    .map(|(hash, stores)| SkillHashGroup {
                        hash,
                        stores: stores.into_iter().collect(),
                    })
                    .collect(),
            })
        })
        .collect()
}
