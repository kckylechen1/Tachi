use crate::tool_params::ExportSkillsParams;
use crate::utils::sanitize_safe_path_name;
use crate::MemoryServer;
use memcore::HubCapability;
use serde_json::json;
use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use tachi_hub::{capability_visibility_for_cap, CapabilityVisibility};

#[cfg(all(test, unix))]
#[path = "export_ownership_tests.rs"]
mod ownership_tests;

#[cfg(all(test, unix))]
#[path = "export_file_tests.rs"]
mod file_tests;

#[cfg(all(test, unix))]
#[test]
fn hub_file_exports_replace_a_hardlink_without_changing_the_linked_file() {
    use std::fs;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let external = root.join("external");
    fs::write(&external, "external bytes").unwrap();
    let output = ExportDir::open_or_create(root.join("out")).unwrap();
    let leaf = output.path.join("skill.md");
    fs::hard_link(&external, &leaf).unwrap();

    output.write_file("skill.md", "exported bytes").unwrap();

    assert_eq!(fs::read_to_string(&external).unwrap(), "external bytes");
    assert_eq!(fs::read_to_string(&leaf).unwrap(), "exported bytes");
}

#[cfg(all(test, unix))]
#[test]
fn hub_file_exports_preserve_private_0600_mode_on_overwrite() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let output = ExportDir::open_or_create(root.join("out")).unwrap();
    let leaf = output.path.join("private-skill.md");
    fs::write(&leaf, "initial private content").unwrap();
    fs::set_permissions(&leaf, fs::Permissions::from_mode(0o600)).unwrap();

    output
        .write_file("private-skill.md", "updated private content")
        .unwrap();

    assert_eq!(
        fs::read_to_string(&leaf).unwrap(),
        "updated private content"
    );
    let mode = fs::symlink_metadata(&leaf).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "re-exporting over a 0600 file must not widen permissions"
    );
}

#[cfg(all(test, unix))]
#[test]
fn hub_file_exports_preserve_execute_bits_and_sweep_stale_staging_files() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let out_dir = root.join("out");
    fs::create_dir_all(&out_dir).unwrap();

    // Existing executable file should retain 0755 across overwrite.
    let exec_leaf = out_dir.join("run.sh");
    fs::write(&exec_leaf, "#!/bin/sh\necho old\n").unwrap();
    fs::set_permissions(&exec_leaf, fs::Permissions::from_mode(0o755)).unwrap();

    // Seed one stale (>2h old) UUID v4, one stale non-UUID-v4 lookalike, one non-RFC4122 variant lookalike, and one fresh staging file.
    let stale_tmp = out_dir.join(format!(".tachi-export-{}", uuid::Uuid::new_v4()));
    let stale_lookalike = out_dir.join(format!(".tachi-export-{}", "a".repeat(36)));
    let stale_non_rfc4122 = out_dir.join(".tachi-export-00000000-0000-4000-0000-000000000000");
    let fresh_tmp = out_dir.join(format!(".tachi-export-{}", uuid::Uuid::new_v4()));
    fs::write(&stale_tmp, "abandoned").unwrap();
    fs::write(&stale_lookalike, "user-file").unwrap();
    fs::write(&stale_non_rfc4122, "non-rfc4122-user-file").unwrap();
    fs::write(&fresh_tmp, "in-flight").unwrap();
    let two_hours_ago = jiff::Timestamp::now().as_second() - 7200;
    let times = [
        libc::timespec {
            tv_sec: two_hours_ago,
            tv_nsec: 0,
        },
        libc::timespec {
            tv_sec: two_hours_ago,
            tv_nsec: 0,
        },
    ];
    for path in [&stale_tmp, &stale_lookalike, &stale_non_rfc4122] {
        let c_path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        // SAFETY: `c_path` is valid NUL-terminated path and `times` is 2-element array.
        let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
        assert_eq!(rc, 0, "failed to backdate stale file");
    }

    let output = ExportDir::open_or_create(out_dir.clone()).unwrap();
    assert!(!stale_tmp.exists(), "stale staging remnant must be swept");
    assert!(
        stale_lookalike.exists(),
        "non-UUID-v4 lookalike file must never be swept"
    );
    assert!(
        stale_non_rfc4122.exists(),
        "non-RFC4122 variant lookalike file must never be swept"
    );
    assert!(fresh_tmp.exists(), "fresh staging file must not be swept");

    output
        .write_file("run.sh", "#!/bin/sh\necho new\n")
        .unwrap();
    let mode = fs::symlink_metadata(&exec_leaf)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o755,
        "re-exporting over a 0755 file must preserve execute bits"
    );
}

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

fn check_export_destination(file_name: &str, is_regular: io::Result<bool>) -> io::Result<()> {
    match is_regular {
        Ok(true) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(false) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to replace non-regular export file: {file_name}"),
        )),
        Err(err) => Err(err),
    }
}

#[cfg(unix)]
mod export_dir_unix {
    use super::{check_export_destination, ExportDir};
    use std::ffi::{CStr, CString, OsStr};
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
            let dir = Self { path, fd };
            dir.sweep_stale_staging_files(3600);
            Ok(dir)
        }

        pub(super) fn sweep_stale_staging_files(&self, max_age_secs: i64) {
            let now_secs = jiff::Timestamp::now().as_second();
            // SAFETY: `self.fd` is a live directory fd.
            let dup_fd = unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
            if dup_fd < 0 {
                return;
            }
            // SAFETY: `dup_fd` is a fresh directory descriptor owned by `fdopendir`.
            let dirp = unsafe { libc::fdopendir(dup_fd) };
            if dirp.is_null() {
                unsafe {
                    libc::close(dup_fd);
                }
                return;
            }
            loop {
                // SAFETY: `dirp` is a valid non-null DIR pointer.
                let entry = unsafe { libc::readdir(dirp) };
                if entry.is_null() {
                    break;
                }
                // SAFETY: `d_name` is a NUL-terminated C string in the dirent buffer.
                let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
                let bytes = name.to_bytes();
                let is_canonical_v4_staging = bytes
                    .strip_prefix(b".tachi-export-")
                    .and_then(|suffix| std::str::from_utf8(suffix).ok())
                    .is_some_and(|suffix| {
                        uuid::Uuid::parse_str(suffix).is_ok_and(|u| {
                            u.get_version() == Some(uuid::Version::Random)
                                && u.get_variant() == uuid::Variant::RFC4122
                                && u.hyphenated().to_string() == suffix
                        })
                    });
                if is_canonical_v4_staging {
                    if let Ok(stat) = self.stat_entry(name) {
                        if (stat.st_mode & libc::S_IFMT) == libc::S_IFREG
                            && stat.st_nlink == 1
                            && now_secs.saturating_sub(stat.st_mtime) >= max_age_secs
                        {
                            let _ = self.unlink_file(name);
                        }
                    }
                }
            }
            // SAFETY: `dirp` is non-null and closed exactly once.
            unsafe {
                libc::closedir(dirp);
            }
        }

        pub(super) fn write_file(&self, file_name: &str, content: &str) -> io::Result<()> {
            let destination = c_component(OsStr::new(file_name))?;
            let temporary = CString::new(format!(".tachi-export-{}", uuid::Uuid::new_v4()))
                .expect("generated temporary name has no NUL");
            // Create the staging file owner-only (0600) so contents are never
            // exposed to other local users prior to publication.
            // SAFETY: the directory fd is live, the name is a single
            // component and O_CREAT consumes the supplied mode.
            let fd = unsafe {
                libc::openat(
                    self.fd.as_raw_fd(),
                    temporary.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600 as libc::c_uint,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `fd` is freshly opened and exclusively owned.
            let mut file = unsafe { File::from_raw_fd(fd) };
            let result = file
                .write_all(content.as_bytes())
                .and_then(|()| {
                    let existing = self.stat_entry(&destination);
                    check_export_destination(
                        file_name,
                        existing
                            .as_ref()
                            .map(|stat| (stat.st_mode & libc::S_IFMT) == libc::S_IFREG)
                            .map_err(|err| {
                                io::Error::from_raw_os_error(err.raw_os_error().unwrap_or(0))
                            }),
                    )?;
                    if let Ok(stat) = existing {
                        let target_mode = (stat.st_mode & 0o777) as libc::mode_t;
                        if target_mode != 0o600 {
                            // SAFETY: `file` owns a live fd; fchmod preserves existing
                            // regular-file access bits (including execute bits, stripping
                            // setuid/setgid/sticky) without widening 0600 files.
                            if unsafe { libc::fchmod(file.as_raw_fd(), target_mode) } != 0 {
                                return Err(io::Error::last_os_error());
                            }
                        }
                    }
                    file.sync_all()
                })
                .and_then(|()| {
                    drop(file);
                    // SAFETY: both names are single components under the
                    // live fd. renameat replaces the entry without opening it.
                    let result = unsafe {
                        libc::renameat(
                            self.fd.as_raw_fd(),
                            temporary.as_ptr(),
                            self.fd.as_raw_fd(),
                            destination.as_ptr(),
                        )
                    };
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    // SAFETY: `self.fd` is a live directory descriptor; fsync persists
                    // the atomic rename entry across crash/power loss.
                    let _ = unsafe { libc::fsync(self.fd.as_raw_fd()) };
                    Ok(())
                });
            if result.is_err() {
                let _ = self.unlink_file(&temporary);
            }
            result
        }

        pub(super) fn remove_regular_file(&self, file_name: &OsStr) -> io::Result<()> {
            let c_name = c_component(file_name)?;
            if self.is_regular_file(&c_name)? {
                self.unlink_file(&c_name)?;
            }
            Ok(())
        }

        fn stat_entry(&self, c_name: &CStr) -> io::Result<libc::stat> {
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
            Ok(unsafe { stat.assume_init() })
        }

        fn is_regular_file(&self, c_name: &CStr) -> io::Result<bool> {
            Ok((self.stat_entry(c_name)?.st_mode & libc::S_IFMT) == libc::S_IFREG)
        }

        fn unlink_file(&self, c_name: &CStr) -> io::Result<()> {
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
        use std::fs;
        use std::io::Write;

        let destination = self.path.join(file_name);
        let temporary = self
            .path
            .join(format!(".tachi-export-{}", uuid::Uuid::new_v4()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let written = file
            .write_all(content.as_bytes())
            .and_then(|()| file.sync_all());
        drop(file);

        let result = written
            .and_then(|()| {
                check_export_destination(
                    file_name,
                    destination.symlink_metadata().map(|meta| meta.is_file()),
                )
            })
            .and_then(|()| fs::rename(&temporary, &destination));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
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
