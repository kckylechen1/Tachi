use super::classify::classify_and_extract_metadata;
use super::frontmatter::{parse_frontmatter, serialize_frontmatter, Frontmatter};
use super::paths::{
    is_archive_dir, is_markdown_file, scan_md_files_recursive, secure_join, unique_archive_target,
};
use super::tasks::sync_tasks_in_content;
use crate::server_state::MemoryServer;
use chrono::Utc;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 执行 docs 整理与索引生成的核心主流程
pub(crate) async fn handle_wiki_organize(
    server: &MemoryServer,
    dir_path: &str,
    dry_run: bool,
) -> Result<String, String> {
    let root = Path::new(dir_path);
    if !root.is_dir() {
        return Err(format!("Provided path '{}' is not a directory", dir_path));
    }

    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize docs path '{}': {e}", dir_path))?;

    // Confine to known workspace roots: CWD, home directory, temp, or global_db_path parent
    let canonical_bases: Vec<PathBuf> = [
        std::env::current_dir().ok(),
        dirs::home_dir(),
        Some(std::env::temp_dir()),
        server.global_db_path.parent().map(|p| p.to_path_buf()),
    ]
    .iter()
    .filter_map(|opt| opt.as_ref().and_then(|p| p.canonicalize().ok()))
    .collect();
    let confined = canonical_bases
        .iter()
        .any(|base| canonical_root.starts_with(base));
    if !confined {
        return Err(format!(
            "dir_path '{}' is outside workspace roots (cwd, home, temp, or DB parent)",
            dir_path
        ));
    }

    // 白名单保护文件列表
    let whitelist = ["README.md", "INSTALL.md", "_index.md"];

    let mut moved_count = 0;
    let mut synced_count = 0;
    let mut log_messages = Vec::new();

    // 建立标准分类结构目录
    let standard_dirs = [
        "engineering/architecture",
        "engineering/devops",
        "engineering/code-review",
        "engineering/debugging",
        "product",
        "agent",
        "archive",
    ];
    for sub in &standard_dirs {
        if dry_run {
            if !canonical_root.join(sub).is_dir() {
                log_messages.push(format!(
                    "[dry-run] Would create standard directory '{}'",
                    sub
                ));
            }
            continue;
        }
        fs::create_dir_all(canonical_root.join(sub))
            .map_err(|e| format!("Failed to create standard directory '{}': {e}", sub))?;
    }

    // 1. 递归扫描 Markdown 文件并收集需要处理的列表
    // 我们用广度优先/手动队列进行递归扫描以避免递归溢出且保持对符号链接等安全的处理
    let mut queue = vec![canonical_root.clone()];
    let mut md_paths = Vec::new();
    while let Some(current_dir) = queue.pop() {
        let entries = fs::read_dir(&current_dir)
            .map_err(|e| format!("Failed to read dir {}: {e}", current_dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)
                .map_err(|e| format!("Failed to inspect path {}: {e}", path.display()))?;
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                // 排除 archive
                if is_archive_dir(&path) {
                    continue;
                }
                queue.push(path);
            } else if meta.is_file() && is_markdown_file(&path) {
                // 排除根目录白名单
                let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if path.parent() == Some(&canonical_root) && whitelist.contains(&filename) {
                    continue;
                }
                md_paths.push(path);
            }
        }
    }
    md_paths.sort();

    // 记录标准分类目录前缀
    let standard_subdirs = ["engineering/", "product/", "agent/"];

    // 2. 对扫描出的每个 Markdown 执行 Frontmatter 解析、分类和物理移动
    for path in md_paths {
        let relative_path = path
            .strip_prefix(&canonical_root)
            .map_err(|e| format!("Strip prefix failed: {e}"))?;
        let relative_str = relative_path.to_string_lossy().replace('\\', "/");

        let content = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read file {}: {e}", path.display()))?;

        let (fm_opt, body) = parse_frontmatter(&content);

        // 检查 organize 逃生舱
        if let Some(ref fm) = fm_opt {
            if fm.organize == Some(false) {
                log_messages.push(format!(
                    "Skipped '{}': organize is explicitly set to false",
                    relative_str
                ));

                // Task sync on organize:false files is best-effort — errors are
                // logged but must not abort the remaining file scan.
                let (new_body, task_modified) = sync_tasks_in_content(server, body);
                if task_modified {
                    if dry_run {
                        log_messages.push(format!(
                            "[dry-run] Would sync task checkmarks in-place: '{}'",
                            relative_str
                        ));
                        synced_count += 1;
                        continue;
                    }
                    let new_content = if let Some(ref fm) = fm_opt {
                        format!("{}{}", serialize_frontmatter(fm), new_body)
                    } else {
                        new_body
                    };
                    let tmp = path.with_extension("md.tmp");
                    if let Err(e) =
                        fs::write(&tmp, &new_content).and_then(|_| fs::rename(&tmp, &path))
                    {
                        log_messages.push(format!(
                            "WARN: task sync write failed for '{}': {e}",
                            relative_str
                        ));
                    } else {
                        synced_count += 1;
                    }
                }
                continue;
            }
        }

        // 确定该文件当前是否已经在标准分类目录中
        let is_already_categorized = standard_subdirs
            .iter()
            .any(|sub| relative_str.starts_with(sub));

        // 提取或预测分类
        let (target_category_path, title, summary) = if let Some(ref fm) = fm_opt {
            if let Some(ref cat) = fm.category {
                let cat_rel = cat.strip_prefix("docs/").unwrap_or(cat);
                if standard_subdirs
                    .iter()
                    .any(|sub| cat_rel.starts_with(sub) || format!("{}/", cat_rel).starts_with(sub))
                {
                    // 使用已有的合法 category
                    let standard_cat = format!("docs/{}", cat_rel);
                    (
                        standard_cat,
                        fm.title.clone().unwrap_or_else(|| {
                            relative_path
                                .file_stem()
                                .unwrap()
                                .to_string_lossy()
                                .to_string()
                        }),
                        fm.summary.clone().unwrap_or_default(),
                    )
                } else {
                    // 原 category 不合法，调用 LLM
                    classify_and_extract_metadata(server, &relative_str, &content).await
                }
            } else {
                classify_and_extract_metadata(server, &relative_str, &content).await
            }
        } else {
            classify_and_extract_metadata(server, &relative_str, &content).await
        };

        // 解析标准分类路径为相对于 docs/ 的路径
        // target_category_path 类似 "docs/engineering/architecture"
        let dest_rel_dir = target_category_path
            .strip_prefix("docs/")
            .unwrap_or(&target_category_path);

        // 构造目标物理路径
        let filename = path.file_name().ok_or("Invalid filename")?;
        let dest_dir = secure_join(&canonical_root, dest_rel_dir)?;
        let dest_path = dest_dir.join(filename);

        // 确保目的地目录的父目录存在
        if !dry_run {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    format!("Failed to create directory '{}': {e}", parent.display())
                })?;
            }
        }

        let dest_rel_path = format!("{}/{}", dest_rel_dir, filename.to_string_lossy());

        // 更新/写入 Frontmatter
        let mut fm = fm_opt.unwrap_or(Frontmatter {
            title: Some(title),
            summary: Some(summary),
            category: Some(dest_rel_dir.to_string()),
            organize: Some(true),
            other_fields: Vec::new(),
        });

        // 保证 category 正确且同步
        fm.category = Some(dest_rel_dir.to_string());

        // 就地任务状态检测与勾选
        let (new_body, task_modified) = sync_tasks_in_content(server, body);
        let final_content = format!("{}{}", serialize_frontmatter(&fm), new_body);

        if task_modified {
            synced_count += 1;
        }

        // 如果物理路径不需要移动 (即已经在标准目录，且目的地一致)
        if is_already_categorized && path == dest_path {
            if dry_run {
                if final_content != content {
                    log_messages.push(format!(
                        "[dry-run] Would sync tasks/frontmatter in-place: '{}'",
                        relative_str
                    ));
                }
                continue;
            }
            // 只写入可能更新后的内容（就地勾选/Frontmatter 补齐）
            fs::write(&path, &final_content)
                .map_err(|e| format!("Failed to update file {}: {e}", path.display()))?;
            log_messages.push(format!(
                "Synced tasks/frontmatter in-place: '{}'",
                relative_str
            ));
        } else {
            if dry_run {
                if dest_path.exists() {
                    let mtime_src = fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    let mtime_dest = fs::metadata(&dest_path)
                        .and_then(|m| m.modified())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    if mtime_src >= mtime_dest {
                        log_messages.push(format!(
                            "[dry-run] Would move (newer) '{}' to '{}' and archive older destination",
                            relative_str, dest_rel_path
                        ));
                    } else {
                        log_messages.push(format!(
                            "[dry-run] Would archive '{}' to 'archive/' (destination '{}' is newer)",
                            relative_str, dest_rel_path
                        ));
                    }
                } else {
                    log_messages.push(format!(
                        "[dry-run] Would move '{}' to '{}'",
                        relative_str, dest_rel_path
                    ));
                }
                moved_count += 1;
                continue;
            }
            // 处理物理移动与同名冲突
            if dest_path.exists() {
                // 读修改时间 (mtime)
                let meta_src = fs::metadata(&path).map_err(|e| e.to_string())?;
                let meta_dest = fs::metadata(&dest_path).map_err(|e| e.to_string())?;

                let mtime_src = meta_src.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                let mtime_dest = meta_dest.modified().unwrap_or(SystemTime::UNIX_EPOCH);

                let archive_dir = canonical_root.join("archive");
                fs::create_dir_all(&archive_dir).map_err(|e| {
                    format!(
                        "Failed to create archive directory '{}': {e}",
                        archive_dir.display()
                    )
                })?;
                let stem = path.file_stem().unwrap().to_string_lossy().to_string();

                if mtime_src >= mtime_dest {
                    // 源文件（当前文件）更新，覆盖 dest，并将 dest 上的旧文件移至 archive
                    let (archive_path, archive_filename) =
                        unique_archive_target(&archive_dir, &stem);

                    fs::rename(&dest_path, &archive_path).map_err(|e| {
                        format!(
                            "Failed to archive older file '{}' -> '{}': {e}",
                            dest_path.display(),
                            archive_path.display()
                        )
                    })?;

                    // Atomic write: write to temp file then rename to avoid corruption on crash
                    let tmp_dest = dest_path.with_extension("md.tmp");
                    fs::write(&tmp_dest, &final_content).map_err(|e| {
                        format!("Failed to write temp file {}: {e}", tmp_dest.display())
                    })?;
                    fs::rename(&tmp_dest, &dest_path).map_err(|e| {
                        format!(
                            "Failed to rename {} -> {}: {e}",
                            tmp_dest.display(),
                            dest_path.display()
                        )
                    })?;
                    fs::remove_file(&path).map_err(|e| {
                        format!("Failed to remove source file {}: {e}", path.display())
                    })?;

                    log_messages.push(format!(
                        "Moved (newer) '{}' to '{}' (archived older to 'archive/{}')",
                        relative_str, dest_rel_path, archive_filename
                    ));
                } else {
                    // 目的地文件更新，放弃移动源文件，而是直接把源文件归档
                    let (archive_path, archive_filename) =
                        unique_archive_target(&archive_dir, &stem);

                    // Atomic write to archive
                    let tmp_archive = archive_path.with_extension("md.tmp");
                    fs::write(&tmp_archive, &final_content)
                        .map_err(|e| format!("Failed to write archive temp: {e}"))?;
                    fs::rename(&tmp_archive, &archive_path)
                        .map_err(|e| format!("Failed to rename archive temp: {e}"))?;
                    fs::remove_file(&path)
                        .map_err(|e| format!("Failed to remove source file after archive: {e}"))?;

                    log_messages.push(format!(
                        "Archived older '{}' directly to 'archive/{}' (destination '{}' was newer)",
                        relative_str, archive_filename, dest_rel_path
                    ));
                }
            } else {
                // 无同名冲突，直接写新路径，删旧路径
                fs::write(&dest_path, &final_content).map_err(|e| {
                    format!(
                        "Failed to write to destination {}: {e}",
                        dest_path.display()
                    )
                })?;
                fs::remove_file(&path)
                    .map_err(|e| format!("Failed to remove source file {}: {e}", path.display()))?;
                log_messages.push(format!("Moved '{}' to '{}'", relative_str, dest_rel_path));
            }
            moved_count += 1;
        }
    }

    // 3. 重新扫描标准分类目录，生成 _index.md
    let mut index_lines = Vec::new();
    index_lines.push("# Tachi Workspace Documents Index".to_string());
    index_lines.push(format!("*Last Updated: {}*\n", Utc::now().to_rfc3339()));
    index_lines.push(
        "This document tree is automatically maintained by Tachi. Do not edit manually.\n"
            .to_string(),
    );

    // 收集所有标准分类目录下的文件以建目录树
    // 标准子目录包括：engineering/architecture, engineering/devops, engineering/code-review, engineering/debugging, product, agent
    let categories = [
        (
            "Engineering: Architecture Specifications",
            "engineering/architecture",
        ),
        ("Engineering: DevOps & SOP Playbooks", "engineering/devops"),
        (
            "Engineering: Code Reviews & Decisions",
            "engineering/code-review",
        ),
        (
            "Engineering: Debugging & Post-mortems",
            "engineering/debugging",
        ),
        ("Product Roadmaps & PRDs", "product"),
        ("Agent Identities & Handoffs", "agent"),
    ];

    for (title, rel_dir) in &categories {
        let dir = canonical_root.join(rel_dir);
        if dir.is_dir() {
            let paths = scan_md_files_recursive(&dir);

            if !paths.is_empty() {
                index_lines.push(format!("## {}", title));
                for path in paths {
                    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    let relative_url = path
                        .strip_prefix(&canonical_root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");

                    let file_content = match fs::read_to_string(&path) {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!("docs_ops: failed to read {}: {e}", path.display());
                            continue;
                        }
                    };
                    let (fm_opt, _) = parse_frontmatter(&file_content);

                    let doc_title = fm_opt
                        .as_ref()
                        .and_then(|f| f.title.clone())
                        .unwrap_or_else(|| filename.replace(".md", ""));
                    let doc_summary = fm_opt
                        .as_ref()
                        .and_then(|f| f.summary.clone())
                        .unwrap_or_default();

                    if doc_summary.is_empty() {
                        index_lines.push(format!("- [{}](<{}>)", doc_title, relative_url));
                    } else {
                        index_lines.push(format!(
                            "- [{}](<{}>) - {}",
                            doc_title, relative_url, doc_summary
                        ));
                    }
                }
                index_lines.push("".to_string());
            }
        }
    }

    let index_content = index_lines.join("\n");
    let index_path = canonical_root.join("_index.md");
    if dry_run {
        let existing = fs::read_to_string(&index_path).unwrap_or_default();
        if existing != index_content {
            log_messages.push("[dry-run] Would rebuild docs/_index.md".to_string());
        }
    } else if let Err(e) = fs::write(&index_path, &index_content) {
        log_messages.push(format!("WARN: failed to write _index.md: {e}"));
    }

    let result = json!({
        "status": "success",
        "dry_run": dry_run,
        "moved_files": moved_count,
        "synced_tasks": synced_count,
        "log": log_messages,
    });

    Ok(serde_json::to_string_pretty(&result).unwrap())
}
