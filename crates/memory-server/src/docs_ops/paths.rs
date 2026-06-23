use chrono::Utc;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// 校验路径是否逃逸出 docs 根目录，并安全规范化
pub(super) fn secure_join(root: &Path, rel_part: &str) -> Result<PathBuf, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize root: {e}"))?;

    // 检查是否有符号链接或 .. 跨越
    let mut cursor = root.to_path_buf();
    for component in Path::new(rel_part).components() {
        match component {
            Component::Normal(part) => {
                cursor.push(part);
                if let Ok(meta) = fs::symlink_metadata(&cursor) {
                    if meta.file_type().is_symlink() {
                        return Err(format!(
                            "Symlink traversal detected at '{}'",
                            cursor.display()
                        ));
                    }
                }
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("Escape path validation failed for '{}'", rel_part));
            }
        }
    }

    // 如果目标目录已存在，检查 canonical 关系
    if cursor.exists() {
        let canonical_cursor = cursor
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize target path: {e}"))?;
        if !canonical_cursor.starts_with(&canonical_root) {
            return Err(format!(
                "Path '{}' escapes root '{}'",
                cursor.display(),
                root.display()
            ));
        }
    }

    // Final safeguard: verify the resolved path doesn't escape via any TOCTOU race
    // For new (non-existent) files, canonicalize the parent directory instead
    if let Some(parent) = cursor.parent() {
        if let Ok(canonical_parent) = parent.canonicalize() {
            if canonical_parent != canonical_root && !canonical_parent.starts_with(&canonical_root)
            {
                return Err(format!(
                    "Resolved path '{}' escapes root after canonicalize",
                    cursor.display()
                ));
            }
        }
    }

    Ok(cursor)
}

pub(super) fn is_archive_dir(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "archive")
}

fn is_hidden_or_config_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.starts_with('.')
                || matches!(
                    name,
                    "node_modules"
                        | "__pycache__"
                        | ".git"
                        | ".github"
                        | ".claude"
                        | ".cursor"
                        | ".gemini"
                        | ".codex"
                        | ".vscode"
                        | ".idea"
                )
        })
}

pub(super) fn is_markdown_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

/// 递归扫描指定目录下的所有 Markdown 文件，排除 archive 目录并按字典序排序
pub(super) fn scan_md_files_recursive(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut queue = vec![dir.to_path_buf()];
    while let Some(current_dir) = queue.pop() {
        if let Ok(entries) = fs::read_dir(&current_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                let Ok(meta) = fs::symlink_metadata(&path) else {
                    continue;
                };
                if meta.file_type().is_symlink() {
                    continue;
                }
                if meta.is_dir() {
                    if is_archive_dir(&path) || is_hidden_or_config_dir(&path) {
                        continue;
                    }
                    queue.push(path);
                } else if meta.is_file() && is_markdown_file(&path) {
                    files.push(path);
                }
            }
        }
    }
    files.sort();
    files
}

pub(super) fn unique_archive_target(archive_dir: &Path, stem: &str) -> (PathBuf, String) {
    let timestamp = Utc::now().timestamp_millis();
    for suffix in 0..1000 {
        let filename = if suffix == 0 {
            format!("{}.{}.md", stem, timestamp)
        } else {
            format!("{}.{}.{}.md", stem, timestamp, suffix)
        };
        let path = archive_dir.join(&filename);
        if !path.exists() {
            return (path, filename);
        }
    }
    let filename = format!(
        "{}.{}.{}.md",
        stem,
        timestamp,
        uuid::Uuid::new_v4().as_simple()
    );
    (archive_dir.join(&filename), filename)
}
