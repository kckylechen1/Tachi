use crate::tool_params::ExportSkillsParams;
use crate::utils::sanitize_safe_path_name;
use crate::MemoryServer;
use memory_core::HubCapability;
use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;
use tachi_hub::{capability_visibility_for_cap, CapabilityVisibility};

/// Export Hub skills to agent-specific file formats.
///
/// Supports:
/// - **claude**: `~/.tachi/skills/<name>/SKILL.md` + symlinks to `~/.claude/skills/<name>`
/// - **openclaw**: `~/.openclaw/plugins/tachi-skills.json` with hook configuration
/// - **cursor**: `.cursor/rules/<name>.mdc` rule files in the working directory
/// - **generic**: Raw SKILL.md files to a specified directory
pub(crate) async fn handle_export_skills(
    server: &MemoryServer,
    params: ExportSkillsParams,
) -> Result<String, String> {
    let agent = params.agent.to_ascii_lowercase();
    let vis_filter = params.visibility.to_ascii_lowercase();

    // ── 1. Collect all skills from global + project DBs ──────────────────────
    let global_skills = server.with_global_store(|store| {
        store
            .hub_list(Some("skill"), true)
            .map_err(|e| format!("list global skills: {e}"))
    })?;

    let project_skills = if server.has_project_db() {
        server.with_project_store(|store| {
            store
                .hub_list(Some("skill"), true)
                .map_err(|e| format!("list project skills: {e}"))
        })?
    } else {
        vec![]
    };

    // Project-scoped skills take priority (same dedup pattern as hub_discover)
    let mut seen = HashSet::new();
    let mut all_skills: Vec<HubCapability> = Vec::new();

    for cap in project_skills {
        seen.insert(cap.id.clone());
        all_skills.push(cap);
    }
    for cap in global_skills {
        if seen.insert(cap.id.clone()) {
            all_skills.push(cap);
        }
    }

    // ── 2. Filter by requested skill IDs ─────────────────────────────────────
    if let Some(ref ids) = params.skill_ids {
        let id_set: HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
        all_skills.retain(|c| id_set.contains(c.id.as_str()));
    }

    // ── 3. Filter by visibility ──────────────────────────────────────────────
    if vis_filter != "all" {
        all_skills.retain(|cap| {
            let vis = capability_visibility_for_cap(cap);
            match vis_filter.as_str() {
                "listed" => vis == CapabilityVisibility::Listed,
                "discoverable" => {
                    vis == CapabilityVisibility::Discoverable || vis == CapabilityVisibility::Listed
                }
                _ => true,
            }
        });
    }

    // ── 4. Filter by owner_agent for agent-local skills ──────────────────────
    all_skills.retain(|cap| {
        if let Ok(def) = serde_json::from_str::<serde_json::Value>(&cap.definition) {
            let scope = def
                .pointer("/policy/scope")
                .and_then(|v| v.as_str())
                .unwrap_or("pack-shared");
            if scope == "agent-local" {
                let owner = def
                    .pointer("/policy/owner_agent")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                // Agent-local skills are only exported to the owning agent
                return owner.is_empty() || owner == agent;
            }
        }
        true // non-agent-local skills are always included
    });

    if all_skills.is_empty() {
        return serde_json::to_string(&json!({
            "agent": agent,
            "exported": 0,
            "message": "No skills matched the filter criteria"
        }))
        .map_err(|e| format!("serialize: {e}"));
    }

    // ── 5. Dispatch to agent-specific exporter ───────────────────────────────
    match agent.as_str() {
        "claude" => export_for_claude(&all_skills, &params),
        "openclaw" => export_for_openclaw(&all_skills, &params),
        "cursor" => export_for_cursor(&all_skills, &params),
        "generic" => export_for_generic(&all_skills, &params),
        other => Err(format!(
            "Unsupported agent target '{other}'. Supported: claude, openclaw, cursor, generic"
        )),
    }
}

/// Extract skill name from ID (e.g. "skill:code-review" -> "code-review")
fn skill_name_from_id(id: &str) -> String {
    sanitize_safe_path_name(id.strip_prefix("skill:").unwrap_or(id))
}

/// Extract the SKILL.md content from a capability's definition JSON.
fn skill_content(cap: &HubCapability) -> Option<String> {
    let def: serde_json::Value = serde_json::from_str(&cap.definition).ok()?;
    def.get("content")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Export skills for Claude Code: write SKILL.md to ~/.tachi/skills/<name>/ and
/// create symlinks in ~/.claude/skills/.
fn export_for_claude(
    skills: &[HubCapability],
    params: &ExportSkillsParams,
) -> Result<String, String> {
    let home = dirs::home_dir().ok_or_else(|| "Cannot determine home directory".to_string())?;
    let tachi_skills_dir = if let Some(ref dir) = params.output_dir {
        PathBuf::from(dir)
    } else {
        home.join(".tachi").join("skills")
    };
    let claude_skills_dir = home.join(".claude").join("skills");

    std::fs::create_dir_all(&tachi_skills_dir)
        .map_err(|e| format!("create {:?}: {e}", tachi_skills_dir))?;
    std::fs::create_dir_all(&claude_skills_dir)
        .map_err(|e| format!("create {:?}: {e}", claude_skills_dir))?;

    let mut exported = Vec::new();
    let mut errors = Vec::new();

    for cap in skills {
        let name = skill_name_from_id(&cap.id);
        let content = match skill_content(cap) {
            Some(c) => c,
            None => {
                errors.push(json!({ "id": cap.id, "error": "no content in definition" }));
                continue;
            }
        };

        let skill_dir = tachi_skills_dir.join(&name);
        if let Err(e) = std::fs::create_dir_all(&skill_dir) {
            errors.push(json!({ "id": cap.id, "error": format!("mkdir: {e}") }));
            continue;
        }

        let skill_file = skill_dir.join("SKILL.md");
        if let Err(e) = std::fs::write(&skill_file, &content) {
            errors.push(json!({ "id": cap.id, "error": format!("write: {e}") }));
            continue;
        }

        // Create/update symlink in ~/.claude/skills/
        let link_target = claude_skills_dir.join(&name);
        // Remove stale symlink or directory if it exists
        let _ = std::fs::remove_file(&link_target);
        let _ = std::fs::remove_dir(&link_target);
        #[cfg(unix)]
        {
            if let Err(e) = std::os::unix::fs::symlink(&skill_dir, &link_target) {
                errors.push(json!({
                    "id": cap.id,
                    "warning": format!("symlink: {e}"),
                    "file": skill_file.display().to_string()
                }));
            }
        }

        exported.push(json!({
            "id": cap.id,
            "name": name,
            "file": skill_file.display().to_string(),
            "symlink": link_target.display().to_string()
        }));
    }

    // Clean stale skills if requested
    if params.clean {
        let exported_names: HashSet<String> =
            skills.iter().map(|c| skill_name_from_id(&c.id)).collect();
        if let Ok(entries) = std::fs::read_dir(&claude_skills_dir) {
            for entry in entries.flatten() {
                let fname = entry.file_name();
                let name = fname.to_string_lossy();
                if !exported_names.contains(name.as_ref()) {
                    let _ = std::fs::remove_file(entry.path());
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
    }

    serde_json::to_string(&json!({
        "agent": "claude",
        "exported": exported.len(),
        "skills_dir": tachi_skills_dir.display().to_string(),
        "claude_skills_dir": claude_skills_dir.display().to_string(),
        "skills": exported,
        "errors": errors
    }))
    .map_err(|e| format!("serialize: {e}"))
}

/// Export skills for OpenClaw: generate a tachi-skills.json plugin config.
fn export_for_openclaw(
    skills: &[HubCapability],
    params: &ExportSkillsParams,
) -> Result<String, String> {
    let home = dirs::home_dir().ok_or_else(|| "Cannot determine home directory".to_string())?;
    let output_dir = if let Some(ref dir) = params.output_dir {
        PathBuf::from(dir)
    } else {
        home.join(".openclaw").join("plugins")
    };

    std::fs::create_dir_all(&output_dir).map_err(|e| format!("create {:?}: {e}", output_dir))?;

    // Build OpenClaw plugin manifest with skill hooks
    let skill_entries: Vec<serde_json::Value> = skills
        .iter()
        .filter_map(|cap| {
            let name = skill_name_from_id(&cap.id);
            let def: serde_json::Value = serde_json::from_str(&cap.definition).ok()?;
            let description = if cap.description.is_empty() {
                name.clone()
            } else {
                cap.description.clone()
            };
            Some(json!({
                "id": cap.id,
                "name": name,
                "description": description,
                "prompt": def.get("prompt").or(def.get("template")).cloned(),
                "system": def.get("system").cloned(),
                "model": def.get("model").cloned(),
                "temperature": def.get("temperature").cloned(),
                "trigger": format!("@tachi-skill-{}", name)
            }))
        })
        .collect();

    let manifest = json!({
        "plugin": "tachi-skills",
        "version": "1.0.0",
        "description": "Tachi Hub skills exported for OpenClaw",
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "hooks": {
            "before_agent_start": {
                "type": "skill-injection",
                "skills": skill_entries
            }
        },
        "skills": skill_entries
    });

    let manifest_path = output_dir.join("tachi-skills.json");
    let content =
        serde_json::to_string_pretty(&manifest).map_err(|e| format!("serialize manifest: {e}"))?;
    std::fs::write(&manifest_path, &content)
        .map_err(|e| format!("write {:?}: {e}", manifest_path))?;

    serde_json::to_string(&json!({
        "agent": "openclaw",
        "exported": skills.len(),
        "manifest": manifest_path.display().to_string(),
        "skills": skill_entries.iter().map(|s| s["name"].clone()).collect::<Vec<_>>()
    }))
    .map_err(|e| format!("serialize: {e}"))
}

/// Export skills for Cursor: generate .mdc rule files in .cursor/rules/.
fn export_for_cursor(
    skills: &[HubCapability],
    params: &ExportSkillsParams,
) -> Result<String, String> {
    let output_dir = if let Some(ref dir) = params.output_dir {
        PathBuf::from(dir)
    } else {
        PathBuf::from(".cursor").join("rules")
    };

    std::fs::create_dir_all(&output_dir).map_err(|e| format!("create {:?}: {e}", output_dir))?;

    let mut exported = Vec::new();
    let mut errors = Vec::new();

    for cap in skills {
        let name = skill_name_from_id(&cap.id);
        let content = match skill_content(cap) {
            Some(c) => c,
            None => {
                // Fall back to prompt template
                let def: serde_json::Value =
                    serde_json::from_str(&cap.definition).unwrap_or(json!({}));
                match def
                    .get("prompt")
                    .or(def.get("template"))
                    .and_then(|v| v.as_str())
                {
                    Some(p) => p.to_string(),
                    None => {
                        errors.push(json!({ "id": cap.id, "error": "no content or prompt" }));
                        continue;
                    }
                }
            }
        };

        // Cursor .mdc format with frontmatter
        let mdc_content = format!(
            "---\ndescription: {}\nalwaysApply: false\n---\n\n# {}\n\n{}",
            cap.description.replace('\n', " "),
            cap.name,
            content
        );

        let file_path = output_dir.join(format!("tachi-{}.mdc", name));
        if let Err(e) = std::fs::write(&file_path, &mdc_content) {
            errors.push(json!({ "id": cap.id, "error": format!("write: {e}") }));
            continue;
        }

        exported.push(json!({
            "id": cap.id,
            "name": name,
            "file": file_path.display().to_string()
        }));
    }

    // Clean stale cursor rules if requested
    if params.clean {
        let exported_names: HashSet<String> =
            skills.iter().map(|c| skill_name_from_id(&c.id)).collect();
        if let Ok(entries) = std::fs::read_dir(&output_dir) {
            for entry in entries.flatten() {
                let fname = entry.file_name();
                let name_str = fname.to_string_lossy();
                if name_str.starts_with("tachi-") && name_str.ends_with(".mdc") {
                    let skill_name = name_str
                        .strip_prefix("tachi-")
                        .and_then(|s| s.strip_suffix(".mdc"))
                        .unwrap_or("");
                    if !exported_names.contains(skill_name) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }

    serde_json::to_string(&json!({
        "agent": "cursor",
        "exported": exported.len(),
        "output_dir": output_dir.display().to_string(),
        "skills": exported,
        "errors": errors
    }))
    .map_err(|e| format!("serialize: {e}"))
}

/// Export skills as raw SKILL.md files to a generic directory.
fn export_for_generic(
    skills: &[HubCapability],
    params: &ExportSkillsParams,
) -> Result<String, String> {
    let output_dir = if let Some(ref dir) = params.output_dir {
        PathBuf::from(dir)
    } else {
        PathBuf::from("exported-skills")
    };

    std::fs::create_dir_all(&output_dir).map_err(|e| format!("create {:?}: {e}", output_dir))?;

    let mut exported = Vec::new();
    let mut errors = Vec::new();

    for cap in skills {
        let name = skill_name_from_id(&cap.id);
        let content = match skill_content(cap) {
            Some(c) => c,
            None => {
                errors.push(json!({ "id": cap.id, "error": "no content in definition" }));
                continue;
            }
        };

        let file_path = output_dir.join(format!("{}.md", name));
        if let Err(e) = std::fs::write(&file_path, &content) {
            errors.push(json!({ "id": cap.id, "error": format!("write: {e}") }));
            continue;
        }

        exported.push(json!({
            "id": cap.id,
            "name": name,
            "file": file_path.display().to_string()
        }));
    }

    serde_json::to_string(&json!({
        "agent": "generic",
        "exported": exported.len(),
        "output_dir": output_dir.display().to_string(),
        "skills": exported,
        "errors": errors
    }))
    .map_err(|e| format!("serialize: {e}"))
}
