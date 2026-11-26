use crate::tool_params::ExportSkillsParams;
use crate::utils::sanitize_safe_path_name;
use crate::MemoryServer;
use memcore::HubCapability;
use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;
use tachi_hub::{capability_visibility_for_cap, CapabilityVisibility};

#[cfg(all(test, unix))]
#[path = "export_ownership_tests.rs"]
mod ownership_tests;

#[cfg(all(test, unix))]
#[path = "export_file_tests.rs"]
mod file_tests;

fn claude_cleanup_report(requested: bool) -> serde_json::Value {
    json!({
        "requested": requested,
        "performed": false,
        "reason": "ownership_unverified; no entries pruned"
    })
}

fn empty_export_result(agent: &str, clean: bool) -> serde_json::Value {
    let mut result = json!({
        "agent": agent,
        "exported": 0,
        "message": "No skills matched the filter criteria"
    });
    if agent == "claude" {
        result["cleanup"] = claude_cleanup_report(clean);
        result["publication_policy"] = json!("create_only; existing_entries_preserved");
    }
    result
}

/// Export Hub skills to agent-specific file formats.
///
/// Supports:
/// - **claude**: `~/.tachi/skills/<name>/SKILL.md` + symlinks to `~/.claude/skills/<name>`
/// - **cursor**: `.cursor/rules/<name>.mdc` rule files in the working directory
/// - **generic**: Raw SKILL.md files to a specified directory
pub(crate) async fn handle_export_skills(
    server: &MemoryServer,
    params: ExportSkillsParams,
) -> Result<String, String> {
    let agent = params.agent.to_ascii_lowercase();
    let exporter: fn(&[HubCapability], &ExportSkillsParams) -> Result<String, String> =
        match agent.as_str() {
            "claude" => export_for_claude,
            "cursor" => export_for_cursor,
            "generic" => export_for_generic,
            other => {
                return Err(format!(
                    "Unsupported agent target '{other}'. Supported: claude, cursor, generic"
                ));
            }
        };
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
    all_skills.retain(|cap| !crate::builtins::is_retired_builtin_capability_id(&cap.id));

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
        return serde_json::to_string(&empty_export_result(&agent, params.clean))
            .map_err(|e| format!("serialize: {e}"));
    }

    // ── 5. Dispatch to agent-specific exporter ───────────────────────────────
    exporter(&all_skills, &params)
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
    export_for_claude_to_dirs(skills, params, &tachi_skills_dir, &claude_skills_dir)
}

fn export_for_claude_to_dirs(
    skills: &[HubCapability],
    params: &ExportSkillsParams,
    tachi_skills_dir: &std::path::Path,
    claude_skills_dir: &std::path::Path,
) -> Result<String, String> {
    // The legacy exporter has no ownership receipts. Neither a familiar
    // directory name nor a link into it authorizes replacing existing entries.
    // New publication is exclusive; cleanup waits for the custody contract.
    let mut publisher = super::claude_export_fs::Publisher::open(
        tachi_skills_dir,
        claude_skills_dir,
    )
    .map_err(|error| {
        format!("{error}; new export root directories may exist; existing entries not replaced")
    })?;

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

        match publisher.publish(&name, &content) {
            Ok((skill_file, link_target)) => exported.push(json!({
                "id": cap.id,
                "name": name,
                "file": skill_file.display().to_string(),
                "symlink": link_target.display().to_string()
            })),
            Err(error) => errors.push(json!({
                "id": cap.id,
                "error": error,
                "effect": "new_export_files_may_exist; existing_entries_not_replaced"
            })),
        }
    }

    serde_json::to_string(&json!({
        "agent": "claude",
        "exported": exported.len(),
        "skills_dir": tachi_skills_dir.display().to_string(),
        "claude_skills_dir": claude_skills_dir.display().to_string(),
        "skills": exported,
        "errors": errors,
        "publication_policy": "create_only; existing_entries_preserved",
        "cleanup": claude_cleanup_report(params.clean)
    }))
    .map_err(|e| format!("serialize: {e}"))
}

/// One line for the `.mdc` frontmatter, serialized as a YAML double-quoted
/// scalar so embedded `: `, `#`, flow brackets, line terminators, or `---`
/// cannot split or inject mapping keys.
fn frontmatter_value(value: &str) -> String {
    let single_line: String = value
        .chars()
        .map(|ch| match ch {
            '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}' => ' ',
            other => other,
        })
        .collect();
    let sanitized = single_line.replace("---", "\u{2014}");
    serde_json::to_string(&sanitized).unwrap_or_else(|_| "\"\"".to_string())
}

/// Directory handle for skill exports that refuses symlinked targets and
/// post-open directory swaps on Unix via `O_NOFOLLOW` + `openat`.
struct ExportDir {
    path: PathBuf,
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
}

#[cfg(unix)]
mod export_dir_unix {
    use super::ExportDir;
    use std::ffi::{CString, OsStr};
    use std::fs::File;
    use std::io::{self, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Component, PathBuf};

    fn c_component(component: &OsStr) -> io::Result<CString> {
        if component.is_empty()
            || component == OsStr::new(".")
            || component == OsStr::new("..")
            || component.as_bytes().contains(&b'/')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "export path component must be an ordinary path segment",
            ));
        }
        CString::new(component.as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    }

    fn open_existing_child_dir(parent: &OwnedFd, name: &OsStr) -> io::Result<OwnedFd> {
        let c_name = c_component(name)?;
        // SAFETY: `parent` is a live directory descriptor and `c_name` is a single component.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                c_name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: `fd` is freshly opened.
            Ok(unsafe { OwnedFd::from_raw_fd(fd) })
        }
    }

    fn open_or_create_child_dir(parent: &OwnedFd, name: &OsStr) -> io::Result<OwnedFd> {
        match open_existing_child_dir(parent, name) {
            Ok(fd) => Ok(fd),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let c_name = c_component(name)?;
                // SAFETY: create missing component without following symlinks on the child name.
                if unsafe { libc::mkdirat(parent.as_raw_fd(), c_name.as_ptr(), 0o755) } != 0 {
                    let mkdir_err = io::Error::last_os_error();
                    if mkdir_err.kind() != io::ErrorKind::AlreadyExists {
                        return Err(mkdir_err);
                    }
                }
                open_existing_child_dir(parent, name)
            }
            Err(err) => Err(err),
        }
    }

    impl ExportDir {
        pub(super) fn open_or_create(path: PathBuf) -> io::Result<Self> {
            if path.as_os_str().is_empty()
                || path.components().any(|c| matches!(c, Component::ParentDir))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid export directory: {}", path.display()),
                ));
            }
            let fd = if path.is_relative() {
                // SAFETY: `"."` is valid NUL-terminated static C string.
                let dot = unsafe {
                    libc::open(
                        c".".as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                    )
                };
                if dot < 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: `dot` is freshly opened.
                let mut current = unsafe { OwnedFd::from_raw_fd(dot) };
                for component in path.components() {
                    if let Component::Normal(part) = component {
                        current = open_or_create_child_dir(&current, part)?;
                    }
                }
                current
            } else {
                // SAFETY: `"/"` is valid NUL-terminated static C string.
                let root = unsafe {
                    libc::open(
                        c"/".as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                    )
                };
                if root < 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: `root` is freshly opened.
                let mut current = unsafe { OwnedFd::from_raw_fd(root) };
                let mut depth = 0usize;
                for component in path.components() {
                    if let Component::Normal(part) = component {
                        // On macOS, `/var`, `/tmp`, and `/etc` are top-level OS
                        // symlinks to `/private/*`. Step through `/private` with
                        // `O_NOFOLLOW` only at depth 0; every deeper component
                        // rejects symlinks without exception.
                        #[cfg(target_os = "macos")]
                        if depth == 0
                            && (part == OsStr::new("var")
                                || part == OsStr::new("tmp")
                                || part == OsStr::new("etc"))
                        {
                            match open_existing_child_dir(&current, part) {
                                Ok(dir) => {
                                    current = dir;
                                    depth += 1;
                                    continue;
                                }
                                Err(err)
                                    if matches!(
                                        err.raw_os_error(),
                                        Some(libc::ELOOP | libc::ENOTDIR)
                                    ) && std::fs::read_link(
                                        std::path::Path::new("/").join(part),
                                    )
                                    .is_ok_and(|target| {
                                        target == std::path::Path::new("private").join(part)
                                    }) =>
                                {
                                    let private =
                                        open_existing_child_dir(&current, OsStr::new("private"))?;
                                    current = open_existing_child_dir(&private, part)?;
                                    depth += 1;
                                    continue;
                                }
                                Err(err) => return Err(err),
                            }
                        }
                        current = open_or_create_child_dir(&current, part)?;
                        depth += 1;
                    }
                }
                current
            };
            Ok(Self { path, fd })
        }

        pub(super) fn write_file(&self, file_name: &str, content: &str) -> io::Result<()> {
            let c_name = c_component(OsStr::new(file_name))?;
            // SAFETY: `self.fd` is an open directory fd; O_NOFOLLOW | O_NONBLOCK
            // refuses symlinks and FIFOs without blocking.
            let fd = unsafe {
                libc::openat(
                    self.fd.as_raw_fd(),
                    c_name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_TRUNC
                        | libc::O_NOFOLLOW
                        | libc::O_NONBLOCK
                        | libc::O_CLOEXEC,
                    0o644 as libc::c_uint,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `fd` is freshly opened and exclusively owned.
            let mut file = unsafe { File::from_raw_fd(fd) };
            if !file.metadata()?.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("refusing to write non-regular file: {file_name}"),
                ));
            }
            file.write_all(content.as_bytes())
        }

        pub(super) fn remove_regular_file(&self, file_name: &OsStr) -> io::Result<()> {
            let c_name = c_component(file_name)?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: `self.fd` is live, `c_name` is one component, AT_SYMLINK_NOFOLLOW
            // inspects the link itself rather than its target.
            if unsafe {
                libc::fstatat(
                    self.fd.as_raw_fd(),
                    c_name.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: fstatat succeeded and initialized `stat`.
            let stat = unsafe { stat.assume_init() };
            if (stat.st_mode & libc::S_IFMT) != libc::S_IFREG {
                return Ok(());
            }
            // SAFETY: `unlinkat` without AT_REMOVEDIR never removes directories or follows symlinks.
            if unsafe { libc::unlinkat(self.fd.as_raw_fd(), c_name.as_ptr(), 0) } != 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(not(unix))]
impl ExportDir {
    fn open_or_create(path: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn write_file(&self, file_name: &str, content: &str) -> std::io::Result<()> {
        std::fs::write(self.path.join(file_name), content)
    }

    fn remove_regular_file(&self, file_name: &std::ffi::OsStr) -> std::io::Result<()> {
        let target = self.path.join(file_name);
        if target
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_file())
        {
            std::fs::remove_file(target)?;
        }
        Ok(())
    }
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

    let dir = ExportDir::open_or_create(output_dir.clone())
        .map_err(|e| format!("create {:?}: {e}", output_dir))?;

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
            frontmatter_value(&cap.description),
            cap.name,
            content
        );

        let file_name = format!("tachi-{}.mdc", name);
        let file_path = dir.path.join(&file_name);
        if let Err(e) = dir.write_file(&file_name, &mdc_content) {
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
        if let Ok(entries) = std::fs::read_dir(&dir.path) {
            for entry in entries.flatten() {
                let fname = entry.file_name();
                let name_str = fname.to_string_lossy();
                if name_str.starts_with("tachi-") && name_str.ends_with(".mdc") {
                    let skill_name = name_str
                        .strip_prefix("tachi-")
                        .and_then(|s| s.strip_suffix(".mdc"))
                        .unwrap_or("");
                    if !exported_names.contains(skill_name) {
                        let _ = dir.remove_regular_file(&fname);
                    }
                }
            }
        }
    }

    serde_json::to_string(&json!({
        "agent": "cursor",
        "exported": exported.len(),
        "output_dir": dir.path.display().to_string(),
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

    let dir = ExportDir::open_or_create(output_dir.clone())
        .map_err(|e| format!("create {:?}: {e}", output_dir))?;

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

        let file_name = format!("{}.md", name);
        let file_path = dir.path.join(&file_name);
        if let Err(e) = dir.write_file(&file_name, &content) {
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
        "output_dir": dir.path.display().to_string(),
        "skills": exported,
        "errors": errors
    }))
    .map_err(|e| format!("serialize: {e}"))
}
