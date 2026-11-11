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
            hosts: Vec::new(),
        },
        SkillStoreSpec {
            id: "tachi",
            role: "source",
            path: home.join(".tachi").join("skills"),
            format: SkillStoreFormat::SkillMd,
            hosts: Vec::new(),
        },
        SkillStoreSpec {
            id: "agents",
            role: "source",
            path: home.join(".agents").join("skills"),
            format: SkillStoreFormat::SkillMd,
            hosts: if hosts.contains(&"factory") {
                vec!["factory".to_string()]
            } else {
                Vec::new()
            },
        },
    ];

    for host in hosts {
        match *host {
            "claude" => specs.push(SkillStoreSpec {
                id: "claude",
                role: "host",
                path: home.join(".claude").join("skills"),
                format: SkillStoreFormat::SkillMd,
                hosts: vec![host.to_string()],
            }),
            "codex" => specs.push(SkillStoreSpec {
                id: "codex",
                role: "host",
                path: home.join(".codex").join("skills"),
                format: SkillStoreFormat::SkillMd,
                hosts: vec![host.to_string()],
            }),
            "gemini" => specs.push(SkillStoreSpec {
                id: "gemini",
                role: "host",
                path: home.join(".gemini").join("skills"),
                format: SkillStoreFormat::SkillMd,
                hosts: vec![host.to_string()],
            }),
            "cursor" => specs.push(SkillStoreSpec {
                id: "cursor",
                role: "host",
                path: home.join(".cursor").join("rules"),
                format: SkillStoreFormat::CursorMdc,
                hosts: vec![host.to_string()],
            }),
            "antigravity" => specs.push(SkillStoreSpec {
                id: "antigravity",
                role: "host",
                path: home.join(".gemini").join("config").join("skills"),
                format: SkillStoreFormat::SkillMd,
                hosts: vec![host.to_string()],
            }),
            "factory" => specs.extend([
                SkillStoreSpec {
                    id: "factory",
                    role: "host",
                    path: home.join(".factory/skills"),
                    format: SkillStoreFormat::SkillMd,
                    hosts: vec![host.to_string()],
                },
                SkillStoreSpec {
                    id: "factory-agent",
                    role: "host",
                    path: home.join(".agent/skills"),
                    format: SkillStoreFormat::SkillMd,
                    hosts: vec![host.to_string()],
                },
                SkillStoreSpec {
                    id: "factory-plugins",
                    role: "plugin",
                    path: home.join(".factory/plugins/cache"),
                    format: SkillStoreFormat::FactoryPlugins,
                    hosts: vec![host.to_string()],
                },
            ]),
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
        hosts: spec.hosts.clone(),
        read_only: true,
        issues: Vec::new(),
    };
    let mut entries = Vec::new();

    if !spec.path.exists() {
        return (summary, entries);
    }

    match spec.format {
        SkillStoreFormat::SkillMd => scan_skill_md_store(spec, &mut summary, &mut entries),
        SkillStoreFormat::CursorMdc => scan_cursor_store(spec, &mut summary, &mut entries),
        SkillStoreFormat::FactoryPlugins => {
            scan_factory_plugin_store(spec, &mut summary, &mut entries)
        }
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
            projection: None,
            plugin: None,
            comparison_hash: None,
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
            projection: None,
            plugin: None,
            comparison_hash: None,
        });
    }
}

fn scan_factory_plugin_store(
    spec: &SkillStoreSpec,
    summary: &mut SkillStoreSummary,
    entries: &mut Vec<SkillEntryStatus>,
) {
    // This store is inventory only. Cached versions are not proof of runtime
    // activation, and project/organization settings are outside this home scan.
    let mut settings_path = spec.path.clone();
    settings_path.pop(); // cache -> plugins
    settings_path.pop(); // plugins -> .factory
    settings_path.push("settings.json");
    let enabled_plugins = match std::fs::read_to_string(&settings_path) {
        Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(settings) => match settings.get("enabledPlugins") {
                Some(serde_json::Value::Object(plugins)) => plugins.clone(),
                None => serde_json::Map::new(),
                Some(_) => {
                    summary
                        .issues
                        .push("invalid_enabledPlugins_settings".into());
                    serde_json::Map::new()
                }
            },
            Err(error) => {
                summary
                    .issues
                    .push(format!("invalid_plugin_settings:{error}"));
                serde_json::Map::new()
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(error) => {
            summary
                .issues
                .push(format!("unreadable_plugin_settings:{error}"));
            serde_json::Map::new()
        }
    };

    let mut pending = vec![spec.path.clone()];
    let mut roots = Vec::new();
    while let Some(path) = pending.pop() {
        let read_dir = match std::fs::read_dir(&path) {
            Ok(read_dir) => read_dir,
            Err(error) => {
                summary.issues.push(format!(
                    "unreadable_plugin_cache:{}:{error}",
                    path.display()
                ));
                continue;
            }
        };
        for child in read_dir.flatten() {
            // Do not follow cache directory symlinks (including cycles). Skill
            // links inside a discovered skills root use the normal scanner.
            if !child.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            if child.file_name() == "skills" {
                roots.push(child.path());
            } else {
                pending.push(child.path());
            }
        }
    }
    roots.sort();
    for root in roots {
        let id = factory_plugin_id(&spec.path, &root);
        let enabled = id
            .as_ref()
            .and_then(|id| enabled_plugins.get(id))
            .and_then(|value| value.as_bool());
        let start = entries.len();
        scan_skill_md_store(
            &SkillStoreSpec {
                path: root,
                format: SkillStoreFormat::SkillMd,
                ..spec.clone()
            },
            summary,
            entries,
        );
        for entry in &mut entries[start..] {
            entry.plugin = Some(FactoryPluginStatus {
                id: id.clone(),
                enabled,
                enablement_scope: "user_settings",
                settings_path: settings_path.display().to_string(),
            });
        }
    }
}

fn factory_plugin_id(cache: &Path, skills: &Path) -> Option<String> {
    let parts = skills
        .strip_prefix(cache)
        .ok()?
        .iter()
        .map(|part| part.to_str())
        .collect::<Option<Vec<_>>>()?;
    // Only bind settings for recognized cache layouts. Other discovered roots
    // still appear in inventory with unknown identity and enablement.
    match parts.as_slice() {
        [marketplace, plugin, _revision, "skills"] => Some(format!("{plugin}@{marketplace}")),
        [marketplace, scope, plugin, _revision, "skills"] if scope.starts_with('@') => {
            Some(format!("{scope}/{plugin}@{marketplace}"))
        }
        _ => None,
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
        SkillStoreFormat::FactoryPlugins => "plugin_skill_md",
    }
}

pub(super) fn build_drift_groups(entries: &[SkillEntryStatus]) -> Vec<SkillDriftGroup> {
    let mut by_name: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
    for entry in entries {
        // Plugin skill names belong to independent packages, not this canonical corpus.
        if entry.role == "plugin" {
            continue;
        }
        let Some(hash) = entry.comparison_hash.as_deref().or(entry.hash.as_deref()) else {
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
